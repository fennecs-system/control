use super::*;
fn captured(backend: Backend, model: &str) -> Target {
    Target {
        backend,
        model: Some(model.into()),
        ..Default::default()
    }
}
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
    let inherited = resolve(&runtime, &captured(Backend::Claude, "old-captured-model"), None).unwrap();
    assert_eq!(inherited.model.as_deref(), Some("old-captured-model"));
    assert_eq!(inherited.backend, Backend::Claude);
    assert_eq!(
        resolve(&runtime, &captured(Backend::Codex, "old"), Some("claude:reviewer"))
            .unwrap()
            .backend,
        Backend::Claude
    );
    assert_eq!(
        resolve(&runtime, &captured(Backend::Claude, "old"), Some("shared"))
            .unwrap()
            .backend,
        Backend::Claude
    );
    assert!(resolve(&runtime, &captured(Backend::Codex, "old"), Some("unknown")).is_err());
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
    assert_eq!(state.runtime.runs[0].target.backend, Backend::Claude);
    assert_eq!(state.runtime.runs[0].target.model.as_deref(), Some("reviewer"));
    crate::chat::send(&mut state, &chat, "/model default", "test").unwrap();
    assert_eq!(state.runtime.runs[0].target.backend, Backend::Claude);
    assert!(crate::chat::send(&mut state, &chat, "/model unavailable", "test").is_err());
    let restored: crate::State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.chats[0].model.as_ref().unwrap().backend, Backend::Codex);
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
            resolve(&state.runtime, &captured(Backend::Codex, "old"), Some(selector)),
            Err(SelectionError::TextOnly),
            "{selector}"
        );
    }
    assert!(resolve(&state.runtime, &captured(Backend::Codex, "old"), Some("local:missing")).is_err());
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
    assert_eq!(
        resolve(
            &state.runtime,
            &captured(Backend::Codex, "old"),
            Some("local:local-qwen")
        ),
        Ok(captured(Backend::Local, "local-qwen"))
    );
    assert!(crate::compaction::compactor_for(&state, &state.chats[0]).is_none());
    crate::chat::send(&mut state, &chat, "/model local:local-qwen", "test").unwrap();
    assert_eq!(state.chats[0].model, Some(local));
    assert_eq!(
        crate::compaction::compactor_for(&state, &state.chats[0]).unwrap().id,
        "local-qwen"
    );
    crate::chat::send(&mut state, &chat, "Inspect this fixture", "test").unwrap();
    assert_eq!(state.runtime.runs[0].target.backend, Backend::Local);
    assert_eq!(state.runtime.runs[0].target.model.as_deref(), Some("local-qwen"));
    let capacity = crate::providers::capacity(&state.runtime, Backend::Local, "local-qwen").unwrap();
    assert_eq!((capacity.context_tokens, capacity.output_tokens), (32768, 1024));
    // An explicit operator compaction choice for this chat still wins.
    let mut hosted = crate::model_calls::tests::endpoint();
    hosted.id = "operator-choice".into();
    state.runtime.compaction.models.push(hosted);
    state.runtime.compaction.settings.enabled = true;
    state.runtime.compaction.settings.model = Some("operator-choice".into());
    state.runtime.compaction.settings.chats = vec![chat.clone()];
    assert_eq!(
        crate::compaction::compactor_for(&state, &state.chats[0]).unwrap().id,
        "operator-choice"
    );
}
#[test]
fn model_listing_separates_text_only_local_models_from_selectable_ones() {
    let mut state = crate::State::default();
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    crate::chat::create(&mut state);
    let listing = command(&mut state.chats[0], &state.runtime, "").unwrap();
    let (configured, text) = listing.split_once("Text-only").unwrap();
    assert!(!configured.contains("local-qwen"), "{listing}");
    assert!(
        text.contains("local:local-qwen") && text.contains("Serves tool calls"),
        "{listing}"
    );
    state.runtime.compaction.models[0].tools = true;
    let listing = command(&mut state.chats[0], &state.runtime, "").unwrap();
    assert!(
        listing.contains("local:local-qwen") && !listing.contains("Text-only"),
        "{listing}"
    );
}

fn accounts(json: &str) -> crate::accounts::Store<Backend> {
    crate::accounts::Store::parse(json.as_bytes()).unwrap()
}
const TWO_CODEX: &str = r#"{"accounts":[
    {"backend":"codex","name":"second","kind":"subscription"},
    {"backend":"codex","name":"off","kind":"api_key","enabled":false}]}"#;

#[test]
fn the_grammar_parses_every_form_and_refuses_the_rest_with_its_reason() {
    use AccountChoice::{Default as Implicit, Named, Pool};
    use SelectionError as E;
    let name = |text: &str| AccountName::try_from(text).unwrap();
    let named = |backend, account| BackendChoice::Named { backend, account };
    let remote = |name: &str, domain: Option<&str>| NodeChoice::Remote {
        name: NodeName(name.into()),
        domain: domain.map(|d| Domain(d.into())),
    };
    let parsed = |backend, model: &str, node| {
        Ok(Selector {
            backend,
            model: ModelId(model.into()),
            node,
        })
    };
    for (text, expected) in [
        // Today's forms, unchanged.
        (
            "codex:gpt-6.1-sol",
            parsed(named(Backend::Codex, Implicit), "gpt-6.1-sol", NodeChoice::Local),
        ),
        (
            "gpt-6.1-sol",
            parsed(BackendChoice::Any, "gpt-6.1-sol", NodeChoice::Local),
        ),
        (
            "local:qwen",
            parsed(named(Backend::Local, Implicit), "qwen", NodeChoice::Local),
        ),
        (
            "  claude:opus\t",
            parsed(named(Backend::Claude, Implicit), "opus", NodeChoice::Local),
        ),
        (
            "codex:a:b",
            parsed(named(Backend::Codex, Implicit), "a:b", NodeChoice::Local),
        ),
        // Accounts.
        (
            "codex+second:gpt-6.1-sol",
            parsed(
                named(Backend::Codex, Named(name("second"))),
                "gpt-6.1-sol",
                NodeChoice::Local,
            ),
        ),
        (
            "codex+default:m",
            parsed(named(Backend::Codex, Named(name("default"))), "m", NodeChoice::Local),
        ),
        (
            "codex+*:gpt-6.1-sol",
            parsed(named(Backend::Codex, Pool), "gpt-6.1-sol", NodeChoice::Local),
        ),
        // Node addresses.
        (
            "codex:gpt-6.1-sol@fen",
            parsed(named(Backend::Codex, Implicit), "gpt-6.1-sol", remote("fen", None)),
        ),
        (
            "codex+*:a:b@fen:example.org",
            parsed(named(Backend::Codex, Pool), "a:b", remote("fen", Some("example.org"))),
        ),
        ("gpt@fen", parsed(BackendChoice::Any, "gpt", remote("fen", None))),
        // Refusals.
        ("", Err(E::Empty)),
        ("   ", Err(E::Empty)),
        ("codex: gpt", Err(E::Whitespace)),
        ("nope:gpt", Err(E::UnknownBackend("nope".into()))),
        ("Codex:gpt", Err(E::UnknownBackend("Codex".into()))),
        ("nope+x:gpt", Err(E::UnknownBackend("nope".into()))),
        (":gpt", Err(E::MissingBackend)),
        ("+second:gpt", Err(E::MissingBackend)),
        ("codex+:gpt", Err(E::Account(crate::accounts::NameError::Empty))),
        (
            "codex+Second:gpt",
            Err(E::Account(crate::accounts::NameError::Character('S'))),
        ),
        ("codex+a+b:gpt", Err(E::TwoAccounts)),
        ("codex+second", Err(E::MissingModel)),
        ("codex:", Err(E::EmptyModel)),
        ("codex+*:", Err(E::EmptyModel)),
        ("codex:-x", Err(E::InvalidModel)),
        ("codex:gpt@a@b", Err(E::TwoNodes)),
        ("codex:user@name@host", Err(E::TwoNodes)),
        ("codex:gpt@", Err(E::EmptyNode)),
        ("codex:gpt@fen:", Err(E::EmptyNode)),
        ("codex:gpt@:example.org", Err(E::EmptyNode)),
        ("codex:gpt@Fen", Err(E::InvalidNode)),
        ("codex:gpt@fen:exa_mple.org", Err(E::InvalidNode)),
        ("codex:gpt@fen:example..org", Err(E::InvalidNode)),
    ] {
        let result = parse(text);
        assert_eq!(result, expected, "{text:?}");
        // Every accepted form prints back to text that parses to the same selector.
        if let Ok(selector) = result {
            assert_eq!(parse(&selector.to_string()), Ok(selector.clone()), "{text:?}");
            assert_eq!(selector.to_string(), text.trim(), "{text:?}");
        }
    }
    // The backend list in refusals is derived from the backends.
    let unknown = E::UnknownBackend("nope".into()).to_string();
    let invalid = E::InvalidModel.to_string();
    for backend in Backend::ALL {
        assert!(
            unknown.contains(backend.id()) && invalid.contains(backend.id()),
            "{unknown} {invalid}"
        );
    }
}

#[test]
fn accounts_are_checked_when_chosen_and_captured_when_admitted() {
    let mut runtime = Runtime {
        models: vec![Model::captured(Backend::Codex, "gpt".into())],
        backend: Backend::Codex,
        model: Some("gpt".into()),
        accounts: accounts(TWO_CODEX),
        ..Default::default()
    };
    let inherited = captured(Backend::Codex, "gpt");
    let second = AccountName::try_from("second").unwrap();
    // No account typed: the default account, exactly as before.
    let plain = resolve(&runtime, &inherited, Some("codex:gpt")).unwrap();
    assert_eq!(plain, captured(Backend::Codex, "gpt"));
    assert_eq!(plain.to_string(), "codex:gpt");
    let chosen = choose(&runtime, Backend::Codex, "codex+second:gpt").unwrap();
    assert_eq!(chosen.account, AccountChoice::Named(second.clone()));
    assert_eq!(chosen.to_string(), "codex+second:gpt");
    let target = admit(&runtime, &chosen).unwrap();
    assert_eq!(target.account, second);
    assert_eq!(target.to_string(), "codex+second:gpt");
    // Missing, disabled and foreign accounts are refused, listing what exists.
    for (text, why) in [
        (
            "codex+third:gpt",
            "codex has no account third. Accounts of codex: default, second, off.",
        ),
        (
            "codex+off:gpt",
            "Account codex+off is disabled. Accounts of codex: default, second, off.",
        ),
    ] {
        assert_eq!(choose(&runtime, Backend::Codex, text).unwrap_err().to_string(), why);
    }
    runtime.models.push(Model::captured(Backend::Claude, "opus".into()));
    assert_eq!(
        choose(&runtime, Backend::Codex, "claude+second:opus")
            .unwrap_err()
            .to_string(),
        "claude has no account second. Accounts of claude: default."
    );
    // A node address parses but never runs here yet.
    let remote = choose(&runtime, Backend::Codex, "codex:gpt@fen:example.org").unwrap_err();
    assert_eq!(
        remote.to_string(),
        "Running on another node (@fen:example.org) is not available yet; omit the @ address to run on this node."
    );
    // A pool stays a pool until admission, then takes the least-loaded account.
    let pool = choose(&runtime, Backend::Codex, "codex+*:gpt").unwrap();
    assert_eq!(
        (pool.account.clone(), pool.to_string()),
        (AccountChoice::Pool, "codex+*:gpt".into())
    );
    assert!(admit(&runtime, &pool).unwrap().account.is_implicit());
    runtime.enqueue("busy".into(), crate::runtime::RunKind::Chat, "test");
    assert_eq!(admit(&runtime, &pool).unwrap().account, second);
    // Omission inherits the captured account, even a pooled one.
    let pinned = Target {
        account: second.clone(),
        ..inherited.clone()
    };
    assert_eq!(resolve(&runtime, &pinned, None).unwrap(), pinned);
    // A later change of default never re-points captured work.
    runtime.accounts = accounts(&TWO_CODEX.replacen('{', r#"{"defaults":{"codex":"second"},"#, 1));
    assert_eq!(runtime.runs[0].target.account, AccountName::implicit());
    assert_eq!(runtime.default_target().account, second);
}

#[test]
fn chat_choices_are_admitted_per_turn_and_children_inherit_the_captured_account() {
    let mut state = crate::State::default();
    state
        .runtime
        .models
        .push(Model::captured(Backend::Claude, "reviewer".into()));
    state.runtime.accounts = accounts(&TWO_CODEX.replace("codex", "claude"));
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/model claude+second:reviewer", "test").unwrap();
    assert!(state.chats[0].messages[0].reply.contains("claude+second:reviewer"));
    let refused = crate::chat::send(&mut state, &chat, "/model claude+nobody:reviewer", "test").unwrap_err();
    assert!(
        refused.contains("Accounts of claude: default, second, off.") && refused.contains("Forms: "),
        "{refused}"
    );
    crate::chat::send(&mut state, &chat, "Inspect this fixture", "test").unwrap();
    let run = &state.runtime.runs[0];
    assert_eq!(
        (run.target.backend, run.target.account.as_str()),
        (Backend::Claude, "second")
    );
    // The chat's saved choice round-trips with its account.
    let saved = serde_json::to_value(&state.chats[0].model).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({"backend":"claude","account":"second","model":"reviewer"})
    );
    // A child chat keeps the parent's captured account as its choice.
    assert_eq!(run.target.choice().to_string(), "claude+second:reviewer");
    assert_eq!(Target::default().choice().account, AccountChoice::Default);
    // An account removed after it was chosen is refused before anything is queued.
    state.runtime.accounts = crate::accounts::Store::default();
    let queued = state.runtime.runs.len();
    assert!(crate::chat::send(&mut state, &chat, "Again", "test").is_err());
    assert_eq!(state.runtime.runs.len(), queued);
}

#[test]
fn listing_and_errors_teach_the_forms_with_this_nodes_backends() {
    let mut state = crate::State::default();
    state
        .runtime
        .models
        .push(Model::captured(Backend::Claude, "reviewer".into()));
    crate::chat::create(&mut state);
    let listing = command(&mut state.chats[0], &state.runtime, "").unwrap();
    let forms = "Forms: codex:gpt-6.1-sol · claude:reviewer. Another account: control accounts add codex NAME, then codex+NAME:gpt-6.1-sol.";
    assert!(listing.contains(forms), "{listing}");
    assert!(listing.starts_with("Selected codex:gpt-6.1-sol."), "{listing}");
    let error = command(&mut state.chats[0], &state.runtime, "codex+").unwrap_err();
    assert!(error.starts_with("Add :MODEL") && error.ends_with(forms), "{error}");
}

#[test]
fn the_listing_names_this_nodes_real_accounts_one_form_each() {
    let mut state = crate::State::default();
    state.runtime.accounts = accounts(TWO_CODEX);
    crate::chat::create(&mut state);
    let listing = command(&mut state.chats[0], &state.runtime, "").unwrap();
    // A disabled account is not offered; the pool is, once a backend has more than one.
    assert!(
        listing.ends_with(
            "Forms: codex:gpt-6.1-sol · codex+second:gpt-6.1-sol · codex+*:gpt-6.1-sol (any enabled account)."
        ),
        "{listing}"
    );
}

#[test]
fn an_account_at_its_cap_holds_its_next_turn_and_says_why() {
    let mut state = crate::State::default();
    state.runtime.accounts = accounts(TWO_CODEX);
    let chats: Vec<String> = (0..2)
        .map(|_| {
            let chat = crate::chat::create(&mut state);
            crate::chat::send(&mut state, &chat, "/model codex+second:gpt-6.1-sol", "test").unwrap();
            crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
            chat
        })
        .collect();
    assert!(
        state
            .runtime
            .runs
            .iter()
            .all(|run| run.target.account.as_str() == "second")
    );
    assert_eq!(crate::scheduler::next_in(&state), Some(0));
    // Codex rewrites its own sign-in, so a named Codex account runs one at a time.
    state.runtime.runs[0].status = "running".into();
    assert_eq!(crate::scheduler::next_in(&state), None, "the second turn is held");
    let held = crate::providers::accounts::hold(&state.runtime, &state.runtime.runs[1]).unwrap();
    assert_eq!(held, crate::accounts::Hold::Cap(std::num::NonZeroU8::MIN));
    let page = crate::chat::transcript(&state, &chats[1]).unwrap();
    assert!(
        page.contains("codex+second is running its limit of 1 at once"),
        "{page}"
    );
    // Work on another account of the same backend is not held by it.
    let other = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &other, "Build", "test").unwrap();
    assert_eq!(crate::scheduler::next_in(&state), Some(2));
}
