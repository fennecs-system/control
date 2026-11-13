
use super::*;
fn spawned(n: usize) -> State {
    let (mut state, id) = super::super::tests::active();
    request(
        &mut state,
        &id,
        "parent-attempt",
        "callback-batch",
        super::super::tests::input(n),
        |_| Ok(()),
    )
    .unwrap();
    state
}
#[test]
fn parent_chat_remains_available_and_completion_uses_current_workspace() {
    let mut state = spawned(1);
    let parent_chat = state.chats[0].id.clone();
    let child = state.runtime.runs[1].id.clone();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[1].status = "running".into();
    chat::send(&mut state, &parent_chat, "How is the bot doing?", "test").unwrap();
    assert_eq!(
        crate::scheduler::next(&state.runtime),
        Some(2),
        "A running child must not reserve its parent's chat"
    );
    state.chats[0].workspace = Some("newer-human-workspace".into());
    state.runtime.runs[1].status = "completed".into();
    state.runtime.runs[1].result = "Finished child work".into();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(reconcile(&mut state, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 4);
    let callback = &state.runtime.runs[3];
    assert_eq!(callback.kind, RunKind::AgentCompletion);
    assert_eq!(callback.chat_id.as_deref(), Some(parent_chat.as_str()));
    assert!(callback.continuation.is_none());
    assert!(callback.queue_order > state.runtime.runs[2].queue_order);
    assert_eq!(
        state.chats[0].workspace.as_deref(),
        Some("newer-human-workspace")
    );
    assert_eq!(crate::scheduler::next(&state.runtime), Some(2));
    let (_, children) = crate::delegated_results::scope(&state, callback).unwrap();
    assert!(children.contains(child.as_str()));
    assert!(
        chat::transcript(&state, &parent_chat)
            .unwrap()
            .contains("Control update")
    );
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    reconcile(&mut restored, |_| {
        panic!("Saved completion must not replay")
    })
    .unwrap();
    assert_eq!(restored.runtime.runs.len(), 4);
}
#[test]
fn completions_are_coalesced_and_do_not_keep_enqueuing_while_a_callback_waits() {
    let mut state = spawned(3);
    state.runtime.runs[0].status = "completed".into();
    for child in &mut state.runtime.runs[1..3] {
        child.status = "completed".into();
        child.result = "verified".into();
    }
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 5);
    assert_eq!(state.runtime.runs[0].spawns[0].completions.len(), 2);
    state.runtime.runs[3].status = "failed".into();
    reconcile(&mut state, |_| panic!("Coalesce behind pending callback")).unwrap();
    state.runtime.runs[4].status = "completed".into();
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 6);
    assert!(state.runtime.runs[5].objective.contains("failed"));
    assert_eq!(state.runtime.runs[0].spawns[0].completions.len(), 3);
}
#[test]
fn cancellation_legacy_batches_and_revoked_actors_do_not_wake() {
    for mode in 0..4 {
        let mut state = spawned(1);
        state.runtime.runs[1].status = "completed".into();
        match mode {
            0 => state.runtime.runs[0].spawns[0].cancelled = true,
            1 => state.runtime.runs[0].status = "cancelled".into(),
            2 => state.runtime.runs[0].spawns[0].notify = false,
            _ => state.runtime.runs[0].actor = identity::Actor::Unknown,
        }
        reconcile(&mut state, |_| panic!("Fenced task must not wake")).unwrap();
        assert_eq!(state.runtime.runs.len(), 2);
    }
}
#[test]
fn full_queue_defers_without_consuming_the_completion_receipt() {
    let mut state = spawned(1);
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[1].status = "completed".into();
    for _ in 0..crate::scheduler::MAX_PENDING {
        state
            .runtime
            .enqueue_for(
                "Other work".into(),
                RunKind::Task,
                "test",
                identity::Actor::LocalOperator,
                identity::Action::Task,
            )
            .unwrap();
    }
    reconcile(&mut state, |_| panic!("Full queue must not write")).unwrap();
    assert!(state.runtime.runs[0].spawns[0].completions.is_empty());
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.last().unwrap().kind,
        RunKind::AgentCompletion
    );
}
#[test]
fn child_continuations_finish_before_notification_and_cannot_cross_scope() {
    let mut state = spawned(1);
    let child = state.runtime.runs[1].clone();
    state
        .runtime
        .enqueue_for(
            "Continue child".into(),
            RunKind::DesignAnswer,
            "test",
            child.actor.clone(),
            child.action,
        )
        .unwrap();
    let next = state.runtime.runs.last_mut().unwrap();
    next.chat_id = child.chat_id.clone();
    next.persona = child.persona.clone();
    next.status = "running".into();
    let next_id = next.id.clone();
    state.runtime.runs[1].status = "continued".into();
    state.runtime.runs[1].question = Some(crate::design::Question {
        id: "fixture-question".into(),
        generation: "fixture-generation".into(),
        nonce: "fixture-nonce".into(),
        input: crate::design::Input {
            question: "Direction?".into(),
            options: vec!["A".into(), "B".into()],
            recommended: None,
        },
        answer: Some("A".into()),
        continuation: Some(next_id),
    });
    reconcile(&mut state, |_| {
        panic!("Active continuation must not notify")
    })
    .unwrap();
    state.runtime.runs[2].status = "failed".into();
    state.runtime.runs[2].actor = identity::Actor::Unknown;
    assert!(reconcile(&mut state, |_| panic!("Mismatched scope must not notify")).is_err());
    assert!(state.runtime.runs[0].spawns[0].completions.is_empty());
    state.runtime.runs[2].actor = child.actor;
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 4);
    assert!(state.runtime.runs[3].objective.contains("failed"));
    assert!(state.runtime.runs[3].objective.contains(&child.id));
}
