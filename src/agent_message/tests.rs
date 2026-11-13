
use super::*;

pub(super) fn family() -> (State, String, String, String) {
    let mut state = State::default();
    let lead = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &lead, "Lead the playtest", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        ..Default::default()
    });
    let child = spawned(&mut state, &lead);
    let grandchild = spawned(&mut state, &child);
    (state, lead, child, grandchild)
}
pub(super) fn spawned(state: &mut State, parent: &str) -> String {
    let chat = crate::chat::create(state);
    state
        .chats
        .iter_mut()
        .find(|session| session.id == chat)
        .unwrap()
        .parent_chat = Some(parent.into());
    crate::chat::send(state, &chat, "Set up the bot", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    chat
}
fn send(state: &mut State, nonce: &str, chat: &str) -> io::Result<Value> {
    let run = state.runtime.runs[0].id.clone();
    handle(
        state,
        &AttemptRef {
            run: &run,
            generation: "attempt-a",
            nonce,
        },
        MessageCall {
            chat_id: chat.into(),
            text: "Ports are free; retry".into(),
            services: vec![],
        },
        |_| Ok(()),
    )
}

#[test]
fn a_parent_continues_its_descendants_once_per_request() {
    let (mut state, lead, child, grandchild) = family();
    let first = send(&mut state, "n1", &child).unwrap();
    let queued = state.runtime.runs.last().unwrap();
    assert_eq!(
        (
            queued.kind.as_str(),
            queued.chat_id.as_deref(),
            queued.status.as_str()
        ),
        ("agent-message", Some(child.as_str()), "queued")
    );
    assert!(queued.objective.contains("Ports are free; retry") && queued.objective.contains(&lead));
    assert_eq!(
        queued.kind.speaker(),
        control_context::agent_policy::MessageSpeaker::Control,
        "not a human turn that supersedes approvals"
    );
    let runs = state.runtime.runs.len();
    assert_eq!(
        send(&mut state, "n1", &child).unwrap()["run_id"],
        first["run_id"],
        "retries are idempotent"
    );
    assert_eq!(state.runtime.runs.len(), runs);
    assert!(
        send(&mut state, "n1", &grandchild).is_err(),
        "a request identity names one chat"
    );
    send(&mut state, "n2", &grandchild).unwrap();
    assert_eq!(
        state.runtime.runs.last().unwrap().chat_id.as_deref(),
        Some(grandchild.as_str())
    );
    assert!(
        state
            .chats
            .iter()
            .find(|session| session.id == grandchild)
            .unwrap()
            .messages
            .last()
            .unwrap()
            .text
            .starts_with(&format!("Message from {lead}"))
    );
}

#[test]
fn messages_reach_only_same_actor_task_scope_from_active_attempts() {
    let (mut state, lead, child, _) = family();
    let stranger = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &stranger, "Unrelated", "test").unwrap();
    assert!(
        send(&mut state, "n1", &stranger).is_err(),
        "not spawned by the sender"
    );
    assert!(
        send(&mut state, "n2", &lead).is_err(),
        "not the sender itself"
    );
    state
        .chats
        .iter_mut()
        .find(|session| session.id == child)
        .unwrap()
        .actor = crate::chat::discord_actor("d-123-456").unwrap();
    assert!(
        send(&mut state, "n3", &child).is_err(),
        "another actor's chat"
    );
    let other = spawned(&mut state, &lead);
    assert!(send(&mut state, "bad nonce", &other).is_err());
    state.runtime.runs[0]
        .attempts
        .last_mut()
        .unwrap()
        .workspace_id = Some("attempt-b".into());
    assert!(send(&mut state, "n4", &other).is_err(), "stale attempts");
}

pub(super) fn activate(state: &mut State, chat: &str) -> String {
    let index = crate::chat::latest_task(state, chat, &state.chats[0].actor).unwrap();
    let run = &mut state.runtime.runs[index];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("peer-attempt".into()),
        ..Default::default()
    });
    run.id.clone()
}

#[test]
fn siblings_exchange_targeted_callbacks_after_steering_finishes_without_a_root_bot() {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    state.runtime.runs[0].status = "completed".into();
    let alice_run = activate(&mut state, &alice);
    let cass_index = crate::chat::latest_task(&state, &cass, &state.chats[0].actor).unwrap();
    state.runtime.runs[cass_index].model = Some("recipient-model".into());
    let root_messages = state.chats[0].messages.len();
    let call = || MessageCall {
        chat_id: cass.clone(),
        text: "The handoff point is clear; collect the tool".into(),
        services: vec![],
    };
    let attempt = AttemptRef {
        run: &alice_run,
        generation: "peer-attempt",
        nonce: "direct",
    };
    let first = handle(&mut state, &attempt, call(), |_| Ok(())).unwrap();
    let queued = state.runtime.runs.last().unwrap();
    assert_eq!(queued.chat_id.as_deref(), Some(cass.as_str()));
    assert_eq!(
        queued.model.as_deref(),
        Some("recipient-model"),
        "the recipient retains its own captured execution settings"
    );
    assert_eq!(queued.kind, RunKind::AgentMessage);
    assert_eq!(
        queued.status, "completed",
        "an unapproved peer receives evidence, not an unattended turn"
    );
    assert_eq!(first["queued"], false);
    assert_eq!(first["retained"], true);
    assert_eq!(
        state
            .runtime
            .runs
            .iter()
            .filter(|run| run.status == "queued")
            .count(),
        0
    );
    assert!(queued.objective.contains("not operator steering"));
    assert!(!queued.objective.contains("agent that spawned you"));
    assert!(
        state.collective.posts.is_empty(),
        "targeted messages do not also broadcast room posts"
    );
    assert!(!crate::bots::defined(&state, &lead));
    assert_eq!(
        state.chats[0].messages.len(),
        root_messages,
        "no steering-chat relay or completion callback"
    );

    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let count = restored.runtime.runs.len();
    assert_eq!(
        handle(&mut restored, &attempt, call(), |_| panic!(
            "retry must not save"
        ))
        .unwrap(),
        first
    );
    assert_eq!(restored.runtime.runs.len(), count);
    let cass_run = activate(&mut restored, &cass);
    handle(
        &mut restored,
        &AttemptRef {
            run: &cass_run,
            generation: "peer-attempt",
            nonce: "reply",
        },
        MessageCall {
            chat_id: alice.clone(),
            text: "Pickup verified; the tool dependency is released".into(),
            services: vec![],
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        restored.runtime.runs.last().unwrap().chat_id.as_deref(),
        Some(alice.as_str())
    );
    assert_eq!(
        restored
            .runtime
            .runs
            .iter()
            .filter(|run| run.chat_id.as_deref() == Some(&lead))
            .count(),
        1
    );
    assert_eq!(restored.chats[0].messages.len(), root_messages);
}

#[test]
fn peers_cannot_route_to_the_operator_or_cross_tasks_actors_and_broken_lineage() {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    let unrelated = crate::chat::create(&mut state);
    let outsider = spawned(&mut state, &unrelated);
    let alice_run = activate(&mut state, &alice);
    let attempt = AttemptRef {
        run: &alice_run,
        generation: "peer-attempt",
        nonce: "direct",
    };
    let send_to = |state: &mut State, target: &str| {
        handle(
            state,
            &attempt,
            MessageCall {
                chat_id: target.into(),
                text: "Coordinate".into(),
                services: vec![],
            },
            |_| panic!("rejected messages must not save"),
        )
    };
    assert!(
        send_to(&mut state, &lead).is_err(),
        "steering is not an automatic peer relay"
    );
    assert!(send_to(&mut state, &alice).is_err());
    assert!(send_to(&mut state, &outsider).is_err());
    for chat in &mut state.chats {
        chat.room = Some(format!("general/{lead}"));
    }
    assert!(
        send_to(&mut state, &outsider).is_err(),
        "joining a room does not grant cross-task messaging"
    );
    state
        .chats
        .iter_mut()
        .find(|chat| chat.id == cass)
        .unwrap()
        .actor = crate::chat::discord_actor("d-123-456").unwrap();
    assert!(send_to(&mut state, &cass).is_err());
    state
        .chats
        .iter_mut()
        .find(|chat| chat.id == cass)
        .unwrap()
        .actor = state.chats[0].actor.clone();
    state.chats[0].parent_chat = Some(alice.clone());
    assert!(
        send_to(&mut state, &cass).is_err(),
        "a cyclic task cannot authorize peers"
    );
    state.chats[0].parent_chat = Some("missing-root".into());
    assert!(send_to(&mut state, &cass).is_err());
}

#[test]
fn peers_preserve_stops_action_scope_judges_and_atomic_delivery() {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    let alice_run = activate(&mut state, &alice);
    let cass_index = crate::chat::latest_task(&state, &cass, &state.chats[0].actor).unwrap();
    let attempt = AttemptRef {
        run: &alice_run,
        generation: "peer-attempt",
        nonce: "direct",
    };
    let call = || MessageCall {
        chat_id: cass.clone(),
        text: "Coordinate".into(),
        services: vec![],
    };
    for status in [
        "cancelled",
        "cancelling",
        "failed",
        "interrupted",
        "blocked",
    ] {
        state.runtime.runs[cass_index].status = status.into();
        assert!(
            handle(&mut state, &attempt, call(), |_| panic!(
                "stopped work must not save"
            ))
            .is_err(),
            "{status}"
        );
    }
    state.runtime.runs[cass_index].status = "completed".into();
    state.runtime.runs[cass_index].action = identity::Action::Improve;
    assert!(
        handle(&mut state, &attempt, call(), |_| panic!(
            "action changes must not save"
        ))
        .is_err()
    );
    state.runtime.runs[cass_index].action = identity::Action::Task;
    let mut persona = crate::delegation::lineage(&state.runtime.runs[cass_index]);
    persona.role = crate::delegation::Role::Judge;
    state.runtime.runs[cass_index].persona = Some(persona);
    assert!(
        handle(&mut state, &attempt, call(), |_| panic!(
            "judges must not save"
        ))
        .is_err()
    );
    state.runtime.runs[cass_index].persona = None;
    state
        .chats
        .iter_mut()
        .find(|chat| chat.id == cass)
        .unwrap()
        .room = Some("general".into());
    crate::bots::command(&mut state, &cass, "on").unwrap();
    crate::bots::command(&mut state, &cass, "pause").unwrap();
    let received = handle(&mut state, &attempt, call(), |_| Ok(())).unwrap();
    assert_eq!(
        received["queued"], false,
        "paused bots retain evidence without an automatic turn"
    );
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(crate::scheduler::next_in(&state).is_none());
    crate::bots::command(&mut state, &cass, "resume").unwrap();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        handle(
            &mut state,
            &AttemptRef {
                nonce: "failed-save",
                ..attempt
            },
            call(),
            |_| Err(fail("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
}

#[test]
fn peer_messages_share_only_explicitly_selected_accessible_service_generations() {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    let service = late_service(&mut state, &lead);
    let alice_run = activate(&mut state, &alice);
    state
        .runtime
        .runs
        .iter_mut()
        .find(|run| run.id == alice_run)
        .unwrap()
        .attempts
        .last_mut()
        .unwrap()
        .node_id = Some("node-a".into());
    let attempt = AttemptRef {
        run: &alice_run,
        generation: "peer-attempt",
        nonce: "direct",
    };
    let call = || MessageCall {
        chat_id: cass.clone(),
        text: "Service ready".into(),
        services: vec![service.clone()],
    };
    assert!(
        handle(&mut state, &attempt, call(), |_| panic!(
            "ungranted services must not save"
        ))
        .is_err(),
        "peer messaging does not grant the sender access to the lead's service"
    );
    state.services[0].resource.owner.chat = alice.try_into().unwrap();
    handle(
        &mut state,
        &AttemptRef {
            nonce: "plain",
            ..attempt
        },
        MessageCall {
            chat_id: cass.clone(),
            text: "Ready".into(),
            services: vec![],
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        state
            .runtime
            .runs
            .last()
            .unwrap()
            .resource_grants
            .is_empty()
    );
    let before = serde_json::to_vec(&state).unwrap();
    assert!(handle(&mut state, &attempt, call(), |_| Err(fail("disk full"))).is_err());
    assert_eq!(
        serde_json::to_vec(&state).unwrap(),
        before,
        "peer grants and message roll back together"
    );
    handle(&mut state, &attempt, call(), |_| Ok(())).unwrap();
    let queued = state.runtime.runs.last().unwrap();
    assert_eq!(queued.resource_grants, vec![state.services[0].grant()]);
    assert!(crate::services::accessible(
        &state.services[0],
        &state.services[0].resource.owner.node,
        queued
    ));
}

#[test]
fn peer_archive_failure_rolls_back_message_receipt_and_grants_before_save() {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    let run = activate(&mut state, &alice);
    let before = serde_json::to_vec(&state).unwrap();
    let result = handle_with_archive(
        &mut state,
        &AttemptRef {
            run: &run,
            generation: "peer-attempt",
            nonce: "archive",
        },
        MessageCall {
            chat_id: cass,
            text: "New dependency evidence".into(),
            services: vec![],
        },
        |evidence| {
            assert_eq!(evidence.status, "completed");
            assert_eq!(evidence.source, "peer-message");
            Err(fail("journal full"))
        },
        |_| panic!("a failed archive must not checkpoint inline evidence"),
    );
    assert!(result.is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
}

#[test]
fn failed_saves_leave_nothing_queued() {
    let (mut state, _, child, _) = family();
    let before = serde_json::to_vec(&state).unwrap();
    let run = state.runtime.runs[0].id.clone();
    let result = handle(
        &mut state,
        &AttemptRef {
            run: &run,
            generation: "attempt-a",
            nonce: "n1",
        },
        MessageCall {
            chat_id: child.clone(),
            text: "Retry".into(),
            services: vec![],
        },
        |_| Err(io::Error::other("disk full")),
    );
    assert!(result.is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
}

#[test]
fn receipts_of_running_attempts_are_never_evicted() {
    let (mut state, _, child, _) = family();
    let active = state.runtime.runs[0].id.clone();
    let first = send(&mut state, "n0", &child).unwrap();
    state.agent_messages.extend((1..MAX_SENT).map(|index| Sent {
        key: format!("other/{index}"),
        origin: active.clone(),
        chat: child.clone(),
        run: format!("r{index}"),
        services: vec![],
        peer_cursor: None,
    }));
    assert!(
        send(&mut state, "n1", &child).is_err(),
        "a full cache of live receipts refuses instead of evicting"
    );
    assert_eq!(
        send(&mut state, "n0", &child).unwrap()["run_id"],
        first["run_id"]
    );
    for sent in state.agent_messages.iter_mut().skip(1) {
        sent.origin = "finished-run".into();
    }
    send(&mut state, "n1", &child).unwrap();
    assert_eq!(
        send(&mut state, "n0", &child).unwrap()["run_id"],
        first["run_id"],
        "stale receipts of finished attempts go first"
    );
}

fn late_service(state: &mut State, owner: &str) -> ResourceId {
    let id = ResourceId::try_from("svc-late".to_owned()).unwrap();
    state.runtime.runs[0].attempts[0].node_id = Some("node-a".into());
    state.services.push(serde_json::from_value(json!({
            "resource":{"id":id,"owner":{"node":"node-a","chat":owner},"generation":1,
                "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},
                "lease":[1,u64::MAX],"phase":"Requested"},
            "origin":{"run":state.runtime.runs[0].id,"attempt":"attempt-a"},
            "actor":{"type":"local_operator"},"command":"sleep 60","uid":502,"gid":20
        })).unwrap());
    id
}

#[test]
fn explicit_late_service_grants_reach_only_the_queued_turn_and_survive_continuation() {
    let (mut state, lead, child, _) = family();
    let service = late_service(&mut state, &lead);
    let node = state.services[0].resource.owner.node.clone();
    let before_child = state
        .runtime
        .runs
        .iter()
        .find(|run| run.chat_id.as_deref() == Some(&child))
        .unwrap()
        .clone();
    assert!(!crate::services::accessible(
        &state.services[0],
        &node,
        &before_child
    ));
    send(&mut state, "plain", &child).unwrap();
    assert!(
        !crate::services::accessible(
            &state.services[0],
            &node,
            state.runtime.runs.last().unwrap()
        ),
        "ordinary messages do not grant access"
    );
    let run = state.runtime.runs[0].id.clone();
    let call = || MessageCall {
        chat_id: child.clone(),
        text: "The new service is ready".into(),
        services: vec![service.clone()],
    };
    let attempt = AttemptRef {
        run: &run,
        generation: "attempt-a",
        nonce: "share",
    };
    let response = handle(&mut state, &attempt, call(), |_| Ok(())).unwrap();
    assert!(crate::services::accessible(
        &state.services[0],
        &node,
        state.runtime.runs.last().unwrap()
    ));
    assert!(
        !crate::services::accessible(&state.services[0], &node, &before_child),
        "running or finished attempts are immutable"
    );
    let count = state.runtime.runs.len();
    assert_eq!(
        handle(&mut state, &attempt, call(), |_| Ok(())).unwrap(),
        response
    );
    assert_eq!(state.runtime.runs.len(), count);
    let mut changed = call();
    changed.services.clear();
    assert!(
        handle(&mut state, &attempt, changed, |_| Ok(())).is_err(),
        "a nonce cannot change the grant selection"
    );
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let from = restored.runtime.runs.len() - 1;
    crate::chat::enqueue_continuation(
        &mut restored,
        &child,
        from,
        "Continue".into(),
        RunKind::AgentMessage,
        "test",
    )
    .unwrap();
    assert!(crate::services::accessible(
        &restored.services[0],
        &node,
        restored.runtime.runs.last().unwrap()
    ));
    restored.services[0].resource.generation = serde_json::from_value(json!(2)).unwrap();
    assert!(
        !crate::services::accessible(
            &restored.services[0],
            &node,
            restored.runtime.runs.last().unwrap()
        ),
        "a replacement generation is not granted"
    );
}

#[test]
fn late_grants_survive_design_answers_inserted_before_the_grant_turn() {
    let (mut state, lead, child, _) = family();
    let service = late_service(&mut state, &lead);
    let child_index = crate::chat::latest_task(&state, &child, &state.chats[0].actor).unwrap();
    let run = &mut state.runtime.runs[child_index];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("child-attempt".into()),
        ..Default::default()
    });
    let old_id = run.id.clone();
    let lead_run = state.runtime.runs[0].id.clone();
    handle(
        &mut state,
        &AttemptRef {
            run: &lead_run,
            generation: "attempt-a",
            nonce: "share",
        },
        MessageCall {
            chat_id: child.clone(),
            text: "Service ready".into(),
            services: vec![service],
        },
        |_| Ok(()),
    )
    .unwrap();
    let grant_index = state.runtime.runs.len() - 1;
    crate::design::tests::ask_without_io(
        &mut state,
        &old_id,
        "question",
        crate::design::tests::input(),
    )
    .unwrap();
    let old = &mut state.runtime.runs[child_index];
    old.status = "waiting".into();
    old.attempts[0].status = "waiting".into();
    old.attempts[0].artifact = Some("paused-workspace".into());
    crate::design::answer(&mut state, &child, None, "1", "test").unwrap();
    let answer_index = state.runtime.runs.len() - 1;
    assert_eq!(crate::scheduler::next(&state.runtime), Some(answer_index));
    assert!(
        state.runtime.runs[answer_index].resource_grants.is_empty(),
        "the earlier continuation must not receive a future grant"
    );
    state.runtime.runs[answer_index].status = "completed".into();
    assert_eq!(crate::scheduler::next(&state.runtime), Some(grant_index));
    state.runtime.runs[grant_index].status = "completed".into();
    state.runtime.runs[0].status = "completed".into();
    for source in ["agent", "human", "bot", "listener", "fork"] {
        let mut restored: State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        match source {
            "agent" => {
                restored.runtime.runs[0].status = "running".into();
                send(&mut restored, "next", &child).unwrap();
            }
            "human" => crate::chat::send(&mut restored, &child, "Continue", "test").unwrap(),
            "fork" => {
                crate::chat::fork(&mut restored, &child, "Use the service", "test").unwrap();
            }
            "listener" => {
                restored
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == child)
                    .unwrap()
                    .room = Some("general".into());
                crate::rooms::listen(&mut restored, &child, true).unwrap();
                restored.collective.posts.push(crate::rooms::Post {
                    room: "general".into(),
                    text: "New evidence".into(),
                    chat: None,
                    run: None,
                    key: "test-post".into(),
                    delivery: Default::default(),
                });
                crate::rooms::reconcile(&mut restored, |_| Ok(())).unwrap();
                assert_eq!(
                    restored.runtime.runs.last().unwrap().kind,
                    RunKind::RoomNotification
                );
            }
            _ => {
                restored
                    .chats
                    .iter_mut()
                    .find(|chat| chat.id == child)
                    .unwrap()
                    .room = Some("general".into());
                crate::bots::command(&mut restored, &child, "on").unwrap();
                restored.collective.posts.push(crate::rooms::Post {
                    room: "general".into(),
                    text: "New evidence".into(),
                    chat: None,
                    run: None,
                    key: "test-post".into(),
                    delivery: Default::default(),
                });
                crate::bots::reconcile(&mut restored, |_| Ok(())).unwrap();
                assert_eq!(restored.runtime.runs.last().unwrap().kind, RunKind::BotWake);
            }
        }
        assert!(
            crate::services::accessible(
                &restored.services[0],
                &restored.services[0].resource.owner.node,
                restored.runtime.runs.last().unwrap()
            ),
            "{source} follow-up must inherit the logically latest grant"
        );
    }
}

#[test]
fn late_grants_reject_foreign_services_and_roll_back_atomically() {
    let (mut state, lead, child, grandchild) = family();
    let service = late_service(&mut state, &grandchild);
    let run = state.runtime.runs[0].id.clone();
    let call = || MessageCall {
        chat_id: child.clone(),
        text: "Continue".into(),
        services: vec![service.clone()],
    };
    let attempt = AttemptRef {
        run: &run,
        generation: "attempt-a",
        nonce: "share",
    };
    assert!(
        handle(&mut state, &attempt, call(), |_| Ok(())).is_err(),
        "being an ancestor does not itself grant service access"
    );
    state.services[0].resource.owner.chat = lead.try_into().unwrap();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        handle(&mut state, &attempt, call(), |_| Err(io::Error::other(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(
        serde_json::to_vec(&state).unwrap(),
        before,
        "grant, message and queued turn commit together"
    );
    state.services[0].resource.owner.node = "other-node".to_owned().try_into().unwrap();
    assert!(handle(&mut state, &attempt, call(), |_| Ok(())).is_err());
    state.services[0].resource.owner.node = "node-a".to_owned().try_into().unwrap();
    state.services[0].resource.phase =
        Phase::Stopped(control_core::resources::StopReason::Requested);
    assert!(handle(&mut state, &attempt, call(), |_| Ok(())).is_err());
}
