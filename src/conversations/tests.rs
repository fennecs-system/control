
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
        [first.as_str(), second.as_str(), child.as_str()]
    );
    assert_eq!(entries[2].depth, 1);
    assert_eq!(entries[2].parent, Some(second.as_str()));
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        ordered(&restored)
            .iter()
            .map(|e| e.session.id.as_str())
            .collect::<Vec<_>>(),
        [first.as_str(), second.as_str(), child.as_str()]
    );
    let api = chat::list(&restored);
    assert_eq!(api[2]["parent_chat"], second);
    assert_eq!(api[2]["depth"], 1);
    let html = crate::navigation::conversations(&restored, &child);
    assert!(html.contains(&format!("data-chat-branch='{second}' open")));
    let html = crate::navigation::conversations(&restored, &second);
    assert!(
        html.contains(&format!("data-chat-branch='{second}' open")),
        "Opening a parent conversation must keep its children reachable"
    );
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
