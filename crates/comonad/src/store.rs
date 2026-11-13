//! The store comonad: a space of positions with one in focus.
//!
//! `extract` reads the focus, `seek` refocuses, and `extend` evaluates a
//! context-sensitive function at every position, each seeing the space from
//! there. Positions are opaque to the comonad, so any history that can be
//! addressed (a slice, a chat tree, a file-backed spool) can be woven through.

/// Something addressable by position: a timeline, a tree, a spool.
pub trait Space {
    type Pos: Copy + Eq;
    /// What a position yields: a reference for in-memory spaces, an owned
    /// value for spaces read from storage.
    type Item<'a>
    where
        Self: 'a;
    fn at(&self, pos: Self::Pos) -> Option<Self::Item<'_>>;
    /// Every position, in the space's natural order.
    fn positions(&self) -> impl Iterator<Item = Self::Pos> + '_;
}

impl<T> Space for [T] {
    type Pos = usize;
    type Item<'a>
        = &'a T
    where
        T: 'a;
    #[inline]
    fn at(&self, pos: usize) -> Option<&T> {
        self.get(pos)
    }
    fn positions(&self) -> impl Iterator<Item = usize> + '_ {
        0..self.len()
    }
}

/// A space viewed from one position. Copying a store copies a reference and a
/// position, never the history.
pub struct Store<'a, S: Space + ?Sized> {
    space: &'a S,
    pos: S::Pos,
}
impl<S: Space + ?Sized> Clone for Store<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Space + ?Sized> Copy for Store<'_, S> {}

impl<'a, S: Space + ?Sized> Store<'a, S> {
    /// Focuses `pos`, or `None` when the space has nothing there.
    pub fn new(space: &'a S, pos: S::Pos) -> Option<Self> {
        space.at(pos).map(|_| Self { space, pos })
    }
    #[inline]
    pub fn pos(&self) -> S::Pos {
        self.pos
    }
    #[inline]
    pub fn space(&self) -> &'a S {
        self.space
    }
    /// The focused item. Always present: a store is only built on a valid position.
    #[inline]
    pub fn extract(&self) -> S::Item<'a> {
        self.space.at(self.pos).expect("a store focuses a valid position")
    }
    /// The item at another position, seen from here.
    #[inline]
    pub fn peek(&self, pos: S::Pos) -> Option<S::Item<'a>> {
        self.space.at(pos)
    }
    /// The same space focused elsewhere.
    #[inline]
    pub fn seek(&self, pos: S::Pos) -> Option<Self> {
        Self::new(self.space, pos)
    }
    /// `f` evaluated with the focus at every position, in space order: the
    /// comonad's `extend`, materialised. `duplicate` is `extend(|s| s)`.
    pub fn extend<B>(&self, mut f: impl FnMut(Store<'a, S>) -> B) -> Vec<B> {
        let space = self.space;
        space.positions().map(|pos| f(Store { space, pos })).collect()
    }
}

impl<'a, T> Store<'a, [T]> {
    /// Up to `radius` items either side of the focus, the focus included.
    #[inline]
    pub fn window(&self, radius: usize) -> &'a [T] {
        let start = self.pos.saturating_sub(radius);
        let end = self.pos.saturating_add(radius).saturating_add(1).min(self.space.len());
        &self.space[start..end]
    }
    /// Everything from the focus to the end: what happened since.
    #[inline]
    pub fn since(&self) -> &'a [T] {
        &self.space[self.pos..]
    }
    /// Everything before the focus.
    #[inline]
    pub fn before(&self) -> &'a [T] {
        &self.space[..self.pos]
    }
    #[inline]
    pub fn left(&self) -> Option<Self> {
        self.pos.checked_sub(1).and_then(|pos| self.seek(pos))
    }
    #[inline]
    pub fn right(&self) -> Option<Self> {
        self.seek(self.pos + 1)
    }
}

#[cfg(test)]
mod tests;
