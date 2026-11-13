
#[test]
fn conversation_pages_keep_threads_whole_and_cursors_stable() {
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
        picker.matches("href='/chat/").count(),
        11,
        "Ten parent threads plus their nested child"
    );
    assert!(picker.contains("/chats/history"));
    assert!(picker.contains(&format!("data-chat-branch='{}' open", ids[0])));
    assert!(
        super::conversations(&state, &ids[0])
            .contains(&format!("data-chat-branch='{}' open", ids[0])),
        "The active parent's children must remain reachable"
    );
    let page = super::history(&state, Some(&roots[9]), None);
    assert_eq!(page.matches("href='/chat/").count(), 10);
    for id in &roots[10..20] {
        assert!(page.contains(&format!("href='/chat/{id}'")));
    }
    assert!(!page.contains(&format!("href='/chat/{child}'")));
    crate::chat::create(&mut state);
    assert_eq!(
        super::history(&state, Some(&roots[9]), None),
        page,
        "New chats must not shift an older cursor boundary"
    );
    let last = super::history(&state, Some(&roots[19]), None);
    assert_eq!(
        last.matches("href='/chat/").count(),
        6,
        "The oldest root retains its nested child on the final page"
    );
    assert!(!last.contains("Older →"));
    assert!(last.contains("← Newer"));
    assert_eq!(super::history(&state, None, Some(&roots[20])), page);
    let mut response = Vec::new();
    assert!(crate::session_http::handle(
        &mut response,
        &mut state,
        "GET",
        &format!("/chats/history?older={}", roots[9]),
        "none",
        "",
        &[]
    ));
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains(&page));
    assert!(response.contains("href='/chats/history' aria-current='page'"));
    assert!(
        !response.contains("body-grid chat-workspace"),
        "History must use the normal page layout, not the live chat shell"
    );
}
#[test]
fn history_shows_latest_model_per_chat_and_keeps_threads_collapsible() {
    let mut state = crate::State::default();
    let parent = crate::chat::create(&mut state);
    let child = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(parent.clone());
    for (chat, model) in [
        (&parent, "obsolete"),
        (&child, "child-model"),
        (&parent, "<latest>"),
    ] {
        state
            .runtime
            .enqueue("Task".into(), crate::runtime::RunKind::Chat, "test");
        let run = state.runtime.runs.last_mut().unwrap();
        run.chat_id = Some(chat.clone());
        run.model = Some(model.into());
        run.status = "completed".into();
    }
    let html = super::history(&state, None, None);
    assert!(html.contains("&lt;latest&gt;") && html.contains("child-model"));
    assert!(!html.contains("obsolete") && !html.contains("<latest>"));
    assert!(html.contains(&format!("data-chat-branch='{parent}'><summary data-conversation-depth='0'><a class='conversation-link'")));
    assert_eq!(html.matches("class='conversation-meta'").count(), 2);
    assert!(
        !super::conversations(&state, &parent).contains("class='conversation-meta'"),
        "The compact picker does not need per-turn metadata"
    );
}
#[test]
fn run_pages_render_only_ten_records_and_keep_cursor_boundaries() {
    let mut state = crate::State::default();
    for index in 0..25 {
        state.runtime.enqueue(
            format!("task-{index}"),
            crate::runtime::RunKind::Chat,
            "test",
        );
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
    }
    let newest = state.runtime.runs_page(None, None);
    assert_eq!(newest.matches("<article class='record'>").count(), 10);
    assert!(newest.contains("<h3>task-24</h3>"));
    assert!(!newest.contains("<h3>task-14</h3>"));
    assert!(!newest.contains("← Newer"));
    let cursor = state.runtime.runs[15].id.clone();
    let middle = state.runtime.runs_page(Some(&cursor), None);
    assert_eq!(middle.matches("<article class='record'>").count(), 10);
    assert!(middle.contains("<h3>task-14</h3>"));
    assert!(middle.contains("<h3>task-5</h3>"));
    state
        .runtime
        .enqueue("new task".into(), crate::runtime::RunKind::Chat, "test");
    assert_eq!(state.runtime.runs_page(Some(&cursor), None), middle);
    let html = crate::public_render(
        &state,
        &crate::fields(&format!("view=runs&older={cursor}")),
        "none",
    );
    assert!(html.contains(&middle));
    assert!(super::primary("runs").contains("href='/chats/history' aria-current='page'"));
    let last_cursor = state.runtime.runs[5].id.clone();
    let last = state.runtime.runs_page(Some(&last_cursor), None);
    assert_eq!(last.matches("<article class='record'>").count(), 5);
    assert!(!last.contains("Older →"));
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
    assert!(landing.contains("href='/chats?new=1' aria-current='page'"));
    assert!(chat.contains("chat-workspace"));
    assert!(super::primary("session-list").contains("href='/chats' aria-current='page'"));
}
