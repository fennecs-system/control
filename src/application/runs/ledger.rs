//! Every run in creation order, with a constant-time lookup by id.
//!
//! The index cannot go stale by construction: a run's id is private to
//! `application::runs` (no `&mut Run` can change it), and the order changes
//! only through the methods here (`push`, `pop`, `truncate`, `extend`,
//! replacing the whole ledger), each of which keeps the index in step. There
//! is no `DerefMut` to a slice, so nothing can sort, swap or reverse runs in
//! place. Lookups still confirm the id at the indexed position, so a run
//! replaced wholesale through `&mut Run` (no code does that) shows up as a
//! miss in release builds and as an assertion failure in tests.
use super::{Run, RunId};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Runs {
    list: Vec<Run>,
    /// Each id's first position; ids are unique, and first-wins keeps the
    /// semantics of the linear `find` this replaced if they were not.
    index: HashMap<RunId, usize>,
}
impl Runs {
    fn indexed(list: Vec<Run>) -> Self {
        let mut index = HashMap::with_capacity(list.len());
        for (position, run) in list.iter().enumerate() {
            index.entry(run.id().clone()).or_insert(position);
        }
        Self { list, index }
    }
    /// The run with this id.
    pub(crate) fn get(&self, id: &str) -> Option<&Run> {
        self.position(id).map(|position| &self.list[position])
    }
    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut Run> {
        self.position(id).map(|position| &mut self.list[position])
    }
    /// Where the run with this id sits, for edits that hold a position.
    pub(crate) fn position(&self, id: &str) -> Option<usize> {
        let found = self
            .index
            .get(id)
            .copied()
            .filter(|&position| self.list.get(position).is_some_and(|run| run.id() == id));
        debug_assert!(
            found.is_some() || self.list.iter().all(|run| run.id() != id),
            "run index is stale for {id}"
        );
        found
    }
    pub(crate) fn push(&mut self, run: Run) {
        self.index.entry(run.id().clone()).or_insert(self.list.len());
        self.list.push(run);
    }
    pub(crate) fn pop(&mut self) -> Option<Run> {
        let run = self.list.pop()?;
        if self.index.get(run.id()) == Some(&self.list.len()) {
            self.index.remove(run.id());
        }
        Some(run)
    }
    /// Drops runs added after the first `len` (rolling back an unsaved edit).
    pub(crate) fn truncate(&mut self, len: usize) {
        if len >= self.list.len() {
            return;
        }
        for (position, run) in self.list.iter().enumerate().skip(len) {
            if self.index.get(run.id()) == Some(&position) {
                self.index.remove(run.id());
            }
        }
        self.list.truncate(len);
    }
    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, Run> {
        self.list.iter_mut()
    }
    pub(crate) fn last_mut(&mut self) -> Option<&mut Run> {
        self.list.last_mut()
    }
    /// Test fixtures reshape runs freely (remove, rename, reorder); the index
    /// is rebuilt afterwards.
    #[cfg(test)]
    pub(crate) fn rebuild(&mut self, edit: impl FnOnce(&mut Vec<Run>)) {
        let mut list = std::mem::take(&mut self.list);
        edit(&mut list);
        *self = Self::indexed(list);
    }
    /// The runs in creation order, leaving this ledger empty.
    pub(crate) fn into_vec(self) -> Vec<Run> {
        self.list
    }
}
impl std::ops::Deref for Runs {
    type Target = [Run];
    fn deref(&self) -> &[Run] {
        &self.list
    }
}
// Positions and ranges read like a slice; only single runs can be edited in place.
// Positions and ranges read like a slice; only single runs are edited in place.
impl<I: std::slice::SliceIndex<[Run]>> std::ops::Index<I> for Runs {
    type Output = I::Output;
    fn index(&self, position: I) -> &I::Output {
        &self.list[position]
    }
}
impl std::ops::IndexMut<usize> for Runs {
    fn index_mut(&mut self, position: usize) -> &mut Run {
        &mut self.list[position]
    }
}
impl From<Vec<Run>> for Runs {
    fn from(list: Vec<Run>) -> Self {
        Self::indexed(list)
    }
}
impl FromIterator<Run> for Runs {
    fn from_iter<I: IntoIterator<Item = Run>>(runs: I) -> Self {
        Self::indexed(runs.into_iter().collect())
    }
}
impl Extend<Run> for Runs {
    fn extend<I: IntoIterator<Item = Run>>(&mut self, runs: I) {
        runs.into_iter().for_each(|run| self.push(run));
    }
}
/// Saved as the plain array it always was; the index is rebuilt on load.
impl<'de> serde::Deserialize<'de> for Runs {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<Run>::deserialize(deserializer).map(Self::indexed)
    }
}
impl serde::Serialize for Runs {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.list.serialize(serializer)
    }
}
