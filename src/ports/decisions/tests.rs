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
    assert!(
        advance_with_wake(
            &mut state,
            |_| Err(io::Error::other("disk full")),
            || wakes.set(wakes.get() + 1)
        )
        .is_err()
    );
    assert_eq!(wakes.get(), 0);
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    advance_with_wake(
        &mut state,
        |state| {
            assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
            assert_eq!(state.runtime.runs.last().unwrap().kind, RunKind::PortDecision);
            checkpointed.set(true);
            Ok(())
        },
        || {
            assert!(checkpointed.get());
            wakes.set(wakes.get() + 1);
        },
    )
    .unwrap();
    assert_eq!(wakes.get(), 1);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    advance_with_wake(
        &mut restored,
        |_| panic!("no replay checkpoint"),
        || panic!("no replay wake"),
    )
    .unwrap();
}
#[test]
fn approval_waits_for_turn_and_wakes_once_with_captured_provider() {
    let (mut state, chat) = approved();
    advance(&mut state, |_| panic!("active turn must not queue another run")).unwrap();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].target.model = Some("captured-model".into());
    let before = serde_json::to_vec(&state).unwrap();
    assert!(advance(&mut state, |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 2);
    assert_eq!(state.runtime.runs[1].chat_id.as_deref(), Some(chat.as_str()));
    assert_eq!(state.runtime.runs[1].target.model.as_deref(), Some("captured-model"));
    assert_eq!(state.runtime.runs[1].kind, RunKind::PortDecision);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    advance(&mut restored, |_| panic!("acknowledged decisions must not replay")).unwrap();
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
    crate::chat::send(&mut state, &chat, "Stop the playtest and leave resources alone", "test").unwrap();
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
        status: crate::design::Status::Answered {
            answer: "Relay".into(),
            asked_at: None,
        },
        continuation: Some(continuation.id.clone()),
    });
    state.ports[0].origin_run = continuation.id.clone();
    state.runtime.runs.push(continuation);
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(state.runtime.runs[2].chat_id.as_deref(), Some(chat.as_str()));
    assert_eq!(state.runtime.runs[2].kind, RunKind::PortDecision);
}
#[test]
fn requests_from_a_resumed_turn_wake_the_conversation_again() {
    let (mut state, chat) = approved();
    state.runtime.runs[0].status = "completed".into();
    advance(&mut state, |_| Ok(())).unwrap();
    let resumed = state.runtime.runs.len() - 1;
    state.runtime.runs[resumed].status = "completed".into();
    state.runtime.runs[resumed].attempts.push(crate::usage::Attempt {
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
        state.chats.iter_mut().find(|s| s.id == child).unwrap().parent_chat = Some(chat.clone());
        let mut run = state.runtime.runs[0].clone();
        run.id = id.into();
        run.kind = crate::runtime::RunKind::Spawned;
        run.chat_id = Some(child.clone());
        run.persona = Some(crate::delegation::Persona {
            role: crate::delegation::Role::Worker,
            root: root.clone(),
            depth,
            parent: Some(parent.clone()),
            bundle: None,
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
    state.services.extend(other.services);
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(state.runtime.runs[2].chat_id.as_deref(), Some(chat.as_str()));
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
#[test]
fn approving_egress_wakes_a_chat_whose_turn_was_cut_off_with_the_relay_line() {
    let (mut state, run, chat) = super::super::tests::fixture();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "notice",
        Input {
            service_id: "svc-a".to_owned().try_into().unwrap(),
            name: "download".to_owned().try_into().unwrap(),
            direction: Direction::Egress,
            service_port: std::num::NonZeroU16::new(3128).unwrap(),
            target: Some(Endpoint {
                host: "files.example".into(),
                port: std::num::NonZeroU16::new(443).unwrap(),
            }),
            peer_port_id: None,
        },
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    // The requesting turn spent its model steps retrying while the request was pending.
    state.runtime.runs[0].status = "failed".into();
    state.runtime.runs[0].attempts[0].termination = Some(crate::attempt_control::Termination::StepLimit);
    advance(&mut state, |_| panic!("nothing is decided yet")).unwrap();
    state.ports[0].status = Status::Approved;
    state.ports[0].decision = Some(Receipt::pending(true));
    advance(&mut state, |_| Ok(())).unwrap();
    let wake = state.runtime.runs.last().unwrap();
    assert_eq!(
        (wake.kind, wake.chat_id.as_deref()),
        (RunKind::PortDecision, Some(chat.as_str()))
    );
    assert!(
        wake.objective.contains("download") && wake.objective.contains("approved"),
        "{}",
        wake.objective
    );
    assert!(
        wake.objective.contains("HTTPS_PROXY") && wake.objective.contains("curl -fsSLO https://files.example/…"),
        "{}",
        wake.objective
    );
    assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
    // A denial wakes a timed-out requester the same way, without relay instructions.
    let (mut denied, _) = approved();
    denied.ports[0].status = Status::Stopping;
    denied.ports[0].approval = None;
    denied.ports[0].decision = Some(Receipt::pending(false));
    denied.runtime.runs[0].status = "failed".into();
    denied.runtime.runs[0].attempts[0].termination = Some(crate::attempt_control::Termination::TimedOut);
    advance(&mut denied, |_| Ok(())).unwrap();
    let wake = denied.runtime.runs.last().unwrap();
    assert_eq!(wake.kind, RunKind::PortDecision);
    assert!(
        wake.objective.contains("denied") && !wake.objective.contains("HTTPS_PROXY"),
        "{}",
        wake.objective
    );
    // An ordinary failure (no host checkpoint) still does not wake.
    let (mut failed, _) = approved();
    failed.runtime.runs[0].status = "failed".into();
    advance(&mut failed, |_| panic!("a plain failure must not wake")).unwrap();
    // Nor does it later, whatever the eligibility rules become: it is retired.
    assert!(failed.ports[0].decision.as_ref().unwrap().acknowledged);
}
/// The requester's turn spent its model steps and failed with a host checkpoint.
fn cut_off(state: &mut State) {
    state.runtime.runs[0].status = "failed".into();
    state.runtime.runs[0].attempts[0].termination = Some(crate::attempt_control::Termination::StepLimit);
}
#[test]
fn a_decision_read_back_after_a_restart_never_queues_a_turn() {
    // Decided before the restart and never delivered: the requester's turn had
    // been cut off, which earlier releases did not wake.
    let (mut state, _) = approved();
    cut_off(&mut state);
    let mut restarted: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    advance_with_wake(
        &mut restarted,
        |_| panic!("nothing to checkpoint"),
        || panic!("no wake after a restart"),
    )
    .unwrap();
    assert_eq!(restarted.runtime.runs.len(), 1);
    assert!(
        restarted.ports[0].decision.as_ref().unwrap().acknowledged,
        "retired once, carried by the next save"
    );
    // The same decision in the process that made it resumes the requester.
    advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.last().unwrap().kind, RunKind::PortDecision);
}
#[test]
fn a_chat_the_operator_cancelled_is_not_resumed_by_a_decision() {
    // Delivered after the operator cancelled the team.
    let (mut state, chat) = approved();
    cut_off(&mut state);
    crate::chat::send(&mut state, &chat, "/cancel all", "test").unwrap();
    advance(&mut state, |_| panic!("cancelled work is not resumed")).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
    // Decided after the cancellation, on a request still pending from before it.
    let (mut state, run, chat) = super::super::tests::fixture();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "notice",
        Input {
            service_id: "svc-a".to_owned().try_into().unwrap(),
            name: "download".to_owned().try_into().unwrap(),
            direction: Direction::Egress,
            service_port: std::num::NonZeroU16::new(3128).unwrap(),
            target: Some(Endpoint {
                host: "files.example".into(),
                port: std::num::NonZeroU16::new(443).unwrap(),
            }),
            peer_port_id: None,
        },
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    cut_off(&mut state);
    crate::chat::send(&mut state, &chat, "/cancel all", "test").unwrap();
    state.ports[0].status = Status::Approved;
    state.ports[0].decision = Some(Receipt::pending(true));
    advance(&mut state, |_| panic!("cancelled work is not resumed")).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    // A requester whose own latest turn was cancelled is not resumed either.
    let (mut state, _) = approved();
    state.runtime.runs[0].status = "cancelled".into();
    state.runtime.runs[0].attempts[0].termination = Some(crate::attempt_control::Termination::Cancelled);
    advance(&mut state, |_| panic!("cancelled turn is not resumed")).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
}
#[test]
fn an_approval_for_a_port_stopped_before_delivery_is_retired() {
    let (mut state, _) = approved(); // the requester's turn is still running
    advance(&mut state, |_| panic!("the turn is still running")).unwrap();
    state.ports[0].status = Status::Stopped;
    cut_off(&mut state);
    advance(&mut state, |_| panic!("a stopped port does not wake")).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
    // Restarting the port does not bring the old decision back.
    state.ports[0].status = Status::Approved;
    advance(&mut state, |_| panic!("retired decisions stay retired")).unwrap();
}
