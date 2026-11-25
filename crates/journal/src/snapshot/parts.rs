//! A document held as shared parts, so a save re-encodes and diffs only what
//! changed.
//!
//! A [`Tree`] is an encoded document whose objects and arrays may be made of
//! other trees instead of one contiguous [`Encoded`]. Trees are reference
//! counted: a part that did not change since the last save is the same
//! allocation in both documents, and [`super::diff_trees`] skips it by
//! pointer. Its bytes and its patches are exactly those of the joined
//! document.
//!
//! [`Part`] is where a value keeps its tree between saves. Any `&mut` access
//! (field assignment through `DerefMut` included) forgets it, so a stale
//! encoding cannot be saved: the cache is valid by construction, with no dirty
//! flags to forget. [`split`] builds a value's tree: an object or array is
//! split into members only where some member is a part (or a sequence of
//! parts); everything else is one contiguous encoding.
use super::{Encoded, SEGMENT, encode};
use serde::{
    Deserialize, Serialize, Serializer,
    ser::{self, Impossible},
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    sync::{Arc, OnceLock},
};

/// An encoded document made of shared parts.
#[derive(Clone, Debug)]
pub struct Tree(Arc<Shape>);
#[derive(Debug)]
enum Shape {
    Leaf(Encoded),
    /// Members as raw JSON key bytes and values, and the joined length.
    Object(Vec<(Box<[u8]>, Tree)>, usize),
    Array(Vec<Tree>, usize),
}
impl Tree {
    /// One contiguous encoding.
    pub fn leaf(encoded: Encoded) -> Self {
        Self(Arc::new(Shape::Leaf(encoded)))
    }
    fn object(members: Vec<(Box<[u8]>, Tree)>) -> Self {
        let len = 1 + members
            .iter()
            .map(|(key, value)| key.len() + 1 + value.len() + 1)
            .sum::<usize>();
        Self(Arc::new(Shape::Object(members, len.max(2))))
    }
    fn array(items: Vec<Tree>) -> Self {
        let len = 1 + items.iter().map(|item| item.len() + 1).sum::<usize>();
        Self(Arc::new(Shape::Array(items, len.max(2))))
    }
    /// The length of the joined document.
    pub fn len(&self) -> usize {
        self.at().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The joined document's bytes.
    pub fn bytes(&self) -> Cow<'_, [u8]> {
        self.at().bytes()
    }
    pub(super) fn at(&self) -> At<'_> {
        match &*self.0 {
            Shape::Leaf(encoded) => At::Node(encoded, 0),
            Shape::Object(members, len) => At::Object(members, *len),
            Shape::Array(items, len) => At::Array(items, *len),
        }
    }
}

/// A value inside a document being diffed: a node of a contiguous encoding,
/// or an object or array made of parts.
#[derive(Clone, Copy)]
pub(super) enum At<'a> {
    Node(&'a Encoded, usize),
    Object(&'a [(Box<[u8]>, Tree)], usize),
    Array(&'a [Tree], usize),
}
impl<'a> At<'a> {
    fn len(self) -> usize {
        match self {
            Self::Node(encoded, node) => encoded.range(node).len(),
            Self::Object(_, len) | Self::Array(_, len) => len,
        }
    }
    pub(super) fn first(self) -> u8 {
        match self {
            Self::Node(encoded, node) => encoded.range(node)[0],
            Self::Object(..) => b'{',
            Self::Array(..) => b'[',
        }
    }
    /// The opening bracket when members are recorded: as in an encoding,
    /// only containers of at least [`SEGMENT`] bytes have them.
    pub(super) fn container(self) -> Option<u8> {
        match self {
            Self::Node(encoded, node) => encoded.container(node),
            Self::Object(..) | Self::Array(..) => (self.len() >= SEGMENT).then(|| self.first()),
        }
    }
    /// Members with their raw keys (empty for array elements).
    pub(super) fn children(self) -> Vec<(&'a [u8], At<'a>)> {
        match self {
            Self::Node(encoded, node) => encoded
                .children(node)
                .into_iter()
                .map(|child| (encoded.key(child), Self::Node(encoded, child)))
                .collect(),
            Self::Object(members, _) => members.iter().map(|(key, value)| (&key[..], value.at())).collect(),
            Self::Array(items, _) => items.iter().map(|item| (&[][..], item.at())).collect(),
        }
    }
    /// Whether both are the same allocation, so equal without reading them.
    fn same(self, other: Self) -> bool {
        match (self, other) {
            (Self::Node(a, i), Self::Node(b, j)) => std::ptr::eq(a, b) && i == j,
            (Self::Object(a, _), Self::Object(b, _)) => std::ptr::eq(a, b),
            (Self::Array(a, _), Self::Array(b, _)) => std::ptr::eq(a, b),
            _ => false,
        }
    }
    /// Byte equality of the joined values. Containers with recorded members
    /// compare member by member: a valid JSON container's bytes determine its
    /// members, so equal members mean equal bytes.
    pub(super) fn equals(self, other: Self) -> bool {
        if self.same(other) {
            return true;
        }
        if self.len() != other.len() {
            return false;
        }
        match (self, other) {
            (Self::Node(a, i), Self::Node(b, j)) => a.range(i) == b.range(j),
            _ => match (self.container(), other.container()) {
                (Some(x), Some(y)) if x == y => {
                    let (a, b) = (self.children(), other.children());
                    a.len() == b.len() && a.iter().zip(&b).all(|((ka, va), (kb, vb))| ka == kb && va.equals(*vb))
                }
                _ => self.bytes() == other.bytes(),
            },
        }
    }
    pub(super) fn bytes(self) -> Cow<'a, [u8]> {
        match self {
            Self::Node(encoded, node) => Cow::Borrowed(encoded.range(node)),
            _ => {
                let mut out = Vec::with_capacity(self.len());
                self.write(&mut out);
                Cow::Owned(out)
            }
        }
    }
    pub(super) fn write(self, out: &mut Vec<u8>) {
        match self {
            Self::Node(encoded, node) => out.extend_from_slice(encoded.range(node)),
            Self::Object(members, _) => {
                out.push(b'{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(key);
                    out.push(b':');
                    value.at().write(out);
                }
                out.push(b'}');
            }
            Self::Array(items, _) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    item.at().write(out);
                }
                out.push(b']');
            }
        }
    }
}

/// A value that keeps its encoded [`Tree`] until it is next borrowed mutably.
/// It serializes as the value itself; only [`split`] asks for the tree.
#[derive(Default)]
pub struct Part<T> {
    value: T,
    tree: OnceLock<Tree>,
}
impl<T> Part<T> {
    /// A value whose encoding is already known, such as one read back from
    /// saved bytes.
    pub fn restored(value: T, tree: Tree) -> Self {
        Self {
            value,
            tree: OnceLock::from(tree),
        }
    }
    /// The tree the last save built, when the value is unchanged since.
    pub fn saved(&self) -> Option<&Tree> {
        self.tree.get()
    }
    /// Mutable access for an edit that leaves the saved form unchanged (such
    /// as filling in text the save writes elsewhere). Keeps the tree.
    pub fn unchanged_mut(&mut self) -> &mut T {
        &mut self.value
    }
    pub fn into_inner(self) -> T {
        self.value
    }
}
impl<T: Serialize> Part<T> {
    fn tree(&self) -> serde_json::Result<Tree> {
        match self.tree.get() {
            Some(tree) => Ok(tree.clone()),
            None => split(&self.value).map(|tree| self.tree.get_or_init(|| tree).clone()),
        }
    }
}
impl<T> From<T> for Part<T> {
    fn from(value: T) -> Self {
        Self {
            value,
            tree: OnceLock::new(),
        }
    }
}
impl<T: Clone> Clone for Part<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            tree: self.tree.clone(),
        }
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for Part<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}
impl<T: PartialEq> PartialEq for Part<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl<T: Eq> Eq for Part<T> {}
impl<T> std::ops::Deref for Part<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> std::ops::DerefMut for Part<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.tree.take();
        &mut self.value
    }
}
impl<T: IntoIterator> IntoIterator for Part<T> {
    type Item = T::Item;
    type IntoIter = T::IntoIter;
    fn into_iter(self) -> T::IntoIter {
        self.value.into_iter()
    }
}
impl<A, T: FromIterator<A>> FromIterator<A> for Part<T> {
    fn from_iter<I: IntoIterator<Item = A>>(items: I) -> Self {
        T::from_iter(items).into()
    }
}
impl<'a, T> IntoIterator for &'a Part<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&self.value).into_iter()
    }
}
impl<'a, T> IntoIterator for &'a mut Part<T>
where
    &'a mut T: IntoIterator,
{
    type Item = <&'a mut T as IntoIterator>::Item;
    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&mut **self).into_iter()
    }
}
impl<T: Serialize> Serialize for Part<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match offer(serializer, || self.tree()) {
            Ok(offered) => offered,
            Err(serializer) => self.value.serialize(serializer),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Part<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::from)
    }
}

thread_local! {
    /// Set while [`split`] asks a member whether it is a part; the first
    /// part to serialize takes it, so nothing nested answers in its place.
    static ASKING: Cell<bool> = const { Cell::new(false) };
    static OFFERED: RefCell<Option<Tree>> = const { RefCell::new(None) };
}

/// For a `Serialize` impl of a value that knows its own tree: hands `tree()`
/// to [`split`] when it is asking, else gives the serializer back to
/// serialize the value as usual. The bytes are the same either way.
pub fn offer<S: Serializer>(
    serializer: S,
    tree: impl FnOnce() -> serde_json::Result<Tree>,
) -> Result<Result<S::Ok, S::Error>, S> {
    if !ASKING.with(|asking| asking.replace(false)) {
        return Err(serializer);
    }
    Ok(match tree() {
        Ok(tree) => {
            OFFERED.with(|offered| *offered.borrow_mut() = Some(tree));
            serializer.serialize_unit()
        }
        Err(error) => Err(ser::Error::custom(error)),
    })
}

/// `serialize_with` for a field that is split at every save rather than
/// cached, for a value whose encoding can change without `&mut` access.
pub fn split_each_save<T: Serialize, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    match offer(serializer, || split(value)) {
        Ok(offered) => offered,
        Err(serializer) => value.serialize(serializer),
    }
}

/// The value's tree: split into members where a member offers its own tree
/// (a [`Part`], or a sequence of them), one contiguous encoding otherwise.
pub fn split<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Tree> {
    if let Some(tree) = offered(value) {
        return Ok(tree);
    }
    match value.serialize(Splitter) {
        Ok(Some(tree)) => Ok(tree),
        _ => encode(value).map(Tree::leaf),
    }
}

/// The tree `value` offers, if it is a part.
fn offered<T: Serialize + ?Sized>(value: &T) -> Option<Tree> {
    OFFERED.with(|offered| offered.borrow_mut().take());
    ASKING.with(|asking| asking.set(true));
    let answered = value.serialize(Asker);
    ASKING.with(|asking| asking.set(false));
    let tree = OFFERED.with(|offered| offered.borrow_mut().take());
    answered.ok().and(tree)
}

/// A member's tree inside a split: its own when it offers one, a split when
/// it is a sequence of parts, else its encoding (`None` with `plain`).
fn member<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<(Tree, bool)> {
    if let Some(tree) = offered(value) {
        return Ok((tree, true));
    }
    match value.serialize(Sequence) {
        Ok(Some(tree)) => Ok((tree, true)),
        _ => encode(value).map(|encoded| (Tree::leaf(encoded), false)),
    }
}

fn not_split() -> serde_json::Error {
    ser::Error::custom("not split")
}

/// Answers only a part's offer; anything else is not a part.
struct Asker;
macro_rules! refuse {
    ($($method:ident($($arg:ty),*) -> $out:ty;)*) => {
        $(fn $method(self, $(_: $arg),*) -> Result<$out, Self::Error> { Err(not_split()) })*
    };
}
macro_rules! refuse_values {
    () => {
        refuse! {
            serialize_bool(bool) -> Self::Ok;
            serialize_i8(i8) -> Self::Ok;
            serialize_i16(i16) -> Self::Ok;
            serialize_i32(i32) -> Self::Ok;
            serialize_i64(i64) -> Self::Ok;
            serialize_u8(u8) -> Self::Ok;
            serialize_u16(u16) -> Self::Ok;
            serialize_u32(u32) -> Self::Ok;
            serialize_u64(u64) -> Self::Ok;
            serialize_f32(f32) -> Self::Ok;
            serialize_f64(f64) -> Self::Ok;
            serialize_char(char) -> Self::Ok;
            serialize_str(&str) -> Self::Ok;
            serialize_bytes(&[u8]) -> Self::Ok;
            serialize_none() -> Self::Ok;
            serialize_unit_struct(&'static str) -> Self::Ok;
            serialize_unit_variant(&'static str, u32, &'static str) -> Self::Ok;
            serialize_tuple(usize) -> Self::SerializeTuple;
            serialize_tuple_struct(&'static str, usize) -> Self::SerializeTupleStruct;
            serialize_tuple_variant(&'static str, u32, &'static str, usize) -> Self::SerializeTupleVariant;
            serialize_struct_variant(&'static str, u32, &'static str, usize) -> Self::SerializeStructVariant;
        }
        fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<Self::Ok, Self::Error> {
            Err(not_split())
        }
        fn serialize_newtype_variant<T: Serialize + ?Sized>(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: &T,
        ) -> Result<Self::Ok, Self::Error> {
            Err(not_split())
        }
    };
}
impl Serializer for Asker {
    type Ok = ();
    type Error = serde_json::Error;
    type SerializeSeq = Impossible<(), serde_json::Error>;
    type SerializeTuple = Impossible<(), serde_json::Error>;
    type SerializeTupleStruct = Impossible<(), serde_json::Error>;
    type SerializeTupleVariant = Impossible<(), serde_json::Error>;
    type SerializeMap = Impossible<(), serde_json::Error>;
    type SerializeStruct = Impossible<(), serde_json::Error>;
    type SerializeStructVariant = Impossible<(), serde_json::Error>;
    refuse_values!();
    refuse! {
        serialize_seq(Option<usize>) -> Self::SerializeSeq;
        serialize_map(Option<usize>) -> Self::SerializeMap;
        serialize_struct(&'static str, usize) -> Self::SerializeStruct;
    }
    fn serialize_unit(self) -> Result<(), serde_json::Error> {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, _: &T) -> Result<(), serde_json::Error> {
        Err(not_split())
    }
}

/// Splits a sequence whose elements are parts; refuses anything else, at the
/// first element that is not one.
struct Sequence;
impl Serializer for Sequence {
    type Ok = Option<Tree>;
    type Error = serde_json::Error;
    type SerializeSeq = Items;
    type SerializeTuple = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeTupleStruct = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeTupleVariant = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeMap = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeStruct = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeStructVariant = Impossible<Option<Tree>, serde_json::Error>;
    refuse_values!();
    refuse! {
        serialize_unit() -> Self::Ok;
        serialize_map(Option<usize>) -> Self::SerializeMap;
        serialize_struct(&'static str, usize) -> Self::SerializeStruct;
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Items, serde_json::Error> {
        Ok(Items(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: &T,
    ) -> Result<Option<Tree>, serde_json::Error> {
        Err(not_split())
    }
}
struct Items(Vec<Tree>);
impl ser::SerializeSeq for Items {
    type Ok = Option<Tree>;
    type Error = serde_json::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), serde_json::Error> {
        self.0.push(offered(value).ok_or_else(not_split)?);
        Ok(())
    }
    // An empty sequence has nothing to share: it is encoded whole.
    fn end(self) -> Result<Option<Tree>, serde_json::Error> {
        Ok((!self.0.is_empty()).then(|| Tree::array(self.0)))
    }
}

/// Splits an object, or a sequence of parts, into members. `None` when no
/// member is a part: the caller encodes the value whole.
struct Splitter;
impl Serializer for Splitter {
    type Ok = Option<Tree>;
    type Error = serde_json::Error;
    type SerializeSeq = Items;
    type SerializeTuple = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeTupleStruct = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeTupleVariant = Impossible<Option<Tree>, serde_json::Error>;
    type SerializeMap = Members;
    type SerializeStruct = Members;
    type SerializeStructVariant = Impossible<Option<Tree>, serde_json::Error>;
    refuse_values!();
    refuse! {
        serialize_unit() -> Self::Ok;
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Items, serde_json::Error> {
        Sequence.serialize_seq(len)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Members, serde_json::Error> {
        Ok(Members::new(len.unwrap_or(0)))
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<Members, serde_json::Error> {
        Ok(Members::new(len))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Option<Tree>, serde_json::Error> {
        value.serialize(self)
    }
}
struct Members {
    members: Vec<(Box<[u8]>, Tree)>,
    key: Option<Box<[u8]>>,
    split: bool,
}
impl Members {
    fn new(len: usize) -> Self {
        Self {
            members: Vec::with_capacity(len),
            key: None,
            split: false,
        }
    }
    fn push<T: Serialize + ?Sized>(&mut self, key: Box<[u8]>, value: &T) -> serde_json::Result<()> {
        let (tree, part) = member(value)?;
        self.split |= part;
        self.members.push((key, tree));
        Ok(())
    }
    fn end(self) -> serde_json::Result<Option<Tree>> {
        Ok(self.split.then(|| Tree::object(self.members)))
    }
}
/// A member name as JSON writes it; only string keys are split.
fn key<T: Serialize + ?Sized>(key: &T) -> serde_json::Result<Box<[u8]>> {
    match serde_json::to_value(key)? {
        serde_json::Value::String(key) => serde_json::to_vec(&key).map(Vec::into_boxed_slice),
        _ => Err(not_split()),
    }
}
impl ser::SerializeMap for Members {
    type Ok = Option<Tree>;
    type Error = serde_json::Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, name: &T) -> Result<(), serde_json::Error> {
        self.key = Some(key(name)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), serde_json::Error> {
        let name = self.key.take().ok_or_else(not_split)?;
        self.push(name, value)
    }
    fn end(self) -> Result<Option<Tree>, serde_json::Error> {
        Members::end(self)
    }
}
impl ser::SerializeStruct for Members {
    type Ok = Option<Tree>;
    type Error = serde_json::Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        self.push(serde_json::to_vec(name)?.into_boxed_slice(), value)
    }
    fn end(self) -> Result<Option<Tree>, serde_json::Error> {
        Members::end(self)
    }
}

#[cfg(test)]
#[path = "parts/tests.rs"]
mod tests;
