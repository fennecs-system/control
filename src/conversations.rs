//! Authorized, borrowed conversation forest. Linear cycle repair and subtree
//! activity aggregation keep saved orphan/cyclic links visible without recursion.
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
    let mut activity: Vec<_> = visible.iter().map(|chat| chat.activity_order).collect();
    let mut stack: Vec<_> = roots.iter().map(|index| (*index, false)).collect();
    while let Some((index, visited)) = stack.pop() {
        if visited {
            if let Some(parent) = parents[index] {
                activity[parent] = activity[parent].max(activity[index]);
            }
        } else {
            stack.push((index, true));
            stack.extend(children[index].iter().map(|child| (*child, false)));
        }
    }
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
mod tests {
    use super::*;
    use crate::chat;
    #[test]
    fn recent_threads_contain_their_children_and_keep_order_after_restart() {
        let mut state = State::default();
        let first = chat::create(&mut state);
        let second = chat::create(&mut state);
        assert_eq!(ordered(&state)[0].session.id, second);
        chat::send(&mut state, &first, "Continue older conversation", "test").unwrap();
        assert_eq!(ordered(&state)[0].session.id, first);
        let child = chat::create(&mut state);
        state.chats.last_mut().unwrap().parent_chat = Some(second.clone());
        let entries = ordered(&state);
        assert_eq!(
            entries
                .iter()
                .map(|e| e.session.id.as_str())
                .collect::<Vec<_>>(),
            [second.as_str(), child.as_str(), first.as_str()]
        );
        assert_eq!(entries[1].depth, 1);
        assert_eq!(entries[1].parent, Some(second.as_str()));
        let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(
            ordered(&restored)
                .iter()
                .map(|e| e.session.id.as_str())
                .collect::<Vec<_>>(),
            [second.as_str(), child.as_str(), first.as_str()]
        );
        let api = chat::list(&restored);
        assert_eq!(api[1]["parent_chat"], second);
        assert_eq!(api[1]["depth"], 1);
        let html = crate::navigation::conversations(&restored, &child);
        assert!(html.contains(&format!("data-chat-branch='{second}' open")));
        let html = crate::navigation::conversations(&restored, &second);
        assert!(!html.contains(&format!("data-chat-branch='{second}' open")));
    }
    #[test]
    fn malformed_parents_never_hide_chats_or_cross_identity_boundaries() {
        let mut state = State::default();
        let a = chat::create(&mut state);
        let b = chat::create(&mut state);
        let orphan = chat::create(&mut state);
        state.chats[0].parent_chat = Some(b.clone());
        state.chats[1].parent_chat = Some(a.clone());
        state.chats[2].parent_chat = Some("c-missing".into());
        let entries = ordered(&state);
        assert_eq!(entries.len(), 3);
        assert!(
            entries
                .iter()
                .any(|entry| entry.session.id == orphan && entry.depth == 0)
        );
        assert_eq!(
            entries
                .iter()
                .map(|e| e.session.id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        state.chats[0].actor = crate::identity::Actor::Unknown;
        let entries = ordered(&state);
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .find(|e| e.session.id == b)
                .unwrap()
                .parent
                .is_none()
        );
        let html = crate::navigation::conversations(&state, &b);
        assert!(!html.contains(&format!("/chat/{a}")));
    }
    #[test]
    fn legacy_activity_defaults_and_deep_lineages_remain_iterative() {
        let mut state = State::default();
        let mut parent = None;
        for _ in 0..256 {
            let id = chat::create(&mut state);
            let chat = state.chats.last_mut().unwrap();
            chat.parent_chat = parent;
            chat.activity_order = 0;
            parent = Some(id);
        }
        let entries = ordered(&state);
        assert_eq!(entries.len(), 256);
        assert_eq!(entries.last().unwrap().depth, 255);
        let mut value = serde_json::to_value(&state).unwrap();
        for chat in value["chats"].as_array_mut().unwrap() {
            chat.as_object_mut().unwrap().remove("activity_order");
        }
        let restored: State = serde_json::from_value(value).unwrap();
        assert_eq!(ordered(&restored).len(), 256);
    }
}
