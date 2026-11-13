
use super::super::tests::{fixture, post};
use super::*;

fn listen_fixture() -> (State, String) {
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    configure(&mut state, &chat, true).unwrap();
    state.runtime.runs[0].status = "completed".into();
    (state, chat)
}
fn tick(state: &mut State, now: u64) {
    reconcile(state, now, &mut |_| Ok(())).unwrap();
}

#[test]
fn future_human_and_agent_posts_wake_repeatedly_without_self_echoes() {
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    post(&mut state, "research", "old", None);
    configure(&mut state, &chat, true).unwrap();
    state.runtime.runs[0].status = "completed".into();
    post(&mut state, "research", "own post", Some(chat.clone()));
    post(&mut state, "engineering", "unrelated", None);
    tick(&mut state, 100);
    assert_eq!(state.runtime.runs.len(), 1);
    for (now, author) in [(100, None), (130, Some("other-agent".into())), (160, None)] {
        post(&mut state, "research", "relevant update", author);
        let before = state.runtime.runs.len();
        tick(&mut state, now);
        assert_eq!(state.runtime.runs.len(), before + 1);
        let run = state.runtime.runs.last().unwrap();
        assert_eq!(run.source, "room-listener");
        assert_eq!(run.chat_id.as_deref(), Some(chat.as_str()));
        assert_eq!(run.model.as_deref(), Some("captured-model"));
        assert_eq!(run.persona.as_ref().unwrap().root, state.runtime.runs[0].id);
        // An active turn cannot receive a second queued listener response.
        tick(&mut state, now + 1);
        assert_eq!(state.runtime.runs.len(), before + 1);
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        // Inbox acknowledgements are independent of automatic delivery.
        tick(&mut state, now + 30);
        assert_eq!(state.runtime.runs.len(), before + 1);
        state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    }
    assert_eq!(status(&state, &chat).unwrap()["state"], "listening");
}

#[test]
fn recorded_updates_do_not_spend_listener_turns_or_acknowledge_history() {
    let (mut state, _) = listen_fixture();
    let cursor = state.collective.room_watches[0].cursor;
    for _ in 0..BATCH + 1 {
        post(
            &mut state,
            "research",
            "Retained audit",
            Some("peer".into()),
        );
        state.collective.posts.last_mut().unwrap().delivery = crate::rooms::Delivery::Record;
    }
    let before = state.runtime.runs.len();
    assert!(reconcile(&mut state, 100, &mut |_| Err(std::io::Error::other("disk"))).is_err());
    assert_eq!(
        state.collective.room_watches[0]
            .listener
            .as_ref()
            .unwrap()
            .through,
        cursor
    );
    for _ in 0..3 {
        tick(&mut state, 100);
    }
    assert_eq!(state.runtime.runs.len(), before);
    assert_eq!(state.collective.room_watches[0].cursor, cursor);
    assert_eq!(
        batch(&state.collective.room_watches[0], &state.collective.posts)
            .posts
            .len(),
        BATCH
    );
    state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    post(&mut state, "research", "Human asks for a decision", None);
    tick(&mut state, 100);
    assert_eq!(state.runtime.runs.len(), before + 1);
    assert_eq!(state.collective.room_watches[0].cursor, cursor);
}

#[test]
fn batching_cooldown_and_queue_capacity_leave_later_posts_pending() {
    let (mut state, _) = listen_fixture();
    for i in 0..20 {
        post(&mut state, "research", &format!("update {i}"), None);
    }
    tick(&mut state, 100);
    assert_eq!(
        state.collective.room_watches[0]
            .listener
            .as_ref()
            .unwrap()
            .through,
        8
    );
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    tick(&mut state, 129);
    assert_eq!(state.runtime.runs.len(), 2);
    tick(&mut state, 130);
    assert_eq!(
        state.collective.room_watches[0]
            .listener
            .as_ref()
            .unwrap()
            .through,
        16
    );
    let next = state.runtime.runs.last_mut().unwrap();
    next.status = "completed".into();
    // The regular queue gate remains authoritative.
    let limit = crate::scheduler::MAX_PENDING;
    for _ in 0..limit {
        state
            .runtime
            .enqueue_for(
                "busy".into(),
                RunKind::Chat,
                "test",
                identity::Actor::LocalOperator,
                identity::Action::Task,
            )
            .unwrap();
    }
    let before = state.runtime.runs.len();
    tick(&mut state, 160);
    assert_eq!(state.runtime.runs.len(), before);
    assert_eq!(
        state.collective.room_watches[0]
            .listener
            .as_ref()
            .unwrap()
            .through,
        16
    );
}

#[test]
fn failed_checkpoint_rolls_back_delivery_and_pause() {
    let (mut state, chat) = listen_fixture();
    post(&mut state, "research", "hello", None);
    let before = serde_json::to_value(&state).unwrap();
    let fail = &mut |_: &State| Err(std::io::Error::other("save failure"));
    assert!(reconcile(&mut state, 100, fail).is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    state.runtime.runs[0].status = "failed".into();
    let before = serde_json::to_value(&state).unwrap();
    assert!(reconcile(&mut state, 100, fail).is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    let revision = state.chats[0].revision;
    tick(&mut state, 100);
    assert_eq!(status(&state, &chat).unwrap()["state"], "paused");
    assert!(state.chats[0].revision > revision);
}

#[test]
fn owner_can_stop_a_queued_followup_and_models_cannot_replace_intent() {
    let (mut state, chat) = listen_fixture();
    post(&mut state, "research", "hello", None);
    tick(&mut state, 100);
    state.runtime.runs.last_mut().unwrap().status = "running".into();
    let run = state.runtime.runs.last().unwrap().id.clone();
    for args in [
        json!({"enabled":false}),
        json!({"enabled":true,"wake":true}),
    ] {
        assert!(super::super::call(&mut state, &run, "control_room_watch", &args).is_err());
    }
    configure(&mut state, &chat, false).unwrap();
    assert_eq!(state.runtime.runs.last().unwrap().status, "cancelling");
    assert!(status(&state, &chat).is_none());
    assert!(state.collective.room_watches.is_empty());
    post(&mut state, "research", "later", None);
    tick(&mut state, 200);
    assert_eq!(state.runtime.runs.len(), 2);
}

#[test]
fn child_lineage_is_preserved_and_changed_room_pauses() {
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    let mut persona = crate::delegation::lineage(&state.runtime.runs[0]);
    persona.depth = 2;
    persona.parent = Some("parent".into());
    state.runtime.runs[0].persona = Some(persona.clone());
    configure(&mut state, &chat, true).unwrap();
    state.runtime.runs[0].status = "completed".into();
    post(
        &mut state,
        "research",
        "other bot",
        Some("other-agent".into()),
    );
    tick(&mut state, 100);
    assert_eq!(state.runtime.runs.last().unwrap().persona, Some(persona));
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    state.chats[0].room = Some("engineering".into());
    tick(&mut state, 200);
    assert_eq!(status(&state, &chat).unwrap()["state"], "paused");
    assert_eq!(state.runtime.runs.len(), 2);
}

#[test]
fn chat_command_controls_listener_without_spending_a_model_turn() {
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    crate::chat::send(&mut state, &chat, "/listen", "test").unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    assert_eq!(
        crate::chat::detail(&state, &chat).unwrap()["room_listener"]["state"],
        "listening"
    );
    assert!(
        crate::chat::transcript(&state, &chat)
            .unwrap()
            .contains("Room #research · listening")
    );
    crate::chat::send(&mut state, &chat, "/listen off", "test").unwrap();
    assert!(status(&state, &chat).is_none());
    assert_eq!(state.runtime.runs.len(), 1);
}

#[test]
fn acknowledged_posts_during_cooldown_do_not_trigger_redundant_turns() {
    let (mut state, _) = listen_fixture();
    post(&mut state, "research", "first message", None);
    tick(&mut state, 100);
    let run = state.runtime.runs.last_mut().unwrap();
    run.status = "running".into();
    let id = run.id.clone();
    post(
        &mut state,
        "research",
        "message handled by active turn",
        Some("teammate".into()),
    );
    let inbox =
        super::super::call(&mut state, &id, "control_room_notifications", &json!({})).unwrap();
    assert_eq!(inbox["posts"].as_array().unwrap().len(), 2);
    super::super::call(
        &mut state,
        &id,
        "control_room_ack",
        &json!({"through":inbox["through"]}),
    )
    .unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    tick(&mut state, 130);
    assert_eq!(state.runtime.runs.len(), 2);
    post(&mut state, "research", "new unprocessed message", None);
    tick(&mut state, 160);
    assert_eq!(state.runtime.runs.len(), 3);
}

#[test]
fn all_ready_chats_progress_in_one_pass_without_retaining_origin_runs() {
    let (mut state, first) = listen_fixture();
    let second = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().room = Some("research".into());
    crate::chat::send(&mut state, &second, "Coordinate with the team", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    configure(&mut state, &second, true).unwrap();
    post(&mut state, "research", "new assignment", None);
    tick(&mut state, 100);
    assert_eq!(state.runtime.runs.len(), 4);
    for chat in [&first, &second] {
        assert_eq!(
            state
                .runtime
                .runs
                .iter()
                .filter(|run| run.source == "room-listener" && run.chat_id.as_ref() == Some(chat))
                .count(),
            1
        );
    }
    state
        .runtime
        .runs
        .retain(|run| run.source == "room-listener");
    for run in &mut state.runtime.runs {
        run.status = "completed".into();
    }
    post(
        &mut state,
        "research",
        "another assignment",
        Some("teammate".into()),
    );
    tick(&mut state, 130);
    assert_eq!(state.runtime.runs.len(), 4);
    assert_eq!(status(&state, &first).unwrap()["state"], "listening");
    assert_eq!(status(&state, &second).unwrap()["state"], "listening");
}

#[test]
#[ignore = "explicit local listener CPU measurement; no models or containers"]
fn profile_listener_reconciliation() {
    let (mut state, _) = listen_fixture();
    let template = state.runtime.runs[0].clone();
    for index in 0..20_000 {
        let mut old = template.clone();
        old.id = format!("old-{index}");
        old.chat_id = None;
        state.runtime.runs.push(old);
    }
    let started = std::time::Instant::now();
    for _ in 0..100 {
        reconcile(std::hint::black_box(&mut state), 100, &mut |_| {
            panic!("idle listener must not save")
        })
        .unwrap();
    }
    eprintln!(
        "Idle listener / 20,001 retained runs: {:?} per reconcile",
        started.elapsed() / 100
    );
}
