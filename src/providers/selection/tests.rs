use super::*;
#[test]
fn explicit_choices_are_configured_and_omission_preserves_captured_models() {
    let runtime = Runtime {
        models: vec![
            Model::captured(Backend::Claude, "reviewer".into()),
            Model::captured(Backend::Codex, "shared".into()),
            Model::captured(Backend::Claude, "shared".into()),
        ],
        ..Default::default()
    };
    let inherited = resolve(&runtime, Backend::Claude, "old-captured-model", None).unwrap();
    assert_eq!(inherited.model, "old-captured-model");
    assert_eq!(inherited.backend, Backend::Claude);
    assert_eq!(
        resolve(&runtime, Backend::Codex, "old", Some("claude:reviewer"))
            .unwrap()
            .backend,
        Backend::Claude
    );
    assert_eq!(
        resolve(&runtime, Backend::Claude, "old", Some("shared"))
            .unwrap()
            .backend,
        Backend::Claude
    );
    assert!(resolve(&runtime, Backend::Codex, "old", Some("unknown")).is_err());
}
#[test]
fn chat_selection_is_durable_does_not_wake_models_and_preserves_active_turns() {
    let mut state = crate::State::default();
    state
        .runtime
        .models
        .push(Model::captured(Backend::Claude, "reviewer".into()));
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/model", "test").unwrap();
    crate::chat::send(&mut state, &chat, "/model claude:reviewer", "test").unwrap();
    assert!(state.runtime.runs.is_empty());
    crate::chat::send(&mut state, &chat, "Inspect this fixture", "test").unwrap();
    assert_eq!(state.runtime.runs[0].backend, Backend::Claude);
    assert_eq!(state.runtime.runs[0].model.as_deref(), Some("reviewer"));
    crate::chat::send(&mut state, &chat, "/model default", "test").unwrap();
    assert_eq!(state.runtime.runs[0].backend, Backend::Claude);
    assert!(crate::chat::send(&mut state, &chat, "/model unavailable", "test").is_err());
    let restored: crate::State =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        restored.chats[0].model.as_ref().unwrap().backend,
        Backend::Codex
    );
    let conversation = crate::conversation::Conversation::of(&state, &state.chats[0]);
    for index in [0, 1, 3] {
        assert_eq!(conversation.entry(index).unwrap().render(), "");
    }
}
