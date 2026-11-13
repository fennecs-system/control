
use super::*;
#[test]
fn child_chat_messages_are_collapsed_scoped_and_refresh_ancestors() {
    let mut state = State::default();
    let parent = crate::chat::create(&mut state);
    let child = crate::chat::create(&mut state);
    state.chats[1].parent_chat = Some(parent.clone());
    crate::chat::send(&mut state, &child, "Check inventory", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    let run_id = run.id.clone();
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("generation".into()),
        ..Default::default()
    });
    let revision = state.chats[0].revision;
    let event = Activity {
        id: "message-1".into(),
        kind: "message".into(),
        text: "Checking <inventory>.".into(),
        output: String::new(),
        status: "running".into(),
    };
    assert!(crate::worker::record_activity_for(
        &mut state,
        &run_id,
        "generation",
        event
    ));
    assert!(state.chats[0].revision > revision);
    let html = crate::chat::transcript(&state, &parent).unwrap();
    assert!(html.contains(
        "data-transcript-key='agent-activity'><summary>Agent activity · 1 active agent</summary>"
    ));
    assert!(html.contains("<p class='preserve'>Checking &lt;inventory&gt;.</p>"));
    assert!(!html.contains("<pre>Checking"));
    let detail = crate::chat::detail(&state, &parent).unwrap();
    assert_eq!(detail["agent_activity"][0]["activity"]["kind"], "message");
    let mut stored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(
        crate::chat::transcript(&stored, &child)
            .unwrap()
            .contains("Checking &lt;inventory&gt;.")
    );
    stored.runtime.runs[0].result = "Checking <inventory>.".into();
    assert_eq!(
        crate::chat::transcript(&stored, &child)
            .unwrap()
            .matches("Checking &lt;inventory&gt;.")
            .count(),
        1,
        "Do not repeat the final answer as a progress message"
    );
    stored.chats[1].actor = crate::identity::Actor::Unknown;
    assert!(agent_activity(&stored, &parent).is_empty());
}
