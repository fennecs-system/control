
use super::*;
fn approved() -> (State, String) {
    let (mut state, run, chat) = super::super::tests::fixture();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "notice",
        Input {
            service_id: "svc-a".to_owned().try_into().unwrap(),
            name: "server".to_owned().try_into().unwrap(),
            direction: Direction::Ingress,
            service_port: std::num::NonZeroU16::new(25565).unwrap(),
            target: None,
            peer_port_id: None,
        },
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    let id = state.ports[0].id.as_str().to_owned();
    approve(&mut state, &id, 60, false, None, now(), |_| Ok(())).unwrap();
    (state, chat)
}

#[test]
fn dispatcher_wakes_only_after_acknowledged_checkpoint_and_never_on_replay() {
    let (mut state, _) = approved();
    state.runtime.runs[0].status = "completed".into();
    let checkpointed = std::cell::Cell::new(false);
    let wakes = std::cell::Cell::new(0);
    let before = serde_json::to_vec(&state).unwrap();
    assert!(advance_with_wake(&mut state, |_| Err(fail("disk full")), || wakes.set(wakes.get() + 1)).is_err());
    assert_eq!(wakes.get(), 0);
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    advance_with_wake(&mut state, |state| {
        assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
        assert_eq!(state.runtime.runs.last().unwrap().kind, RunKind::PortDecision);
        checkpointed.set(true);
        Ok(())
    }, || {
        assert!(checkpointed.get());
        wakes.set(wakes.get() + 1);
    }).unwrap();
    assert_eq!(wakes.get(), 1);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    advance_with_wake(&mut restored, |_| panic!("no replay checkpoint"), || panic!("no replay wake")).unwrap();
}
#[test]
fn approval_waits_for_turn_and_wakes_once_with_captured_provider_after_reload() {
    let (mut state, chat) = approved();
    advance(&mut state, |_| {
        panic!("active turn must not queue another run")
    })
    .unwrap();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].model = Some("captured-model".into());
    let before = serde_json::to_vec(&state).unwrap();
    assert!(advance(&mut state, |_| Err(fail("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    let mut state: State = serde_json::from_slice(&before).unwrap();
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 2);
    assert_eq!(
        state.runtime.runs[1].chat_id.as_deref(),
        Some(chat.as_str())
    );
    assert_eq!(
        state.runtime.runs[1].model.as_deref(),
        Some("captured-model")
    );
    assert_eq!(state.runtime.runs[1].kind, RunKind::PortDecision);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    advance(&mut restored, |_| {
        panic!("acknowledged decisions must not replay")
    })
    .unwrap();
    assert_eq!(restored.runtime.runs.len(), 2);
}
#[test]
fn bot_wake_turns_do_not_supersede_approvals() {
    let (mut state, chat) = approved();
    state.runtime.runs[0].status = "completed".into();
    let mut wake = state.runtime.runs[0].clone();
    wake.id = "bot-wake".into();
    wake.kind = crate::runtime::RunKind::BotWake;
    state.runtime.runs.push(wake);
    let count = state.runtime.runs.len();
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.len(),
        count + 1,
        "a bot waking is not a human steering the task"
    );
    assert_eq!(
        state.runtime.runs.last().unwrap().chat_id.as_deref(),
        Some(chat.as_str())
    );
}
#[test]
fn newer_human_instruction_suppresses_delayed_approval_wake() {
    let (mut state, chat) = approved();
    state.runtime.runs[0].status = "completed".into();
    crate::chat::send(
        &mut state,
        &chat,
        "Stop the playtest and leave resources alone",
        "test",
    )
    .unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    let count = state.runtime.runs.len();
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), count);
    assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
}
#[test]
fn request_from_answered_design_continuation_wakes_the_original_conversation() {
    let (mut state, chat) = approved();
    let mut continuation = state.runtime.runs[0].clone();
    continuation.id = "after-design".into();
    continuation.status = "completed".into();
    continuation.kind = RunKind::DesignAnswer;
    continuation.persona = Some(crate::delegation::lineage(&state.runtime.runs[0]));
    state.runtime.runs[0].status = "continued".into();
    state.runtime.runs[0].question = Some(crate::design::Question {
        id: "q-route".into(),
        generation: "attempt-a".into(),
        nonce: "route".into(),
        input: crate::design::Input {
            question: "Route?".into(),
            options: vec!["Relay".into(), "Stop".into()],
            recommended: None,
        },
        answer: Some("Relay".into()),
        continuation: Some(continuation.id.clone()),
    });
    state.ports[0].origin_run = continuation.id.clone();
    state.runtime.runs.push(continuation);
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(
        state.runtime.runs[2].chat_id.as_deref(),
        Some(chat.as_str())
    );
    assert_eq!(state.runtime.runs[2].kind, RunKind::PortDecision);
}
#[test]
fn requests_from_a_resumed_turn_wake_the_conversation_again() {
    let (mut state, chat) = approved();
    state.runtime.runs[0].status = "completed".into();
    advance(&mut state, |_| Ok(())).unwrap();
    let resumed = state.runtime.runs.len() - 1;
    state.runtime.runs[resumed].status = "completed".into();
    state.runtime.runs[resumed]
        .attempts
        .push(crate::usage::Attempt {
            workspace_id: Some("attempt-b".into()),
            ..Default::default()
        });
    let mut second = state.ports[0].clone();
    second.id = "port-second".to_owned().try_into().unwrap();
    second.origin_run = state.runtime.runs[resumed].id.clone();
    second.origin_attempt = "attempt-b".into();
    second.decision = Some(Receipt::pending(true));
    state.ports.push(second);
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.len(),
        resumed + 2,
        "a second round of approvals resumes the chat"
    );
    assert_eq!(
        state.runtime.runs.last().unwrap().chat_id.as_deref(),
        Some(chat.as_str())
    );
    assert!(state.ports[1].decision.as_ref().unwrap().acknowledged);
}
#[test]
fn requests_from_a_grandchild_spawned_by_a_later_turn_resume_the_lead() {
    let (mut state, lead) = approved();
    state.runtime.runs[0].status = "completed".into();
    state.ports[0].decision = None;
    let root = state.runtime.runs[0].id.clone();
    let mut later = state.runtime.runs[0].clone();
    later.id = "later".into();
    later.kind = crate::runtime::RunKind::AgentCompletion;
    state.runtime.runs.push(later);
    let mut parent = "later".to_owned();
    let mut chat = lead.clone();
    for (depth, id) in [(1, "helper"), (2, "coordinator")] {
        let child = crate::chat::create(&mut state);
        state
            .chats
            .iter_mut()
            .find(|s| s.id == child)
            .unwrap()
            .parent_chat = Some(chat.clone());
        let mut run = state.runtime.runs[0].clone();
        run.id = id.into();
        run.kind = crate::runtime::RunKind::Spawned;
        run.chat_id = Some(child.clone());
        run.persona = Some(crate::delegation::Persona {
            role: crate::delegation::Role::Worker,
            root: root.clone(),
            depth,
            parent: Some(parent.clone()),
        });
        run.attempts = vec![crate::usage::Attempt {
            workspace_id: Some(format!("{id}-attempt")),
            ..Default::default()
        }];
        state.runtime.runs.push(run);
        (parent, chat) = (id.to_owned(), child);
    }
    let mut port = state.ports[0].clone();
    port.id = "port-grandchild".to_owned().try_into().unwrap();
    port.origin_run = "coordinator".into();
    port.origin_attempt = "coordinator-attempt".into();
    port.decision = Some(Receipt::pending(true));
    state.ports.push(port);
    let count = state.runtime.runs.len();
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.len(),
        count + 1,
        "the lead resumes with the grandchild's approval"
    );
    assert_eq!(
        state.runtime.runs.last().unwrap().chat_id.as_deref(),
        Some(lead.as_str())
    );
    assert!(state.ports[1].decision.as_ref().unwrap().acknowledged);
}
#[test]
fn waiting_chat_does_not_starve_an_independent_ready_callback() {
    let (mut state, _) = approved(); // first task is still running
    let (mut other, chat) = approved();
    other.runtime.runs[0].status = "completed".into();
    state.chats.extend(other.chats);
    state.runtime.runs.extend(other.runtime.runs);
    state.ports.extend(other.ports);
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(
        state.runtime.runs[2].chat_id.as_deref(),
        Some(chat.as_str())
    );
    assert!(!state.ports[0].decision.as_ref().unwrap().acknowledged);
}
#[test]
fn denial_wakes_without_granting_access_and_cancelled_requests_never_wake() {
    let (mut state, _) = approved();
    state.ports[0].status = Status::Stopping;
    state.ports[0].approval = None;
    state.ports[0].decision = Some(Receipt::pending(false));
    state.runtime.runs[0].status = "completed".into();
    advance(&mut state, |_| Ok(())).unwrap();
    assert!(state.runtime.runs[1].objective.contains("denied"));
    assert!(state.ports[0].approval.is_none());
    let (mut cancelled, _) = approved();
    cancelled.runtime.runs[0].status = "cancelled".into();
    advance(&mut cancelled, |_| panic!("cancelled origin must not wake")).unwrap();
    assert_eq!(cancelled.runtime.runs.len(), 1);
}
