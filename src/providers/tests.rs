use super::*;
use crate::runtime::RunKind;
#[test]
fn capacity_metadata_survives_selection_and_rolls_back_when_save_fails() {
    let mut state = crate::State::default();
    let model = state.runtime.configured_model();
    let mut fields = HashMap::from_iter(
        [
            ("action", "capacity"),
            ("provider", "codex"),
            ("model", model.as_str()),
            ("context_tokens", "32768"),
            ("output_tokens", "1024"),
            ("overhead_tokens", "4096"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
    assert!(save_with(&mut state.runtime, &fields, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert!(capacity(&state.runtime, crate::backend::Backend::Codex, &model).is_none());
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    fields.insert("action".into(), "select".into());
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    assert_eq!(
        capacity(&state.runtime, crate::backend::Backend::Codex, &model)
            .unwrap()
            .context_tokens,
        32768
    );
    assert_eq!(configured(&state.runtime).len(), 1);
}
#[test]
fn text_configuration_is_validated_and_failed_saves_restore_the_previous_settings() {
    let mut state = crate::State::default();
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    let fields = HashMap::from_iter(
        [
            ("action", "compaction"),
            ("enabled", "on"),
            ("model", "local-qwen"),
            ("chats", "c-test"),
            ("recent_entries", "8"),
            ("leaf_entries", "4"),
            ("history_bytes", "20000"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
    let before = state.runtime.compaction.settings.clone();
    assert!(save_with(&mut state.runtime, &fields, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(state.runtime.compaction.settings, before);
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    assert!(state.runtime.compaction.settings.enabled);
    let mut invalid = fields;
    invalid.insert("leaf_entries".into(), "3".into());
    assert!(
        save_with(&mut state.runtime, &invalid, |_| panic!(
            "invalid settings must not reach persistence"
        ))
        .is_err()
    );
    let remove = HashMap::from_iter([
        ("action".into(), "text-remove".into()),
        ("id".into(), "local-qwen".into()),
    ]);
    assert!(
        save_with(&mut state.runtime, &remove, |_| panic!(
            "active compactor cannot disappear"
        ))
        .is_err()
    );
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
            model: "sonnet".into(),
            ..Default::default()
        }]
    );
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    initialize_in(&directory, Backend::Codex).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    // An existing explicit custom/default library is equally immutable to setup.
    let custom = br#"{"models":[{"backend":"codex","model":"custom"}],"backend":"codex","model":"custom"}"#;
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
    let ambiguous = r#"{"models":[{"backend":"claude","model":"sonnet"},{"backend":"claude","model":"opus"}]}"#;
    assert!(migrate(serde_json::from_str(ambiguous).unwrap(), &[]).is_err());
    let explicit_wrong = r#"{"models":[{"backend":"claude","model":"sonnet"}],"backend":"codex","model":"custom"}"#;
    assert!(migrate(serde_json::from_str(explicit_wrong).unwrap(), &[]).is_err());
}
#[test]
fn one_add_form_offers_every_hosted_backend_with_its_default_as_an_example() {
    let html = render(&crate::runtime::Runtime::default());
    let form = html
        .split("<details class='model-add'>")
        .nth(1)
        .unwrap()
        .split("</form>")
        .next()
        .unwrap();
    // One form, however many backends: each is an option, none needs a page edit.
    assert_eq!(form.matches("name='action' value='add'").count(), 1);
    for backend in crate::backend::Backend::hosted() {
        assert!(
            form.contains(&format!(
                "<option value='{}'>{}</option>",
                backend.id(),
                backend.label()
            )),
            "{form}"
        );
        assert!(
            form.split("placeholder='")
                .nth(1)
                .unwrap()
                .split('\'')
                .next()
                .unwrap()
                .contains(&backend.default_model())
        );
    }
    assert!(!form.contains("type='radio'"));
}
#[test]
fn init_options_are_explicit_and_reject_invalid_or_repeated_flags() {
    let parse = |args: &[&str]| initialization_options(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(parse(&[]).unwrap(), (None, None));
    assert_eq!(
        parse(&["--provider", "claude", "--source-root", "/checkout"]).unwrap(),
        (Some(crate::backend::Backend::Claude), Some("/checkout".into()))
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
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.backend, Backend::Codex);
    assert_eq!(state.runtime.configured_model(), original);
    assert_eq!(state.runtime.models.len(), 2);
    fields.insert("action".into(), "select".into());
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    state
        .runtime
        .enqueue("Uses saved selection".into(), RunKind::Task, "test");
    fields.insert("action".into(), "edit".into());
    fields.insert("old_model".into(), "sonnet".into());
    fields.insert("model".into(), "opus".into());
    let previous_models = state.runtime.models.clone();
    assert!(save_with(&mut state.runtime, &fields, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(state.runtime.models, previous_models);
    assert_eq!(state.runtime.configured_model(), "sonnet");
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.configured_model(), "opus");
    assert_eq!(state.runtime.models.len(), 2);
    assert_eq!(state.runtime.runs[0].target.model.as_deref(), Some("sonnet"));
    fields.insert("action".into(), "select".into());
    fields.insert("model".into(), "unknown-model".into());
    assert!(
        save_with(&mut state.runtime, &fields, |_| panic!(
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
    state.runtime.enqueue("Codex task".into(), RunKind::Task, "test");
    let fields = HashMap::from([("provider".into(), "claude".into()), ("model".into(), "sonnet".into())]);
    assert!(save_with(&mut state.runtime, &fields, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(state.runtime.backend, Backend::Codex);
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    state.runtime.enqueue("Claude task".into(), RunKind::Task, "test");
    assert_eq!(state.runtime.runs[0].target.backend, Backend::Codex);
    assert_eq!(state.runtime.runs[1].target.backend, Backend::Claude);
    let mut restored: crate::State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.runtime.runs[1].target.backend, Backend::Claude);
    let id = restored.runtime.runs[1].id.clone();
    restored.runtime.runs[1].status = "failed".into();
    restored
        .runtime
        .action("/retry-run", &HashMap::from([("id".into(), id)]));
    assert_eq!(restored.runtime.runs[1].target.backend, Backend::Claude);
    assert_eq!(restored.runtime.runs[1].target.model.as_deref(), Some("sonnet"));
}
#[test]
fn model_settings_persist_atomically_and_runs_keep_their_selected_model() {
    let mut state = crate::State::default();
    let mut fields = HashMap::from([
        ("provider".into(), "codex".into()),
        ("model".into(), "test-model-one".into()),
    ]);
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    state.runtime.enqueue("First".into(), RunKind::Task, "test");
    fields.insert("model".into(), "test-model-two".into());
    assert!(save_with(&mut state.runtime, &fields, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(state.runtime.configured_model(), "test-model-one");
    save_with(&mut state.runtime, &fields, |_| Ok(())).unwrap();
    state.runtime.enqueue("Second".into(), RunKind::Task, "test");
    assert_eq!(state.runtime.runs[0].target.model.as_deref(), Some("test-model-one"));
    assert_eq!(state.runtime.runs[1].target.model.as_deref(), Some("test-model-two"));
    let checkpoint = serde_json::to_value(&state).unwrap();
    assert!(checkpoint["runtime"].get("model").is_none());
    assert_eq!(checkpoint["runtime"]["runs"][0]["model"], "test-model-one");
    fields.insert("provider".into(), "anthropic".into());
    assert!(
        save_with(&mut state.runtime, &fields, |_| panic!(
            "must not persist unsupported adapter"
        ))
        .is_err()
    );
    fields.insert("provider".into(), "codex".into());
    fields.insert("model".into(), "--override\nsecret".into());
    assert!(save_with(&mut state.runtime, &fields, |_| panic!("must not persist invalid ID")).is_err());
}
fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}
fn edit_fields<'a>(id: &'a str, url: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("action", "text-edit"),
        ("id", id),
        ("model", "qwen-compactor"),
        ("url", url),
        ("revision", "weights-sha-and-runtime"),
        ("context_tokens", "32768"),
        ("output_tokens", "1024"),
        ("input_bytes", "28000"),
        ("output_bytes", "8192"),
        ("timeout_seconds", "600"),
    ]
}
#[test]
fn text_models_are_edited_in_place_and_validated_like_new_entries() {
    let mut state = crate::State::default();
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    state.runtime.compaction.settings = crate::compaction::Settings {
        enabled: true,
        model: Some("local-qwen".into()),
        ..Default::default()
    };
    let mut fields = edit_fields("local-qwen", "http://127.0.0.1:8081/v1");
    fields.push(("tools", "on"));
    // A failed save leaves the entry as it was.
    assert!(
        save_with(&mut state.runtime, &form(&fields), |_| Err(std::io::Error::other(
            "disk full"
        )))
        .is_err()
    );
    assert!(!state.runtime.compaction.models[0].tools);
    save_with(&mut state.runtime, &form(&fields), |_| Ok(())).unwrap();
    let edited = &state.runtime.compaction.models[0];
    assert_eq!(
        (edited.id.as_str(), edited.tools, edited.timeout_seconds),
        ("local-qwen", true, 600)
    );
    assert_eq!(state.runtime.compaction.models.len(), 1);
    // The compactor still points at the same entry, which now also runs agent turns.
    assert_eq!(state.runtime.compaction.settings.model.as_deref(), Some("local-qwen"));
    let local = Model::captured(crate::backend::Backend::Local, "local-qwen".into());
    assert!(agent_capable(&state.runtime, &local));
    // The thinking level: medium by default and then left out of model.json.
    use control_ai::model::Thinking;
    assert_eq!(edited.thinking, Thinking::Medium);
    assert!(
        !serde_json::to_string(edited).unwrap().contains("thinking"),
        "existing identities are unchanged"
    );
    fields.push(("thinking", "low"));
    save_with(&mut state.runtime, &form(&fields), |_| Ok(())).unwrap();
    assert_eq!(state.runtime.compaction.models[0].thinking, Thinking::Low);
    assert!(
        serde_json::to_string(&state.runtime.compaction.models[0])
            .unwrap()
            .contains(r#""thinking":"low""#)
    );
    assert!(render(&state.runtime).contains("<option value='low' selected>low</option>"));
    *fields.last_mut().unwrap() = ("thinking", "maximal");
    assert!(
        save_with(&mut state.runtime, &form(&fields), |_| panic!(
            "an unknown level must not persist"
        ))
        .is_err()
    );
    // Unticking the box makes it text-only again, without a re-add.
    save_with(
        &mut state.runtime,
        &form(&edit_fields("local-qwen", "http://127.0.0.1:8081/v1")),
        |_| Ok(()),
    )
    .unwrap();
    assert!(!agent_capable(&state.runtime, &local));
    // Create-time validation applies: no cloud endpoints, no unknown entries.
    let before = state.runtime.compaction.models.clone();
    for (id, url) in [
        ("local-qwen", "https://example.com/v1"),
        ("missing", "http://127.0.0.1:8081/v1"),
    ] {
        assert!(
            save_with(&mut state.runtime, &form(&edit_fields(id, url)), |_| panic!(
                "invalid edit must not persist"
            ))
            .is_err(),
            "{id} {url}"
        );
    }
    let mut bad = edit_fields("local-qwen", "http://127.0.0.1:8081/v1");
    bad[5] = ("context_tokens", "12");
    assert!(
        save_with(&mut state.runtime, &form(&bad), |_| panic!(
            "invalid limits must not persist"
        ))
        .is_err()
    );
    assert_eq!(state.runtime.compaction.models, before);
    // A CLI entry keeps its adapter and cannot declare tool calls.
    let mut cli = crate::model_calls::tests::endpoint();
    (cli.id, cli.url, cli.cli) = (
        "cli-summary".into(),
        String::new(),
        Some(crate::backend::Backend::Codex),
    );
    state.runtime.compaction.models.push(cli);
    let mut fields = edit_fields("cli-summary", "");
    fields.push(("tools", "on"));
    assert!(
        save_with(&mut state.runtime, &form(&fields), |_| panic!(
            "CLI tool declaration must not persist"
        ))
        .is_err()
    );
    fields.pop();
    save_with(&mut state.runtime, &form(&fields), |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.compaction.models[1].cli,
        Some(crate::backend::Backend::Codex)
    );
}
#[test]
fn one_entry_per_endpoint_model_and_roles_point_at_entries() {
    let mut state = crate::State::default();
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    // A second entry for the same endpoint and model is refused; edit the first.
    let mut add = edit_fields("qwen-agent", "http://127.0.0.1:8081/v1/");
    add[0] = ("action", "text-add");
    add.extend([("adapter", "endpoint"), ("tools", "on")]);
    assert!(
        save_with(&mut state.runtime, &form(&add), |_| panic!(
            "duplicate must not persist"
        ))
        .is_err()
    );
    // A different model on another port is its own entry: a 7B compactor next to a 27B agent.
    let mut small = add.clone();
    small[1] = ("id", "qwen-small");
    small[2] = ("model", "qwen-7b");
    small[3] = ("url", "http://127.0.0.1:8082/v1");
    small.pop();
    save_with(&mut state.runtime, &form(&small), |_| Ok(())).unwrap();
    assert_eq!(state.runtime.compaction.models.len(), 2);
}
#[test]
fn duplicate_role_entries_merge_on_load_keeping_settings_and_references() {
    use crate::model_calls::tests::endpoint;
    let summary = crate::model_calls::Endpoint {
        id: "qwen-summary".into(),
        ..endpoint()
    };
    let agent = crate::model_calls::Endpoint {
        id: "qwen-local".into(),
        url: "http://127.0.0.1:8081/v1/".into(),
        tools: true,
        timeout_seconds: 1800,
        ..endpoint()
    };
    let small = crate::model_calls::Endpoint {
        id: "qwen-small".into(),
        model: "qwen-7b".into(),
        url: "http://127.0.0.1:8082/v1".into(),
        ..endpoint()
    };
    let mut models = vec![summary, small.clone(), agent.clone()];
    let mut settings = crate::compaction::Settings {
        enabled: true,
        model: Some("qwen-summary".into()),
        ..Default::default()
    };
    let renamed = merge_duplicates(&mut models, &mut settings);
    assert_eq!(renamed, vec![("qwen-summary".to_owned(), "qwen-local".to_owned())]);
    // The tool-capable entry survives in the first entry's place; distinct models stay.
    assert_eq!(models, vec![agent, small]);
    assert_eq!(settings.model.as_deref(), Some("qwen-local"));
    validate_text(&models, &settings).unwrap();
    let mut state = crate::State::default();
    crate::chat::create(&mut state);
    state.chats[0].model = Some(Model::captured(crate::backend::Backend::Local, "qwen-summary".into()));
    assert!(repoint(&mut state.chats, &renamed));
    assert_eq!(state.chats[0].model.as_ref().unwrap().model, "qwen-local");
    // Two text-only duplicates keep the first name; nothing to merge is a no-op.
    let mut models = vec![
        endpoint(),
        crate::model_calls::Endpoint {
            id: "copy".into(),
            ..endpoint()
        },
    ];
    let mut settings = crate::compaction::Settings {
        model: Some("copy".into()),
        ..Default::default()
    };
    assert_eq!(
        merge_duplicates(&mut models, &mut settings),
        vec![("copy".to_owned(), "local-qwen".to_owned())]
    );
    assert_eq!((models.len(), settings.model.as_deref()), (1, Some("local-qwen")));
    assert!(merge_duplicates(&mut models, &mut settings).is_empty());
}
#[test]
fn agent_models_group_integrations_and_local_and_summary_only_entries_sit_apart() {
    let mut state = crate::State::default();
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    let agent = crate::model_calls::Endpoint {
        id: "qwen-agent".into(),
        model: "qwen-27b".into(),
        tools: true,
        ..crate::model_calls::tests::endpoint()
    };
    state.runtime.compaction.models.push(agent);
    let html = render(&state.runtime);
    let (agents, summaries) = html.split_once("aria-labelledby='summary-models-heading'").unwrap();
    let (integrations, local) = agents.split_once("<h4 class='model-group'>Local</h4>").unwrap();
    // Integrations: the hosted default, selectable from its row.
    assert!(
        integrations.contains("<h4 class='model-group'>Integrations</h4>")
            && integrations.contains("name='provider' value='codex'")
    );
    assert!(integrations.contains("aria-pressed='true'") && integrations.contains("class='model-default'>Default"));
    // Local: the tool-serving endpoint, once, with the same one-click default
    // in its row; the text-only endpoint is not an agent model.
    let select = "name='action' value='select'><input type='hidden' name='provider' value='local'><input type='hidden' name='model' value='qwen-agent'><button class='model-choice' aria-pressed='false'>";
    assert!(local.contains(select), "{local}");
    assert!(!agents.contains("title='local-qwen'") && !summaries.contains("title='qwen-agent'"));
    assert_eq!(html.matches("title='qwen-agent'").count(), 1);
    // Summary-only entries have no default control; their settings are behind Edit, in one place.
    let row = |html: &str, id: &str| {
        html.split(&format!("title='{id}'"))
            .nth(1)
            .unwrap()
            .split("</section>")
            .next()
            .unwrap()
            .to_owned()
    };
    let text_only = row(summaries, "local-qwen");
    assert!(text_only.contains("<dd>summaries · text generation</dd>") && !text_only.contains("value='select'"));
    assert!(text_only.contains("value='text-edit'") && text_only.contains("name='tools' type='checkbox'>"));
    assert!(text_only.contains("value='http://127.0.0.1:8081/v1'"));
    let tools = row(local, "qwen-agent");
    assert!(
        tools.contains("summaries · text generation · agent turns")
            && tools.contains("name='tools' type='checkbox' checked")
    );
    assert_eq!(
        html.matches("name='url'").count(),
        4,
        "one connection form per entry and per add form"
    );
    // Nothing but the list is open on load, and the old section name is gone.
    assert!(!html.contains("<details open") && !html.contains("Text models"));

    // One click makes the local model the node default, saved without becoming a hosted entry.
    let pick = |provider: &str, model: &str| form(&[("action", "select"), ("provider", provider), ("model", model)]);
    assert!(
        save_with(&mut state.runtime, &pick("local", "local-qwen"), |_| panic!(
            "a text-only endpoint is not a default"
        ))
        .is_err()
    );
    assert!(
        save_with(&mut state.runtime, &pick("local", "missing"), |_| panic!(
            "an unknown endpoint is not a default"
        ))
        .is_err()
    );
    let mut saved = Vec::new();
    save_with(&mut state.runtime, &pick("local", "qwen-agent"), |runtime| {
        saved = serde_json::to_vec(&Settings {
            models: configured(runtime),
            backend: runtime.backend,
            model: runtime.configured_model(),
            text_models: runtime.compaction.models.clone(),
            compaction: runtime.compaction.settings.clone(),
        })
        .unwrap();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        (state.runtime.backend, state.runtime.configured_model()),
        (crate::backend::Backend::Local, "qwen-agent".to_owned())
    );
    assert!(
        configured(&state.runtime)
            .iter()
            .all(|m| m.backend != crate::backend::Backend::Local)
    );
    assert!(row(&render(&state.runtime), "qwen-agent").contains("class='model-default'>Default"));
    // The saved settings load back: a local default is checked against its endpoint.
    let settings = migrate(serde_json::from_slice(&saved).unwrap(), &[]).unwrap();
    assert!(local_default(
        &settings.text_models,
        &Model::captured(settings.backend, settings.model.clone())
    ));
    // New DMs run on it, and compact on it by default.
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Inspect this fixture", "test").unwrap();
    assert_eq!(
        (
            state.runtime.runs[0].target.backend,
            state.runtime.runs[0].target.model.as_deref()
        ),
        (crate::backend::Backend::Local, Some("qwen-agent"))
    );
    assert_eq!(
        crate::compaction::compactor_for(&state, &state.chats[0]).unwrap().id,
        "qwen-agent"
    );
    // The default cannot be removed or lose tool calls until another is chosen.
    let remove = form(&[("action", "text-remove"), ("id", "qwen-agent")]);
    assert_eq!(
        save_with(&mut state.runtime, &remove, |_| panic!("the default must stay")),
        Err(LOCAL_DEFAULT)
    );
    let hosted = configured(&state.runtime)[0].model.clone();
    save_with(
        &mut state.runtime,
        &form(&[("action", "select"), ("provider", "codex"), ("model", &hosted)]),
        |_| Ok(()),
    )
    .unwrap();
    save_with(&mut state.runtime, &remove, |_| Ok(())).unwrap();
}
#[test]
fn local_endpoint_cards_carry_a_live_statistics_line_read_only_while_open() {
    // No local endpoint: nothing for the page client to poll.
    let mut runtime = crate::runtime::Runtime::default();
    let cli = crate::model_calls::Endpoint {
        id: "codex-summary".into(),
        cli: Some(crate::backend::Backend::Codex),
        url: String::new(),
        ..crate::model_calls::tests::endpoint()
    };
    runtime.compaction.models.push(cli);
    let html = render(&runtime);
    assert!(!html.contains("data-live-src") && !html.contains("data-live="));
    // A local endpoint's card gets a slot keyed by its name, filled from
    // /providers/live every 5 s while the page is open.
    runtime.compaction.models.push(crate::model_calls::tests::endpoint());
    let html = render(&runtime);
    assert_eq!(
        html.matches("data-live-src='/providers/live' data-live-every='5'")
            .count(),
        1
    );
    let card = html
        .split("title='local-qwen'")
        .nth(1)
        .unwrap()
        .split("</section>")
        .next()
        .unwrap();
    assert!(card.contains("data-live='local-qwen' data-live-fallback='Live statistics unavailable.'"));
    let cli_card = html
        .split("title='codex-summary'")
        .nth(1)
        .unwrap()
        .split("</section>")
        .next()
        .unwrap();
    assert!(!cli_card.contains("data-live="));
    // The endpoint answers only GET and only for its own path.
    let state = std::sync::Mutex::new(crate::State::default());
    let mut response = Vec::new();
    assert!(!live_http(&mut response, &state, "GET", "/providers"));
    assert!(live_http(&mut response, &state, "POST", "/providers/live"));
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 400"));
    response.clear();
    assert!(live_http(&mut response, &state, "GET", "/providers/live"));
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 200") && text.ends_with("{}"), "{text}");
}

/// Selections saved before accounts existed load and save back unchanged:
/// nothing about accounts is written unless an account is chosen.
#[test]
fn todays_saved_selections_load_and_save_unchanged() {
    // model.json: a library entry with capacity and one without.
    let models = r#"[{"backend":"codex","model":"gpt-6.1-sol"},{"backend":"claude","model":"opus","capacity":{"context_tokens":200000,"output_tokens":2048,"overhead_tokens":4096}}]"#;
    let loaded: Vec<Model> = serde_json::from_str(models).unwrap();
    assert!(loaded.iter().all(|m| m.account.is_default()));
    assert_eq!(serde_json::to_string(&loaded).unwrap(), models);
    let settings = format!(r#"{{"models":{models},"backend":"claude","model":"opus"}}"#);
    let settings: Settings = serde_json::from_str(&settings).unwrap();
    let saved: Settings = serde_json::from_slice(&serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
    assert_eq!(saved.models, settings.models);

    // State: a chat's selection, a run and its attempt, in today's saved form.
    let mut state = crate::State::default();
    state
        .runtime
        .models
        .push(Model::captured(crate::backend::Backend::Claude, "reviewer".into()));
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/model claude:reviewer", "test").unwrap();
    crate::chat::send(&mut state, &chat, "Inspect this fixture", "test").unwrap();
    let mut attempt = crate::usage::Attempt::new();
    attempt.backend = crate::backend::Backend::Claude;
    attempt.model = "reviewer".into();
    state.runtime.runs[0].attempts.push(attempt);
    let today = serde_json::to_value(&state).unwrap();
    assert_eq!(
        today["chats"][0]["model"],
        serde_json::json!({"backend":"claude","model":"reviewer"})
    );
    let run = &today["runtime"]["runs"][0];
    assert_eq!((&run["backend"], &run["model"]), (&"claude".into(), &"reviewer".into()));
    assert_eq!(run["attempts"][0]["backend"], "claude");
    assert!(!today.to_string().contains("\"account\""), "{today}");
    let restored: crate::State = serde_json::from_value(today.clone()).unwrap();
    assert_eq!(serde_json::to_value(&restored).unwrap(), today);
    let run = &restored.runtime.runs[0];
    assert!(run.target.account.is_implicit() && run.attempts[0].account.is_implicit());
    // A record from before `target` existed reads as the same target.
    let mut old = today["runtime"]["runs"][0].clone();
    old.as_object_mut().unwrap().remove("model");
    let record: crate::application::runs::Record = serde_json::from_value(old).unwrap();
    assert_eq!(
        (record.target.backend, record.target.model),
        (crate::backend::Backend::Claude, None)
    );
}
