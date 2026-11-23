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
#[test]
fn local_endpoint_models_are_listed_as_text_only_and_refused_for_agent_turns() {
    let mut state = crate::State::default();
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    let mut cli = crate::model_calls::tests::endpoint();
    cli.id = "cli-summary".into();
    cli.url.clear();
    cli.cli = Some(Backend::Codex);
    state.runtime.compaction.models.push(cli);
    let listed = super::super::choices(&state.runtime);
    assert!(listed.contains(&Model::captured(Backend::Local, "local-qwen".into())));
    assert!(!listed.iter().any(|m| m.model == "cli-summary"));
    for selector in ["local:local-qwen", "local-qwen"] {
        assert_eq!(
            resolve(&state.runtime, Backend::Codex, "old", Some(selector)),
            Err(crate::backend::LOCAL_TEXT_ONLY),
            "{selector}"
        );
    }
    assert!(resolve(&state.runtime, Backend::Codex, "old", Some("local:missing")).is_err());
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/model", "test").unwrap();
    assert!(crate::chat::send(&mut state, &chat, "/model local:local-qwen", "test").is_err());
    assert!(state.chats[0].model.is_none());
    assert!(state.runtime.runs.is_empty());
    // A local model is never a saved hosted entry; a local default names its endpoint.
    let local = Model::captured(Backend::Local, "local-qwen".into());
    assert!(super::super::validate_models(std::slice::from_ref(&local), &local).is_err());

    // Declaring tool calls makes the same endpoint selectable for agent turns,
    // captured with the run, and its chat compacts on that endpoint by default.
    state.runtime.compaction.models[0].tools = true;
    assert_eq!(resolve(&state.runtime, Backend::Codex, "old", Some("local:local-qwen")), Ok(local.clone()));
    assert!(crate::compaction::compactor_for(&state, &state.chats[0]).is_none());
    crate::chat::send(&mut state, &chat, "/model local:local-qwen", "test").unwrap();
    assert_eq!(state.chats[0].model, Some(local));
    assert_eq!(crate::compaction::compactor_for(&state, &state.chats[0]).unwrap().id, "local-qwen");
    crate::chat::send(&mut state, &chat, "Inspect this fixture", "test").unwrap();
    assert_eq!(state.runtime.runs[0].backend, Backend::Local);
    assert_eq!(state.runtime.runs[0].model.as_deref(), Some("local-qwen"));
    let capacity = crate::providers::capacity(&state.runtime, Backend::Local, "local-qwen").unwrap();
    assert_eq!((capacity.context_tokens, capacity.output_tokens), (32768, 1024));
    // An explicit operator compaction choice for this chat still wins.
    let mut hosted = crate::model_calls::tests::endpoint();
    hosted.id = "operator-choice".into();
    state.runtime.compaction.models.push(hosted);
    state.runtime.compaction.settings.enabled = true;
    state.runtime.compaction.settings.model = Some("operator-choice".into());
    state.runtime.compaction.settings.chats = vec![chat.clone()];
    assert_eq!(crate::compaction::compactor_for(&state, &state.chats[0]).unwrap().id, "operator-choice");
}
#[test]
fn model_listing_separates_text_only_local_models_from_selectable_ones() {
    let mut state = crate::State::default();
    state.runtime.compaction.models.push(crate::model_calls::tests::endpoint());
    let chat = crate::chat::create(&mut state);
    let listing = command(&mut state, &chat, "").unwrap();
    let (configured, text) = listing.split_once("Text-only").unwrap();
    assert!(!configured.contains("local-qwen"), "{listing}");
    assert!(text.contains("local:local-qwen") && text.contains("Serves tool calls"), "{listing}");
    state.runtime.compaction.models[0].tools = true;
    let listing = command(&mut state, &chat, "").unwrap();
    assert!(listing.contains("local:local-qwen") && !listing.contains("Text-only"), "{listing}");
}
