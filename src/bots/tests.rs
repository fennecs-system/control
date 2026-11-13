
use super::*;

#[test]
#[ignore = "synthetic release request-view benchmark, no credentials or models"]
fn requests_profile() {
    use std::{hint::black_box, time::Instant};
    for count in [0, 32] {
        let (mut state, parent, _) = requester();
        for index in 0..count {
            let child = spawned(&mut state, &parent, &format!("Worker {index}"));
            ask(&mut state, &format!("bench-{index}"), &child, Some(5)).unwrap();
        }
        let iterations = std::env::var("CONTROL_PROFILE_ITERATIONS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(200);
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(crate::chat::transcript(black_box(&state), &parent).unwrap());
                black_box(crate::chat::requests(black_box(&state), &parent).unwrap());
            }
            samples.push(start.elapsed().as_nanos() / u128::from(iterations));
        }
        samples.sort_unstable();
        println!(
            "{}",
            json!({"case":"chat_and_requests","pending":count,"median_ns":samples[4],"min_ns":samples[0]})
        );
    }
}

fn fixture() -> (State, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    crate::chat::send(&mut state, &chat, "Watch the game and coordinate", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    (state, chat)
}
fn post(state: &mut State, chat: Option<&str>, text: &str) {
    state.collective.posts.push(crate::rooms::Post {
        room: "general".into(),
        text: text.into(),
        chat: chat.map(Into::into),
        run: None,
        key: format!("k{}", state.collective.posts.len()),
        delivery: Default::default(),
    });
}
#[test]
fn model_selection_does_not_wake_a_bot_but_is_captured_on_its_next_callback() {
    let (mut state,chat)=fixture();
    state.runtime.models.push(crate::providers::Model::captured(crate::backend::Backend::Claude,"reviewer".into()));
    crate::chat::send(&mut state,&chat,"/model claude:reviewer","test").unwrap();
    assert_eq!(state.runtime.runs.len(),1);
    command_at(&mut state,&chat,"on",Now(0)).unwrap();
    peer_message_at(&mut state,&chat,1,Now(1));
    reconcile_at(&mut state,2,|_|Ok(())).unwrap();
    let runs=bot_runs(&state);assert_eq!(runs.len(),1);
    assert_eq!(runs[0].backend,crate::backend::Backend::Claude);
    assert_eq!(runs[0].model.as_deref(),Some("reviewer"));
    assert_eq!(state.runtime.runs[0].backend,crate::backend::Backend::Codex);
}
fn bot_runs(state: &State) -> Vec<&crate::runtime::Run> {
    state
        .runtime
        .runs
        .iter()
        .filter(|run| run.bot_turn.is_some())
        .collect()
}
#[test]
fn targeted_peer_wakes_require_bot_approval_and_share_its_cooldown_and_budget() {
    let (mut state, chat) = fixture();
    peer_message_at(&mut state, &chat, 1, Now(0));
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state).is_empty(),
        "ordinary workers gain no unattended scheduling authority"
    );
    command_at(&mut state, &chat, "on", Now(2)).unwrap();
    let mut definition = state.bots.supervisor.definitions().next().unwrap().clone();
    definition.budget.turns_per_window = 1;
    state
        .bots
        .supervisor
        .step(Event::Define(definition), Now(3));
    peer_message_at(&mut state, &chat, 2, Now(4));
    peer_message_at(&mut state, &chat, 3, Now(5));
    reconcile_at(&mut state, 6, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        1,
        "multiple targeted messages coalesce into one approved turn"
    );
    let index = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[index].status = "completed".into();
    reconcile_at(&mut state, 7, |_| Ok(())).unwrap();
    peer_message_at(&mut state, &chat, 4, Now(8));
    reconcile_at(&mut state, 31_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        1,
        "peer traffic cannot bypass the exhausted turn budget"
    );
    reconcile_at(&mut state, WINDOW_MS + 10, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        2,
        "the retained event is admitted when the existing budget reopens"
    );
}
#[test]
fn queued_peer_wakes_recheck_pause_and_stop_at_dispatch_and_survive_restart() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    peer_message_at(&mut state, &chat, 2, Now(1));
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    let queued = crate::scheduler::next_in(&state).unwrap();
    command_at(&mut state, &chat, "pause", Now(3)).unwrap();
    assert!(
        crate::scheduler::next_in(&state).is_none(),
        "a turn queued before pause cannot dispatch"
    );
    let mut state: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(crate::scheduler::next_in(&state).is_none());
    let human = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &human, "An unrelated interactive task", "test").unwrap();
    assert_eq!(
        crate::scheduler::next_in(&state),
        Some(state.runtime.runs.len() - 1),
        "a held bot does not block steering"
    );
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    command_at(&mut state, &chat, "resume", Now(4)).unwrap();
    assert_eq!(
        crate::scheduler::next_in(&state),
        Some(queued),
        "resume reuses the original turn instead of duplicating it"
    );
    command_at(&mut state, &chat, "off", Now(5)).unwrap();
    assert_eq!(state.runtime.runs[queued].status, "cancelled");
    reconcile_at(&mut state, 6, |_| Ok(())).unwrap();
    assert!(!defined(&state, &chat));
    peer_message_at(&mut state, &chat, 3, Now(7));
    reconcile_at(&mut state, 8, |_| Ok(())).unwrap();
    assert!(
        crate::scheduler::next_in(&state).is_none(),
        "later messages cannot restart an off bot"
    );
}
#[test]
fn processed_peer_evidence_retires_pending_callbacks_while_paused() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    command_at(&mut state, &chat, "pause", Now(1)).unwrap();
    peer_message_at(&mut state, &chat, 2, Now(2));
    assert!(state.bots.requests.is_empty());
    let effects = state.bots.supervisor.step(
        Event::Processed {
            bot: bot(&chat),
            topic: Topic::new("conversation", &chat),
            through: 2,
        },
        Now(3),
    );
    record(&mut state, effects);
    command_at(&mut state, &chat, "resume", Now(4)).unwrap();
    reconcile_at(&mut state, 5, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state).is_empty(),
        "processing by an active worker prevents a redundant wake"
    );
}
#[test]
fn publication_precedes_deferred_peer_admission_for_an_ordinary_active_worker() {
    use control_context::{
        turn_mailbox::{Disposition, Range, WorkingState},
        working_state::Payload,
    };
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    let run = state.runtime.runs[0].id.clone();
    state.runtime.runs[0].status = "running".into();
    state.agent_messages.push(serde_json::from_value(json!({"key":"peer-receipt","origin":"peer-run","chat":chat,"run":"peer-evidence","peer_cursor":2})).unwrap());
    assert!(
        state.bots.requests.is_empty(),
        "retaining a peer receipt does not request a turn inside the tool"
    );
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state).is_empty(),
        "a reconcile between the active worker's read and publication must defer the wake"
    );
    assert!(state.bots.requests.is_empty());
    state.runtime.runs[0].status = "completed".into();
    state.chats[0].workspace = Some("published".into());
    state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed {
        working: Some(WorkingState {
            revision: 1,
            run,
            generation: "ordinary-active-attempt".into(),
            checkpoint: "published".into(),
            payload: Payload {
                subgoal: "Dependency processed while working".into(),
                ..Default::default()
            },
            processed: vec![Range::Conversation {
                chat: chat.clone(),
                after: 0,
                through: 2,
            }],
            disposition: Disposition::Done,
        }),
        ..Default::default()
    }));
    let previous = serde_json::to_vec(&state).unwrap();
    assert!(reconcile_at(&mut state, 1, |_| Err(std::io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), previous);
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state).is_empty(),
        "already processed peer evidence must not admit an extra callback"
    );
    assert!(state.bots.requests.is_empty());

    let mut scoped: State = serde_json::from_slice(&previous).unwrap();
    let grant: crate::services::Grant = serde_json::from_value(
        json!({"id":"svc-peer","owner":{"node":"node-a","chat":"peer-owner"},"generation":1}),
    )
    .unwrap();
    scoped.services.push(serde_json::from_value(json!({
            "resource":{"id":"svc-peer","owner":{"node":"node-a","chat":"peer-owner"},"generation":1,
                "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},"lease":[1,u64::MAX],"phase":"Requested"},
            "origin":{"run":"peer-run","attempt":"peer-attempt"},"actor":{"type":"local_operator"},"command":"true","uid":502,"gid":20
        })).unwrap());
    let mut evidence = scoped.runtime.runs[0].clone();
    evidence.id = "peer-evidence".into();
    evidence.kind = RunKind::AgentMessage;
    evidence.source = "peer-message".into();
    evidence.queue_order += 1;
    evidence.resource_grants.push(grant.clone());
    scoped.runtime.runs.push(evidence);
    let mut owned: State = serde_json::from_slice(&serde_json::to_vec(&scoped).unwrap()).unwrap();
    let owned_service = serde_json::from_value(json!({
            "resource":{"id":"svc-owned","owner":{"node":"node-a","chat":chat},"generation":1,
                "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},"lease":[1,u64::MAX],"phase":"Requested"},
            "origin":{"run":"active-run","attempt":"active-attempt"},"actor":{"type":"local_operator"},"command":"true","uid":502,"gid":20
        })).unwrap();
    owned.services = vec![owned_service];
    let own_grant = owned.services[0].grant();
    owned.runtime.runs.last_mut().unwrap().resource_grants = vec![own_grant];
    reconcile_at(&mut owned, 2, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&owned).is_empty(),
        "implicit ownership already grants access; processed plain peer evidence needs no extra turn"
    );
    reconcile_at(&mut scoped, 2, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&scoped).len(),
        1,
        "reading a grant offer must not suppress its next-turn activation"
    );
    assert_eq!(bot_runs(&scoped)[0].resource_grants, vec![grant]);
}
#[test]
fn one_shot_wakes_do_not_enable_unattended_continuation() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on every 5", Now(0)).unwrap();
    assert!(continuation_enabled(&state, &chat));
    let mut definition = state.bots.supervisor.definitions().next().unwrap().clone();
    definition.restart = Restart::Temporary;
    state
        .bots
        .supervisor
        .step(Event::Define(definition), Now(1));
    assert!(defined(&state, &chat));
    assert!(!continuation_enabled(&state, &chat));
}
#[test]
fn a_new_bot_does_not_receive_another_bots_undrained_backlog() {
    let (mut state, first) = fixture();
    command_at(&mut state, &first, "on", Now(0)).unwrap();
    post(&mut state, None, "Only the first bot was subscribed");
    let second = crate::chat::create(&mut state);
    state
        .chats
        .iter_mut()
        .find(|c| c.id == second)
        .unwrap()
        .room = Some("general".into());
    crate::chat::send(&mut state, &second, "Start a fresh playtest", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    command_at(&mut state, &second, "on", Now(1)).unwrap();
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    assert_eq!(bot_runs(&state)[0].chat_id.as_deref(), Some(first.as_str()));
    post(&mut state, None, "Both are now subscribed");
    reconcile_at(&mut state, 3, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state)
            .iter()
            .any(|r| r.chat_id.as_deref() == Some(second.as_str()))
    );
}
#[test]
fn missing_handoff_recovery_is_durable_once_and_never_replays_failed_effects() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed::default()));
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1000, |_| Ok(())).unwrap();
    let index = state.runtime.runs.len() - 1;
    let id = state.runtime.runs[index].id.clone();
    let generation = format!("{id}-attempt-1");
    let run = &mut state.runtime.runs[index];
    run.status = "completed".into();
    *run.result = "Verified the worker entrypoint at /workspace/task/main.rs".into();
    run.attempts.push(crate::usage::Attempt {
        context_trace: Some(Box::new(crate::turn_context::TraceRef {
            generation: generation.clone(),
            at: 0,
            sha256: String::new(),
            bytes: 0,
            version: 1,
        })),
        workspace_id: Some(generation),
        artifact: Some("published".into()),
        turn_mailbox: Some(Box::default()).into(),
        ..Default::default()
    });
    state.chats[0].workspace = Some("published".into());
    assert!(needs_recovery(&state, index));
    for status in ["failed", "interrupted", "cancelled", "waiting", "continued"] {
        state.runtime.runs[index].status = status.into();
        assert!(
            !needs_recovery(&state, index),
            "{status} requires its own recovery/decision path"
        );
    }
    state.runtime.runs[index].status = "completed".into();
    state.chats[0].workspace = Some("newer-checkpoint".into());
    assert!(!needs_recovery(&state, index));
    state.chats[0].workspace = Some("published".into());
    state.chats[0].project = Some(crate::projects::Project {
        pending: true,
        ..Default::default()
    });
    assert!(
        !needs_recovery(&state, index),
        "folder approval must not become an automatic retry loop"
    );
    state.chats[0].project = None;
    state.bots.approvals.push(Approval {
        id: "pending".into(),
        chat: chat.clone(),
        requester: chat.clone(),
        origin: id.clone(),
        every_minutes: None,
        room: true,
        decision: Decision::Pending,
        acknowledged: false,
    });
    assert!(
        !needs_recovery(&state, index),
        "operator approval is a genuine wait"
    );
    state.bots.approvals.clear();
    state.runtime.runs[index].spawns.push(serde_json::from_value(json!({"generation":"attempt","nonce":"n","input":{"objectives":["Inspect a dependency"]},"children":[],"notify":true})).unwrap());
    assert!(
        !needs_recovery(&state, index),
        "asynchronous child completion already supplies a callback"
    );
    state.runtime.runs[index].spawns.clear();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(reconcile_at(&mut state, 40_000, |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 2);
    let successor = state.runtime.runs.last().unwrap();
    assert_eq!(
        successor.bot_turn.as_ref().unwrap().recovery.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(
        successor.bot_turn.as_ref().unwrap().reasons,
        [Reason::Continue]
    );
    let stored = serde_json::to_vec(&state).unwrap();
    let mut recovered: State = serde_json::from_slice(&stored).unwrap();
    reconcile_at(&mut recovered, 41_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&recovered).len(),
        2,
        "recovery retains the outstanding successor after restart"
    );

    let mut steered: State = serde_json::from_slice(&before).unwrap();
    crate::chat::send(&mut steered, &chat, "Use the new direction instead", "test").unwrap();
    assert!(
        !needs_recovery(&steered, index),
        "newer user input supplies the next writer"
    );
    let mut direct: State = serde_json::from_slice(&before).unwrap();
    direct.chats[0].continuity.as_mut().unwrap().working =
        Some(control_context::turn_mailbox::WorkingState {
            revision: 1,
            run: direct.runtime.runs[0].id.clone(),
            generation: "earlier".into(),
            checkpoint: "published".into(),
            payload: control_context::working_state::Payload {
                subgoal: "Earlier goal completed".into(),
                ..Default::default()
            },
            processed: vec![],
            disposition: control_context::turn_mailbox::Disposition::Done,
        });
    assert!(
        !needs_recovery(&direct, index),
        "a room callback cannot silently reopen a completed goal"
    );
    direct.runtime.runs[index].bot_turn = None;
    direct.runtime.runs[index].kind = RunKind::Chat;
    assert!(
        needs_recovery(&direct, index),
        "new authorised input can start more work after an earlier goal completed"
    );
    reconcile_at(&mut direct, 40_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&direct).len(),
        1,
        "a direct managed chat session also recovers without a human continue"
    );
    let stored = serde_json::to_vec(&direct).unwrap();
    let mut direct: State = serde_json::from_slice(&stored).unwrap();
    reconcile_at(&mut direct, 41_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&direct).len(),
        1,
        "the direct-session recovery stamp is durable"
    );
}
#[test]
fn committed_idle_wait_does_not_turn_telemetry_into_constant_restarts() {
    use control_context::{
        turn_mailbox::{Disposition, WorkingState},
        working_state::Payload,
    };
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    let id = state.runtime.runs[0].id.clone();
    state.chats[0].workspace = Some("published".into());
    state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed {
        working: Some(WorkingState {
            revision: 1,
            run: id,
            generation: "attempt".into(),
            checkpoint: "published".into(),
            payload: Payload {
                subgoal: "Wait for a meaningful condition".into(),
                ..Default::default()
            },
            processed: vec![],
            disposition: Disposition::Wait {
                topics: vec![],
                recheck_seconds: Some(60),
            },
        }),
        ..Default::default()
    }));
    reconcile_at(&mut state, 1000, |_| Ok(())).unwrap();
    post(&mut state, Some("peer"), "An unchanged observation");
    reconcile_at(&mut state, 2000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    reconcile_at(&mut state, 60_999, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    reconcile_at(&mut state, 61_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        1,
        "the bounded recheck preserves liveness"
    );
}
#[test]
fn committed_runnable_work_resumes_once_and_failed_saves_retain_the_intent() {
    use control_context::{
        turn_mailbox::{Disposition, WorkingState},
        working_state::Payload,
    };
    let (mut state, chat) = fixture();
    command(&mut state, &chat, "on").unwrap();
    let id = state.runtime.runs[0].id.clone();
    state.chats[0].workspace = Some("published".into());
    state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed {
        working: Some(WorkingState {
            revision: 1,
            run: id,
            generation: "attempt".into(),
            checkpoint: "published".into(),
            payload: Payload {
                subgoal: "Finish the goal".into(),
                ..Default::default()
            },
            processed: vec![],
            disposition: Disposition::Continue,
        }),
        ..Default::default()
    }));
    let before = serde_json::to_value(&state).unwrap();
    assert!(reconcile_at(&mut state, 1_000, |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    assert!(
        bot_runs(&state)[0]
            .objective
            .contains("unfinished runnable work")
    );
    assert!(
        bot_runs(&state)[0]
            .objective
            .contains("while meaningful progress is possible")
    );
    let stored = serde_json::to_vec(&state).unwrap();
    let mut recovered: State = serde_json::from_slice(&stored).unwrap();
    recovered.runtime.runs.last_mut().unwrap().status = "completed".into();
    reconcile_at(&mut recovered, 40_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&recovered).len(),
        1,
        "one successor per committed working revision, including after restart"
    );
}

#[test]
fn room_messages_wake_a_bot_once_and_its_finished_turn_releases_the_next() {
    let (mut state, chat) = fixture();
    assert_eq!(
        command(&mut state, &chat, "on"),
        Ok("Bot started. It sleeps until room messages or its timer wake it.")
    );
    assert_eq!(
        status(&state, &chat).as_deref(),
        Some("Bot · sleeping · room #general")
    );
    reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty(), "no events yet");
    post(&mut state, Some("c-other"), "Alice found iron");
    reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
    let runs = bot_runs(&state);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].source.as_str(), runs[0].chat_id.as_deref()),
        ("bot", Some(chat.as_str()))
    );
    assert!(runs[0].objective.contains("new messages in room #general"));
    assert!(
        state.chats[0]
            .messages
            .last()
            .unwrap()
            .text
            .starts_with("Bot woke:")
    );
    post(&mut state, Some("c-other"), "Cass needs wood");
    post(&mut state, Some(&chat), "own post");
    reconcile_at(&mut state, 3_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1, "one turn at a time");
    let first = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[first].status = "completed".into();
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        2,
        "pending messages wake the next turn after the cooldown"
    );
    assert_eq!(
        status(&state, &chat).as_deref(),
        Some("Bot · queued · room #general")
    );
}

#[test]
fn recorded_room_updates_do_not_wake_or_acknowledge_and_notifications_still_wake() {
    let (mut state, chat) = fixture();
    command(&mut state, &chat, "on").unwrap();
    let cursor = crate::rooms::inbox_cursor(&state, &chat);
    for _ in 0..100 {
        post(&mut state, Some("peer"), "Retained progress observation");
        state.collective.posts.last_mut().unwrap().delivery = crate::rooms::Delivery::Record;
    }
    assert!(reconcile_at(&mut state, 1_000, |_| Err(std::io::Error::other("disk"))).is_err());
    reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    assert_eq!(crate::rooms::inbox_cursor(&state, &chat), cursor);
    state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    post(
        &mut state,
        Some("peer"),
        "Dependency ready; continue the build",
    );
    reconcile_at(&mut state, 3_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    let first = state
        .runtime
        .runs
        .iter()
        .position(|r| r.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[first].status = "completed".into();
    post(
        &mut state,
        Some("lead"),
        "Recorded audit of the same milestone",
    );
    state.collective.posts.last_mut().unwrap().delivery = crate::rooms::Delivery::Record;
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        1,
        "a recorded lead update must not create an echo turn"
    );
    post(&mut state, None, "Human steering remains actionable");
    reconcile_at(&mut state, 80_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 2);
}

#[test]
fn timers_wake_bots_without_rooms_and_commands_validate() {
    let (mut state, chat) = fixture();
    state.chats[0].room = None;
    assert!(
        command(&mut state, &chat, "on").is_err(),
        "no room and no interval"
    );
    assert!(command(&mut state, &chat, "on every 0").is_err());
    assert!(command(&mut state, &chat, "on every 2000").is_err());
    assert!(command(&mut state, &chat, "pause").is_err());
    assert!(command(&mut state, &chat, "dance").is_err());
    command(&mut state, &chat, "on every 5").unwrap();
    let started = now_ms();
    reconcile_at(&mut state, started + 60_000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    reconcile_at(&mut state, started + 300_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    assert!(bot_runs(&state)[0].objective.contains("scheduled check-in"));
}

#[test]
fn listening_and_bots_are_exclusive_and_stop_waits_for_the_turn() {
    let (mut state, chat) = fixture();
    crate::rooms::listen(&mut state, &chat, true).unwrap();
    assert!(command(&mut state, &chat, "on").is_err());
    crate::rooms::listen(&mut state, &chat, false).unwrap();
    command(&mut state, &chat, "on").unwrap();
    post(&mut state, None, "operator: can any agent scout the cave?");
    reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
    command(&mut state, &chat, "off").unwrap();
    assert_eq!(
        status(&state, &chat).as_deref(),
        Some("Bot · stopping · room #general")
    );
    let turn = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[turn].status = "completed".into();
    reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
    assert!(!defined(&state, &chat));
    post(&mut state, None, "after stop");
    reconcile_at(&mut state, 50_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
}

#[test]
fn failed_saves_roll_back_and_full_queues_keep_the_request() {
    let (mut state, chat) = fixture();
    command(&mut state, &chat, "on").unwrap();
    post(&mut state, None, "hello");
    let runs = state.runtime.runs.len();
    assert!(reconcile_at(&mut state, 1_000, |_| Err(std::io::Error::other("disk"))).is_err());
    assert_eq!(state.runtime.runs.len(), runs);
    assert_eq!(state.bots.room_cursor, 0, "the post is offered again");
    for index in 0..crate::scheduler::MAX_PENDING {
        state
            .runtime
            .enqueue(format!("filler {index}"), RunKind::Chat, "test");
    }
    reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    assert_eq!(
        state.bots.requests.len(),
        1,
        "the requested turn waits for queue space"
    );
    for run in &mut state.runtime.runs {
        if run.source == "test" {
            run.status = "completed".into();
        }
    }
    reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    assert!(state.bots.requests.is_empty());
}

#[test]
fn command_steps_never_drop_turn_requests_for_any_bot() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    command_at(&mut state, &chat, "pause", Now(1)).unwrap();
    post(&mut state, Some("c-other"), "while paused");
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    command_at(&mut state, &chat, "resume", Now(3)).unwrap();
    assert_eq!(
        state.bots.requests.len(),
        1,
        "resuming with pending events records the turn"
    );
    reconcile_at(&mut state, 4, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);

    let other = crate::chat::create(&mut state);
    let index = state
        .chats
        .iter()
        .position(|session| session.id == other)
        .unwrap();
    state.chats[index].room = None;
    crate::chat::send(&mut state, &other, "Check the farm", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    command_at(&mut state, &other, "on every 1", Now(10)).unwrap();
    command_at(&mut state, &chat, "pause", Now(60_010)).unwrap();
    assert!(
        state
            .bots
            .requests
            .iter()
            .any(|request| request.chat.0 == other),
        "another bot's due timer turn is recorded during this command"
    );
    reconcile_at(&mut state, 60_020, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state)
            .iter()
            .any(|run| run.chat_id.as_deref() == Some(other.as_str()))
    );
}

#[test]
fn requested_turns_survive_a_restart_and_are_queued_once() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    for index in 0..crate::scheduler::MAX_PENDING {
        state
            .runtime
            .enqueue(format!("filler {index}"), RunKind::Chat, "test");
    }
    post(&mut state, Some("c-other"), "while the queue is full");
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert_eq!(state.bots.requests.len(), 1);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    for run in &mut restored.runtime.runs {
        if run.source == "test" {
            run.status = "completed".into();
        }
    }
    reconcile_at(&mut restored, 2, |_| Ok(())).unwrap();
    reconcile_at(&mut restored, 3, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&restored).len(),
        1,
        "the persisted request is queued exactly once"
    );
    assert!(restored.bots.requests.is_empty());
}

#[test]
fn large_room_backlogs_are_offered_in_bounded_passes() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    for index in 0..600 {
        post(&mut state, Some("c-other"), &format!("message {index}"));
    }
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert_eq!(state.bots.room_cursor, POSTS_PER_PASS);
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    reconcile_at(&mut state, 3, |_| Ok(())).unwrap();
    assert_eq!(state.bots.room_cursor, 600);
    assert_eq!(
        bot_runs(&state).len(),
        1,
        "the backlog coalesces into one turn"
    );
}

#[test]
fn bot_commands_through_chat_roll_back_when_the_save_fails() {
    let (mut state, chat) = fixture();
    let send = serde_json::json!({"id": chat, "message": "/bot on every 10"});
    assert!(
        crate::session_http::change_with_save(&mut state, "control.chat.send", &send, |_| Err(
            std::io::Error::other("disk")
        ))
        .is_err()
    );
    assert!(
        !defined(&state, &chat),
        "a failed save leaves no bot behind"
    );
    crate::session_http::change_with_save(&mut state, "control.chat.send", &send, |_| Ok(()))
        .unwrap();
    assert_eq!(
        status(&state, &chat).as_deref(),
        Some("Bot · sleeping · room #general, every 10 min")
    );
    let listen = serde_json::json!({"id": chat, "message": "/listen"});
    assert!(
        crate::session_http::change_with_save(&mut state, "control.chat.send", &listen, |_| Ok(()))
            .is_err(),
        "/listen refuses while a bot runs"
    );
    let off = serde_json::json!({"id": chat, "message": "/listen off"});
    assert!(
        crate::session_http::change_with_save(&mut state, "control.chat.send", &off, |_| Ok(()))
            .is_err(),
        "/listen off cannot remove a bot's inbox"
    );
    assert!(
        state
            .collective
            .room_watches
            .iter()
            .any(|watch| watch.chat == chat)
    );
}

#[test]
fn idle_passes_skip_work_until_an_event_or_deadline() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on every 5", Now(0)).unwrap();
    let mut saves = 0;
    reconcile_at(&mut state, 60_000, |_| {
        saves += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(saves, 0, "a sleeping bot with nothing due does no work");
    post(&mut state, Some("c-other"), "news");
    reconcile_at(&mut state, 61_000, |_| {
        saves += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(saves, 1, "new room posts are offered");
}

fn requester() -> (State, String, String) {
    let (mut state, parent) = fixture();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        ..Default::default()
    });
    let child = spawned(&mut state, &parent, "Farm agent");
    (state, parent, child)
}
fn spawned(state: &mut State, parent: &str, title: &str) -> String {
    let child = crate::chat::create(state);
    let session = state
        .chats
        .iter_mut()
        .find(|session| session.id == child)
        .unwrap();
    session.parent_chat = Some(parent.into());
    session.room = Some("general".into());
    crate::chat::send(state, &child, "Farm wheat", "test").unwrap();
    state
        .chats
        .iter_mut()
        .find(|session| session.id == child)
        .unwrap()
        .title = title.into();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    child
}
fn ask(
    state: &mut State,
    nonce: &str,
    chat: &str,
    every_minutes: Option<u64>,
) -> io::Result<Value> {
    let run = state.runtime.runs[0].id.clone();
    let attempt = AttemptRef {
        run: &run,
        generation: "attempt-a",
        nonce,
    };
    handle(
        state,
        &attempt,
        BotCall::Request {
            chat_id: chat.into(),
            every_minutes,
            room: true,
        },
        |_| Ok(()),
    )
}
#[test]
fn descendant_bot_requests_reach_ancestors_and_fence_other_actors_and_trees() {
    let (mut state, parent, child) = requester();
    let grandchild = spawned(&mut state, &child, "Grandchild");
    let index = state.runtime.runs.len() - 1;
    state.runtime.runs[index].status = "running".into();
    state.runtime.runs[index]
        .attempts
        .push(crate::usage::Attempt {
            workspace_id: Some("child-attempt".into()),
            ..Default::default()
        });
    let run = state.runtime.runs[index].id.clone();
    let before = decision_revision(&state);
    let value = handle(
        &mut state,
        &AttemptRef {
            run: &run,
            generation: "child-attempt",
            nonce: "nested-bot",
        },
        BotCall::Request {
            chat_id: grandchild.clone(),
            every_minutes: Some(5),
            room: true,
        },
        |_| Ok(()),
    )
    .unwrap();
    let id = value["request_id"].as_str().unwrap();
    assert_ne!(before, decision_revision(&state));
    assert_eq!(pending_count(&state, &parent), 1);
    assert_eq!(pending_count(&state, &child), 1);
    assert!(crate::chat::requests(&state, &parent).unwrap().contains(id));
    assert!(
        crate::chat::detail(&state, &parent).unwrap()["pending_bots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["request_id"] == id)
    );
    let unrelated = crate::chat::create(&mut state);
    assert_eq!(pending_count(&state, &unrelated), 0);
    assert!(decide(&mut state, id, true, Some(&unrelated), Now(0)).is_err());
    let parent_index = state.chats.iter().position(|c| c.id == parent).unwrap();
    state.chats[parent_index].actor = identity::Actor::Unknown;
    assert_eq!(pending_count(&state, &parent), 0);
    assert!(decide(&mut state, id, true, Some(&parent), Now(0)).is_err());
    state.chats[parent_index].actor = identity::Actor::LocalOperator;
    let shared = Arc::new(Mutex::new(state));
    let payload = format!("id={id}&return_chat={parent}&allow=yes&response=json");
    let mut response = Vec::new();
    http_with_save(
        &mut response,
        &shared,
        "POST",
        "/tools/bots",
        payload.as_bytes(),
        |_| Err(io::Error::other("full")),
    );
    assert!(
        String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 400")
    );
    assert_eq!(pending_count(&shared.lock().unwrap(), &parent), 1);
    let mut response = Vec::new();
    http_with_save(
        &mut response,
        &shared,
        "POST",
        "/tools/bots",
        payload.as_bytes(),
        |_| Ok(()),
    );
    assert!(
        String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 200")
    );
    let state = shared.lock().unwrap();
    assert!(defined(&state, &grandchild));
    assert_eq!(pending_count(&state, &parent), 0);
}
fn finish_requesting_turn(state: &mut State) {
    state.runtime.runs[0].status = "completed".into();
}

#[test]
fn an_agent_requests_bots_for_its_children_and_resumes_with_the_operators_decisions() {
    let (mut state, parent, child) = requester();
    let other = spawned(&mut state, &parent, "Mine agent");
    let first = ask(&mut state, "n1", &child, None).unwrap();
    let id = first["request_id"].as_str().unwrap().to_owned();
    assert_eq!(
        ask(&mut state, "n1", &child, None).unwrap()["request_id"],
        first["request_id"],
        "retries are idempotent"
    );
    assert!(
        ask(&mut state, "n2", &child, None).is_err(),
        "one pending request per chat"
    );
    let second = ask(&mut state, "n3", &other, Some(10)).unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!defined(&state, &child), "a request grants nothing");
    assert!(pending_panel(&state, &parent).contains(&format!("value='{id}'")));
    assert_eq!(pending_json(&state, &parent).as_array().unwrap().len(), 2);

    crate::chat::send(&mut state, &parent, &format!("/bot allow {id}"), "discord").unwrap();
    assert!(defined(&state, &child));
    finish_requesting_turn(&mut state);
    let runs = state.runtime.runs.len();
    reconcile_at(&mut state, 10, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.len(),
        runs,
        "the requester waits until every request is decided"
    );

    command_at(&mut state, &other, &format!("deny {second}"), Now(20)).unwrap();
    assert!(!defined(&state, &other));
    reconcile_at(&mut state, 30, |_| Ok(())).unwrap();
    let resumed = state.runtime.runs.last().unwrap();
    assert_eq!(
        (resumed.kind.as_str(), resumed.chat_id.as_deref()),
        ("bot-decision", Some(parent.as_str()))
    );
    assert!(
        resumed.objective.contains("Farm agent")
            && resumed.objective.contains("approved")
            && resumed.objective.contains("denied")
    );
    assert!(pending_json(&state, &parent).as_array().unwrap().is_empty());
    let runs = state.runtime.runs.len();
    reconcile_at(&mut state, 40, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.len(),
        runs,
        "decisions resume the requester once"
    );
}

#[test]
fn bots_may_request_bots_only_within_their_conversation() {
    let (mut state, parent, child) = requester();
    let grandchild = spawned(&mut state, &child, "Scout");
    assert!(
        ask(&mut state, "n1", &grandchild, None).is_ok(),
        "descendants through children"
    );
    let stranger = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().room = Some("general".into());
    crate::chat::send(&mut state, &stranger, "Unrelated", "test").unwrap();
    assert!(
        ask(&mut state, "n2", &stranger, None).is_err(),
        "unrelated chats"
    );
    state
        .chats
        .iter_mut()
        .find(|session| session.id == child)
        .unwrap()
        .room = None;
    assert!(
        ask(&mut state, "n3", &child, None).is_err(),
        "no room and no timer"
    );
    assert!(ask(&mut state, "n4", &child, Some(0)).is_err());
    assert!(ask(&mut state, "n5", &child, Some(5)).is_ok());
    assert!(ask(&mut state, "bad nonce", &parent, Some(5)).is_err());
    state.runtime.runs[0]
        .attempts
        .last_mut()
        .unwrap()
        .workspace_id = Some("attempt-b".into());
    assert!(
        ask(&mut state, "n6", &parent, Some(5)).is_err(),
        "stale attempts"
    );
    let first = format!("allow {}", state.bots.approvals[0].id);
    assert!(
        command_at(&mut state, &stranger, &first, Now(1)).is_err(),
        "decided only from the requesting or target chat"
    );
}

#[test]
fn a_leads_helper_may_request_bots_for_controllers_another_helper_spawned() {
    let (mut state, parent, _child) = requester();
    let setup = spawned(&mut state, &parent, "Setup");
    let alice = spawned(&mut state, &setup, "Alice");
    let helper = spawned(&mut state, &parent, "Helper");
    let run = state
        .runtime
        .runs
        .iter_mut()
        .rev()
        .find(|run| run.chat_id.as_deref() == Some(helper.as_str()))
        .unwrap();
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-h".into()),
        ..Default::default()
    });
    let id = run.id.clone();
    let mut call = |chat: &str, nonce: &str| {
        handle(
            &mut state,
            &AttemptRef {
                run: &id,
                generation: "attempt-h",
                nonce,
            },
            BotCall::Request {
                chat_id: chat.into(),
                every_minutes: Some(5),
                room: true,
            },
            |_| Ok(()),
        )
    };
    assert!(
        call(&alice, "n1").is_ok(),
        "a cousin in the same conversation"
    );
    let other = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &other, "Another lead", "test").unwrap();
    let stranger = spawned(&mut state, &other, "Stranger");
    let mut call = |chat: &str, nonce: &str| {
        handle(
            &mut state,
            &AttemptRef {
                run: &id,
                generation: "attempt-h",
                nonce,
            },
            BotCall::Request {
                chat_id: chat.into(),
                every_minutes: Some(5),
                room: true,
            },
            |_| Ok(()),
        )
    };
    assert!(
        call(&stranger, "n2").is_err(),
        "another conversation's agents"
    );
    assert!(call(&other, "n3").is_err(), "another lead");
}

#[test]
fn decisions_for_a_superseded_requester_are_recorded_without_a_new_turn() {
    let (mut state, parent, child) = requester();
    let id = ask(&mut state, "n1", &child, None).unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    finish_requesting_turn(&mut state);
    crate::chat::send(&mut state, &parent, "Something else", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    command_at(&mut state, &parent, &format!("deny {id}"), Now(1)).unwrap();
    let runs = state.runtime.runs.len();
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), runs);
    let session = state
        .chats
        .iter()
        .find(|session| session.id == parent)
        .unwrap();
    assert!(session.messages.last().unwrap().text.contains("denied"));
    assert!(state.bots.approvals.iter().all(|a| a.acknowledged));
}

#[test]
fn approval_dispatch_follows_checkpoint_and_failed_saves_leave_no_wake() {
    let (mut state, parent, child) = requester();
    let id = ask(&mut state, "n1", &child, None).unwrap()["request_id"].as_str().unwrap().to_owned();
    finish_requesting_turn(&mut state);
    command_at(&mut state, &parent, &format!("deny {id}"), Now(1)).unwrap();
    let before = serde_json::to_vec(&state).unwrap();
    let checkpointed = std::cell::Cell::new(false);
    let wakes = std::cell::Cell::new(0);
    assert!(reconcile_with_wake_at(&mut state, 2, |_| Err(io::Error::other("disk full")), || wakes.set(wakes.get() + 1)).is_err());
    assert_eq!(wakes.get(), 0);
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    reconcile_with_wake_at(&mut state, 2, |state| {
        assert!(state.bots.approvals.iter().all(|a| a.acknowledged));
        assert_eq!(state.runtime.runs.last().unwrap().kind, RunKind::BotDecision);
        checkpointed.set(true);
        Ok(())
    }, || {
        assert!(checkpointed.get());
        wakes.set(wakes.get() + 1);
    }).unwrap();
    assert_eq!(wakes.get(), 1);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    reconcile_with_wake_at(&mut restored, 2, |_| Ok(()), || panic!("no replay wake")).unwrap();
}

#[test]
fn the_chat_panel_route_decides_and_rolls_back_failed_saves() {
    let (mut state, parent, child) = requester();
    let id = ask(&mut state, "n1", &child, None).unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shared = Arc::new(Mutex::new(state));
    let body = format!("id={id}&return_chat={parent}&allow=yes&response=json");
    let mut out = Vec::new();
    assert!(http_with_save(
        &mut out,
        &shared,
        "POST",
        "/tools/bots",
        body.as_bytes(),
        |_| Err(io::Error::other("disk full"))
    ));
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"));
    assert!(
        !defined(&shared.lock().unwrap(), &child),
        "a failed save leaves no bot behind"
    );
    let mut out = Vec::new();
    http_with_save(
        &mut out,
        &shared,
        "POST",
        "/tools/bots",
        body.as_bytes(),
        |_| Ok(()),
    );
    assert!(String::from_utf8_lossy(&out).contains("\"ok\":true"));
    assert!(defined(&shared.lock().unwrap(), &child));
    let mut out = Vec::new();
    http_with_save(
        &mut out,
        &shared,
        "POST",
        "/tools/bots",
        format!("id={id}&return_chat={parent}&deny=yes").as_bytes(),
        |_| Ok(()),
    );
    assert!(
        String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"),
        "decided requests are not pending"
    );
    assert!(!http_with_save(
        &mut Vec::new(),
        &shared,
        "POST",
        "/tools/ports",
        b"",
        |_| Ok(())
    ));
}

#[test]
fn earlier_continued_runs_do_not_block_resuming_the_requester() {
    let (mut state, parent, child) = requester();
    let index = state.runtime.runs.len();
    crate::chat::send(&mut state, &parent, "Earlier delegated work", "test").unwrap();
    state.runtime.runs[index].status = "continued".into();
    let requesting = state.runtime.runs.remove(0);
    state.runtime.runs.push(requesting);
    let id = ask_last(&mut state, "n1", &child).unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    command_at(&mut state, &parent, &format!("deny {id}"), Now(1)).unwrap();
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.last().unwrap().kind,
        RunKind::BotDecision
    );
}
fn ask_last(state: &mut State, nonce: &str, chat: &str) -> io::Result<Value> {
    let run = state.runtime.runs.last().unwrap().id.clone();
    let attempt = AttemptRef {
        run: &run,
        generation: "attempt-a",
        nonce,
    };
    handle(
        state,
        &attempt,
        BotCall::Request {
            chat_id: chat.into(),
            every_minutes: None,
            room: true,
        },
        |_| Ok(()),
    )
}

#[test]
fn a_one_shot_callback_can_request_the_next_stage_and_pause_is_durable() {
    let (mut state, chat) = fixture();
    watching(&mut state, &chat, "w-chain");
    assert!(arm_at(&mut state, &chat, Now(0)).unwrap());
    state.watches[0].observe_for_test(1, None);
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    let first = state.runtime.runs.len() - 1;
    assert!(arm_at(&mut state, &chat, Now(2)).unwrap());
    state.watches[0].observe_for_test(2, None);
    state.runtime.runs[first].status = "completed".into();
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    reconcile_at(&mut restored, 3, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&restored).len(),
        2,
        "the explicit next callback survives restart"
    );
    restored.runtime.runs.last_mut().unwrap().status = "completed".into();
    reconcile_at(&mut restored, 4, |_| Ok(())).unwrap();
    assert!(
        !defined(&restored, &chat),
        "one rearm does not turn into a permanent bot"
    );

    command_at(&mut state, &chat, "off", Now(5)).unwrap();
    reconcile_at(&mut state, 6, |_| Ok(())).unwrap();
    command_at(&mut state, &chat, "on every 1", Now(7)).unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.status = "running".into();
    let mut attempt = crate::usage::Attempt::new();
    attempt.workspace_id = Some("pause-attempt".into());
    run.attempts.push(attempt);
    let run = run.id.clone();
    let attempt = AttemptRef {
        run: &run,
        generation: "pause-attempt",
        nonce: "pause",
    };
    assert!(
        handle(&mut state, &attempt, BotCall::Pause, |_| Err(
            io::Error::other("disk full")
        ))
        .is_err()
    );
    assert_ne!(
        state.bots.supervisor.status(&bot(&chat)),
        Some(Status::Paused)
    );
    handle(&mut state, &attempt, BotCall::Pause, |_| Ok(())).unwrap();
    assert_eq!(
        state.bots.supervisor.status(&bot(&chat)),
        Some(Status::Paused)
    );
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    let count = state.runtime.runs.len();
    reconcile_at(&mut state, 1_000_000, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.len(),
        count,
        "a real pause stops timer wakes"
    );
}

#[test]
fn control_initiated_turns_after_a_request_do_not_drop_its_resume() {
    let (mut state, parent, child) = requester();
    let id = ask(&mut state, "n1", &child, None).unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    finish_requesting_turn(&mut state);
    let continuation = crate::chat::continuation(
        &state,
        &parent,
        0,
        "Message from the lead".into(),
        RunKind::AgentMessage,
        "agent-message",
    )
    .unwrap();
    state.runtime.runs.push(continuation);
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    command_at(&mut state, &parent, &format!("deny {id}"), Now(1)).unwrap();
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs.last().unwrap().kind,
        RunKind::BotDecision
    );
}

fn watching(state: &mut State, chat: &str, id: &str) {
    state.watches.push(crate::watches::Watch {
        id: id.into(),
        name: "game".into(),
        chat: chat.into(),
        service: "svc".into(),
        generation: 1,
        permanent: true,
        command: "node bot.js".into(),
        stopped: false,
        wake_policy: Default::default(),
        notification_channel: false,
        notifications: None,
        created_by: None,
        seen: 0,
        lines: 0,
        line_seq: 0,
        wake_seq: None,
        gap: None,
        exits: Vec::new(),
        last_exit: None,
        recent: Vec::new(),
    });
}
fn subscribed(state: &State, chat: &str) -> Vec<String> {
    state
        .bots
        .supervisor
        .definitions()
        .filter(|d| d.chat.0 == chat)
        .flat_map(|d| {
            d.sources.iter().filter_map(|s| match s {
                Source::Topic(t) if t.kind == "watch" => Some(t.name.clone()),
                _ => None,
            })
        })
        .collect()
}

#[test]
fn watch_read_receipts_retire_only_published_evidence_and_survive_restart() {
    use crate::watches::delivery::{prepare, record};
    use control_context::{
        turn_mailbox::{Disposition, Handoff, Mailbox},
        working_state::Payload,
    };
    for notifications in [false, true] {
        let (mut state, chat) = fixture();
        watching(&mut state, &chat, "w-result");
        state.watches[0].notification_channel = notifications;
        command_at(&mut state, &chat, "on", Now(0)).unwrap();
        state.runtime.runs[0].status = "running".into();
        let id = state.runtime.runs[0].id.clone();
        state.runtime.runs[0].attempts.push(crate::usage::Attempt {
            workspace_id: Some("attempt".into()),
            turn_mailbox: Some(Box::new(Mailbox::default())).into(),
            ..Default::default()
        });
        let cx = AttemptRef {
            run: &id,
            generation: "attempt",
            nonce: "read",
        };
        // The worker reads output before the bot adapter offers its wake.
        let observe = |state: &mut State, seq| {
            if notifications {
                state.watches[0].observe(&crate::watch_frame::Frame {
                    seq,
                    event: crate::watch_frame::Event::Notification(b"task result".to_vec()),
                });
            } else {
                state.watches[0].observe_for_test(seq, None);
            }
        };
        observe(&mut state, 1);
        let page = prepare(&state, &cx, "w-result", None).unwrap();
        let before = serde_json::to_value(&state).unwrap();
        assert!(
            record(&mut state, &cx, "w-result", &page, 1, false, |_| Err(
                io::Error::other("disk full")
            ))
            .is_err()
        );
        assert_eq!(serde_json::to_value(&state).unwrap(), before);
        let receipt = record(&mut state, &cx, "w-result", &page, 1, false, |_| Ok(()))
            .unwrap()
            .unwrap();
        let replay = prepare(&state, &cx, "w-result", None).unwrap();
        assert_eq!((replay.after, replay.maximum), (0, Some(1)));
        assert!(
            crate::watches::delivery::prepare_view(&state, &cx, "w-result", None, false).is_err(),
            "a request identity cannot change its event/log view"
        );
        assert_eq!(
            record(&mut state, &cx, "w-result", &replay, 1, false, |_| panic!(
                "replay must not save"
            ))
            .unwrap(),
            Some(receipt.clone())
        );
        assert!(prepare(&state, &cx, "w-result", Some(99)).is_err());
        {
            let mut skipped: State =
                serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
            let other = AttemptRef {
                nonce: "skip",
                ..cx
            };
            let page = prepare(&skipped, &other, "w-result", Some(2)).unwrap();
            assert!(page.gap);
            let receipt = record(
                &mut skipped,
                &other,
                "w-result",
                &page,
                4,
                false,
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
            let replay = prepare(&skipped, &other, "w-result", Some(2)).unwrap();
            assert!(replay.gap);
            assert_eq!(
                record(
                    &mut skipped,
                    &other,
                    "w-result",
                    &replay,
                    4,
                    false,
                    |_| panic!("replay must not save")
                )
                .unwrap(),
                Some(receipt.clone())
            );
            let mailbox = skipped.runtime.runs[0].attempts[0]
                .turn_mailbox
                .as_mut()
                .unwrap();
            assert!(
                mailbox
                    .stage(
                        "gap".into(),
                        Handoff {
                            base_revision: 0,
                            payload: Payload {
                                subgoal: "Missing evidence".into(),
                                ..Default::default()
                            },
                            processed: vec![receipt],
                            resynchronised: vec![],
                            disposition: Disposition::Done,
                            progress: None
                        },
                        0
                    )
                    .is_err()
            );
            skipped.watches[0].chat = "foreign".into();
            let page = prepare(&skipped, &other, "w-result", None).unwrap();
            assert_eq!(
                record(
                    &mut skipped,
                    &other,
                    "w-result",
                    &page,
                    5,
                    false,
                    |_| panic!("foreign observation must not save")
                )
                .unwrap(),
                None
            );
            skipped.runtime.runs[0].status = "cancelled".into();
            assert!(
                record(
                    &mut skipped,
                    &other,
                    "w-result",
                    &page,
                    5,
                    false,
                    |_| Ok(())
                )
                .is_err()
            );
        }
        for n in 0..300 {
            let nonce = format!("empty-{n}");
            let other = AttemptRef {
                nonce: &nonce,
                ..cx
            };
            let page = prepare(&state, &other, "w-result", None).unwrap();
            assert_eq!(page.after, 1);
            record(&mut state, &other, "w-result", &page, 1, false, |_| Ok(())).unwrap();
        }
        assert_eq!(
            state.runtime.runs[0].attempts[0]
                .turn_mailbox
                .as_ref()
                .unwrap()
                .deliveries
                .len(),
            2,
            "empty polls reuse a receipt"
        );
        // A read alone has not advanced committed processing.
        assert!(
            state.chats[0]
                .continuity
                .as_ref()
                .and_then(|m| m.working.as_ref())
                .is_none()
        );
        let mailbox = state.runtime.runs[0].attempts[0]
            .turn_mailbox
            .as_mut()
            .unwrap();
        mailbox
            .stage(
                "handoff".into(),
                Handoff {
                    base_revision: 0,
                    payload: Payload {
                        subgoal: "Wait for the next result".into(),
                        ..Default::default()
                    },
                    processed: vec![receipt],
                    resynchronised: vec![],
                    disposition: Disposition::Wait {
                        topics: vec!["watch:w-result".into()],
                        recheck_seconds: None,
                    },
                    progress: None,
                },
                0,
            )
            .unwrap();
        let working = mailbox
            .publication(None, &id, "attempt", "published")
            .unwrap()
            .unwrap();
        state.runtime.runs[0].status = "completed".into();
        state.chats[0].workspace = Some("published".into());
        state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed {
            working: Some(working),
            ..Default::default()
        }));
        let mut state: State =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        reconcile_at(&mut state, 1000, |_| Ok(())).unwrap();
        assert!(
            bot_runs(&state).is_empty(),
            "already processed watch output must not launch another model turn"
        );
        // Newer output arriving after that read must still wake exactly once.
        observe(&mut state, 2);
        reconcile_at(&mut state, 2000, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1);
        assert!(matches!(
            &bot_runs(&state)[0].bot_turn.as_ref().unwrap().reasons[..],
            [Reason::Info { through: 2, .. }]
        ));
        reconcile_at(&mut state, 2001, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1);
    }
}

#[test]
fn explicit_notifications_release_waits_without_log_matching_or_extra_turns() {
    use crate::watch_frame::{Event as WatchEvent, Frame};
    let (mut state, chat) = fixture();
    state.chats[0].room = None;
    watching(&mut state, &chat, "w-result");
    state.watches[0].notification_channel = true;
    state.watches[0].wake_policy = crate::watches::WakePolicy::ExitOnly {};
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    state.bots.supervisor.step(
        Event::Wait {
            bot: bot(&chat),
            topics: vec![watch("w-result")],
            until: None,
        },
        Now(1),
    );
    for seq in 1..=500 {
        state.watches[0].observe(&Frame {
            seq,
            event: WatchEvent::Line(
                b"task complete, ERROR or unchanged status is still only a log".to_vec(),
            ),
        });
    }
    reconcile_at(&mut state, 86_400_000, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty());
    state.watches[0].observe(&Frame {
        seq: 501,
        event: WatchEvent::Notification(br#"{"task":"task-54","status":"completed"}"#.to_vec()),
    });
    let before = serde_json::to_value(&state).unwrap();
    assert!(
        reconcile_at(&mut state, 86_400_001, |_| Err(io::Error::other(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    let mut state: State = serde_json::from_value(before).unwrap();
    reconcile_at(&mut state, 86_400_002, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    let run = bot_runs(&state)[0];
    assert!(matches!(
        &run.bot_turn.as_ref().unwrap().reasons[..],
        [Reason::Info {
            through: 501,
            info: Message::Notifications(1),
            ..
        }]
    ));
    assert!(run.objective.contains("task-54") && !run.objective.contains("unchanged status"));
    assert!(!state.watches[0].observe(&Frame {
        seq: 501,
        event: WatchEvent::Notification(b"replay".to_vec())
    }));
    reconcile_at(&mut state, 86_400_003, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    assert!(
        !state.watches[0]
            .notifications
            .as_ref()
            .unwrap()
            .recent
            .contains(&"replay".into())
    );
}
#[test]
fn selected_watch_wakes_keep_exact_cursors_across_save_failure_and_restart() {
    use crate::watch_frame::{Event as WatchEvent, Frame};
    let (mut state, chat) = fixture();
    state.chats[0].room = None;
    watching(&mut state, &chat, "w-select");
    state.watches[0].wake_policy = crate::watches::WakePolicy::Prefixes {
        prefixes: vec!["event:".into()],
    };
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    for seq in 1..=100 {
        state.watches[0].observe(&Frame {
            seq,
            event: WatchEvent::Line(b"status: unchanged".to_vec()),
        });
        reconcile_at(&mut state, seq, |_| Ok(())).unwrap();
    }
    assert!(
        bot_runs(&state).is_empty(),
        "telemetry alone admits no model turns"
    );
    state.watches[0].observe(&Frame {
        seq: 101,
        event: WatchEvent::Line(b"event: completed".to_vec()),
    });
    state.watches[0].observe(&Frame {
        seq: 102,
        event: WatchEvent::Line(b"status: unchanged".to_vec()),
    });
    let before = serde_json::to_value(&state).unwrap();
    assert!(reconcile_at(&mut state, 103, |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    let mut state: State = serde_json::from_value(before).unwrap();
    reconcile_at(&mut state, 104, |_| Ok(())).unwrap();
    let runs = bot_runs(&state);
    assert_eq!(runs.len(), 1);
    assert!(matches!(
        &runs[0].bot_turn.as_ref().unwrap().reasons[..],
        [Reason::Info {
            through: 101,
            info: Message::Lines(1),
            ..
        }]
    ));
    assert!(!runs[0].objective.contains("status: unchanged"));
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    state.watches[0].observe_for_test(103, Some(1));
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        2,
        "the owner's own process exit still wakes it"
    );
    assert!(bot_runs(&state)[1].objective.contains("exited with code 1"));
    reconcile_at(&mut state, 40_001, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 2, "no duplicate successor");
}

#[test]
fn exit_only_watches_still_deliver_source_gaps_and_human_steering() {
    use crate::watch_frame::{Event as WatchEvent, Frame};
    let (mut state, chat) = fixture();
    watching(&mut state, &chat, "w-gap");
    state.watches[0].wake_policy = crate::watches::WakePolicy::ExitOnly {};
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    state.watches[0].observe(&Frame {
        seq: 4,
        event: WatchEvent::Line(b"status: unchanged".to_vec()),
    });
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state)[0]
            .objective
            .contains("missing source events after 0 through 3")
    );
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    post(&mut state, None, "Change direction");
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        2,
        "watch policy cannot hide human room input"
    );
}

#[test]
fn a_bot_follows_its_chats_watches_and_wakes_on_lines_and_exits() {
    let (mut state, chat) = fixture();
    state.chats[0].room = None;
    watching(&mut state, &chat, "w-game");
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    assert_eq!(
        subscribed(&state, &chat),
        ["w-game"],
        "a chat with only a watch can run as a bot"
    );
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(bot_runs(&state).is_empty(), "no output yet");
    let w = state.watches.iter_mut().find(|w| w.id == "w-game").unwrap();
    (w.lines, w.line_seq) = (3, 3);
    w.exits.push(crate::watches::Exit {
        seq: 4,
        code: Some(1),
        signal: None,
        restarts: 0,
        gave_up: false,
    });
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    let runs = bot_runs(&state);
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0]
            .objective
            .contains("3 new lines from watch game (w-game)")
            && runs[0].objective.contains("control_watch_events"),
        "{}",
        runs[0].objective
    );
    assert!(!pending_watch_events(&state), "offered events are cleared");
    let first = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[first].status = "completed".into();
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state)[1]
            .objective
            .contains("watch game (w-game) exited with code 1 (1 time(s))"),
        "the exit waits for the next turn"
    );

    watching(&mut state, &chat, "w-logs");
    reconcile_at(&mut state, 40_001, |_| Ok(())).unwrap();
    assert_eq!(
        subscribed(&state, &chat),
        ["w-game", "w-logs"],
        "new watches subscribe the running bot"
    );
    state
        .watches
        .iter_mut()
        .find(|w| w.id == "w-logs")
        .unwrap()
        .stopped = true;
    reconcile_at(&mut state, 40_002, |_| Ok(())).unwrap();
    assert_eq!(
        subscribed(&state, &chat),
        ["w-game"],
        "stopped watches unsubscribe it"
    );
}

#[test]
fn the_bots_page_lists_sleeping_bots() {
    let (mut state, chat) = fixture();
    assert!(overview(&state).contains("No bots"));
    command_at(&mut state, &chat, "on every 10", Now(0)).unwrap();
    let page = overview(&state);
    assert!(
        page.contains("sleeping") && page.contains(&format!("/chat/{chat}")),
        "{page}"
    );
}

#[test]
fn an_armed_wake_fires_once_on_watch_output_then_ends() {
    let (mut state, chat) = fixture();
    state.chats[0].room = None;
    assert!(
        arm_at(&mut state, &chat, Now(0)).is_err(),
        "a wake needs a watch"
    );
    watching(&mut state, &chat, "w-server");
    state.watches[0].lines = 4;
    assert_eq!(arm_at(&mut state, &chat, Now(0)), Ok(true));
    assert_eq!(
        arm_at(&mut state, &chat, Now(0)),
        Ok(false),
        "already armed"
    );
    assert!(status(&state, &chat).unwrap().starts_with("Wake once"));
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state).is_empty(),
        "output from before the arm does not spend the wake"
    );
    let w = state
        .watches
        .iter_mut()
        .find(|w| w.id == "w-server")
        .unwrap();
    (w.lines, w.line_seq) = (1, 1);
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    assert_eq!(bot_runs(&state).len(), 1);
    let w = state
        .watches
        .iter_mut()
        .find(|w| w.id == "w-server")
        .unwrap();
    (w.lines, w.line_seq) = (5, 6);
    let turn = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[turn].status = "completed".into();
    reconcile_at(&mut state, 3, |_| Ok(())).unwrap();
    assert!(!defined(&state, &chat), "the wake ends with its turn");
    reconcile_at(&mut state, 4, |_| Ok(())).unwrap();
    assert_eq!(
        bot_runs(&state).len(),
        1,
        "later output does not wake an unarmed chat"
    );
    command_at(&mut state, &chat, "on", Now(5)).unwrap();
    assert_eq!(
        arm_at(&mut state, &chat, Now(5)),
        Ok(false),
        "a bot already wakes on output"
    );
}

#[test]
fn watch_output_without_a_bot_is_dropped_rather_than_replayed_later() {
    let (mut state, chat) = fixture();
    watching(&mut state, &chat, "w-game");
    let w = state.watches.iter_mut().find(|w| w.id == "w-game").unwrap();
    (w.lines, w.line_seq) = (500, 500);
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(!pending_watch_events(&state));
    command_at(&mut state, &chat, "on", Now(2)).unwrap();
    reconcile_at(&mut state, 3, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state).is_empty(),
        "a new bot is not woken by output from before it existed"
    );
}

#[test]
fn a_finished_watch_still_delivers_its_last_output_and_exit() {
    let (mut state, chat) = fixture();
    watching(&mut state, &chat, "w-once");
    state.watches[0].permanent = false;
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    assert_eq!(subscribed(&state, &chat), ["w-once"]);
    let w = &mut state.watches[0];
    w.observe_for_test(1, None);
    w.observe_for_test(2, Some(0));
    assert!(
        !state.watches[0].active(),
        "a transient watch ends with its exit"
    );
    reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state)[0]
            .objective
            .contains("1 new lines from watch game"),
        "the room-and-watch bot hears the final line"
    );
    assert!(
        subscribed(&state, &chat).is_empty(),
        "and is unsubscribed once it is offered"
    );
    let first = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[first].status = "completed".into();
    reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
    assert!(
        bot_runs(&state)[1].objective.contains("exited with code 0"),
        "the exit was kept for the next turn"
    );
}

#[test]
fn paused_queued_bot_is_not_reported_working_but_an_active_turn_remains_visible() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    peer_message_at(&mut state, &chat, 1, Now(1));
    reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
    let index = state.runtime.runs.iter().position(|run| run.bot_turn.is_some()).unwrap();
    assert!(status(&state, &chat).unwrap().contains(" · queued · "));
    command_at(&mut state, &chat, "pause", Now(3)).unwrap();
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(status(&restored, &chat).unwrap().contains(" · paused · "));
    assert!(!status(&restored, &chat).unwrap().contains("working"));
    let turn = restored.runtime.runs[index].bot_turn.as_ref().unwrap().turn;
    assert!(!restored.bots.supervisor.may_start(&bot(&chat), &TurnId(turn)));
    restored.runtime.runs[index].status = "running".into();
    assert!(status(&restored, &chat).unwrap().contains("paused · current turn running"));
    restored.runtime.runs[index].status = "cancelling".into();
    assert!(status(&restored, &chat).unwrap().contains("paused · current turn stopping"));
    restored.runtime.runs[index].status = "queued".into();
    command_at(&mut restored, &chat, "resume", Now(4)).unwrap();
    assert!(status(&restored, &chat).unwrap().contains(" · queued · "));
    assert!(restored.bots.supervisor.may_start(&bot(&chat), &TurnId(turn)));
}
