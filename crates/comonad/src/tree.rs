//! An append-only arena tree with a zipper focus: conversation trees, run
//! lineage, summary trees. Nodes are linked by `u32` indices, so moves are
//! O(1), walks allocate nothing, and the arena is one contiguous vector.
use crate::store::Space;

const NONE: u32 = u32::MAX;

/// An index into the tree that produced it. Like any arena index it carries no
/// tree identity: another tree reads whatever node has the same index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);
impl NodeId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug)]
struct Links {
    parent: u32,
    root: u32,
    depth: u32,
    first: u32,
    last: u32,
    next: u32,
    prev: u32,
}

/// Nodes are only appended; children keep insertion order.
#[derive(Clone, Debug)]
pub struct Tree<T> {
    items: Vec<T>,
    links: Vec<Links>,
}
impl<T> Default for Tree<T> {
    fn default() -> Self {
        Self { items: Vec::new(), links: Vec::new() }
    }
}

impl<T> Tree<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        Self { items: Vec::with_capacity(capacity), links: Vec::with_capacity(capacity) }
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    /// Appends `item` as the last child of `parent`, or as a new root.
    /// Panics if `parent` is out of range or the arena is full.
    pub fn push(&mut self, parent: Option<NodeId>, item: T) -> NodeId {
        let id = u32::try_from(self.items.len()).ok().filter(|id| *id != NONE).expect("tree arena is full");
        let parent = parent.map_or(NONE, |p| {
            assert!(p.index() < self.links.len(), "parent outside this tree");
            p.0
        });
        let (root, depth) = if parent == NONE { (id, 0) } else { let p = &self.links[parent as usize]; (p.root, p.depth + 1) };
        let mut prev = NONE;
        if parent != NONE {
            let p = &mut self.links[parent as usize];
            prev = p.last;
            p.last = id;
            if p.first == NONE {
                p.first = id;
            }
        }
        if prev != NONE {
            self.links[prev as usize].next = id;
        }
        self.items.push(item);
        self.links.push(Links { parent, root, depth, first: NONE, last: NONE, next: NONE, prev });
        NodeId(id)
    }
    #[inline]
    pub fn get(&self, id: NodeId) -> Option<&T> {
        self.items.get(id.index())
    }
    #[inline]
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut T> {
        self.items.get_mut(id.index())
    }
    /// A zipper focused on `id`.
    #[inline]
    pub fn focus(&self, id: NodeId) -> Option<Focus<'_, T>> {
        (id.index() < self.items.len()).then_some(Focus { tree: self, at: id.0 })
    }
    /// Bottom-up fold: every node's value from its item and its children's
    /// values, in one pass. Children always follow their parent in the arena,
    /// so a reverse scan sees each child first.
    pub fn fold<B>(&self, mut f: impl FnMut(&T, &mut dyn Iterator<Item = &B>) -> B) -> Vec<B> {
        let mut out: Vec<Option<B>> = (0..self.len()).map(|_| None).collect();
        for i in (0..self.len()).rev() {
            let (head, done) = out.split_at_mut(i + 1);
            let links = &self.links;
            let mut children = std::iter::successors(Some(links[i].first).filter(|c| *c != NONE), |c| Some(links[*c as usize].next).filter(|n| *n != NONE))
                .map(|c| done[c as usize - i - 1].as_ref().expect("child folded before parent"));
            head[i] = Some(f(&self.items[i], &mut children));
        }
        out.into_iter().map(|v| v.expect("every node folded")).collect()
    }
}

impl<T> Space for Tree<T> {
    type Pos = NodeId;
    type Item<'a>
        = &'a T
    where
        T: 'a;
    #[inline]
    fn at(&self, pos: NodeId) -> Option<&T> {
        self.get(pos)
    }
    fn positions(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.items.len() as u32).map(NodeId)
    }
}

/// One node of a tree and the way to every other: the tree zipper. Copying a
/// focus copies a reference and an index.
pub struct Focus<'a, T> {
    tree: &'a Tree<T>,
    at: u32,
}
impl<T> Clone for Focus<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Focus<'_, T> {}
impl<'a, T> Focus<'a, T> {
    #[inline]
    fn to(self, at: u32) -> Option<Self> {
        (at != NONE).then_some(Self { tree: self.tree, at })
    }
    #[inline]
    fn links(&self) -> &'a Links {
        &self.tree.links[self.at as usize]
    }
    #[inline]
    pub fn id(&self) -> NodeId {
        NodeId(self.at)
    }
    #[inline]
    pub fn extract(&self) -> &'a T {
        &self.tree.items[self.at as usize]
    }
    #[inline]
    pub fn up(self) -> Option<Self> {
        self.to(self.links().parent)
    }
    #[inline]
    pub fn down(self) -> Option<Self> {
        self.to(self.links().first)
    }
    #[inline]
    pub fn last_child(self) -> Option<Self> {
        self.to(self.links().last)
    }
    #[inline]
    pub fn next(self) -> Option<Self> {
        self.to(self.links().next)
    }
    #[inline]
    pub fn prev(self) -> Option<Self> {
        self.to(self.links().prev)
    }
    /// The top of this focus's branch: the conversation it belongs to. O(1):
    /// recorded when the node was appended.
    #[inline]
    pub fn root(self) -> Self {
        Self { tree: self.tree, at: self.links().root }
    }
    /// The focus, then each ancestor up to the root.
    pub fn ancestors(self) -> impl Iterator<Item = Focus<'a, T>> {
        std::iter::successors(Some(self), |node| node.up())
    }
    pub fn children(self) -> impl Iterator<Item = Focus<'a, T>> {
        std::iter::successors(self.down(), |node| node.next())
    }
    /// The focus and everything under it, in pre-order, without a stack.
    pub fn subtree(self) -> impl Iterator<Item = Focus<'a, T>> {
        let top = self.at;
        std::iter::successors(Some(self), move |node| {
            if let Some(child) = node.down() {
                return Some(child);
            }
            let mut at = *node;
            loop {
                if at.at == top {
                    return None;
                }
                if let Some(sibling) = at.next() {
                    return Some(sibling);
                }
                at = at.up()?;
            }
        })
    }
    /// Whether `other` lies in this focus's subtree.
    pub fn contains(&self, other: NodeId) -> bool {
        let Some(node) = self.tree.focus(other) else { return false };
        let (mine, theirs) = (self.links(), node.links());
        mine.root == theirs.root && theirs.depth >= mine.depth && node.ancestors().nth((theirs.depth - mine.depth) as usize).is_some_and(|a| a.at == self.at)
    }
    /// Edges between this focus and its root. O(1).
    #[inline]
    pub fn depth(&self) -> usize {
        self.links().depth as usize
    }
}

#[cfg(test)]
mod tests;
