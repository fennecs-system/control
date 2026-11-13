
use super::*;
use crate::{
    backend::Backend,
    identity::{Action, Actor},
    usage::Attempt,
};
pub(super) fn active() -> (State, String) {
    let mut s = State::default();
    let chat = chat::create(&mut s);
    s.runtime
        .enqueue_for(
            "Build a page".into(),
            RunKind::Chat,
            "web",
            Actor::LocalOperator,
            Action::Task,
        )
        .unwrap();
    let run = s.runtime.runs.last_mut().unwrap();
    run.chat_id = Some(chat);
    run.status = "running".into();
    run.backend = Backend::Codex;
    run.model = Some("captured-model".into());
    run.attempts.push(Attempt {
        workspace_id: Some("parent-attempt".into()),
        parent_artifact: Some("immutable-baseline".into()),
        ..Default::default()
    });
    let id = run.id.clone();
    (s, id)
}
pub(super) fn input(n: usize) -> Input {
    Input {
        objectives: (0..n).map(|n| format!("Inspect page {n}")).collect(),
    }
}
#[test]
fn batch_is_nonblocking_isolated_deduplicated_and_available_after_restore() {
    let (mut s, id) = active();
    let first = request(&mut s, &id, "parent-attempt", "nonce", input(3), |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs[0].status, "running");
    assert!(!delegation::pausing(&s.runtime.runs[0]));
    assert_eq!(s.runtime.runs.len(), 4);
    assert_eq!(s.chats.len(), 4);
    for child in &s.runtime.runs[1..] {
        assert_eq!(child.status, "queued");
        assert_eq!(child.model.as_deref(), Some("captured-model"));
        assert_eq!(child.backend, Backend::Codex);
        assert_eq!(
            child.persona.as_ref().unwrap().parent.as_deref(),
            Some(id.as_str())
        );
        let chat = s
            .chats
            .iter()
            .find(|c| Some(&c.id) == child.chat_id.as_ref())
            .unwrap();
        assert_eq!(chat.workspace.as_deref(), Some("immutable-baseline"));
        assert_eq!(chat.parent_chat, s.runtime.runs[0].chat_id);
        let status = crate::delegated_results::execute(
            crate::delegated_results::prepare(
                &s,
                &id,
                json!({"child":child.id,"section":"status"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(status["status"], "queued");
    }
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(
        first,
        request(
            &mut restored,
            &id,
            "parent-attempt",
            "nonce",
            input(3),
            |_| panic!("No second admission save")
        )
        .unwrap()
    );
    assert!(
        request(
            &mut restored,
            &id,
            "parent-attempt",
            "nonce",
            input(2),
            |_| Ok(())
        )
        .is_err()
    );
    assert_eq!(restored.runtime.runs.len(), 4);
    // A later human turn in the same chat can inspect the durable receipt.
    restored.runtime.runs[0].status = "completed".into();
    let parent_chat = restored.runtime.runs[0].chat_id.clone();
    restored
        .runtime
        .enqueue_for(
            "Check the agents".into(),
            RunKind::Chat,
            "web",
            Actor::LocalOperator,
            Action::Task,
        )
        .unwrap();
    let next = restored.runtime.runs.last_mut().unwrap();
    next.chat_id = parent_chat;
    next.status = "running".into();
    let next_id = next.id.clone();
    assert!(
        crate::delegated_results::prepare(
            &restored,
            &next_id,
            json!({"child":restored.runtime.runs[1].id,"section":"status"})
        )
        .is_ok()
    );
    restored.runtime.runs[1].status = "running".into();
    assert!(
        crate::delegated_results::prepare(
            &restored,
            &restored.runtime.runs[1].id,
            json!({"child":restored.runtime.runs[2].id,"section":"status"})
        )
        .is_err()
    );
}
#[test]
fn failed_save_rolls_back_and_full_queue_retains_backlog() {
    let (mut s, id) = active();
    let original = serde_json::to_vec(&s).unwrap();
    assert!(
        request(&mut s, &id, "parent-attempt", "nonce", input(3), |_| Err(
            std::io::Error::other("disk full")
        ))
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&s).unwrap(), original);
    for _ in 0..30 {
        s.runtime
            .enqueue_for(
                "Pending".into(),
                RunKind::Chat,
                "test",
                Actor::LocalOperator,
                Action::Task,
            )
            .unwrap();
    }
    let accepted = request(&mut s, &id, "parent-attempt", "nonce", input(3), |_| Ok(())).unwrap();
    assert_eq!(accepted["requested"], 3);
    assert_eq!(accepted["materialized"], 1);
    assert_eq!(accepted["pending"], 2);
    reconcile(&mut s, |_| panic!("Full queue must not checkpoint a no-op")).unwrap();
}
#[test]
fn child_budget_authority_and_cancellation_are_shared_with_delegation() {
    let (mut s, id) = active();
    s.runtime.execution.max_agents_per_root = 8;
    assert!(request(&mut s, &id, "stale", "nonce", input(1), |_| Ok(())).is_err());
    s.runtime.runs[0].actor = Actor::Unknown;
    assert!(request(&mut s, &id, "parent-attempt", "nonce", input(1), |_| Ok(())).is_err());
    s.runtime.runs[0].actor = Actor::LocalOperator;
    request(&mut s, &id, "parent-attempt", "nonce", input(8), |_| Ok(())).unwrap();
    assert!(delegation::budget(&s, &s.runtime.runs[0], Role::Worker).is_err());
    assert!(
        request(&mut s, &id, "parent-attempt", "another", input(1), |_| Ok(
            ()
        ))
        .is_err()
    );
    crate::lifecycle::request_cancel(&mut s.runtime, &id, &Actor::LocalOperator).unwrap();
    assert!(
        s.runtime.runs[1..]
            .iter()
            .all(|run| run.status == "cancelled")
    );
    let (mut s, id) = active();
    s.runtime.runs[0].action = Action::Improve;
    s.runtime.runs[0].attempts[0].parent_artifact = None;
    assert!(request(&mut s, &id, "parent-attempt", "nonce", input(1), |_| Ok(())).is_err());
}
#[test]
fn fifty_agents_reserve_once_and_materialize_gradually_after_restart() {
    let (mut s, id) = active();
    let accepted = request(
        &mut s,
        &id,
        "parent-attempt",
        "fifty",
        input(50),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(accepted["requested"], 50);
    assert_eq!(accepted["materialized"], 31);
    assert_eq!(accepted["pending"], 19);
    assert_eq!(accepted["children"].as_array().unwrap().len(), 10);
    assert_eq!(accepted["next_offset"], 10);
    // Fifty total reservations, not fifty plus the materialized children.
    assert!(delegation::budget_for(&s, &s.runtime.runs[0], Role::Worker, 14, false).is_ok());
    assert!(delegation::budget_for(&s, &s.runtime.runs[1], Role::Worker, 15, false).is_err());
    let snapshot = serde_json::to_vec(&s).unwrap();
    assert_eq!(
        request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| panic!("Retry must not write")
        )
        .unwrap(),
        accepted
    );
    let mut s: State = serde_json::from_slice(&snapshot).unwrap();
    for child in &mut s.runtime.runs[1..11] {
        child.status = "completed".into();
    }
    s.runtime.runs[0].model = Some("new live model must not leak".into());
    let before = serde_json::to_vec(&s).unwrap();
    assert!(reconcile(&mut s, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&s).unwrap(), before);
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 42);
    assert_eq!(s.runtime.runs[0].spawns[0].pending(), 9);
    for child in &s.runtime.runs[32..] {
        assert_eq!(child.model.as_deref(), Some("captured-model"));
        let chat = s
            .chats
            .iter()
            .find(|c| Some(&c.id) == child.chat_id.as_ref())
            .unwrap();
        assert_eq!(chat.workspace.as_deref(), Some("immutable-baseline"));
    }
    for run in &mut s.runtime.runs {
        run.status = "completed".into();
    }
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 51);
    assert_eq!(s.runtime.runs[0].spawns[0].pending(), 0);
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 52);
    assert_eq!(s.runtime.runs[51].kind, RunKind::AgentCompletion);
    reconcile(&mut s, |_| panic!("No completed backlog effects")).unwrap();
}
#[test]
fn later_originating_turn_pages_and_cancels_backlog_atomically() {
    let (mut s, id) = active();
    let accepted = request(
        &mut s,
        &id,
        "parent-attempt",
        "fifty",
        input(50),
        |_| Ok(()),
    )
    .unwrap();
    let batch = accepted["batch"].clone();
    s.runtime.runs[0].status = "completed".into();
    let originating = s.runtime.runs[0].chat_id.clone();
    s.runtime
        .enqueue_for(
            "Stop the batch".into(),
            RunKind::Chat,
            "test",
            identity::Actor::LocalOperator,
            identity::Action::Task,
        )
        .unwrap();
    let caller = s.runtime.runs.last_mut().unwrap();
    caller.status = "running".into();
    caller.chat_id = originating;
    let caller = caller.id.clone();
    let args = json!({"batch":batch,"offset":10,"limit":20});
    let page = manage(&mut s, &caller, "control_agent_spawn_status", &args, |_| {
        panic!("Read cannot save")
    })
    .unwrap();
    assert_eq!(page["children"].as_array().unwrap().len(), 20);
    assert_eq!(page["next_offset"], 30);
    let before = serde_json::to_vec(&s).unwrap();
    assert!(
        manage(
            &mut s,
            &caller,
            "control_agent_spawn_cancel",
            &json!({"batch":batch}),
            |_| Err(std::io::Error::other("full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&s).unwrap(), before);
    manage(
        &mut s,
        &caller,
        "control_agent_spawn_cancel",
        &json!({"batch":batch}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(s.runtime.runs[0].spawns[0].cancelled);
    assert!(
        s.runtime.runs[1..32]
            .iter()
            .all(|run| run.status == "cancelled")
    );
    reconcile(&mut s, |_| panic!("Cancelled backlog cannot spawn")).unwrap();
    assert_eq!(s.runtime.runs.len(), 33);
    s.runtime.runs.last_mut().unwrap().chat_id = Some("unrelated-chat".into());
    assert!(
        manage(
            &mut s,
            &caller,
            "control_agent_spawn_status",
            &args,
            |_| Ok(())
        )
        .is_err()
    );
}
#[test]
fn cancellation_revocation_and_intent_limits_fence_unmaterialized_work() {
    let (mut s, id) = active();
    s.runtime.execution.max_pending_agent_intents = 40;
    assert!(
        request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| panic!("Over budget cannot save")
        )
        .is_err()
    );
    s.runtime.execution.max_pending_agent_intents = 256;
    request(
        &mut s,
        &id,
        "parent-attempt",
        "fifty",
        input(50),
        |_| Ok(()),
    )
    .unwrap();
    crate::lifecycle::request_cancel(&mut s.runtime, &id, &identity::Actor::LocalOperator).unwrap();
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert!(s.runtime.runs[0].spawns[0].cancelled);
    assert_eq!(s.runtime.runs.len(), 32);
    let (mut s, id) = active();
    request(
        &mut s,
        &id,
        "parent-attempt",
        "fifty",
        input(50),
        |_| Ok(()),
    )
    .unwrap();
    s.runtime.runs[0].actor = identity::Actor::Unknown;
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert!(s.runtime.runs[0].spawns[0].cancelled);
    assert_eq!(s.runtime.runs.len(), 32);
}
#[test]
fn node_backlog_budget_is_shared_across_independent_roots() {
    let (mut s, id) = active();
    request(
        &mut s,
        &id,
        "parent-attempt",
        "fifty",
        input(50),
        |_| Ok(()),
    )
    .unwrap();
    s.runtime.runs[0].status = "completed".into();
    s.runtime.execution.max_pending_agent_intents = 20;
    let next_chat = chat::create(&mut s);
    s.runtime
        .enqueue_for(
            "Another team".into(),
            RunKind::Chat,
            "test",
            identity::Actor::LocalOperator,
            identity::Action::Task,
        )
        .unwrap();
    let root = s.runtime.runs.last_mut().unwrap();
    root.status = "running".into();
    root.chat_id = Some(next_chat);
    root.attempts.push(crate::usage::Attempt {
        workspace_id: Some("other-attempt".into()),
        ..Default::default()
    });
    let root = root.id.clone();
    assert!(
        request(&mut s, &root, "other-attempt", "two", input(2), |_| panic!(
            "Node backlog must apply across roots"
        ))
        .is_err()
    );
    assert_eq!(s.runtime.runs[0].spawns[0].pending(), 19);
    assert!(s.runtime.runs.last().unwrap().spawns.is_empty());
}
#[test]
fn batch_cancel_follows_retained_continuations_and_fences_their_backlog() {
    let (mut s, id) = active();
    let root = request(
        &mut s,
        &id,
        "parent-attempt",
        "root-batch",
        input(1),
        |_| Ok(()),
    )
    .unwrap();
    let mut next = s.runtime.runs[1].clone();
    next.id = "continued-child".into();
    next.kind = RunKind::DelegationResult;
    next.status = "running".into();
    next.attempts.push(crate::usage::Attempt {
        workspace_id: Some("continued-attempt".into()),
        parent_artifact: Some("child-baseline".into()),
        ..Default::default()
    });
    s.runtime.runs[1].status = "continued".into();
    s.runtime.runs[1].delegation = Some(delegation::Delegation {
        input: delegation::Input {
            objective: "Review".into(),
            role: Role::Judge,
        },
        nonce: "judge".into(),
        generation: "old-attempt".into(),
        backend: next.backend,
        model: "captured-model".into(),
        child: Some("completed-review".into()),
        continuation: Some(next.id.clone()),
    });
    s.runtime.runs.push(next);
    request(
        &mut s,
        "continued-child",
        "continued-attempt",
        "more",
        input(50),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(s.runtime.runs[2].spawns[0].pending(), 20);
    let before = serde_json::to_vec(&s).unwrap();
    let args = json!({"batch":root["batch"]});
    assert!(
        manage(&mut s, &id, "control_agent_spawn_cancel", &args, |_| Err(
            std::io::Error::other("full")
        ))
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&s).unwrap(), before);
    manage(&mut s, &id, "control_agent_spawn_cancel", &args, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs[2].status, "cancelling");
    assert!(s.runtime.runs[2].spawns[0].cancelled);
    assert!(
        s.runtime.runs[3..]
            .iter()
            .all(|run| run.status == "cancelled")
    );
    reconcile(&mut s, |_| panic!("Descendant backlog must remain fenced")).unwrap();
}
#[test]
fn configured_team_above_legacy_graph_limit_remains_inspectable() {
    let (mut s, id) = active();
    s.runtime.execution.max_agents_per_root = 1100;
    s.runtime.execution.max_pending_agent_intents = 1100;
    request(
        &mut s,
        &id,
        "parent-attempt",
        "large-team",
        input(1030),
        |_| Ok(()),
    )
    .unwrap();
    while s.runtime.runs[0].spawns[0].pending() > 0 {
        for child in &mut s.runtime.runs[1..] {
            child.status = "completed".into();
        }
        reconcile(&mut s, |_| Ok(())).unwrap();
    }
    assert_eq!(s.runtime.runs.len(), 1031);
    let last = s.runtime.runs.last().unwrap().id.clone();
    let plan = crate::delegated_results::prepare(&s, &id, json!({"child":last,"section":"status"}))
        .unwrap();
    assert_eq!(
        crate::delegated_results::execute(plan).unwrap()["status"],
        "queued"
    );
}
#[test]
#[ignore = "manual optimized profile of the no-backlog reconciliation path"]
fn profile_idle_backlog_reconciliation() {
    let (mut s, _) = active();
    s.runtime.runs[0].status = "completed".into();
    let template = s.runtime.runs[0].clone();
    for index in 0..20000 {
        let mut run = template.clone();
        run.id = format!("history-{index}");
        s.runtime.runs.push(run);
    }
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        reconcile(std::hint::black_box(&mut s), |_| {
            panic!("No backlog must not checkpoint")
        })
        .unwrap();
    }
    eprintln!(
        "idle spawn reconciliation over {} retained runs: {:?}/pass",
        s.runtime.runs.len(),
        start.elapsed() / 1000
    );
}
