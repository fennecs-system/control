//! Authorized, borrowed conversation forest. Linear cycle repair and subtree
//! ordering keeps saved orphan/cyclic links visible without recursion.
use crate::{
    State,
    chat::{Session, valid_id},
    identity::{self, Action},
};
use std::collections::HashMap;

pub struct Entry<'a> {
    pub session: &'a Session,
    pub parent: Option<&'a str>,
    pub depth: usize,
}
pub fn ordered(state: &State) -> Vec<Entry<'_>> {
    let mut authorize = identity::authorizer();
    let visible: Vec<_> = state
        .chats
        .iter()
        .filter(|chat| valid_id(&chat.id) && authorize(&chat.actor, Action::Task).is_ok())
        .collect();
    let by_id: HashMap<_, _> = visible
        .iter()
        .enumerate()
        .map(|(index, chat)| (chat.id.as_str(), index))
        .collect();
    let mut parents: Vec<_> = visible
        .iter()
        .enumerate()
        .map(|(index, chat)| {
            chat.parent_chat
                .as_deref()
                .and_then(|id| by_id.get(id).copied())
                .filter(|parent| *parent != index && visible[*parent].actor == chat.actor)
        })
        .collect();
    let mut marks = vec![0u8; visible.len()];
    for start in 0..visible.len() {
        let mut path = Vec::new();
        let mut cursor = Some(start);
        while let Some(index) = cursor {
            if marks[index] != 0 {
                break;
            }
            marks[index] = 1;
            path.push(index);
            cursor = parents[index];
        }
        if let Some(index) = cursor
            && marks[index] == 1
        {
            parents[index] = None;
        }
        for index in path {
            marks[index] = 2;
        }
    }
    let mut children = vec![Vec::new(); visible.len()];
    let mut roots = Vec::new();
    for (index, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(index);
        } else {
            roots.push(index);
        }
    }
    // Background child creation/completion must not make an old playtest look
    // like the user's most recent conversation when returning from another page.
    let activity: Vec<_> = visible.iter().map(|chat| chat.activity_order).collect();
    let key = |index: &usize| std::cmp::Reverse((activity[*index], *index));
    roots.sort_by_key(key);
    for siblings in &mut children {
        siblings.sort_by_key(key);
    }
    let mut stack: Vec<_> = roots.into_iter().rev().map(|index| (index, 0)).collect();
    let mut entries = Vec::with_capacity(visible.len());
    while let Some((index, depth)) = stack.pop() {
        entries.push(Entry {
            session: visible[index],
            parent: parents[index].map(|p| visible[p].id.as_str()),
            depth,
        });
        stack.extend(
            children[index]
                .iter()
                .rev()
                .map(|child| (*child, depth + 1)),
        );
    }
    entries
}

#[cfg(test)]
mod tests;
