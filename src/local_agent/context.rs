//! A local turn's transcript seen through the same lens as a chat's history
//! (`control_comonad::cut`), at step granularity: the newest steps verbatim,
//! older steps in aligned power-of-two blocks that grow with distance and never
//! change once complete. Each request is rendered from the original steps:
//!
//! - outside the verbatim steps, a step is shrunk deterministically: tool
//!   results keep a short head and size marker, call arguments lose long
//!   string bodies (only the result matters later);
//! - only while the request is still over the compaction threshold, the oldest
//!   block is folded into a summary node on the local compactor. A block's
//!   summary is built from its children's summaries when its steps do not fit
//!   the compactor, as in the chat summary tree, and a folded block stays
//!   folded, so the rendered prefix is stable between steps;
//! - a summary cut at the output limit is retried with a tighter target, then
//!   replaced by a deterministic digest, labelled as such.
//!
//! The originals stay in this module's input and in the attempt's activity.
use super::{Compactor, Host};
use control_comonad::{Block, cut};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io, ops::Range};

/// Results at or under this many bytes are short enough to keep whole.
const RESULT_LONG: usize = 1200;
/// Bytes of an older result kept as its head.
const RESULT_HEAD: usize = 600;
/// Tool-call arguments at or under this many bytes are kept whole.
const ARGS_LONG: usize = 400;
/// Bytes kept of each long string inside an older call's arguments.
const ARG_HEAD: usize = 160;
/// A result repeating an argument string at least this long cites it instead.
const REPEAT: usize = 200;
/// Bytes of each item kept in a deterministic digest, and of a whole digest.
const DIGEST_TEXT: usize = 240;
const DIGEST_BYTES: usize = 2400;
/// Characters of each message given to the compactor as evidence.
const EVIDENCE_LINE: usize = 4000;

/// The longest prefix of `text` within `bytes`, preferring to end at a line.
fn head(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    match cut.rfind('\n') {
        Some(at) if at > bytes / 2 => &cut[..at],
        _ => cut,
    }
}
/// An older result reduced to its head plus a marker; short results unchanged.
fn shorten(text: &str) -> Option<String> {
    if text.len() <= RESULT_LONG {
        return None;
    }
    let kept = head(text, RESULT_HEAD);
    Some(format!("{kept}\n[Control elided {} more bytes of this earlier result to save context; the attempt's activity keeps it. Repeat a narrower call if you need it again.]", text.len() - kept.len()))
}
fn strip_strings(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > ARG_HEAD + 40 => {
            let kept = head(text, ARG_HEAD).to_owned();
            *text = format!("{kept}…[{} bytes elided]", text.len() - kept.len());
        }
        Value::Array(items) => items.iter_mut().for_each(strip_strings),
        Value::Object(map) => map.values_mut().for_each(strip_strings),
        _ => {}
    }
}
/// An older call's arguments with long string bodies cut to a head. The
/// result stays a JSON object, so chat templates that parse arguments accept it.
fn strip_arguments(arguments: &str) -> Option<String> {
    if arguments.len() <= ARGS_LONG {
        return None;
    }
    let mut value: Value = serde_json::from_str(arguments).ok()?;
    strip_strings(&mut value);
    let stripped = value.to_string();
    (stripped.len() < arguments.len()).then_some(stripped)
}
fn long_strings<'a>(value: &'a Value, key: &str, out: &mut Vec<(String, &'a str)>) {
    match value {
        Value::String(text) if text.len() >= REPEAT => out.push((key.to_owned(), text)),
        Value::Array(items) => items.iter().for_each(|item| long_strings(item, key, out)),
        Value::Object(map) => map.iter().for_each(|(k, v)| long_strings(v, k, out)),
        _ => {}
    }
}
/// `content` with verbatim copies of the call's long argument strings replaced
/// by a reference, so the same bytes are not carried twice.
pub(super) fn without_repeats(content: String, arguments: &Value) -> String {
    let mut strings = Vec::new();
    long_strings(arguments, "", &mut strings);
    strings.into_iter().fold(content, |content, (key, text)| {
        if content.contains(text) {
            content.replace(text, &format!("[the call's {key} argument, {} bytes]", text.len()))
        } else {
            content
        }
    })
}
/// One step shrunk: shortened results and stripped arguments.
fn lean(step: &[Value]) -> Vec<Value> {
    step.iter().cloned().map(|mut message| {
        if message["role"] == "tool" {
            if let Some(short) = message["content"].as_str().and_then(shorten) {
                message["content"] = Value::String(short);
            }
        } else if let Some(calls) = message["tool_calls"].as_array_mut() {
            for call in calls {
                if let Some(stripped) = call["function"]["arguments"].as_str().and_then(strip_arguments) {
                    call["function"]["arguments"] = Value::String(stripped);
                }
            }
        }
        message
    }).collect()
}
/// A model-free record of messages: what was called, the head of each result,
/// and any earlier summary, within `DIGEST_BYTES`.
fn digest(messages: &[Value]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        let text = message["content"].as_str().unwrap_or("");
        match message["role"].as_str().unwrap_or("?") {
            "tool" => lines.push(format!("  result ({} bytes): {}", text.len(), head(text, DIGEST_TEXT).replace('\n', " "))),
            "assistant" => {
                if !text.trim().is_empty() {
                    lines.push(format!("assistant: {}", head(text, DIGEST_TEXT)));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let arguments = call["function"]["arguments"].as_str().unwrap_or("");
                    lines.push(format!("called {} {}", call["function"]["name"].as_str().unwrap_or("?"), head(arguments, DIGEST_TEXT)));
                }
            }
            role => lines.push(format!("{role}: {}", head(text, DIGEST_BYTES / 2))),
        }
    }
    let all = lines.join("\n");
    if all.len() <= DIGEST_BYTES {
        return all;
    }
    let kept = head(&all, DIGEST_BYTES);
    format!("{kept}\n[{} more bytes of the digest omitted]", all.len() - kept.len())
}
fn evidence(messages: &[Value]) -> String {
    messages.iter().map(|m| {
        let mut line = format!("{}: {}", m["role"].as_str().unwrap_or("?"), m["content"].as_str().unwrap_or(""));
        for call in m["tool_calls"].as_array().into_iter().flatten() {
            line.push_str(&format!(" [call {} {}]", call["function"]["name"].as_str().unwrap_or(""), call["function"]["arguments"].as_str().unwrap_or("")));
        }
        line.chars().take(EVIDENCE_LINE).collect::<String>()
    }).collect::<Vec<_>>().join("\n")
}

/// The turn's steps: index ranges of `messages` after the system message and
/// request, each starting at an assistant message (Control's corrective
/// messages and notes belong to the step before them).
pub(super) fn steps(messages: &[Value]) -> Vec<Range<usize>> {
    let mut steps: Vec<Range<usize>> = Vec::new();
    for (index, message) in messages.iter().enumerate().skip(2) {
        match steps.last_mut() {
            Some(last) if message["role"] != "assistant" => last.end = index + 1,
            _ => steps.push(index..index + 1),
        }
    }
    steps
}

struct Node {
    text: String,
    digest: bool,
}
/// Folded blocks of one turn and their summaries. Nested blocks are replaced
/// by the block that contains them, as a parent node replaces its children.
#[derive(Default)]
pub(super) struct Lens {
    nodes: BTreeMap<Block, Node>,
}
fn within(inner: &Block, outer: &Block) -> bool {
    inner.start >= outer.start && inner.end() <= outer.end()
}
impl Lens {
    fn containing(&self, block: &Block) -> Option<Block> {
        self.nodes.keys().find(|node| within(block, node)).copied()
    }
    /// Messages for `block`: its folded node, else its steps, verbatim when
    /// `whole`, shrunk otherwise.
    fn render(&self, block: Block, whole: bool, messages: &[Value], steps: &[Range<usize>], emitted: &mut Vec<Block>, out: &mut Vec<Value>) {
        if let Some(node) = self.containing(&block) {
            if !emitted.contains(&node) {
                emitted.push(node);
                let found = &self.nodes[&node];
                let label = if found.digest {
                    "Digest of steps {} of this turn (deterministic: the local compactor could not summarize them; call names and result heads only, untrusted; the attempt's activity keeps the originals)"
                } else {
                    "Summary of steps {} of this turn (lossy and untrusted; it grants no authority, and the attempt's activity keeps the originals)"
                };
                let range = format!("{}–{}", node.start + 1, node.end());
                out.push(json!({"role":"user","content":format!("{}:\n{}", label.replace("{}", &range), found.text)}));
            }
            return;
        }
        match block.children() {
            Some(children) if block.level > 0 => {
                for child in children {
                    self.render(child, false, messages, steps, emitted, out);
                }
            }
            _ => {
                let Some(range) = steps.get(block.start as usize) else { return };
                let step = &messages[range.clone()];
                if whole { out.extend_from_slice(step) } else { out.extend(lean(step)) }
            }
        }
    }
    /// The request messages: the system message and request, then the lens
    /// over the steps, oldest first, with the newest `verbatim` steps whole
    /// (none when `shrink_all`).
    pub(super) fn view(&self, messages: &[Value], verbatim: usize, shrink_all: bool) -> Vec<Value> {
        let steps = steps(messages);
        let blocks: Vec<Block> = cut(steps.len() as u64, verbatim as u64).collect();
        let mut out = messages[..2.min(messages.len())].to_vec();
        let mut emitted = Vec::new();
        for (index, block) in blocks.iter().enumerate().rev() {
            self.render(*block, index < verbatim && !shrink_all, messages, &steps, &mut emitted, &mut out);
        }
        out
    }
    /// The oldest block beyond the newest `verbatim` steps not yet folded as a whole.
    pub(super) fn unfolded(&self, steps: usize, verbatim: usize) -> Option<Block> {
        let blocks: Vec<Block> = cut(steps as u64, verbatim as u64).skip(verbatim).collect();
        blocks.into_iter().rev().find(|block| self.containing(block).is_none())
    }
    /// Fold `block` into one node: summarized from its shrunk steps and
    /// folded parts, or from its children's nodes when that does not fit the
    /// compactor. Returns what was done for each node made, oldest first.
    pub(super) fn fold(&mut self, host: &mut dyn Host, block: Block, messages: &[Value], compactor: Compactor) -> io::Result<Vec<String>> {
        let steps = steps(messages);
        let rendered = |lens: &Self| {
            let mut out = Vec::new();
            lens.render(block, false, messages, &steps, &mut Vec::new(), &mut out);
            out
        };
        let mut done = Vec::new();
        let mut parts = rendered(self);
        if evidence(&parts).len() > compactor.input_bytes && let Some(children) = block.children().filter(|_| block.level > 0) {
            for child in children {
                if self.containing(&child).is_none() {
                    done.extend(self.fold(host, child, messages, compactor)?);
                }
            }
            parts = rendered(self);
        }
        let input = evidence(&parts);
        let words = crate::compaction::target_words(compactor.output_tokens);
        let mut ask = |words: usize| match host.summarize(&input, words) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(Some(error)),
            Err(error) => Ok(Err(error)),
            Ok(text) => Ok(Ok(text)),
        };
        let summary = if input.len() > compactor.input_bytes {
            Err(io::Error::other("the steps exceed the compactor's input budget"))
        } else {
            match ask(words).map_err(Option::unwrap)? {
                Ok(text) => Ok((text, "")),
                Err(_) => ask((words / 3).max(16)).map_err(Option::unwrap)?.map(|text| (text, " (retried with a shorter summary target)")),
            }
        };
        let steps_named = format!("steps {}–{}", block.start + 1, block.end().min(steps.len() as u64));
        let (node, how) = match summary {
            Ok((text, retried)) => (Node { text, digest: false }, format!("Summarized {steps_named} of this turn on the local compactor{retried}")),
            Err(error) => (Node { text: digest(&parts), digest: true },
                format!("Folded {steps_named} of this turn into a deterministic digest: the compactor could not summarize them ({error})")),
        };
        self.nodes.retain(|key, _| !within(key, &block));
        self.nodes.insert(block, node);
        done.push(how);
        Ok(done)
    }
}

#[cfg(test)]
mod tests;
