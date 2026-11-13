
use super::*;
pub fn ask_without_io(
    state: &mut State,
    id: &str,
    nonce: &str,
    input: Input,
) -> Result<Value, &'static str> {
    ask_with_save(state, id, nonce, input, |_| Ok(()))
}
pub fn fixture() -> (State, String) {
    let mut state = State::default();
    let chat = chat::create(&mut state);
    state.chats[0].workspace = Some("last-good".into());
    chat::send(&mut state, &chat, "Build the parser", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    let mut attempt = crate::usage::Attempt::new();
    attempt.workspace_id = Some("generation-1".into());
    attempt.parent_artifact = Some("last-good".into());
    run.attempts.push(attempt);
    (state, chat)
}
pub fn input() -> Input {
    Input {
        question: "Which format? <script>".into(),
        options: vec!["Plain text".into(), "JSON".into()],
        recommended: Some(1),
    }
}
pub fn paused() -> (State, String) {
    let (mut state, chat) = fixture();
    let id = state.runtime.runs[0].id.clone();
    ask_with_save(&mut state, &id, "nonce-1", input(), |_| Ok(())).unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "waiting".into();
    run.attempts[0].status = "waiting".into();
    run.attempts[0].artifact = Some("paused-workspace".into());
    *run.result = summary(run.question.as_ref().unwrap());
    (state, chat)
}
#[test]
fn questions_from_spawned_descendants_surface_in_the_top_level_chat() {
    let (mut state, child) = paused();
    let root = chat::create(&mut state);
    let middle = chat::create(&mut state);
    state
        .chats
        .iter_mut()
        .find(|s| s.id == middle)
        .unwrap()
        .parent_chat = Some(root.clone());
    state
        .chats
        .iter_mut()
        .find(|s| s.id == child)
        .unwrap()
        .parent_chat = Some(middle.clone());
    let question = state.runtime.runs[0].question.as_ref().unwrap().id.clone();
    assert_eq!(pending(&state, &root).len(), 1);
    assert_eq!(answer_target(&state, &root, &question), Ok(child.as_str()));
    let unrelated = chat::create(&mut state);
    assert!(pending(&state, &unrelated).is_empty());
}
#[test]
fn question_requires_valid_active_identity_and_durable_admission() {
    let (mut state, chat) = fixture();
    let run = state.runtime.runs[0].id.clone();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        ask_with_save(&mut state, &run, "nonce", input(), |_| Err(
            std::io::Error::other("disk full")
        ))
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    let first = ask_with_save(&mut state, &run, "nonce", input(), |_| Ok(())).unwrap();
    let again = ask_with_save(&mut state, &run, "nonce", input(), |_| {
        panic!("replay must not write")
    })
    .unwrap();
    assert_eq!(first, again);
    assert!(ask_with_save(&mut state, &run, "other", input(), |_| Ok(())).is_err());
    let html = chat::transcript(&state, &chat).unwrap();
    assert!(!html.contains("<script>"));
    assert!(html.contains("Saving the paused workspace"));
    assert!(!html.contains("value='/answer"));
    let mut invalid = input();
    invalid.recommended = Some(4);
    assert!(ask_with_save(&mut state, &run, "nonce", invalid, |_| Ok(())).is_err());
    state.runtime.runs[0].actor = identity::Actor::Unknown;
    assert!(ask_with_save(&mut state, &run, "nonce", input(), |_| Ok(())).is_err());
}

#[test]
fn question_option_and_recommendation_limits_report_the_actual_field() {
    let (mut state, _) = fixture();
    let run = state.runtime.runs[0].id.clone();
    for (value, expected) in [
        (json!({"question":" ","options":["A","B"]}), "question"),
        (
            json!({"question":"é".repeat(1001),"options":["A","B"]}),
            "question",
        ),
        (json!({"question":"Choose","options":["A"]}), "options"),
        (
            json!({"question":"Choose","options":["A","B","C","D","E"]}),
            "options",
        ),
        (
            json!({"question":"Choose","options":["A","  "]}),
            "Each option",
        ),
        (
            json!({"question":"Choose","options":["A","é".repeat(201)]}),
            "Each option",
        ),
        (
            json!({"question":"Choose","options":["Same"," Same "]}),
            "options must be distinct",
        ),
        (
            json!({"question":"Choose","options":["A","B"],"recommended":2}),
            "recommended",
        ),
    ] {
        let error = ask_with_save(
            &mut state,
            &run,
            "1793455807009221658-5",
            parse_input(value).unwrap(),
            |_| panic!("invalid inputs cannot persist a pause"),
        )
        .unwrap_err();
        assert!(error.starts_with(expected), "{error}");
        assert!(!error.contains("identity"));
        assert!(state.runtime.runs[0].question.is_none());
    }
    let input = parse_input(
        json!({"question":"é".repeat(1000),"options":["A","é".repeat(200)],"recommended":1}),
    )
    .unwrap();
    assert!(ask_without_io(&mut state, &run, "1793455807009221658-5", input).is_ok());
}
#[test]
fn pause_survives_reload_and_answer_keeps_authority_and_workspace_baseline() {
    let (mut state, chat) = paused();
    chat::send(&mut state, &chat, "/steer Add tests", "test").unwrap();
    assert_eq!(crate::scheduler::next(&state.runtime), None);
    let other = chat::create(&mut state);
    chat::send(&mut state, &other, "Unrelated work", "test").unwrap();
    assert_eq!(crate::scheduler::next(&state.runtime), Some(2));
    let mut state: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(waiting(&state, &chat).is_some());
    let question = state.runtime.runs[0].question.as_ref().unwrap().id.clone();
    assert!(answer(&mut state, &other, Some(&question), "1", "test").is_err());
    assert!(answer(&mut state, &chat, Some("q-stale"), "1", "test").is_err());
    assert!(answer(&mut state, &chat, Some(&question), "9", "test").is_err());
    let html = chat::requests(&state, &chat).unwrap();
    assert!(html.contains("Which format? &lt;script&gt;") && html.contains("JSON · recommended"));
    chat::send(&mut state, &chat, "2", "test").unwrap();
    assert_eq!(state.runtime.runs[0].status, "continued");
    assert_eq!(state.chats[0].workspace.as_deref(), Some("last-good"));
    let next = state.runtime.runs.last().unwrap();
    assert_eq!(next.objective, "JSON");
    assert_eq!(next.actor, state.runtime.runs[0].actor);
    assert_eq!(next.action, state.runtime.runs[0].action);
    assert_eq!(next.backend, state.runtime.runs[0].backend);
    assert_eq!(next.model, state.runtime.runs[0].model);
    assert_eq!(
        next.continuation.as_ref().unwrap().checkpoint,
        "paused-workspace"
    );
    assert_eq!(
        next.continuation
            .as_ref()
            .unwrap()
            .published_parent
            .as_deref(),
        Some("last-good")
    );
    assert_eq!(crate::scheduler::next(&state.runtime), Some(3));
    assert!(
        chat::prompt(&state, &next.id)
            .unwrap()
            .contains("Original task:\nBuild the parser")
    );
    assert!(answer(&mut state, &chat, Some(&question), "1", "test").is_err());
}

#[test]
fn answering_a_design_question_preserves_the_existing_team_budget() {
    let (mut state, chat) = paused();
    state.runtime.execution.max_agents_per_root = 1;
    let root = state.runtime.runs[0].id.clone();
    let mut used = state.runtime.runs[0].clone();
    used.id = "already-delegated".into();
    used.kind = RunKind::Delegated;
    used.status = "completed".into();
    used.question = None;
    used.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Worker,
        root: root.clone(),
        depth: 1,
        parent: Some(root.clone()),
    });
    state.runtime.runs.push(used);
    answer(&mut state, &chat, None, "1", "test").unwrap();
    let next = state.runtime.runs.last_mut().unwrap();
    next.status = "running".into();
    next.attempts.push(crate::usage::Attempt {
        workspace_id: Some("design-resume".into()),
        ..Default::default()
    });
    assert_eq!(next.persona.as_ref().unwrap().root, root);
    let next_id = next.id.clone();
    assert_eq!(
        crate::delegation::budget(
            &state,
            state.runtime.runs.last().unwrap(),
            crate::delegation::Role::Worker,
        ),
        Err("Configured per-root agent budget exhausted.")
    );
    assert_eq!(
        crate::spawning::request(
            &mut state,
            &next_id,
            "design-resume",
            "spawn-after-design",
            serde_json::from_value(serde_json::json!({"objectives":["A new task"]})).unwrap(),
            |_| panic!("design continuation must not allocate a fresh team"),
        ),
        Err("Configured per-root agent budget exhausted.")
    );
}

#[test]
fn answering_preserves_an_existing_judge_persona_exactly() {
    let (mut state, chat) = paused();
    let persona = crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "existing-team".into(),
        depth: 1,
        parent: Some("parent".into()),
    };
    state.runtime.runs[0].persona = Some(persona.clone());
    answer(&mut state, &chat, None, "1", "test").unwrap();
    assert_eq!(
        state.runtime.runs.last().unwrap().persona.as_ref(),
        Some(&persona)
    );
}
#[test]
fn own_direction_is_not_permission_escalation_and_cancel_removes_wait() {
    let (mut state, chat) = paused();
    chat::send(
        &mut state,
        &chat,
        "Use a compact binary format instead",
        "test",
    )
    .unwrap();
    assert_eq!(
        state.runtime.runs.last().unwrap().action,
        identity::Action::Task
    );
    assert_eq!(
        state.runtime.runs[0]
            .question
            .as_ref()
            .unwrap()
            .answer
            .as_deref(),
        Some("Use a compact binary format instead")
    );
    let (mut state, chat) = paused();
    chat::send(&mut state, &chat, "/cancel", "test").unwrap();
    assert_eq!(state.runtime.runs[0].status, "cancelled");
    assert!(waiting(&state, &chat).is_none());
    assert!(answer(&mut state, &chat, None, "1", "test").is_err());
    let (mut state, chat) = paused();
    state.chats[0].workspace = Some("changed".into());
    assert!(answer(&mut state, &chat, None, "1", "test").is_err());
}
#[test]
fn discord_question_routing_is_bound_to_exact_user_and_dm_channel() {
    let (mut state, chat) = paused();
    let question = state.runtime.runs[0].question.as_ref().unwrap().id.clone();
    let mut message = crate::discord::Incoming {
        message_id: "303".parse().unwrap(),
        user_id: "101".parse().unwrap(),
        channel_id: "202".parse().unwrap(),
        objective: format!("/answer {question} 2"),
        action: identity::Action::Task,
        direct: true,
    };
    state.runtime.runs[0].actor = message.actor();
    state.chats[0].actor = message.actor();
    assert_eq!(discord_chat(&state, &message), chat);
    message.user_id = "404".parse().unwrap();
    assert_eq!(discord_chat(&state, &message), "d-404-202");
    message.user_id = "101".parse().unwrap();
    message.channel_id = "505".parse().unwrap();
    assert_eq!(discord_chat(&state, &message), "d-101-505");
    message.channel_id = "202".parse().unwrap();
    message.direct = false;
    assert_eq!(discord_chat(&state, &message), "d-101-202");
}
#[test]
fn multiple_decisions_preserve_original_objective_and_publication_baseline() {
    let (mut state, chat) = paused();
    answer(&mut state, &chat, None, "1", "test").unwrap();
    let run = &mut state.runtime.runs[1];
    run.status = "running".into();
    let mut attempt = crate::usage::Attempt::new();
    attempt.workspace_id = Some("generation-2".into());
    attempt.parent_artifact = Some("paused-workspace".into());
    run.attempts.push(attempt);
    let id = run.id.clone();
    ask_with_save(&mut state, &id, "nonce-2", input(), |_| Ok(())).unwrap();
    state.runtime.runs[1].status = "waiting".into();
    state.runtime.runs[1].attempts[0].status = "waiting".into();
    state.runtime.runs[1].attempts[0].artifact = Some("second-pause".into());
    answer(&mut state, &chat, None, "2", "test").unwrap();
    let continuation = state.runtime.runs[2].continuation.as_ref().unwrap();
    assert_eq!(continuation.objective, "Build the parser");
    assert_eq!(continuation.checkpoint, "second-pause");
    assert_eq!(continuation.published_parent.as_deref(), Some("last-good"));
}
#[test]
fn answering_at_capacity_replaces_wait_instead_of_exceeding_admission() {
    let (mut state, chat) = paused();
    for _ in 1..crate::scheduler::MAX_PENDING {
        state
            .runtime
            .enqueue("Other task".into(), RunKind::Task, "test");
    }
    assert!(crate::scheduler::admit(&state.runtime).is_err());
    answer(&mut state, &chat, None, "1", "test").unwrap();
    assert!(crate::scheduler::admit(&state.runtime).is_err());
    assert_eq!(
        state
            .runtime
            .runs
            .iter()
            .filter(|r| ["queued", "running", "waiting"].contains(&r.status.as_str()))
            .count(),
        crate::scheduler::MAX_PENDING
    );
}
