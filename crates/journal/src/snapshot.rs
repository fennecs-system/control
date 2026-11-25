//! A JSON document persisted as a snapshot plus a log of patches.
//!
//! A save is a pure function of two values: [`diff`] compares the previous
//! [`Encoded`] document with the new one and returns only the edits between
//! them, so persisting a change costs the size of the change, not of the
//! document. Each saved patch is one line of a [`crate::FileJournal`]; replaying
//! a valid prefix of those lines over the snapshot ([`apply`]) rebuilds the
//! latest document. Compaction writes a fresh snapshot and starts a new log.
//!
//! The snapshot is the document itself with two extra top-level members,
//! `_snapshot` (a nonce shared with its log's header line) and `_schema` (see
//! [`crate::lens`]). Readers that do not know them ignore them, so a snapshot is
//! still readable as a plain document. A log whose header names a different
//! nonce predates the snapshot and is ignored: the snapshot already holds it.
//!
//! [`encode`] serializes once, recording the byte ranges of members and
//! elements inside containers of at least [`SEGMENT`] bytes. [`diff`] compares
//! those ranges byte for byte and descends only into ranges that changed, so
//! nothing is parsed or cloned. Edits name their location by path (object keys
//! and array indices), in the spirit of JSON Patch:
//!
//! * `["s", path, value]` sets the value at `path` (adding an object member);
//! * `["r", path]` removes an object member;
//! * `["a", path, at, delete, [values…]]` splices the array at `path`.
use crate::lens::Schema;
use serde::Serialize;
use serde_json::{Value, ser::Formatter};
use std::{borrow::Cow, cell::Cell, io, path::Path, rc::Rc};

mod parts;
use parts::At;
#[cfg(debug_assertions)]
pub use parts::fresh;
pub use parts::{Part, Tree, offer, split, split_each_save};

/// Containers smaller than this are rewritten whole when they change; larger
/// ones are diffed member by member.
pub const SEGMENT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Node {
    start: u32,
    end: u32,
    /// The member's key as raw JSON string bytes; empty for array elements.
    key: (u32, u32),
    /// One past this node's last recorded descendant. Children of node `i`
    /// start at `i + 1`; each next sibling starts at the previous one's end.
    subtree_end: u32,
}

/// A document's compact JSON and the ranges of its larger parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Encoded {
    bytes: Vec<u8>,
    nodes: Vec<Node>,
}
impl Encoded {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn range(&self, node: usize) -> &[u8] {
        let node = self.nodes[node];
        &self.bytes[node.start as usize..node.end as usize]
    }
    fn key(&self, node: usize) -> &[u8] {
        let (start, end) = self.nodes[node].key;
        &self.bytes[start as usize..end as usize]
    }
    fn children(&self, node: usize) -> Vec<usize> {
        let mut children = Vec::new();
        let mut child = node + 1;
        while child < self.nodes[node].subtree_end as usize {
            children.push(child);
            child = self.nodes[child].subtree_end as usize;
        }
        children
    }
    fn container(&self, node: usize) -> Option<u8> {
        (self.nodes[node].subtree_end as usize > node + 1).then(|| self.bytes[self.nodes[node].start as usize])
    }
}

struct Tracked {
    out: Vec<u8>,
    at: Rc<Cell<usize>>,
}
impl io::Write for Tracked {
    #[inline]
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_all(bytes)?;
        Ok(bytes.len())
    }
    #[inline]
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.extend_from_slice(bytes);
        self.at.set(self.out.len());
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Ranges {
    at: Rc<Cell<usize>>,
    nodes: Vec<Node>,
    open: Vec<usize>,
    key: (usize, usize),
}
impl Ranges {
    fn open(&mut self, key: (usize, usize)) {
        self.open.push(self.nodes.len());
        self.nodes.push(Node {
            start: self.at.get() as u32,
            end: 0,
            key: (key.0 as u32, key.1 as u32),
            subtree_end: 0,
        });
    }
    fn close(&mut self) {
        let index = self.open.pop().expect("balanced JSON");
        let end = self.at.get();
        if end - (self.nodes[index].start as usize) < SEGMENT {
            self.nodes.truncate(index + 1);
        }
        self.nodes[index].end = end as u32;
        self.nodes[index].subtree_end = self.nodes.len() as u32;
    }
}
/// serde_json takes its formatter by value; this lends it the ranges.
struct Lent<'a>(&'a mut Ranges);
impl Formatter for Lent<'_> {
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if !first {
            writer.write_all(b",")?;
        }
        self.0.open((0, 0));
        Ok(())
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.0.close();
        Ok(())
    }
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if !first {
            writer.write_all(b",")?;
        }
        self.0.key.0 = self.0.at.get();
        Ok(())
    }
    fn end_object_key<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.0.key.1 = self.0.at.get();
        Ok(())
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b":")?;
        self.0.open(self.0.key);
        Ok(())
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.0.close();
        Ok(())
    }
}

/// Serializes `value` as compact JSON, the same bytes `serde_json::to_vec`
/// writes, with the ranges [`diff`] needs.
pub fn encode<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Encoded> {
    let at = Rc::new(Cell::new(0));
    let mut ranges = Ranges {
        at: at.clone(),
        nodes: Vec::new(),
        open: Vec::new(),
        key: (0, 0),
    };
    ranges.open((0, 0));
    let mut writer = Tracked { out: Vec::new(), at };
    let mut serializer = serde_json::Serializer::with_formatter(&mut writer, Lent(&mut ranges));
    value.serialize(&mut serializer)?;
    ranges.close();
    if u32::try_from(writer.out.len()).is_err() {
        return Err(serde::ser::Error::custom("documents are limited to 4 GiB"));
    }
    // Kept until the value changes (a part's encoding), so without the
    // slack the buffers grew by.
    writer.out.shrink_to_fit();
    ranges.nodes.shrink_to_fit();
    Ok(Encoded {
        bytes: writer.out,
        nodes: ranges.nodes,
    })
}

/// What [`encode`] gives for an object whose members (raw JSON keys and
/// encoded values) were encoded one by one: their bytes joined and their
/// ranges moved, so nothing is serialized twice.
fn object(members: Vec<(Box<[u8]>, Encoded)>) -> serde_json::Result<Encoded> {
    let length = 1 + members
        .iter()
        .map(|(key, value)| key.len() + 1 + value.bytes.len() + 1)
        .sum::<usize>()
        .max(1);
    if u32::try_from(length).is_err() {
        return Err(serde::ser::Error::custom("documents are limited to 4 GiB"));
    }
    let mut bytes = Vec::with_capacity(length);
    let mut nodes = vec![Node {
        start: 0,
        end: 0,
        key: (0, 0),
        subtree_end: 0,
    }];
    bytes.push(b'{');
    for (i, (key, value)) in members.into_iter().enumerate() {
        if i > 0 {
            bytes.push(b',');
        }
        let named = (bytes.len() as u32, (bytes.len() + key.len()) as u32);
        bytes.extend_from_slice(&key);
        bytes.push(b':');
        let (offset, base) = (bytes.len() as u32, nodes.len() as u32);
        bytes.extend_from_slice(&value.bytes);
        // An array element's key is the empty (0, 0) wherever it is.
        let moved = |key: (u32, u32)| {
            if key == (0, 0) {
                key
            } else {
                (key.0 + offset, key.1 + offset)
            }
        };
        nodes.extend(value.nodes.iter().enumerate().map(|(j, node)| Node {
            start: node.start + offset,
            end: node.end + offset,
            key: if j == 0 { named } else { moved(node.key) },
            subtree_end: node.subtree_end + base,
        }));
    }
    bytes.push(b'}');
    if bytes.len() < SEGMENT {
        nodes.truncate(1);
    }
    nodes[0].end = bytes.len() as u32;
    nodes[0].subtree_end = nodes.len() as u32;
    Ok(Encoded { bytes, nodes })
}

/// One compact JSON value kept as is, without the ranges of its members: an
/// encoding only held (its bytes saved elsewhere), never diffed member by
/// member. Diffed, it is one value. `bytes` must be one valid JSON value.
pub fn kept(bytes: Vec<u8>) -> io::Result<Encoded> {
    let end = u32::try_from(bytes.len()).map_err(|_| invalid("documents are limited to 4 GiB"))?;
    Ok(Encoded {
        bytes,
        nodes: vec![Node {
            start: 0,
            end,
            key: (0, 0),
            subtree_end: 1,
        }],
    })
}

/// The ranges of a compact JSON document already in hand, such as a snapshot
/// just read: the same [`Encoded`] that [`encode`] would build from the value
/// those bytes serialize, without serializing it. `bytes` must be one valid
/// JSON value (callers have parsed it); anything else is an error, not a panic.
pub fn index(bytes: Vec<u8>) -> io::Result<Encoded> {
    if u32::try_from(bytes.len()).is_err() {
        return Err(invalid("documents are limited to 4 GiB"));
    }
    let mut ranges = Ranges {
        at: Rc::new(Cell::new(0)),
        nodes: Vec::new(),
        open: Vec::new(),
        key: (0, 0),
    };
    ranges.open((0, 0));
    let end = scan(&bytes, 0, &mut ranges, 0)?;
    if end != bytes.len() {
        return Err(invalid("Trailing bytes after the document"));
    }
    ranges.at.set(end);
    ranges.close();
    ranges.nodes.shrink_to_fit();
    Ok(Encoded {
        bytes,
        nodes: ranges.nodes,
    })
}
/// Scans the value at `i`, recording members and elements like the encoder,
/// and returns where it ends.
fn scan(bytes: &[u8], mut i: usize, ranges: &mut Ranges, depth: usize) -> io::Result<usize> {
    let truncated = || invalid("Truncated JSON");
    let byte = |i: usize| bytes.get(i).copied().ok_or_else(truncated);
    if depth > 512 {
        return Err(invalid("JSON nested too deeply"));
    }
    let string = |mut i: usize| -> io::Result<usize> {
        i += 1;
        loop {
            match byte(i)? {
                b'\\' => i += 2,
                b'"' => return Ok(i + 1),
                _ => i += 1,
            }
        }
    };
    match byte(i)? {
        open @ (b'{' | b'[') => {
            let close = if open == b'{' { b'}' } else { b']' };
            i += 1;
            if byte(i)? == close {
                return Ok(i + 1);
            }
            loop {
                let key = if open == b'{' {
                    let start = i;
                    i = string(i)?;
                    let key = (start, i);
                    if byte(i)? != b':' {
                        return Err(invalid("Expected a colon"));
                    }
                    i += 1;
                    key
                } else {
                    (0, 0)
                };
                ranges.at.set(i);
                ranges.open(key);
                i = scan(bytes, i, ranges, depth + 1)?;
                ranges.at.set(i);
                ranges.close();
                match byte(i)? {
                    b',' => i += 1,
                    b if b == close => return Ok(i + 1),
                    _ => return Err(invalid("Expected a comma or a closing bracket")),
                }
            }
        }
        b'"' => string(i),
        _ => {
            // A number or literal: up to the next separator or the end.
            Ok(bytes[i..]
                .iter()
                .position(|b| matches!(b, b',' | b']' | b'}'))
                .map_or(bytes.len(), |n| i + n))
        }
    }
}

enum Seg {
    Key(Vec<u8>),
    Index(usize),
}
fn path(out: &mut Vec<u8>, path: &[Seg]) {
    out.push(b'[');
    for (i, seg) in path.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        match seg {
            Seg::Key(key) => out.extend_from_slice(key),
            Seg::Index(index) => out.extend_from_slice(index.to_string().as_bytes()),
        }
    }
    out.push(b']');
}
fn op(out: &mut Vec<u8>, kind: &[u8], at: &[Seg]) {
    if out.len() > 1 {
        out.push(b',');
    }
    out.push(b'[');
    out.extend_from_slice(kind);
    path(out, at);
}

/// The edits that turn `old` into `new`, as one JSON array; `None` when the
/// documents are identical. Pure: neither input changes.
pub fn diff(old: &Encoded, new: &Encoded) -> Option<Vec<u8>> {
    edits_between(At::Node(old, 0), At::Node(new, 0))
}
/// [`diff`] of two documents held as [`Tree`]s: the same bytes as diffing
/// their joined encodings, but parts both trees share are skipped by pointer
/// without being read, so a save costs what changed.
pub fn diff_trees(old: &Tree, new: &Tree) -> Option<Vec<u8>> {
    edits_between(old.at(), new.at())
}
fn edits_between(old: At<'_>, new: At<'_>) -> Option<Vec<u8>> {
    let mut out = vec![b'['];
    let mut at = Vec::new();
    node(old, new, &mut at, &mut out);
    if out.len() == 1 {
        return None;
    }
    out.push(b']');
    Some(out)
}
fn node(old: At<'_>, new: At<'_>, at: &mut Vec<Seg>, out: &mut Vec<u8>) {
    if old.equals(new) {
        return;
    }
    // A value encoded from saved bytes has no recorded members: index those
    // bytes to diff inside it rather than rewrite it whole.
    let opening = |value: At<'_>| Some(value.first()).filter(|b| matches!(b, b'{' | b'['));
    if opening(old).is_some() && opening(old) == opening(new) {
        let inside = |value: At<'_>| {
            index(value.bytes().into_owned())
                .ok()
                .filter(|local| local.container(0).is_some())
        };
        match (old.container(), new.container()) {
            (None, Some(_)) => {
                if let Some(local) = inside(old) {
                    return node(At::Node(&local, 0), new, at, out);
                }
            }
            (Some(_), None) => {
                if let Some(local) = inside(new) {
                    return node(old, At::Node(&local, 0), at, out);
                }
            }
            (None, None) => {
                if let (Some(before), Some(after)) = (inside(old), inside(new)) {
                    return node(At::Node(&before, 0), At::Node(&after, 0), at, out);
                }
            }
            _ => {}
        }
    }
    match (old.container(), new.container()) {
        (Some(b'{'), Some(b'{')) => {
            let previous = old.children();
            let mut seen = vec![false; previous.len()];
            let mut cursor = 0;
            for (key, child) in new.children() {
                // Members usually keep their order; scan from the last match.
                let found = (cursor..previous.len())
                    .chain(0..cursor)
                    .find(|&i| previous[i].0 == key);
                at.push(Seg::Key(key.to_vec()));
                match found {
                    Some(i) => {
                        seen[i] = true;
                        cursor = i + 1;
                        node(previous[i].1, child, at, out);
                    }
                    None => set(out, at, child),
                }
                at.pop();
            }
            for ((key, _), _) in previous.iter().zip(&seen).filter(|(_, seen)| !**seen) {
                at.push(Seg::Key(key.to_vec()));
                op(out, b"\"r\",", at);
                out.push(b']');
                at.pop();
            }
        }
        (Some(b'['), Some(b'[')) => {
            let a: Vec<At<'_>> = old.children().into_iter().map(|(_, child)| child).collect();
            let b: Vec<At<'_>> = new.children().into_iter().map(|(_, child)| child).collect();
            let prefix = a.iter().zip(&b).take_while(|&(x, y)| x.equals(*y)).count();
            let limit = a.len().min(b.len()) - prefix;
            let suffix = a
                .iter()
                .rev()
                .zip(b.iter().rev())
                .take(limit)
                .take_while(|&(x, y)| x.equals(*y))
                .count();
            // Anchor the middles on elements that are byte-identical on both
            // sides, in order; between anchors, pair what remains positionally
            // (the same element changed) and delete or insert the rest. Runs
            // leaving from anywhere in the array then cost their removal, not a
            // rewrite of everything that shifted. Edits name positions in the
            // array as already edited, so they apply in order.
            let (a_end, b_end) = (a.len() - suffix, b.len() - suffix);
            let mut olds: std::collections::HashMap<Cow<'_, [u8]>, Vec<usize>> = std::collections::HashMap::new();
            for (i, child) in a.iter().enumerate().take(a_end).skip(prefix) {
                olds.entry(child.bytes()).or_default().push(i);
            }
            let mut anchors = Vec::new();
            let mut next = prefix;
            for (j, child) in b.iter().enumerate().take(b_end).skip(prefix) {
                if let Some(&i) = olds
                    .get(&*child.bytes())
                    .and_then(|list| list.iter().find(|&&i| i >= next))
                {
                    anchors.push((i, j));
                    next = i + 1;
                }
            }
            anchors.push((a_end, b_end));
            let (mut i, mut j) = (prefix, prefix);
            for (anchor_a, anchor_b) in anchors {
                let paired = (anchor_a - i).min(anchor_b - j);
                for t in 0..paired {
                    at.push(Seg::Index(j + t));
                    node(a[i + t], b[j + t], at, out);
                    at.pop();
                }
                if anchor_a - i != anchor_b - j {
                    splice(out, at, j + paired, anchor_a - i - paired, &b[j + paired..anchor_b]);
                }
                (i, j) = (anchor_a + 1, anchor_b + 1);
            }
        }
        _ => set(out, at, new),
    }
}
fn splice(out: &mut Vec<u8>, at: &[Seg], position: usize, delete: usize, insert: &[At<'_>]) {
    op(out, b"\"a\",", at);
    out.extend_from_slice(format!(",{position},{delete},[").as_bytes());
    for (i, child) in insert.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        child.write(out);
    }
    out.extend_from_slice(b"]]");
}
fn set(out: &mut Vec<u8>, at: &[Seg], value: At<'_>) {
    op(out, b"\"s\",", at);
    out.push(b',');
    value.write(out);
    out.push(b']');
}

/// One edit, parsed: what [`apply`] and [`crate::lens`] work on.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub path: Vec<Value>,
    pub change: Change,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Set(Value),
    Remove,
    Splice {
        at: usize,
        delete: usize,
        insert: Vec<Value>,
    },
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
/// One save as one log line: `{"sum":"<fnv-1a 64>","ops":[…]}`. The line is
/// the transaction record and its newline the commit marker: recovery applies
/// a line whole or not at all, and the checksum rejects a line that a crash
/// left complete in length but not in content.
pub fn commit(ops: &[u8]) -> Vec<u8> {
    let mut line = format!(r#"{{"sum":"{:016x}","ops":"#, checksum(ops)).into_bytes();
    line.extend_from_slice(ops);
    line.push(b'}');
    line
}
/// The edits of a committed transaction line; an error for anything else.
pub fn transaction(line: &[u8]) -> io::Result<Vec<Edit>> {
    const PREFIX: usize = r#"{"sum":"0123456789abcdef","ops":"#.len();
    let torn = || invalid("Not a committed transaction");
    let (head, rest) = (line.get(..PREFIX).ok_or_else(torn)?, &line[PREFIX..]);
    let ops = rest.strip_suffix(b"}").ok_or_else(torn)?;
    let sum = head
        .strip_prefix(br#"{"sum":""#)
        .and_then(|h| h.strip_suffix(br#"","ops":"#))
        .ok_or_else(torn)?;
    if sum != format!("{:016x}", checksum(ops)).as_bytes() {
        return Err(torn());
    }
    edits(ops)
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
/// Parses one patch (the array [`diff`] returns) into edits.
pub fn edits(line: &[u8]) -> io::Result<Vec<Edit>> {
    let ops: Vec<Value> = serde_json::from_slice(line).map_err(|_| invalid("Patch is not a JSON array"))?;
    ops.into_iter()
        .map(|op| {
            let Value::Array(mut parts) = op else {
                return Err(invalid("Patch edit is not an array"));
            };
            let kind = parts.first().and_then(Value::as_str).map(str::to_owned);
            let Some(Value::Array(path)) = parts.get(1).cloned() else {
                return Err(invalid("Patch edit has no path"));
            };
            let change = match (kind.as_deref(), parts.len()) {
                (Some("s"), 3) => Change::Set(parts.pop().expect("three parts")),
                (Some("r"), 2) => Change::Remove,
                (Some("a"), 5) => {
                    let Some(Value::Array(insert)) = parts.pop() else {
                        return Err(invalid("Splice inserts an array"));
                    };
                    let delete = parts
                        .pop()
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| invalid("Splice delete count"))?;
                    let at = parts
                        .pop()
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| invalid("Splice position"))?;
                    Change::Splice {
                        at: at as usize,
                        delete: delete as usize,
                        insert,
                    }
                }
                _ => return Err(invalid("Unknown patch edit")),
            };
            Ok(Edit { path, change })
        })
        .collect()
}
fn child<'a>(value: &'a mut Value, seg: &Value) -> io::Result<&'a mut Value> {
    match (value, seg) {
        (Value::Object(map), Value::String(key)) => map.get_mut(key),
        (Value::Array(items), Value::Number(n)) => n.as_u64().and_then(|i| items.get_mut(i as usize)),
        _ => None,
    }
    .ok_or_else(|| invalid("Patch path does not exist"))
}
/// Applies edits in order. An edit that does not fit the document is an
/// error, never silently skipped: the log and snapshot disagree.
pub fn apply(document: &mut Value, edits: Vec<Edit>) -> io::Result<()> {
    for Edit { path, change } in edits {
        if let Change::Splice { at, delete, insert } = change {
            let target = path.iter().try_fold(&mut *document, |value, seg| child(value, seg))?;
            let Value::Array(items) = target else {
                return Err(invalid("Splice target is not an array"));
            };
            if at.checked_add(delete).is_none_or(|end| end > items.len()) {
                return Err(invalid("Splice outside the array"));
            }
            items.splice(at..at + delete, insert);
            continue;
        }
        let Some((last, parent)) = path.split_last() else {
            let Change::Set(value) = change else {
                return Err(invalid("Cannot remove the document"));
            };
            *document = value;
            continue;
        };
        let target = parent.iter().try_fold(&mut *document, |value, seg| child(value, seg))?;
        match (change, last, target) {
            (Change::Set(value), Value::String(key), Value::Object(map)) => {
                map.insert(key.clone(), value);
            }
            (Change::Remove, Value::String(key), Value::Object(map)) => {
                map.remove(key)
                    .ok_or_else(|| invalid("Removed member does not exist"))?;
            }
            (Change::Set(value), Value::Number(n), Value::Array(items)) => {
                *n.as_u64()
                    .and_then(|i| items.get_mut(i as usize))
                    .ok_or_else(|| invalid("Patch index outside the array"))? = value;
            }
            _ => return Err(invalid("Patch path does not fit the document")),
        }
    }
    Ok(())
}

/// The header line naming the snapshot a log extends.
pub fn header(nonce: &str, schema: &Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"snapshot": nonce, "schema": schema})).expect("JSON values serialize")
}
/// Snapshot bytes: `document` with the nonce and schema as its first members.
/// `document` must be a JSON object.
pub fn snapshot(document: &[u8], nonce: &str, schema: &Value) -> Vec<u8> {
    let mut out = prefix(nonce, schema);
    let body = &document[1..];
    if body != b"}" {
        out.push(b',');
    }
    out.extend_from_slice(body);
    out
}
fn prefix(nonce: &str, schema: &Value) -> Vec<u8> {
    let mut out = br#"{"_snapshot":"#.to_vec();
    serde_json::to_writer(&mut out, nonce).expect("strings serialize");
    out.extend_from_slice(br#","_schema":"#);
    serde_json::to_writer(&mut out, schema).expect("JSON values serialize");
    out
}
/// The diff base a snapshot file already is: its document without the nonce
/// and schema members, indexed from the bytes rather than re-encoded. `None`
/// when the file is not a snapshot named `nonce` in `schema`.
pub fn base(snapshot_bytes: &[u8], nonce: &str, schema: &Value) -> Option<Encoded> {
    let rest = snapshot_bytes.strip_prefix(prefix(nonce, schema).as_slice())?;
    let body = match rest {
        b"}" => b"{}".to_vec(),
        [b',', member @ ..] => [b"{", member].concat(),
        _ => return None,
    };
    index(body).ok()
}
/// Splits the nonce and schema members off a parsed snapshot.
pub fn identify(document: &mut Value) -> (Option<String>, Option<Value>) {
    let Value::Object(map) = document else {
        return (None, None);
    };
    let nonce = map.remove("_snapshot").and_then(|v| v.as_str().map(str::to_owned));
    (nonce, map.remove("_schema"))
}

/// The header schema and patches of a log that extends the snapshot `nonce`,
/// stopping at the first line that does not parse: a torn or corrupt tail is
/// dropped, never half-applied. `None` when the log belongs to another
/// snapshot (or is empty). Lines are counted with the header, so a writer can
/// truncate to `patches.len() + 1`.
pub fn patches(lines: impl IntoIterator<Item = Vec<u8>>, nonce: &str) -> Option<(Schema, Vec<Vec<Edit>>)> {
    #[derive(serde::Deserialize)]
    struct Header {
        snapshot: String,
        #[serde(default)]
        schema: Schema,
    }
    let mut lines = lines.into_iter();
    let header: Header = serde_json::from_slice(&lines.next()?).ok()?;
    if header.snapshot != nonce {
        return None;
    }
    let patches = lines.map_while(|line| transaction(&line).ok()).collect();
    Some((header.schema, patches))
}

/// The current document for a process that does not own the state: the
/// snapshot plus its log's valid lines, translated to `reader`'s schema. It
/// takes no lock and changes no file; `read` supplies the caller's checked
/// file reader. `None` when there is no snapshot.
pub fn read(
    snapshot: &Path,
    log: &Path,
    reader: &Schema,
    read: impl Fn(&Path) -> io::Result<Vec<u8>>,
) -> io::Result<Option<Value>> {
    let optional = |path: &Path| match read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        result => result.map(Some),
    };
    let mut previous = None;
    loop {
        let Some(bytes) = optional(snapshot)? else {
            return Ok(None);
        };
        let mut document: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("Snapshot is not JSON"))?;
        let (nonce, schema) = identify(&mut document);
        let schema: Schema = schema
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| invalid("Invalid snapshot schema"))?
            .unwrap_or_default();
        let log = optional(log)?.unwrap_or_default();
        let lines = complete_lines(&log);
        let found = nonce.as_deref().and_then(|nonce| patches(lines, nonce));
        // A compaction may have replaced the snapshot between the two reads:
        // look once more before deciding the log is obsolete.
        if found.is_none() && !log.is_empty() && previous.as_ref() != Some(&nonce) {
            previous = Some(nonce);
            continue;
        }
        crate::lens::document(&crate::lens::route(&schema, reader)?, &mut document);
        if let Some((schema, patches)) = found {
            let route = crate::lens::route(&schema, reader)?;
            for patch in patches {
                apply(
                    &mut document,
                    patch
                        .into_iter()
                        .filter_map(|edit| crate::lens::edit(&route, edit))
                        .collect(),
                )?;
            }
        }
        return Ok(Some(document));
    }
}
/// Whole lines only: a final line without its newline is still being written.
pub fn complete_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    bytes[..end]
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// A unique snapshot name: time, process and a per-process counter.
pub fn nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{time:x}-{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
#[path = "snapshot/tests.rs"]
mod tests;
