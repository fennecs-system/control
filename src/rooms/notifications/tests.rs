use super::*;
#[test]
fn a_bot_turn_stays_open_until_its_continuation_finishes() {
    let (mut state, _) = fixture();
    state.runtime.runs[0].status = "completed".into();
    let chat = state.chats[0].id.clone();
    crate::bots::command(&mut state, &chat, "on").unwrap();
    post(
        &mut state,
        "research",
        "operator: can any agent check the logs?",
        None,
    );
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    let turn = state
        .runtime
        .runs
        .iter()
        .position(|run| run.bot_turn.is_some())
        .unwrap();
    running_attempt(&mut state, turn, "bot-attempt");
    let continuation = answer_question(&mut state, turn);
    assert_eq!(state.runtime.runs[turn].status, "continued");
    post(&mut state, "research", "another message", None);
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(
        crate::bots::status(&state, &chat)
            .unwrap()
            .contains("working"),
        "the continuation keeps the turn open"
    );
    assert_eq!(
        state
            .runtime
            .runs
            .iter()
            .filter(|run| run.bot_turn.is_some())
            .count(),
        1
    );
    state.runtime.runs[continuation].status = "completed".into();
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(
        crate::bots::status(&state, &chat)
            .unwrap()
            .contains("sleeping"),
        "the finished leaf releases the bot"
    );
}

#[test]
fn a_bot_owned_inbox_cannot_be_removed_by_its_agent() {
    let (mut state, run) = fixture();
    let chat = state.chats[0].id.clone();
    crate::bots::command(&mut state, &chat, "on").unwrap();
    assert!(
        state
            .collective
            .room_watches
            .iter()
            .any(|watch| watch.chat == chat)
    );
    assert!(
        call(
            &mut state,
            &run,
            "control_room_watch",
            &json!({"enabled": false})
        )
        .is_err()
    );
    assert!(
        state
            .collective
            .room_watches
            .iter()
            .any(|watch| watch.chat == chat),
        "the inbox survives"
    );
    post(
        &mut state,
        "research",
        "operator: can any agent check the logs?",
        None,
    );
    let read = call(&mut state, &run, "control_room_notifications", &json!({})).unwrap();
    assert!(read.to_string().contains("check the logs"));
}

pub(super) fn fixture() -> (State, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].room = Some("research".into());
    crate::chat::send(&mut state, &chat, "Research the source", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.model = Some("captured-model".into());
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-one".into()),
        ..Default::default()
    });
    let id = run.id.clone();
    (state, id)
}
fn watch(state: &mut State, run: &str, wake: bool) {
    call(
        state,
        run,
        "control_room_watch",
        &json!({"enabled":true,"wake":wake}),
    )
    .unwrap();
}
pub(super) fn post(state: &mut State, room: &str, text: &str, chat: Option<String>) {
    state.collective.posts.push(super::super::Post {
        room: room.into(),
        text: text.into(),
        chat,
        run: None,
        key: String::new(),
        delivery: Default::default(),
    });
}
fn rpc(name: &str, args: Value) -> Value {
    json!({"nonce":"1-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}})
}
fn fail(_: &State) -> std::io::Result<()> {
    Err(std::io::Error::other("fixture checkpoint failure"))
}

#[test]
fn inbox_is_fifo_bounded_explicitly_acknowledged_and_durable() {
    let (mut state, run) = fixture();
    post(&mut state, "research", "before subscription", None);
    watch(&mut state, &run, false);
    let chat = state.chats[0].id.clone();
    post(&mut state, "research", "self", Some(chat));
    post(&mut state, "engineering", "other room", None);
    for i in 0..20 {
        post(&mut state, "research", &format!("update {i}"), None);
    }
    let before = call(&mut state, &run, "control_room_notifications", &json!({})).unwrap();
    assert_eq!(before["posts"].as_array().unwrap().len(), BATCH);
    assert_eq!(before["skipped"], 0);
    assert_eq!(before["posts"][0]["text"], "update 0");
    assert_eq!(before["more"], true);
    assert!(
        call(
            &mut state,
            &run,
            "control_room_ack",
            &json!({"through":before["through"].as_u64().unwrap()+1})
        )
        .is_err()
    );
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        call(
            &mut restored,
            &run,
            "control_room_notifications",
            &json!({})
        )
        .unwrap(),
        before
    );
    for _ in 0..2 {
        call(
            &mut restored,
            &run,
            "control_room_ack",
            &json!({"through":before["through"]}),
        )
        .unwrap();
    }
    let mut received: Vec<_> = before["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|post| post["text"].as_str().unwrap().to_owned())
        .collect();
    loop {
        let next = call(
            &mut restored,
            &run,
            "control_room_notifications",
            &json!({}),
        )
        .unwrap();
        assert_eq!(next["skipped"], 0);
        if next["posts"].as_array().unwrap().is_empty() {
            break;
        }
        received.extend(
            next["posts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|post| post["text"].as_str().unwrap().to_owned()),
        );
        call(
            &mut restored,
            &run,
            "control_room_ack",
            &json!({"through":next["through"]}),
        )
        .unwrap();
        restored = serde_json::from_slice(&serde_json::to_vec(&restored).unwrap()).unwrap();
    }
    assert_eq!(
        received,
        (0..20).map(|i| format!("update {i}")).collect::<Vec<_>>()
    );
    for i in 0..WINDOW {
        post(
            &mut restored,
            "engineering",
            &format!("unrelated {i}"),
            None,
        );
    }
    post(&mut restored, "research", "last matching update", None);
    let window = call(
        &mut restored,
        &run,
        "control_room_notifications",
        &json!({}),
    )
    .unwrap();
    assert_eq!(window["more"], true);
    assert_eq!(window["skipped"], 0);
    assert!(window["posts"].as_array().unwrap().is_empty());
    assert!(
        call(
            &mut restored,
            &run,
            "control_room_ack",
            &json!({"through":window["through"].as_u64().unwrap()+1})
        )
        .is_err()
    );
    call(
        &mut restored,
        &run,
        "control_room_ack",
        &json!({"through":window["through"]}),
    )
    .unwrap();
    assert_eq!(
        call(
            &mut restored,
            &run,
            "control_room_notifications",
            &json!({})
        )
        .unwrap()["posts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn shared_service(state: &mut State) -> crate::services::Grant {
    state.services.push(serde_json::from_value(json!({
            "resource":{"id":"svc-shared","owner":{"node":"node-a","chat":"other-owner"},"generation":1,
                "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},
                "lease":[1,u64::MAX],"phase":"Requested"},
            "origin":{"run":"owner-run","attempt":"owner-attempt"},
            "actor":{"type":"local_operator"},"command":"sleep 60","uid":502,"gid":20
        })).unwrap());
    state.services.last().unwrap().grant()
}

#[test]
fn one_shot_retains_grants_received_after_arming() {
    let (mut state, run) = fixture();
    let chat = state.chats[0].id.clone();
    watch(&mut state, &run, true);
    let grant = shared_service(&mut state);
    crate::chat::enqueue_continuation(
        &mut state,
        &chat,
        0,
        "Service ready".into(),
        RunKind::AgentMessage,
        "test",
    )
    .unwrap();
    state.runtime.runs[1].resource_grants.push(grant);
    for run in &mut state.runtime.runs {
        run.status = "completed".into();
    }
    post(&mut state, "research", "New evidence", None);
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(state.runtime.runs[2].kind, RunKind::RoomNotification);
    assert!(crate::services::accessible(
        &state.services[0],
        &state.services[0].resource.owner.node,
        &state.runtime.runs[2]
    ));
    assert!(
        state.runtime.runs[0].resource_grants.is_empty(),
        "arming attempt remains unchanged"
    );
}

#[test]
fn recorded_updates_preserve_one_shot_budget_and_unread_evidence() {
    let (mut state, run) = fixture();
    watch(&mut state, &run, true);
    state.runtime.runs[0].status = "completed".into();
    for _ in 0..WINDOW + 1 {
        post(
            &mut state,
            "research",
            "Retained observation",
            Some("peer".into()),
        );
        state.collective.posts.last_mut().unwrap().delivery = super::super::Delivery::Record;
    }
    assert!(reconcile(&mut state, fail).is_err());
    assert_eq!(
        state.collective.room_watches[0]
            .wake
            .as_ref()
            .unwrap()
            .scanned,
        0
    );
    for _ in 0..3 {
        reconcile(&mut state, |_| Ok(())).unwrap();
    }
    assert_eq!(state.runtime.runs.len(), 1);
    assert_eq!(state.collective.room_watches[0].cursor, 0);
    assert!(state.collective.room_watches[0].wake.is_some());
    state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    state.runtime.runs[0].status = "running".into();
    let page = call(&mut state, &run, "control_room_notifications", &json!({})).unwrap();
    assert_eq!(page["posts"][0]["delivery"], "record");
    state.runtime.runs[0].status = "completed".into();
    post(&mut state, "research", "Decision needed", None);
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 2);
    assert_eq!(
        state.collective.room_watches[0].cursor, 0,
        "admission is not processing"
    );
}

#[test]
fn one_shot_waits_for_idle_and_survives_failed_checkpoint_and_restart() {
    let (mut state, run) = fixture();
    let grant = shared_service(&mut state);
    state.runtime.runs[0].resource_grants.push(grant);
    watch(&mut state, &run, true);
    watch(&mut state, &run, true);
    post(&mut state, "research", "A concise discovery", None);
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    state.runtime.runs[0].status = "waiting".into();
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    state.runtime.runs[0].status = "completed".into();
    let revision = state.chats[0].revision;
    assert!(reconcile(&mut state, fail).is_err());
    assert_eq!(state.runtime.runs.len(), 1);
    assert_eq!(state.chats[0].revision, revision);
    assert!(state.collective.room_watches[0].wake.is_some());
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 2);
    assert!(
        crate::services::accessible(
            &state.services[0],
            &state.services[0].resource.owner.node,
            &state.runtime.runs[1]
        ),
        "a room wake retains the armed task's exact service grant"
    );
    assert_eq!(
        state.runtime.runs[1].persona.as_ref().unwrap().root,
        run,
        "automatic notifications must retain the original team's budget"
    );
    assert_eq!(
        state.runtime.runs[1].model.as_deref(),
        Some("captured-model")
    );
    assert!(
        !state.runtime.runs[1]
            .objective
            .contains("A concise discovery")
    );
    let delivered = state.collective.room_watches[0].delivered.clone();
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    reconcile(&mut restored, |_| panic!("consumed wake must be idle")).unwrap();
    assert_eq!(restored.runtime.runs.len(), 2);
    assert_eq!(restored.collective.room_watches[0].delivered, delivered);
    restored.services[0].resource.generation = serde_json::from_value(json!(2)).unwrap();
    assert!(
        !crate::services::accessible(
            &restored.services[0],
            &restored.services[0].resource.owner.node,
            &restored.runtime.runs[1]
        ),
        "room wakes cannot gain replacement generations"
    );
    restored.runtime.runs[1].status = "running".into();
    restored.runtime.runs[1]
        .attempts
        .push(crate::usage::Attempt {
            workspace_id: Some("notification-attempt".into()),
            ..Default::default()
        });
    let notification = restored.runtime.runs[1].id.clone();
    assert!(
        call(
            &mut restored,
            &notification,
            "control_room_watch",
            &json!({"enabled":true,"wake":true})
        )
        .is_err()
    );
    post(&mut restored, "research", "Another update", None);
    restored.runtime.runs[1].status = "completed".into();
    reconcile(&mut restored, |_| panic!("no implicit rearming")).unwrap();
    assert_eq!(restored.runtime.runs.len(), 2);
}

#[test]
fn cancellation_revocation_scope_changes_and_stale_generation_disarm_wakes() {
    for scenario in 0..6 {
        let (mut state, run) = fixture();
        watch(&mut state, &run, true);
        post(&mut state, "research", "Wake candidate", None);
        state.runtime.runs[0].status = "completed".into();
        match scenario {
            0 => state.runtime.runs[0].status = "cancelled".into(),
            1 => {
                state.runtime.runs[0].actor = identity::Actor::Unknown;
                state.collective.room_watches[0].actor = identity::Actor::Unknown;
                state.chats[0].actor = identity::Actor::Unknown;
            }
            2 => state.chats[0].room = Some("engineering".into()),
            3 => state.runtime.runs[0].attempts[0].workspace_id = Some("retry-generation".into()),
            4 => {
                let chat = state.chats[0].id.clone();
                crate::chat::send(&mut state, &chat, "A newer human task", "test").unwrap();
            }
            _ => state.chats[0].actor = identity::Actor::Unknown,
        }
        let count = state.runtime.runs.len();
        reconcile(&mut state, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(), count);
        assert!(
            state.collective.room_watches[0].wake.is_none(),
            "scenario {scenario}"
        );
    }
}

#[test]
fn self_posts_do_not_wake_and_checkpoint_failure_restores_watch_budget_and_ack() {
    let (mut state, run) = fixture();
    let request = rpc("control_room_watch", json!({"enabled":true,"wake":true}));
    assert!(
        super::super::checkpoint(&mut state, &run, &request, fail)
            .get("error")
            .is_some()
    );
    assert!(state.collective.room_watches.is_empty());
    assert!(!state.runtime.runs[0].room_wake_used);
    assert!(
        super::super::checkpoint(&mut state, &run, &request, |_| Ok(()))
            .get("result")
            .is_some()
    );
    let request = rpc("control_room_post", json!({"message":"My own update"}));
    super::super::checkpoint(&mut state, &run, &request, |_| Ok(()));
    super::super::checkpoint(&mut state, &run, &request, |_| Ok(()));
    assert_eq!(state.collective.posts.len(), 1);
    assert!(
        super::super::checkpoint(
            &mut state,
            &run,
            &rpc("control_room_ack", json!({"through":1})),
            fail
        )
        .get("error")
        .is_some()
    );
    assert_eq!(state.collective.room_watches[0].cursor, 0);
    assert!(
        super::super::checkpoint(
            &mut state,
            &run,
            &rpc("control_room_watch", json!({"enabled":false})),
            fail
        )
        .get("error")
        .is_some()
    );
    assert_eq!(state.collective.room_watches.len(), 1);
    assert!(state.collective.room_watches[0].wake.is_some());
    state.runtime.runs[0].status = "completed".into();
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    assert_eq!(
        state.collective.room_watches[0].cursor, 0,
        "wake scanning is not an inbox acknowledgement"
    );
    assert_eq!(
        state.collective.room_watches[0]
            .wake
            .as_ref()
            .unwrap()
            .scanned,
        1
    );
    assert!(state.collective.room_watches[0].wake.is_some());
}

#[test]
fn budget_cannot_be_refilled_by_disabling_or_by_child_personas() {
    let (mut state, run) = fixture();
    watch(&mut state, &run, true);
    call(
        &mut state,
        &run,
        "control_room_watch",
        &json!({"enabled":false}),
    )
    .unwrap();
    assert!(
        call(
            &mut state,
            &run,
            "control_room_watch",
            &json!({"enabled":true,"wake":true})
        )
        .is_err()
    );
    state.runtime.runs[0].room_wake_used = false;
    state.runtime.runs[0].persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Worker,
        root: "notification-root".into(),
        depth: 1,
        parent: Some("notification-parent".into()),
    });
    assert!(
        call(
            &mut state,
            &run,
            "control_room_watch",
            &json!({"enabled":true,"wake":true})
        )
        .is_err()
    );
    state.runtime.runs[0].persona.as_mut().unwrap().role = crate::delegation::Role::Judge;
    assert!(
        call(
            &mut state,
            &run,
            "control_room_watch",
            &json!({"enabled":true})
        )
        .is_err()
    );
    state.runtime.runs[0].persona = None;
    state.chats[0].actor = identity::Actor::Unknown;
    assert!(
        call(
            &mut state,
            &run,
            "control_room_watch",
            &json!({"enabled":true})
        )
        .is_err()
    );
}

fn running_attempt(state: &mut State, index: usize, generation: &str) {
    let run = &mut state.runtime.runs[index];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some(generation.into()),
        ..Default::default()
    });
}
fn answer_question(state: &mut State, index: usize) -> usize {
    let run = &mut state.runtime.runs[index];
    let generation = run.attempts.last().unwrap().workspace_id.clone().unwrap();
    run.status = "waiting".into();
    let attempt = run.attempts.last_mut().unwrap();
    attempt.status = "waiting".into();
    attempt.artifact = Some(format!("checkpoint-{generation}"));
    run.question = Some(crate::design::Question {
        id: format!("question-{generation}"),
        nonce: "question".into(),
        generation,
        input: crate::design::tests::input(),
        answer: None,
        continuation: None,
    });
    let chat = run.chat_id.clone().unwrap();
    crate::design::answer(state, &chat, None, "1", "test").unwrap();
    state.runtime.runs.len() - 1
}

#[test]
fn watch_survives_real_delegation_and_design_continuations_without_rearming() {
    let (mut state, root) = fixture();
    watch(&mut state, &root, true);
    let parent = &mut state.runtime.runs[0];
    parent.status = "waiting".into();
    parent.attempts[0].status = "waiting".into();
    parent.attempts[0].artifact = Some("parent-checkpoint".into());
    parent.delegation = Some(crate::delegation::Delegation {
        input: crate::delegation::Input {
            objective: "Inspect source".into(),
            role: crate::delegation::Role::Worker,
        },
        nonce: "delegation".into(),
        generation: "attempt-one".into(),
        backend: parent.backend,
        model: parent.model.clone().unwrap(),
        child: None,
        continuation: None,
    });
    crate::delegation::reconcile(&mut state, |_| Ok(())).unwrap();
    post(&mut state, "research", "Assign the next task", None);
    reconcile(&mut state, |_| panic!("child is still working")).unwrap();
    assert!(state.collective.room_watches[0].wake.is_some());
    state.runtime.runs[1].status = "completed".into();
    state.runtime.runs[1].result = "Verified source".into();
    crate::delegation::reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs[0].status, "continued");
    let resumed = state.runtime.runs.len() - 1;
    reconcile(&mut state, |_| panic!("parent continuation is queued")).unwrap();
    running_attempt(&mut state, resumed, "resume-one");
    let resumed_id = state.runtime.runs[resumed].id.clone();
    assert!(
        call(
            &mut state,
            &resumed_id,
            "control_room_watch",
            &json!({"enabled":true,"wake":true})
        )
        .is_err()
    );
    let leaf = answer_question(&mut state, resumed);
    reconcile(&mut state, |_| panic!("design continuation is queued")).unwrap();
    running_attempt(&mut state, leaf, "resume-two");
    state.runtime.runs[leaf].status = "completed".into();
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let count = restored.runtime.runs.len();
    reconcile(&mut restored, |_| Ok(())).unwrap();
    assert_eq!(restored.runtime.runs.len(), count + 1);
    let notification = restored.runtime.runs.last().unwrap();
    assert_eq!(notification.kind, RunKind::RoomNotification);
    assert_eq!(notification.model, state.runtime.runs[0].model);
    assert_eq!(notification.backend, state.runtime.runs[0].backend);
    assert_eq!(notification.context, state.runtime.runs[0].context);
    assert_eq!(notification.actor, state.runtime.runs[0].actor);
    assert_eq!(notification.persona.as_ref().unwrap().root, root);
    reconcile(&mut restored, |_| panic!("wake already consumed")).unwrap();
    assert_eq!(restored.runtime.runs.len(), count + 1);
}

#[test]
fn continued_watch_rejects_cancelled_retried_cross_scope_and_cyclic_receipts() {
    for scenario in 0..8 {
        let (mut state, root) = fixture();
        watch(&mut state, &root, true);
        let first = answer_question(&mut state, 0);
        running_attempt(&mut state, first, "second-attempt");
        let leaf = answer_question(&mut state, first);
        running_attempt(&mut state, leaf, "third-attempt");
        state.runtime.runs[leaf].status = "completed".into();
        post(&mut state, "research", "Task for lead", None);
        match scenario {
            0 => state.runtime.runs[leaf].status = "cancelled".into(),
            1 => state.runtime.runs[leaf]
                .attempts
                .push(crate::usage::Attempt {
                    workspace_id: Some("retry-generation".into()),
                    ..Default::default()
                }),
            2 => {
                state.runtime.runs[first]
                    .question
                    .as_mut()
                    .unwrap()
                    .generation = "stale-generation".into()
            }
            3 => state.runtime.runs[leaf].chat_id = Some("sibling-chat".into()),
            4 => state.runtime.runs[leaf].model = Some("different-model".into()),
            5 => {
                let target = state.runtime.runs[first].id.clone();
                state.runtime.runs[leaf].status = "continued".into();
                state.runtime.runs[leaf].question = Some(crate::design::Question {
                    id: "cycle".into(),
                    nonce: "cycle".into(),
                    generation: "third-attempt".into(),
                    input: crate::design::tests::input(),
                    answer: Some("yes".into()),
                    continuation: Some(target),
                });
            }
            6 => {
                let chat = state.chats[0].id.clone();
                crate::chat::send(&mut state, &chat, "A new human task", "test").unwrap();
            }
            _ => state.runtime.runs[leaf].persona.as_mut().unwrap().depth = 1,
        }
        let count = state.runtime.runs.len();
        reconcile(&mut state, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(), count, "scenario {scenario}");
        assert!(
            state.collective.room_watches[0].wake.is_none(),
            "scenario {scenario}"
        );
    }
}

#[test]
fn nonblocking_spawn_does_not_delay_the_idle_leads_room_wake() {
    let (mut state, root) = fixture();
    watch(&mut state, &root, true);
    let child_chat = crate::chat::create(&mut state);
    let mut child = state.runtime.runs[0].clone();
    child.id = "independent-child".into();
    child.chat_id = Some(child_chat);
    child.kind = RunKind::Spawned;
    child.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Worker,
        root: root.clone(),
        depth: 1,
        parent: Some(root),
    });
    state.runtime.runs.push(child);
    state.runtime.runs[0].status = "completed".into();
    post(&mut state, "research", "Child asks lead a question", None);
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(state.runtime.runs[1].status, "running");
    assert_eq!(state.runtime.runs[2].kind, RunKind::RoomNotification);
}

#[test]
fn existing_design_continuation_without_persona_keeps_its_armed_watch() {
    let (mut state, root) = fixture();
    watch(&mut state, &root, true);
    let leaf = answer_question(&mut state, 0);
    // Older checkpoints copied a root's absent persona into design answers.
    state.runtime.runs[leaf].persona = None;
    running_attempt(&mut state, leaf, "legacy-design-attempt");
    state.runtime.runs[leaf].status = "completed".into();
    post(&mut state, "research", "Pending assignment", None);
    reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), 3);
    assert_eq!(
        state
            .runtime
            .runs
            .last()
            .unwrap()
            .persona
            .as_ref()
            .unwrap()
            .root,
        root
    );
}

#[test]
fn full_regular_queue_retains_pending_wake_without_saving_or_overflowing() {
    let (mut state, run) = fixture();
    watch(&mut state, &run, true);
    state.runtime.runs[0].status = "completed".into();
    post(&mut state, "research", "external update", None);
    for _ in 0..crate::scheduler::MAX_PENDING {
        state
            .runtime
            .enqueue_for(
                "other task".into(),
                RunKind::Task,
                "fixture",
                identity::Actor::LocalOperator,
                identity::Action::Task,
            )
            .unwrap();
    }
    let count = state.runtime.runs.len();
    reconcile(&mut state, |_| {
        panic!("a full queue must not mutate the watch")
    })
    .unwrap();
    assert_eq!(state.runtime.runs.len(), count);
    assert!(state.collective.room_watches[0].wake.is_some());
}

#[test]
#[ignore = "explicit room publication CPU profile; no models or live state"]
fn profile_room_publication() {
    let iterations = std::env::var("CONTROL_ROOM_PUBLICATION_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(10, 10_000);
    for retained in [16usize, 1024, 20_001] {
        let (mut state, id) = fixture();
        watch(&mut state, &id, true);
        state.runtime.runs[0].status = "completed".into();
        let template = state.runtime.runs[0].clone();
        for index in 1..retained {
            let mut run = template.clone();
            run.id = format!("retained-{index}");
            run.kind = RunKind::Task;
            run.queue_order = index as u64;
            state.runtime.runs.push(run);
        }
        post(
            &mut state,
            "research",
            "Peer needs action",
            Some("peer".into()),
        );
        let watch = state.collective.room_watches[0].clone();
        let notice = state.runtime.notice.clone();
        let revision = state.chats[0].revision;
        let messages = state.chats[0].messages.len();
        let mut samples = Vec::new();
        let saves = std::cell::Cell::new(0);
        for _ in 0..10 {
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                reconcile(std::hint::black_box(&mut state), |_| {
                    saves.set(saves.get() + 1);
                    Ok(())
                })
                .unwrap();
                assert!(state.collective.room_watches[0].wake.is_none());
                assert_eq!(state.runtime.runs.len(), retained + 1);
                state.runtime.runs.truncate(retained);
                state.runtime.notice.clone_from(&notice);
                state.chats[0].messages.truncate(messages);
                state.chats[0].revision = revision;
                state.collective.room_watches[0].clone_from(&watch);
            }
            samples.push(started.elapsed().as_nanos() / iterations as u128);
        }
        assert_eq!(saves.get(), 10 * iterations);
        samples.remove(0);
        samples.sort_unstable();
        println!(
            "room publication: retained_runs={retained}, median_ns={}",
            samples[4]
        );
    }
}

#[test]
#[ignore = "explicit local CPU profile; no models or containers"]
fn profile_idle_and_waiting_room_reconciliation() {
    let (mut state, run) = fixture();
    watch(&mut state, &run, false);
    let template = state.runtime.runs[0].clone();
    for i in 0..20_000 {
        let mut old = template.clone();
        old.id = format!("retained-{i}");
        old.status = "completed".into();
        old.kind = RunKind::Task;
        state.runtime.runs.push(old);
    }
    let armed_iterations = std::env::var("CONTROL_ROOM_PROFILE_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(10, 100_000);
    for (armed, iterations) in [(false, 10_000usize), (true, armed_iterations)] {
        if armed {
            watch(&mut state, &run, true);
        }
        let mut samples = Vec::with_capacity(9);
        for _ in 0..9 {
            let started = std::time::Instant::now();
            for _ in 0..iterations {
                reconcile(std::hint::black_box(&mut state), |_| {
                    panic!("waiting must not checkpoint")
                })
                .unwrap();
            }
            samples.push(started.elapsed().as_nanos() / iterations as u128);
        }
        samples.sort_unstable();
        println!(
            "room reconcile: armed={armed}, retained_runs=20001, median_ns={}",
            samples[4]
        );
    }
}
