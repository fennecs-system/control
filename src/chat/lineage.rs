//! Where a chat sits in its task tree, from parent links alone.
use super::Session;
use std::collections::{HashMap, HashSet};

/// Read-only parent links of every chat. A function holding only this can
/// answer lineage questions but cannot reach or change the rest of `State`.
#[derive(Clone, Copy)]
pub(crate) struct Lineage<'a> {
    chats: &'a [Session],
}
impl<'a> Lineage<'a> {
    pub(crate) fn of(chats: &'a [Session]) -> Self {
        Self { chats }
    }
    /// Whether `chat` is `ancestor` or was spawned from it, directly or through children.
    pub(crate) fn within(self, chat: &str, ancestor: &str) -> bool {
        Lineage::ancestors(self, chat).any(|id| id == ancestor)
    }
    /// Every chat under the top-level chat that `chat` was spawned from, itself included.
    pub(crate) fn tree(self, chat: &'a str) -> HashSet<&'a str> {
        let parents = self
            .chats
            .iter()
            .map(|session| (session.id.as_str(), session.parent_chat.as_deref()))
            .collect::<HashMap<_, _>>();
        let top = |id: &'a str| {
            std::iter::successors(Some(id), |id| parents.get(id).copied().flatten())
                .take(64)
                .last()
                .unwrap_or(id)
        };
        let root = top(chat);
        std::iter::once(chat)
            .chain(parents.keys().copied().filter(|id| top(id) == root))
            .collect()
    }
    fn ancestors(self, chat: &'a str) -> impl Iterator<Item = &'a str> {
        std::iter::successors(Some(chat), move |id| {
            self.chats
                .iter()
                .find(|session| session.id == *id)
                .and_then(|session| session.parent_chat.as_deref())
        })
        .take(64)
    }
    /// The top-level chat of `chat`'s task tree: itself when it has no lead.
    pub(crate) fn team_root(self, chat: &'a str) -> &'a str {
        self.ancestors(chat).last().unwrap_or(chat)
    }
    /// Every chat in `chat`'s task tree: its top-level chat and all chats spawned
    /// beneath it.
    pub(crate) fn team(self, chat: &'a str) -> HashSet<&'a str> {
        self.subtree([self.team_root(chat)])
    }
    /// `roots` and every chat spawned beneath them.
    pub(crate) fn subtree(self, roots: impl IntoIterator<Item = &'a str>) -> HashSet<&'a str> {
        let mut chats = roots.into_iter().collect::<HashSet<_>>();
        loop {
            let before = chats.len();
            for session in self.chats {
                if session
                    .parent_chat
                    .as_deref()
                    .is_some_and(|parent| chats.contains(parent))
                {
                    chats.insert(session.id.as_str());
                }
            }
            if chats.len() == before {
                return chats;
            }
        }
    }
}
