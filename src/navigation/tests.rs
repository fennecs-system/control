
#[test]
fn picker_keeps_the_active_thread_and_links_to_history() {
    let mut state = crate::State::default();
    let mut ids = Vec::new();
    for _ in 0..25 {
        ids.push(crate::chat::create(&mut state));
    }
    let child = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(ids[0].clone());
    let roots = crate::conversations::ordered(&state)
        .iter()
        .filter(|entry| entry.depth == 0)
        .map(|entry| entry.session.id.clone())
        .collect::<Vec<_>>();
    let picker = super::conversations(&state, &child);
    assert_eq!(
        picker.matches("href='/dm/").count(),
        11,
        "Ten parent threads plus their nested child"
    );
    assert!(picker.contains("href='/bots/history'"));
    assert!(picker.contains(&format!("data-chat-branch='{}' open", ids[0])));
    assert!(
        super::conversations(&state, &ids[0])
            .contains(&format!("data-chat-branch='{}' open", ids[0])),
        "The active parent's children must remain reachable"
    );
    assert_eq!(roots.len(), 25, "The child is nested, not a root");
    assert!(!picker.contains("conversation-meta"), "The compact picker does not need per-turn metadata");
}
#[test]
fn manage_routes_preserve_section_navigation_and_global_context() {
    let state = crate::State::default();
    for (view, label) in super::MANAGE {
        let html = crate::render(&state, view, "research", "");
        assert!(html.contains("href='/?view=manage' aria-current='location'"));
        assert!(html.contains(&format!(
            "href='/?view={view}' aria-current='page'>{label}</a>"
        )));
        assert!(html.contains("class=\"body-grid with-section-nav\""));
        assert_eq!(html.matches("aria-label='Manage'").count(), 1);
        assert!(!html.contains("nav-more"));
    }
    let entry = crate::render(&state, "manage", "none", "");
    assert!(entry.contains("href='/?view=nodes' aria-current='page'"));
    for view in ["hub", "knowledge-page", "lens-page", "rooms", "monitor"] {
        let html = crate::render(&state, view, "engineering", "");
        assert!(!html.contains("class='section-navigation'"));
        assert!(!html.contains("nav-more"));
    }
    assert!(super::primary("knowledge-page").contains("href='/knowledge' aria-current='page'"));
    let chat = crate::render(&state, "session-list", "none", "");
    let landing = crate::chat::landing(&state, "none", None);
    assert!(landing.contains("<details class='conversation-toggle'>"));
    assert!(!landing.contains("conversation-toggle' open"));
    assert!(landing.contains("href='/dms?new=1' aria-current='page'"));
    assert!(chat.contains("chat-workspace"));
    assert!(super::primary("session-list").contains("href='/dms' aria-current='page'"));
}
#[test]
fn navigation_shows_dna_and_dms_at_their_existing_urls() {
    let nav = super::primary("session");
    assert!(nav.contains("<a href='/dms' aria-current='page'><span aria-hidden='true'>⌁</span><span>DMs</span></a>"));
    assert!(nav.contains("<a href='/dna'><span aria-hidden='true'>◈</span><span>DNA</span></a>"));
    assert!(super::primary("lens-page").contains("<a href='/dna' aria-current='page'><span aria-hidden='true'>◈</span><span>DNA</span></a>"));
    assert!(!nav.contains("<span>Lenses</span>") && !nav.contains("<span>Chat</span>"));
    let page = crate::render(&crate::State::default(), "lens-page", "none", &crate::lens_library::render(&crate::State::default(), "", None, None, false));
    assert!(page.contains("<h2>DNA</h2>") && page.contains("＋ New gene"));
}
