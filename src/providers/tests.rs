
use super::*;
use crate::runtime::RunKind;
#[test]
fn capacity_metadata_survives_selection_and_rolls_back_when_save_fails() {
    let mut state=crate::State::default();let model=state.runtime.configured_model();
    let mut fields=HashMap::from_iter([("action","capacity"),("provider","codex"),("model",model.as_str()),("context_tokens","32768"),("output_tokens","1024"),("overhead_tokens","4096")].map(|(k,v)|(k.to_owned(),v.to_owned())));
    assert!(save_with(&mut state,&fields,|_|Err(std::io::Error::other("disk full"))).is_err());
    assert!(capacity(&state.runtime,crate::backend::Backend::Codex,&model).is_none());
    save_with(&mut state,&fields,|_|Ok(())).unwrap();
    fields.insert("action".into(),"select".into());save_with(&mut state,&fields,|_|Ok(())).unwrap();
    assert_eq!(capacity(&state.runtime,crate::backend::Backend::Codex,&model).unwrap().context_tokens,32768);
    assert_eq!(configured(&state.runtime).len(),1);
}
#[test]
fn text_configuration_is_validated_and_failed_saves_restore_the_previous_settings() {
    let mut state=crate::State::default();
    state.runtime.compaction.models.push(crate::model_calls::tests::endpoint());
    let fields=HashMap::from_iter([
        ("action","compaction"),("enabled","on"),("model","local-qwen"),("chats","c-test"),
        ("recent_entries","8"),("leaf_entries","4"),("history_bytes","20000")
    ].map(|(k,v)|(k.to_owned(),v.to_owned())));
    let before=state.runtime.compaction.settings.clone();
    assert!(save_with(&mut state,&fields,|_|Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(state.runtime.compaction.settings,before);
    save_with(&mut state,&fields,|_|Ok(())).unwrap();
    assert!(state.runtime.compaction.settings.enabled);
    let mut invalid=fields;invalid.insert("leaf_entries".into(),"3".into());
    assert!(save_with(&mut state,&invalid,|_|panic!("invalid settings must not reach persistence")).is_err());
    let remove=HashMap::from_iter([("action".into(),"text-remove".into()),("id".into(),"local-qwen".into())]);
    assert!(save_with(&mut state,&remove,|_|panic!("active compactor cannot disappear")).is_err());
}
#[test]
fn fresh_claude_setup_selects_sonnet_and_reruns_preserve_the_library() {
    use crate::backend::Backend;
    use std::os::unix::fs::PermissionsExt;
    let directory = std::env::temp_dir().join(format!("control-model-init-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    initialize_in(&directory, Backend::Claude).unwrap();
    let path = directory.join("model.json");
    let original = std::fs::read(&path).unwrap();
    let settings = migrate(
        serde_json::from_slice(&original).unwrap(),
        &[Backend::Codex, Backend::Claude],
    )
    .unwrap();
    assert_eq!(settings.backend, Backend::Claude);
    assert_eq!(settings.model, "sonnet");
    assert_eq!(
        settings.models,
        vec![Model {
            backend: Backend::Claude,
            model: "sonnet".into()
            ,..Default::default()
        }]
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    initialize_in(&directory, Backend::Codex).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    // An existing explicit custom/default library is equally immutable to setup.
    let custom =
        br#"{"models":[{"backend":"codex","model":"custom"}],"backend":"codex","model":"custom"}"#;
    std::fs::write(&path, custom).unwrap();
    initialize_in(&directory, Backend::Claude).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), custom);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn sole_configured_model_is_selected_without_an_explicit_default() {
    use crate::backend::Backend;
    let settings = migrate(
        serde_json::from_str(r#"{"models":[{"backend":"claude","model":"sonnet"}]}"#).unwrap(),
        &[Backend::Codex, Backend::Claude],
    )
    .unwrap();
    assert_eq!(settings.backend, Backend::Claude);
    assert_eq!(settings.model, "sonnet");
    assert_eq!(settings.models.len(), 1);
    let ambiguous =
        r#"{"models":[{"backend":"claude","model":"sonnet"},{"backend":"claude","model":"opus"}]}"#;
    assert!(migrate(serde_json::from_str(ambiguous).unwrap(), &[]).is_err());
    let explicit_wrong =
        r#"{"models":[{"backend":"claude","model":"sonnet"}],"backend":"codex","model":"custom"}"#;
    assert!(migrate(serde_json::from_str(explicit_wrong).unwrap(), &[]).is_err());
}
#[test]
fn provider_forms_have_bound_prefilled_defaults() {
    let html = render(&crate::runtime::Runtime::default());
    let forms = html.split("<details class='model-add'>").nth(1).unwrap();
    let claude = forms
        .split("name='provider' value='claude'")
        .nth(1)
        .unwrap();
    assert!(claude.contains("value='sonnet'"));
    assert!(forms.contains(&format!(
        "value='{}'",
        crate::backend::Backend::Codex.default_model()
    )));
    assert!(!forms.contains("type='radio'"));
}
#[test]
fn init_options_are_explicit_and_reject_invalid_or_repeated_flags() {
    let parse = |args: &[&str]| {
        initialization_options(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    };
    assert_eq!(parse(&[]).unwrap(), (None, None));
    assert_eq!(
        parse(&["--provider", "claude", "--source-root", "/checkout"]).unwrap(),
        (
            Some(crate::backend::Backend::Claude),
            Some("/checkout".into())
        )
    );
    for args in [
        vec!["--provider"],
        vec!["--provider", "other"],
        vec!["--source-root", "relative"],
        vec!["--provider", "claude", "--provider", "codex"],
        vec!["--unknown", "value"],
    ] {
        assert!(parse(&args).is_err());
    }
}
#[test]
fn legacy_selection_migrates_and_saved_libraries_roundtrip_without_reseeding() {
    use crate::backend::Backend;
    let old: Settings = serde_json::from_str(r#"{"model":"custom-codex"}"#).unwrap();
    let mut settings = migrate(old, &[Backend::Codex, Backend::Claude]).unwrap();
    assert_eq!(settings.backend, Backend::Codex);
    assert_eq!(settings.model, "custom-codex");
    assert_eq!(settings.models.len(), 2);
    settings.models[1].model = "custom-claude".into();
    let reloaded = migrate(
        serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap(),
        &[Backend::Codex, Backend::Claude],
    )
    .unwrap();
    assert_eq!(reloaded.models, settings.models);
    let invalid = Settings {
        backend: Backend::Claude,
        model: "not-in-list".into(),
        models: settings.models,
        ..Default::default()
    };
    assert!(migrate(invalid, &[]).is_err());
}
#[test]
fn adding_selecting_and_editing_keep_the_library_and_selection_separate() {
    use crate::backend::Backend;
    let mut state = crate::State::default();
    let original = state.runtime.configured_model();
    let mut fields = HashMap::from([
        ("action".into(), "add".into()),
        ("provider".into(), "claude".into()),
        ("model".into(), "sonnet".into()),
    ]);
    save_with(&mut state, &fields, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.backend, Backend::Codex);
    assert_eq!(state.runtime.configured_model(), original);
    assert_eq!(state.runtime.models.len(), 2);
    fields.insert("action".into(), "select".into());
    save_with(&mut state, &fields, |_| Ok(())).unwrap();
    state
        .runtime
        .enqueue("Uses saved selection".into(), RunKind::Task, "test");
    fields.insert("action".into(), "edit".into());
    fields.insert("old_model".into(), "sonnet".into());
    fields.insert("model".into(), "opus".into());
    let previous_models = state.runtime.models.clone();
    assert!(
        save_with(&mut state, &fields, |_| Err(std::io::Error::other(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(state.runtime.models, previous_models);
    assert_eq!(state.runtime.configured_model(), "sonnet");
    save_with(&mut state, &fields, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.configured_model(), "opus");
    assert_eq!(state.runtime.models.len(), 2);
    assert_eq!(state.runtime.runs[0].model.as_deref(), Some("sonnet"));
    fields.insert("action".into(), "select".into());
    fields.insert("model".into(), "unknown-model".into());
    assert!(
        save_with(&mut state, &fields, |_| panic!(
            "unknown selection must not save"
        ))
        .is_err()
    );
    assert_eq!(state.runtime.configured_model(), "opus");
}
#[test]
fn backend_selection_is_captured_and_failed_settings_do_not_reroute_runs() {
    use crate::backend::Backend;
    let mut state = crate::State::default();
    state
        .runtime
        .enqueue("Codex task".into(), RunKind::Task, "test");
    let fields = HashMap::from([
        ("provider".into(), "claude".into()),
        ("model".into(), "sonnet".into()),
    ]);
    assert!(
        save_with(&mut state, &fields, |_| Err(std::io::Error::other(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(state.runtime.backend, Backend::Codex);
    save_with(&mut state, &fields, |_| Ok(())).unwrap();
    state
        .runtime
        .enqueue("Claude task".into(), RunKind::Task, "test");
    assert_eq!(state.runtime.runs[0].backend, Backend::Codex);
    assert_eq!(state.runtime.runs[1].backend, Backend::Claude);
    let mut restored: crate::State =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.runtime.runs[1].backend, Backend::Claude);
    let id = restored.runtime.runs[1].id.clone();
    restored.runtime.runs[1].status = "failed".into();
    restored
        .runtime
        .action("/retry-run", &HashMap::from([("id".into(), id)]));
    assert_eq!(restored.runtime.runs[1].backend, Backend::Claude);
    assert_eq!(restored.runtime.runs[1].model.as_deref(), Some("sonnet"));
}
#[test]
fn model_settings_persist_atomically_and_runs_keep_their_selected_model() {
    let mut state = crate::State::default();
    let mut fields = HashMap::from([
        ("provider".into(), "codex".into()),
        ("model".into(), "test-model-one".into()),
    ]);
    save_with(&mut state, &fields, |_| Ok(())).unwrap();
    state.runtime.enqueue("First".into(), RunKind::Task, "test");
    fields.insert("model".into(), "test-model-two".into());
    assert!(
        save_with(&mut state, &fields, |_| Err(std::io::Error::other(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(state.runtime.configured_model(), "test-model-one");
    save_with(&mut state, &fields, |_| Ok(())).unwrap();
    state
        .runtime
        .enqueue("Second".into(), RunKind::Task, "test");
    assert_eq!(
        state.runtime.runs[0].model.as_deref(),
        Some("test-model-one")
    );
    assert_eq!(
        state.runtime.runs[1].model.as_deref(),
        Some("test-model-two")
    );
    let checkpoint = serde_json::to_value(&state).unwrap();
    assert!(checkpoint["runtime"].get("model").is_none());
    assert_eq!(checkpoint["runtime"]["runs"][0]["model"], "test-model-one");
    fields.insert("provider".into(), "anthropic".into());
    assert!(
        save_with(&mut state, &fields, |_| panic!(
            "must not persist unsupported adapter"
        ))
        .is_err()
    );
    fields.insert("provider".into(), "codex".into());
    fields.insert("model".into(), "--override\nsecret".into());
    assert!(
        save_with(&mut state, &fields, |_| panic!(
            "must not persist invalid ID"
        ))
        .is_err()
    );
}
