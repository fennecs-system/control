
use super::*;
use crate::{
    backend::Backend,
    identity::{Action, Actor},
    usage::Attempt,
};
fn input(role: Role) -> Input {
    Input {
        objective: "Inspect this change and report evidence".into(),
        role,
    }
}
fn active(action: Action) -> State {
    let mut s = State::default();
    let id = chat::create(&mut s);
    s.chats[0].workspace = Some("published-task".into());
    s.chats[0].improve_workspace = Some("published-improve".into());
    s.runtime
        .enqueue_for(
            "Implement the feature".into(),
            RunKind::Task,
            "test",
            Actor::LocalOperator,
            action,
        )
        .unwrap();
    let run = &mut s.runtime.runs[0];
    run.chat_id = Some(id);
    run.status = "running".into();
    run.backend = Backend::Codex;
    run.model = Some("dev-model".into());
    run.attempts.push(Attempt {
        workspace_id: Some("parent-attempt-1".into()),
        parent_artifact: Some(
            if action == Action::Improve {
                "published-improve"
            } else {
                "published-task"
            }
            .into(),
        ),
        ..Attempt::default()
    });
    s
}
fn pause(s: &mut State, role: Role) {
    let id = s.runtime.runs[0].id.clone();
    request_with_save(s, &id, "nonce", input(role), |_| Ok(())).unwrap();
    s.runtime.runs[0].status = "waiting".into();
    let a = &mut s.runtime.runs[0].attempts[0];
    a.status = "waiting".into();
    a.artifact = Some("immutable-paused-source".into());
}
#[test]
fn acknowledgement_is_idempotent_and_fences_other_requests() {
    let mut s = active(Action::Task);
    let id = s.runtime.runs[0].id.clone();
    request_with_save(&mut s, &id, "same", input(Role::Worker), |_| Ok(())).unwrap();
    request_with_save(&mut s, &id, "same", input(Role::Worker), |_| {
        panic!("Acknowledgement replay must not save a second admission")
    })
    .unwrap();
    assert!(request_with_save(&mut s, &id, "changed", input(Role::Worker), |_| Ok(())).is_err());
    let shared = std::sync::Arc::new(std::sync::Mutex::new(s));
    assert!(crate::design::requested(Some((&shared, &id))));
    shared.lock().unwrap().runtime.runs[0].attempts[0].workspace_id =
        Some("later-generation".into());
    assert!(!crate::design::requested(Some((&shared, &id))));
}

#[test]
fn worker_inherits_current_improve_snapshot_without_sharing_publication() {
    let mut s = active(Action::Improve);
    pause(&mut s, Role::Worker);
    let parent = s.runtime.runs[0].id.clone();
    assert!(crate::scheduler::next(&s.runtime).is_none());
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 2);
    let child = &s.runtime.runs[1];
    assert_eq!(child.action, Action::Improve);
    assert_eq!(child.actor, s.runtime.runs[0].actor);
    assert_eq!(
        child.persona.as_ref().unwrap().parent.as_deref(),
        Some(parent.as_str())
    );
    assert_eq!(
        s.chats[1].improve_workspace.as_deref(),
        Some("immutable-paused-source")
    );
    assert_eq!(
        s.chats[0].improve_workspace.as_deref(),
        Some("published-improve")
    );
    assert_eq!(crate::scheduler::next(&s.runtime), Some(1));
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 2);
    // Save/restore the waiting parent and queued child, then collect failure.
    let models = s.runtime.models.clone();
    let mut s: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    s.runtime.models = models;
    s.runtime.runs[1].status = "interrupted".into();
    s.runtime.runs[1].result = "Engine restarted; inspect retained diagnostics".into();
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 3);
    assert_eq!(s.runtime.runs[0].status, "continued");
    assert_eq!(s.runtime.runs[2].action, Action::Improve);
    let c = s.runtime.runs[2].continuation.as_ref().unwrap();
    assert_eq!(c.checkpoint, "immutable-paused-source");
    assert_eq!(c.published_parent.as_deref(), Some("published-improve"));
    assert!(c.decision.question.contains("interrupted"));
    assert!(c.decision.question.contains("NOT applied"));
    assert!(
        chat::prompt(&s, &s.runtime.runs[2].id)
            .unwrap()
            .contains("Engine restarted")
    );
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 3);
}
#[test]
fn judge_inherits_captured_model_and_has_task_ceiling() {
    let mut s = active(Action::Improve);
    s.runtime.models = serde_json::from_value(
        json!([{"backend":"codex","model":"other-dev"},{"backend":"claude","model":"sonnet"}]),
    )
    .unwrap();
    pause(&mut s, Role::Judge);
    reconcile(&mut s, |_| Ok(())).unwrap();
    let child = &s.runtime.runs[1];
    assert!(is_judge(child));
    assert_eq!(child.backend, Backend::Codex);
    assert_eq!(child.model.as_deref(), Some("dev-model"));
    assert_eq!(child.action, Action::Task);
    assert_eq!(
        s.chats[1].workspace.as_deref(),
        Some("immutable-paused-source")
    );
    assert!(!s.chats[1].improve_mode);
    assert!(budget(&s, child, Role::Worker).is_err());
    assert!(
        budget(&s, &s.runtime.runs[0], Role::Judge)
            .unwrap_err()
            .contains("One judge")
    );
}
#[test]
fn all_children_keep_parent_selection_and_failures_resume_the_parent() {
    for backend in [Backend::Codex, Backend::Claude] {
        for role in [Role::Worker, Role::Judge] {
            let mut s = active(Action::Task);
            s.runtime.models=serde_json::from_value(json!([{"backend":"codex","model":"unrelated"},{"backend":"claude","model":"expired-alternate"}])).unwrap();
            s.runtime.runs[0].backend = backend;
            s.runtime.runs[0].model = Some("captured-parent-model".into());
            pause(&mut s, role);
            s.runtime.backend = if backend == Backend::Codex {
                Backend::Claude
            } else {
                Backend::Codex
            };
            s.runtime.model = Some("changed-default".into());
            reconcile(&mut s, |_| Ok(())).unwrap();
            assert_eq!(s.runtime.runs[1].backend, backend);
            assert_eq!(
                s.runtime.runs[1].model.as_deref(),
                Some("captured-parent-model")
            );
            s.runtime.runs[1].status = "failed".into();
            s.runtime.runs[1].result =
                "Synthetic authentication failure; refresh the parent provider login".into();
            reconcile(&mut s, |_| Ok(())).unwrap();
            assert_eq!(s.runtime.runs[0].status, "continued");
            let resumed = &s.runtime.runs[2];
            assert_eq!(resumed.backend, backend);
            assert_eq!(resumed.model.as_deref(), Some("captured-parent-model"));
            assert!(
                resumed
                    .continuation
                    .as_ref()
                    .unwrap()
                    .decision
                    .question
                    .contains("Synthetic authentication failure")
            );
        }
    }
}
#[test]
fn denied_identity_mismatched_actor_depth_and_total_budget_are_enforced() {
    let mut s = active(Action::Task);
    let id = s.runtime.runs[0].id.clone();
    s.runtime.runs[0].actor = Actor::Unknown;
    assert!(request_with_save(&mut s, &id, "n", input(Role::Worker), |_| Ok(())).is_err());
    s.runtime.runs[0].actor = Actor::LocalOperator;
    s.chats[0].actor = Actor::Unknown;
    assert!(request_with_save(&mut s, &id, "n", input(Role::Worker), |_| Ok(())).is_err());
    s.chats[0].actor = Actor::LocalOperator;
    s.runtime.runs[0].persona = Some(Persona {
        root: id.clone(),
        role: Role::Worker,
        depth: MAX_DEPTH,
        parent: None,
    });
    assert!(
        request_with_save(&mut s, &id, "n", input(Role::Worker), |_| Ok(()))
            .unwrap_err()
            .contains("depth")
    );
    s.runtime.runs[0].persona = None;
    for _ in 0..s.runtime.execution.max_agents_per_root {
        let mut child = s.runtime.runs[0].clone();
        child.kind = RunKind::Delegated;
        child.persona = Some(Persona {
            root: id.clone(),
            role: Role::Worker,
            depth: 1,
            parent: Some(id.clone()),
        });
        s.runtime.runs.push(child);
    }
    assert!(
        budget(&s, &s.runtime.runs[0], Role::Worker)
            .unwrap_err()
            .contains("budget")
    );
}
#[test]
fn request_and_transition_failures_rollback_without_duplicate_children() {
    let mut s = active(Action::Task);
    let id = s.runtime.runs[0].id.clone();
    assert!(
        request_with_save(&mut s, &id, "n", input(Role::Worker), |_| Err(
            std::io::Error::other("disk full")
        ))
        .is_err()
    );
    assert!(s.runtime.runs[0].delegation.is_none());
    pause(&mut s, Role::Worker);
    assert!(request_with_save(&mut s, &id, "nonce", input(Role::Worker), |_| Ok(())).is_err()); // no longer active
    assert!(reconcile(&mut s, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(s.chats.len(), 1);
    assert_eq!(s.runtime.runs.len(), 1);
    assert!(
        s.runtime.runs[0]
            .delegation
            .as_ref()
            .unwrap()
            .child
            .is_none()
    );
    reconcile(&mut s, |_| Ok(())).unwrap();
    s.runtime.runs[1].status = "failed".into();
    assert!(reconcile(&mut s, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(s.runtime.runs.len(), 2);
    assert_eq!(s.runtime.runs[0].status, "waiting");
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 3);
}
#[test]
fn project_child_captures_full_checkpoint_and_cancellation_cascades() {
    let mut s = active(Action::Task);
    s.chats[0].project = Some(crate::projects::Project {
        request: "request".into(),
        path: "/synthetic/project".into(),
        grant: Some("grant".into()),
        head: Some("project-before".into()),
        ..Default::default()
    });
    s.runtime.runs[0].attempts[0].project = Some("grant".into());
    s.runtime.runs[0].attempts[0].project_base = Some("project-before".into());
    pause(&mut s, Role::Worker);
    s.runtime.runs[0].attempts[0].project_checkpoint = Some("project-paused".into());
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(
        s.chats[1].project.as_ref().unwrap().head.as_deref(),
        Some("project-paused")
    );
    let id = s.runtime.runs[0].id.clone();
    let before = serde_json::to_vec(&s).unwrap();
    let rollback = runtime::RunStatusCheckpoint::capture(&s.runtime, |r| r.id == id);
    crate::lifecycle::request_cancel(&mut s.runtime, &id, &Actor::LocalOperator).unwrap();
    assert_eq!(s.runtime.runs[1].status, "cancelled");
    assert!(
        rollback
            .save(&mut s, |_| Err(std::io::Error::other("disk full")))
            .is_err()
    );
    assert_eq!(serde_json::to_vec(&s).unwrap(), before);
    crate::lifecycle::request_cancel(&mut s.runtime, &id, &Actor::LocalOperator).unwrap();
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs.len(), 2);
}
#[test]
fn child_question_is_answerable_from_parent_but_not_unrelated_chat() {
    let mut state = active(Action::Task);
    pause(&mut state, Role::Worker);
    reconcile(&mut state, |_| Ok(())).unwrap();
    let parent_chat = state.runtime.runs[0].chat_id.clone().unwrap();
    let child_chat = state.runtime.runs[1].chat_id.clone().unwrap();
    let child = &mut state.runtime.runs[1];
    child.status = "running".into();
    child.attempts.push(Attempt {
        workspace_id: Some("child-attempt".into()),
        parent_artifact: Some("immutable-paused-source".into()),
        ..Default::default()
    });
    let child_id = child.id.clone();
    crate::design::tests::ask_without_io(
        &mut state,
        &child_id,
        "question",
        crate::design::tests::input(),
    )
    .unwrap();
    let child = &mut state.runtime.runs[1];
    child.status = "waiting".into();
    child.attempts[0].status = "waiting".into();
    child.attempts[0].artifact = Some("paused-child".into());
    let question = child.question.as_ref().unwrap().id.clone();
    let other = chat::create(&mut state);
    // A UI-only parent link is not a delegation capability.
    state.chats.last_mut().unwrap().parent_chat = Some(parent_chat.clone());
    assert!(crate::design::pending(&state, &other).is_empty());
    assert!(crate::design::answer(&mut state, &other, Some(&question), "1", "test").is_err());
    assert_eq!(crate::design::pending(&state, &parent_chat).len(), 1);
    assert!(
        chat::requests(&state, &parent_chat)
            .unwrap()
            .contains(&format!("value='/answer {question} 1'"))
    );
    assert_eq!(
        chat::detail(&state, &parent_chat).unwrap()["pending_questions"][0]["chat_id"],
        child_chat
    );
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        crate::session_http::change_with_save(
            &mut state,
            "control.chat.send",
            &serde_json::json!({"id":parent_chat,"message":format!("/answer {question} 2")}),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_vec(&state).unwrap(),
        before,
        "A failed save must restore the child and parent together"
    );
    chat::send(
        &mut state,
        &parent_chat,
        &format!("/answer {question} 2"),
        "test",
    )
    .unwrap();
    let resumed = state.runtime.runs.last().unwrap();
    assert_eq!(resumed.chat_id.as_deref(), Some(child_chat.as_str()));
    assert_eq!(
        resumed.persona.as_ref().unwrap().parent.as_deref(),
        Some(state.runtime.runs[0].id.as_str())
    );
    assert_eq!(
        resumed.continuation.as_ref().unwrap().checkpoint,
        "paused-child"
    );
    assert!(crate::design::pending(&state, &parent_chat).is_empty());
}
#[test]
fn persistent_resources_follow_real_delegation_and_return_to_parent() {
    let mut state = active(Action::Task);
    let (mut resources, _, _) = crate::ports::tests::fixture();
    let mut service = resources.services.remove(0);
    service.resource.owner.chat = state.runtime.runs[0]
        .chat_id
        .clone()
        .unwrap()
        .try_into()
        .unwrap();
    state.services.push(service);
    pause(&mut state, Role::Worker);
    reconcile(&mut state, |_| Ok(())).unwrap();
    let node = state.services[0].resource.owner.node.clone();
    assert!(crate::services::accessible(
        &state.services[0],
        &node,
        &state.runtime.runs[1]
    ));
    let mut created = state.services[0].clone();
    created.resource.id = "svc-created-by-child".to_owned().try_into().unwrap();
    created.resource.owner.chat = state.runtime.runs[1]
        .chat_id
        .clone()
        .unwrap()
        .try_into()
        .unwrap();
    state.services.push(created);
    state.runtime.runs[1].status = "completed".into();
    reconcile(&mut state, |_| Ok(())).unwrap();
    let parent = state.runtime.runs.last().unwrap();
    assert_eq!(parent.chat_id, state.runtime.runs[0].chat_id);
    assert!(crate::services::accessible(
        &state.services[1],
        &node,
        parent
    ));
}
#[test]
fn parent_waits_for_child_design_and_delegation_continuations() {
    for design in [true, false] {
        let mut s = active(Action::Task);
        pause(&mut s, Role::Worker);
        reconcile(&mut s, |_| Ok(())).unwrap();
        let mut next = s.runtime.runs[1].clone();
        next.id.push_str("-continuation");
        next.kind = RunKind::DesignAnswer;
        next.status = "queued".into();
        let next_id = next.id.clone();
        s.runtime.runs[1].status = "continued".into();
        if design {
            s.runtime.runs[1].question = Some(crate::design::Question {
                id: "q".into(),
                generation: "g".into(),
                nonce: "n".into(),
                input: crate::design::Input {
                    question: "Architecture?".into(),
                    options: vec!["A".into(), "B".into()],
                    recommended: None,
                },
                answer: Some("A".into()),
                continuation: Some(next_id),
            });
        } else {
            let mut delegated = s.runtime.runs[0].delegation.clone().unwrap();
            delegated.continuation = Some(next_id);
            s.runtime.runs[1].delegation = Some(delegated);
        }
        s.runtime.runs.push(next);
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs.len(), 3);
        assert_eq!(s.runtime.runs[0].status, "waiting");
        s.runtime.runs[2].status = "completed".into();
        s.runtime.runs[2].result = "Final child evidence after continuation".into();
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs.len(), 4);
        assert!(
            s.runtime.runs[3]
                .continuation
                .as_ref()
                .unwrap()
                .decision
                .question
                .contains("Final child evidence")
        );
    }
}

#[test]
fn changed_workspace_blocks_continuation_and_pending_same_chat_stays_queued() {
    let mut s = active(Action::Improve);
    pause(&mut s, Role::Worker);
    let chat = s.chats[0].id.clone();
    s.runtime
        .enqueue_for(
            "Later turn".into(),
            RunKind::Chat,
            "test",
            Actor::LocalOperator,
            Action::Improve,
        )
        .unwrap();
    s.runtime.runs[1].chat_id = Some(chat);
    assert_eq!(crate::scheduler::next(&s.runtime), None);
    s.chats[0].improve_workspace = Some("different-head".into());
    reconcile(&mut s, |_| Ok(())).unwrap();
    assert_eq!(s.runtime.runs[0].status, "blocked");
    assert_eq!(s.runtime.runs.len(), 2);
    assert_eq!(crate::scheduler::next(&s.runtime), Some(1));
}
fn tree_with_children(count: usize, at_ms: u64, parent_kind: RunKind) -> (State, String) {
    let mut s = State::default();
    let lead = chat::create(&mut s);
    chat::send(&mut s, &lead, "Lead", "test").unwrap();
    for index in 0..count {
        let mut turn = s.runtime.runs[0].clone();
        turn.id = format!("turn-{index}");
        turn.kind = parent_kind;
        let parent = turn.id.clone();
        s.runtime.runs.push(turn);
        let child = chat::create(&mut s);
        s.chats.last_mut().unwrap().parent_chat = Some(lead.clone());
        chat::send(&mut s, &child, "Helper", "test").unwrap();
        let run = s.runtime.runs.last_mut().unwrap();
        run.kind = RunKind::Spawned;
        run.id = format!("{:x}-child{index}", u128::from(at_ms) * 1_000_000);
        run.persona = Some(Persona {
            role: Role::Worker,
            root: parent.clone(),
            depth: 1,
            parent: Some(parent),
        });
    }
    (s, lead)
}

#[test]
fn spawns_are_capped_per_conversation_tree_per_window() {
    let now = 10 * SPAWN_WINDOW_MS;
    let (mut s, lead) = tree_with_children(3, now - 1_000, RunKind::Chat);
    s.runtime.execution.max_agents_per_root = 4;
    let helper = chat::create(&mut s);
    s.chats.last_mut().unwrap().parent_chat = Some(lead);
    chat::send(&mut s, &helper, "A fresh turn in a helper", "test").unwrap();
    let fresh = s.runtime.runs.last().unwrap().clone();
    assert!(spawn_rate(&s, &fresh, 1, now).is_ok());
    assert!(
        spawn_rate(&s, &fresh, 2, now).is_err(),
        "new roots in the same tree share the window"
    );
    assert!(
        spawn_rate(&s, &fresh, 2, now + SPAWN_WINDOW_MS).is_ok(),
        "the window resets"
    );
}

#[test]
fn agents_woken_by_control_events_cannot_keep_respawning() {
    let now = 10 * SPAWN_WINDOW_MS;
    let (s, _) = tree_with_children(
        AUTOMATIC_SPAWN_TURNS_PER_WINDOW,
        now - 1_000,
        RunKind::AgentCompletion,
    );
    let mut woken = s.runtime.runs[0].clone();
    woken.kind = RunKind::AgentCompletion;
    assert!(
        spawn_rate(&s, &woken, 1, now).is_err(),
        "a completion-woken controller re-spawning is capped"
    );
    let mut asked = woken.clone();
    asked.kind = RunKind::Chat;
    assert!(
        spawn_rate(&s, &asked, 1, now).is_ok(),
        "a person asking for more agents is not"
    );
    assert!(spawn_rate(&s, &woken, 1, now + SPAWN_WINDOW_MS).is_ok());
}

#[test]
fn reserved_blocking_children_count_against_the_tree_window() {
    let mut s = active(Action::Task);
    s.runtime.execution.max_agents_per_root = 2;
    let id = s.runtime.runs[0].id.clone();
    request_with_save(&mut s, &id, "n", input(Role::Worker), |_| Ok(())).unwrap();
    let lead = s.chats[0].id.clone();
    let sibling = chat::create(&mut s);
    s.chats.last_mut().unwrap().parent_chat = Some(lead);
    chat::send(&mut s, &sibling, "A fresh root in the same tree", "test").unwrap();
    let fresh = s.runtime.runs.last().unwrap().clone();
    assert!(spawn_rate(&s, &fresh, 1, 10 * SPAWN_WINDOW_MS).is_ok());
    assert!(
        spawn_rate(&s, &fresh, 2, 10 * SPAWN_WINDOW_MS).is_err(),
        "the unmaterialized blocking child is reserved"
    );
}

#[test]
fn automatic_turns_holding_reservations_count_toward_the_cap() {
    let mut s = active(Action::Task);
    let lead = s.chats[0].id.clone();
    for index in 0..AUTOMATIC_SPAWN_TURNS_PER_WINDOW {
        let mut turn = s.runtime.runs[0].clone();
        turn.id = format!("woken-{index}");
        turn.kind = RunKind::AgentCompletion;
        turn.chat_id = Some(lead.clone());
        turn.status = "running".into();
        s.runtime.runs.push(turn);
        request_with_save(
            &mut s,
            &format!("woken-{index}"),
            "n",
            input(Role::Worker),
            |_| Ok(()),
        )
        .unwrap();
    }
    let mut next = s.runtime.runs[0].clone();
    next.id = "woken-next".into();
    next.kind = RunKind::AgentCompletion;
    assert!(
        spawn_rate(&s, &next, 1, 10 * SPAWN_WINDOW_MS).is_err(),
        "unmaterialized reservations from woken turns count"
    );
}
