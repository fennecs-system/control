
use super::*;
#[test]
fn custom_defaults_and_instruction_only_lenses_reach_the_chat_composer() {
    let mut state = crate::State::default();
    state
        .lens_library
        .definitions
        .push(control_context::Definition {
            id: "field-work".into(),
            name: "Field <work>".into(),
            revision: 1,
            instructions: "Local guidance".into(),
            ..Default::default()
        });
    assert_eq!(
        selected(&state.lens_library, "Cookie: control_lens=field-work", None),
        "field-work"
    );
    assert_eq!(
        selected(&state.lens_library, "", Some("autonomy")),
        "autonomy"
    );
    assert_eq!(
        selected(&state.lens_library, "", Some("bad\r\nInjected")),
        "none"
    );
    assert!(cookie("bad\r\nInjected").starts_with("Set-Cookie: control_lens=none;"));
    let cards = library(&state, "field-work");
    assert!(cards.contains("Field &lt;work&gt;") && cards.contains("id=autonomy"));
    let page = crate::chat::landing(&state, "field-work", None);
    assert!(page.contains("value='field-work' form='chat-list-new' checked"));
    assert_eq!(
        crate::chat::form_lenses(&std::collections::HashMap::new(), "field-work"),
        serde_json::json!(["field-work"])
    );
}
#[test]
fn defaults_survive_navigation_and_never_become_redirect_authority() {
    let header = "Cookie: control_operator=unrelated; control_lens=research\r\n";
    let library = control_context::Library::default();
    assert_eq!(selected(&library, header, None), "research");
    assert_eq!(
        selected(&library, header, Some("engineering")),
        "engineering"
    );
    assert_eq!(
        selected(&library, "Cookie: control_lens=bad\r\n", None),
        "none"
    );
    for target in ["//evil.test", "https://evil.test", "/\r\nSet-Cookie: bad=1"] {
        assert_eq!(return_to(target), "/");
    }
    assert_eq!(return_to("/?view=agents"), "/?view=agents");
}
#[test]
fn navigation_is_global_while_the_new_chat_context_stays_selected() {
    let mut state = crate::State::default();
    state
        .runtime
        .enqueue_for(
            "One task".into(),
            crate::runtime::RunKind::Task,
            "test",
            crate::identity::Actor::LocalOperator,
            crate::identity::Action::Task,
        )
        .unwrap();
    for view in ["agents", "monitor", "rooms", "editor", "hub"] {
        let html = crate::render(&state, view, "research", "general");
        assert!(!html.contains("class=\"right-panel\""));
        assert!(!html.contains("id=\"new-chat\""));
        assert!(html.contains("/chats?discuss="));
        assert!(!html.contains("lens-label"));
        assert!(!html.contains("<h2>Engineering"));
    }
    assert!(overview(&state, "research").contains("<h2>Overview</h2>"));
    assert!(overview(&state, "engineering").contains("<b>1</b><span>RUNS</span>"));
}
