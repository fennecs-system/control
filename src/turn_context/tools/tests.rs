
use super::*;
#[test]
fn approval_removes_only_the_implicit_deadline_and_context_reports_it() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "budget",
    };
    let initial = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| {
        Ok(())
    })
    .unwrap();
    assert_eq!(initial["budget"]["limit_seconds"], 600);
    crate::bots::command(&mut state, &chat, "on").unwrap();
    let unlimited = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| {
        Ok(())
    })
    .unwrap();
    assert!(unlimited["budget"]["remaining_seconds"].is_null());
    state.runtime.runs[0].attempts[0].explicit_timeout_seconds = Some(1200);
    let bounded = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| {
        Ok(())
    })
    .unwrap();
    assert_eq!(bounded["budget"]["limit_seconds"], 1200);
}
#[test]
fn empty_receipt_remains_a_valid_delta_cursor() {
    let (mut state, id, generation) = fixture();
    let page = handle(
        &mut state,
        &AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "first",
        },
        "control_turn_context",
        &json!({}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(page["events"].as_array().unwrap().is_empty());
    let chat = state.chats[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer A new instruction", "test").unwrap();
    let next = handle(
        &mut state,
        &AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "next",
        },
        "control_turn_context",
        &json!({"after_receipt":page["receipt"]}),
        |_| Ok(()),
    );
    assert!(
        next.is_ok(),
        "The previous empty delta receipt cannot be resumed: {next:?}"
    );
}
#[test]
fn linking_room_after_bot_start_does_not_deliver_old_history() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    state.chats[0].room = None;
    state.chats[0].continuity = None;
    crate::bots::command(&mut state, &chat, "on every 5").unwrap();
    post(
        &mut state,
        "Old playtest instructions from before subscription",
    );
    let response = crate::rooms::checkpoint(
        &mut state,
        &id,
        &json!({"nonce":"123-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":"Joining this test"}}}}),
        |_| Ok(()),
    );
    assert!(response.get("error").is_none(), "{response}");
    assert!(crate::rooms::inbox_cursor(&state, &chat).unwrap() > 0);
    let page = handle(
        &mut state,
        &AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "read-room",
        },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        page["events"].as_array().unwrap().is_empty(),
        "Old history entered the live callback stream: {page}"
    );
}
#[test]
fn empty_context_reads_reuse_one_durable_cursor() {
    let (mut state, id, generation) = fixture();
    for index in 0..=control_context::turn_mailbox::MAX_DELIVERIES {
        let result = handle(
            &mut state,
            &AttemptRef {
                run: &id,
                generation: &generation,
                nonce: &format!("read-{index}"),
            },
            "control_turn_context",
            &json!({}),
            |_| Ok(()),
        );
        assert!(
            result.is_ok(),
            "Empty context read {index} exhausted the session: {result:?}"
        );
        assert!(result.unwrap()["events"].as_array().unwrap().is_empty());
    }
    assert_eq!(
        state.runtime.runs[0].attempts[0]
            .turn_mailbox
            .as_ref()
            .unwrap()
            .deliveries
            .len(),
        1
    );
}
#[test]
fn fresh_reads_default_to_undelivered_deltas_and_retries_replay_their_range() {
    let (mut state, id, generation) = fixture();
    post(&mut state, "First event");
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "first",
    };
    let first = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    post(&mut state, "Second event");
    let second = handle(
        &mut state,
        &AttemptRef {
            nonce: "second",
            ..cx
        },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(second["events"].as_array().unwrap().len(), 1);
    assert_eq!(second["events"][0]["text"], "Second event");
    let retry = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(retry["events"], first["events"]);
    assert_eq!(retry["receipt"], first["receipt"]);
    let empty = handle(
        &mut state,
        &AttemptRef {
            nonce: "empty",
            ..cx
        },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(empty["events"].as_array().unwrap().is_empty());
    assert!(
        state.chats[0]
            .continuity
            .as_ref()
            .unwrap()
            .working
            .is_none(),
        "delivery does not commit processing"
    );
}
#[test]
fn bot_activation_skips_an_old_inbox_but_reconfiguration_and_reload_keep_new_events() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    crate::rooms::ensure_inbox(&mut state, &chat).unwrap();
    post(&mut state, "Earlier playtest");
    crate::bots::command(&mut state, &chat, "on every 5").unwrap();
    assert_eq!(crate::rooms::inbox_cursor(&state, &chat), Some(1));
    post(&mut state, "New actionable event");
    crate::bots::command(&mut state, &chat, "on every 10").unwrap();
    let mut state: State = serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
    let page = handle(
        &mut state,
        &AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "new-events",
        },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["events"][0]["text"], "New actionable event");
    assert!(
        crate::rooms::read(&state, "general")["posts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["text"] == "Earlier playtest")
    );
}
#[test]
fn failed_late_link_restores_the_room_origin_and_active_mailbox() {
    let (mut state, id, _) = fixture();
    state.chats[0].room = None;
    post(&mut state, "Earlier playtest");
    let before = serde_json::to_value(&state).unwrap();
    let response = crate::rooms::checkpoint(
        &mut state,
        &id,
        &json!({"nonce":"123-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":"Joining"}}}}),
        |_| Err(err("disk full")),
    );
    assert!(response.get("error").is_some());
    assert_eq!(serde_json::to_value(state).unwrap(), before);
}
#[test]
fn failed_bot_activation_restores_the_active_callback_boundary() {
    let (mut state, _, _) = fixture();
    let chat = state.chats[0].id.clone();
    post(&mut state, "Before bot activation");
    let before = serde_json::to_value(&state).unwrap();
    let checkpoint =
        crate::chat::Checkpoint::capture(&state, &std::collections::HashSet::from([chat.as_str()]));
    crate::bots::command(&mut state, &chat, "on every 5").unwrap();
    checkpoint.restore(&mut state);
    assert_eq!(serde_json::to_value(state).unwrap(), before);
}
#[test]
fn earlier_page_cursors_cannot_cross_a_fresh_subscription_origin() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    post(&mut state, "Earlier event already delivered");
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "earlier",
    };
    let earlier = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    post(&mut state, "Old noise before activation");
    crate::bots::command(&mut state, &chat, "on every 5").unwrap();
    post(&mut state, "New event after activation");
    let page = handle(
        &mut state,
        &AttemptRef {
            nonce: "fresh",
            ..cx
        },
        "control_turn_context",
        &json!({"source":"room","after_receipt":earlier["receipt"]}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["events"][0]["text"], "New event after activation");
}
#[test]
fn unadvertised_context_calls_on_a_legacy_attempt_fail_without_upgrading_it() {
    let (mut state, id, generation) = fixture();
    state.chats[0].continuity = None;
    state.runtime.runs[0].attempts[0].turn_mailbox = None.into();
    let before = serde_json::to_value(&state).unwrap();
    assert!(
        handle(
            &mut state,
            &AttemptRef {
                run: &id,
                generation: &generation,
                nonce: "legacy"
            },
            "control_turn_context",
            &json!({}),
            |_| Ok(())
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(state).unwrap(), before);
}
fn fixture() -> (State, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    state.chats[0].continuity = Some(Box::new(Managed::default()));
    crate::chat::send(&mut state, &chat, "Keep building toward the goal", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    let id = run.id.clone();
    let generation = format!("{id}-attempt-1");
    let mut mailbox = Mailbox::default();
    mailbox.baseline.push(Range::Conversation {
        chat: chat.clone(),
        after: 0,
        through: 1,
    });
    run.attempts.push(Attempt {
        workspace_id: Some(generation.clone()),
        turn_mailbox: Some(Box::new(mailbox)).into(),
        ..Attempt::new()
    });
    (state, id, generation)
}
fn post(state: &mut State, text: &str) {
    state.collective.posts.push(crate::rooms::Post {
        room: "general".into(),
        text: text.into(),
        chat: Some("peer".into()),
        run: None,
        key: String::new(),
        delivery: Default::default(),
    });
}
#[test]
fn recorded_room_updates_are_readable_without_callback_hints() {
    let (mut state, id, generation) = fixture();
    post(&mut state, "Retained observation, no action required");
    state.collective.posts.last_mut().unwrap().delivery = crate::rooms::Delivery::Record;
    assert!(!callback_notice(&mut state, &id));
    let page = handle(
        &mut state,
        &AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "read-record",
        },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(page["events"][0]["delivery"], "record");
    assert_eq!(
        page["events"][0]["text"],
        "Retained observation, no action required"
    );
    post(&mut state, "Dependency is ready");
    assert!(callback_notice(&mut state, &id));
}

#[test]
fn callback_hints_are_once_per_change_and_do_not_acknowledge_processing() {
    let (mut state, id, generation) = fixture();
    assert!(!callback_notice(&mut state, &id));
    let chat = state.chats[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer Keep the public API", "test").unwrap();
    assert!(callback_notice(&mut state, &id));
    assert!(!callback_notice(&mut state, &id));
    handle(
        &mut state,
        &AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "callbacks",
        },
        "control_turn_context",
        &json!({}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(!callback_notice(&mut state, &id));
    assert!(
        state.chats[0]
            .continuity
            .as_ref()
            .unwrap()
            .working
            .is_none()
    );
    post(&mut state, "A peer has new evidence");
    assert!(callback_notice(&mut state, &id));
    assert!(!callback_notice(&mut state, &id));
}
#[test]
fn unicode_pages_stay_within_the_byte_budget_and_oversize_data_requires_resynchronisation() {
    let (mut state, id, generation) = fixture();
    post(&mut state, &"🐶".repeat(2000));
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "unicode",
    };
    let page = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(serde_json::to_vec(&page["events"]).unwrap().len() <= 8 * 1024);
    // Metadata may tip a maximum-sized multibyte post over the byte cap;
    // larger retained sources must never be silently acknowledged.
    post(&mut state, &"🐶".repeat(2500));
    let second = handle(
        &mut state,
        &AttemptRef {
            nonce: "oversize",
            ..cx
        },
        "control_turn_context",
        &json!({"source":"room","after_receipt":page["receipt"]}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(second["gap"], true);
    let mailbox = state.runtime.runs[0].attempts[0]
        .turn_mailbox
        .as_ref()
        .unwrap();
    let receipt = second["receipt"].as_str().unwrap();
    assert!(mailbox.deliveries[receipt].gap);
    assert!(second["events"][0]["unavailable"].is_string());
}
#[test]
fn callbacks_arrive_in_the_active_attempt_and_reads_never_acknowledge_them() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer Preserve the public API", "test").unwrap();
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "read-1",
    };
    let response = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| {
        Ok(())
    })
    .unwrap();
    assert!(
        response["events"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Preserve the public API")
    );
    assert_eq!(state.runtime.runs[0].status, "running");
    assert!(
        state.chats[0]
            .continuity
            .as_ref()
            .unwrap()
            .working
            .is_none()
    );
    assert_eq!(
        state.runtime.runs.len(),
        2,
        "context read creates no replacement session"
    );
    assert_eq!(state.runtime.runs[0].attempts.len(), 1);
}
#[test]
fn pagination_is_oldest_first_bounded_and_replays_do_not_claim_newer_events() {
    let (mut state, id, generation) = fixture();
    for i in 0..40 {
        post(&mut state, &format!("peer update {i}"));
    }
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "page-1",
    };
    let first = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), 16);
    assert_eq!(first["events"][0]["text"], "peer update 0");
    assert_eq!(first["through"], 16);
    post(&mut state, "racing arrival");
    let replay = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| panic!("replay must not save again"),
    )
    .unwrap();
    assert_eq!(replay["events"], first["events"]);
    assert_eq!(replay["receipt"], first["receipt"]);
    assert!(
        handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(
            ()
        ))
        .is_err()
    );
    let second = handle(
        &mut state,
        &AttemptRef {
            nonce: "page-2",
            ..cx
        },
        "control_turn_context",
        &json!({"source":"room","after_receipt":first["receipt"]}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(second["events"][0]["text"], "peer update 16");
    let attempt = state.runtime.runs[0].attempts.last().unwrap();
    assert!(attempt.turn_mailbox.as_ref().unwrap().accepted().is_none());
}
#[test]
fn failed_receipt_save_and_failed_handoff_save_leave_no_processing_or_fence() {
    let (mut state, id, generation) = fixture();
    post(&mut state, "peer update");
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "page",
    };
    let before = serde_json::to_value(&state).unwrap();
    assert!(
        handle(
            &mut state,
            &cx,
            "control_turn_context",
            &json!({"source":"room"}),
            |_| Err(err("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    let page = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    let handoff = json!({"base_revision":0,"payload":{"subgoal":"Next useful action","findings":[],"tried":[],"pending":[],"next":[]},"processed":[page["receipt"]],"disposition":{"kind":"continue"},"progress":null});
    let before = serde_json::to_value(&state).unwrap();
    assert!(
        handle(
            &mut state,
            &AttemptRef {
                nonce: "draft",
                ..cx
            },
            "control_turn_handoff",
            &handoff,
            |_| Err(err("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    assert!(
        handle(
            &mut state,
            &AttemptRef {
                generation: "stale",
                ..cx
            },
            "control_turn_handoff",
            &handoff,
            |_| Ok(())
        )
        .is_err()
    );
    handle(
        &mut state,
        &AttemptRef {
            nonce: "draft",
            ..cx
        },
        "control_turn_handoff",
        &handoff,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        state.chats[0]
            .continuity
            .as_ref()
            .unwrap()
            .working
            .is_none(),
        "staging does not publish"
    );
    assert!(
        state.runtime.runs[0].attempts[0]
            .turn_mailbox
            .as_ref()
            .unwrap()
            .accepted()
            .is_some()
    );
}
