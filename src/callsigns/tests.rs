use super::*;

fn chats(count: usize) -> crate::State {
    let mut state = crate::State::default();
    for index in 0..count {
        crate::chat::create(&mut state);
        state.chats[index].id = format!("c-{:x}-{index}", 0x1870_0000 + index * 7919);
    }
    state
}

#[test]
fn callsigns_are_deterministic_words_from_the_list() {
    let state = chats(20);
    let first = all(&state.chats);
    assert_eq!(first, all(&state.chats));
    // A chat's callsign depends only on itself and the chats before it.
    assert_eq!(first[..10], all(&state.chats[..10])[..]);
    for (chat, callsign) in state.chats.iter().zip(&first) {
        let shown = label(chat, *callsign);
        assert!(WORDS.contains(&shown.as_ref()), "{shown}");
        assert_eq!(name(&state.chats, &chat.id).as_deref(), Some(shown.as_ref()));
    }
    assert_eq!(name(&state.chats, "missing"), None);
    assert_eq!(name_or_id(&state.chats, "missing"), "missing");
}

#[test]
fn recent_chats_never_share_a_callsign_while_words_remain() {
    let state = chats(500);
    let shown = all(&state.chats).into_iter().map(|c| c.text(WORDS)).collect::<Vec<_>>();
    for window in shown.windows(RECENT + 1) {
        let distinct = window.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(distinct.len(), window.len());
    }
    // Words recycle once a chat is far enough back.
    assert!(shown.iter().collect::<std::collections::HashSet<_>>().len() <= WORDS.len());
}

#[test]
fn exhausted_lists_append_a_number() {
    let ids = ["a", "b", "c", "d", "e"];
    let mut shown = Vec::new();
    assign(ids.into_iter(), &["Red", "Blue"], 8, |c| shown.push(c));
    let words: &'static [&'static str] = &["Red", "Blue"];
    let shown = shown.into_iter().map(|c| c.text(words).into_owned()).collect::<Vec<_>>();
    assert_eq!(shown.iter().filter(|name| !name.contains(' ')).count(), 2);
    assert_eq!(shown.iter().collect::<std::collections::HashSet<_>>().len(), 5, "{shown:?}");
    assert!(shown[2..].iter().all(|name| name.ends_with(" 2") || name.ends_with(" 3")), "{shown:?}");
}

#[test]
fn renames_are_shown_and_do_not_move_other_callsigns() {
    let mut state = chats(10);
    let before = all(&state.chats);
    let renamed = state.chats[3].id.clone();
    state.chats[3].name = Some(validate("  player1 ").unwrap());
    assert_eq!(all(&state.chats), before);
    assert_eq!(name(&state.chats, &renamed).as_deref(), Some("player1"));
    assert_eq!(label(&state.chats[3], before[3]), "player1");
    assert_eq!(label(&state.chats[4], before[4]), name(&state.chats, &state.chats[4].id).unwrap());
}

#[test]
fn names_are_short_and_printable() {
    assert_eq!(validate("  Lead   bot ").unwrap(), "Lead bot");
    assert_eq!(validate("λ advisor").unwrap(), "λ advisor");
    assert!(validate("").is_err());
    assert!(validate("   ").is_err());
    assert!(validate(&"x".repeat(MAX_NAME + 1)).is_err());
    assert!(validate(&"x".repeat(MAX_NAME)).is_ok());
    assert!(validate("bell\u{7}").is_err());
    assert!(validate("left\u{202e}right").is_err());
}

#[test]
fn chats_saved_without_names_derive_them_on_read() {
    // A session written before names existed has no `name` member.
    let old = serde_json::json!({"id":"c-old-0","actor":{"type":"local_operator"},"title":"mc263-client","messages":[]});
    let session: Session = serde_json::from_value(old).unwrap();
    assert!(session.name.is_none());
    let chats = [session];
    let shown = name(&chats, "c-old-0").unwrap();
    assert!(WORDS.contains(&shown.as_ref()));
    // An unnamed chat still serializes without the member.
    assert!(serde_json::to_value(&chats[0]).unwrap().get("name").is_none());
}
