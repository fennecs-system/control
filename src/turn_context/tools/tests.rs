use super::*;
use crate::runtime::RunStatus;
#[test]
fn approval_removes_only_the_implicit_deadline_and_context_reports_it() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "budget",
    };
    let initial = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(())).unwrap();
    assert_eq!(initial["budget"]["limit_seconds"], 600);
    crate::bots::command(&mut state, &chat, "on").unwrap();
    let unlimited = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(())).unwrap();
    assert!(unlimited["budget"]["remaining_seconds"].is_null());
    state.runtime.runs[0].attempts[0].explicit_timeout_seconds = Some(1200);
    let bounded = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(())).unwrap();
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
    post(&mut state, "Old playtest instructions from before subscription");
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
        &AttemptRef { nonce: "second", ..cx },
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
        &AttemptRef { nonce: "empty", ..cx },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(empty["events"].as_array().unwrap().is_empty());
    assert!(
        state.chats[0].continuity.as_ref().unwrap().working.is_none(),
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
    assert!(crate::toolbox::failure_text(&response).is_some(), "{response}");
    assert_eq!(serde_json::to_value(state).unwrap(), before);
}
#[test]
fn failed_bot_activation_restores_the_active_callback_boundary() {
    let (mut state, _, _) = fixture();
    let chat = state.chats[0].id.clone();
    post(&mut state, "Before bot activation");
    let before = serde_json::to_value(&state).unwrap();
    let checkpoint = crate::chat::Checkpoint::capture(&state, &std::collections::HashSet::from([chat.as_str()]));
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
        &AttemptRef { nonce: "fresh", ..cx },
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
    run.status = RunStatus::Running;
    let id = run.id().to_string();
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
    assert!(!callback_notice(&mut state, &id, true));
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
    assert_eq!(page["events"][0]["text"], "Retained observation, no action required");
    post(&mut state, "Dependency is ready");
    assert!(callback_notice(&mut state, &id, true));
}

#[test]
fn callback_hints_are_once_per_change_and_do_not_acknowledge_processing() {
    let (mut state, id, generation) = fixture();
    assert!(!callback_notice(&mut state, &id, true));
    let chat = state.chats[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer Keep the public API", "test").unwrap();
    assert!(callback_notice(&mut state, &id, true));
    assert!(!callback_notice(&mut state, &id, true));
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
    assert!(!callback_notice(&mut state, &id, true));
    assert!(state.chats[0].continuity.as_ref().unwrap().working.is_none());
    post(&mut state, "A peer has new evidence");
    assert!(callback_notice(&mut state, &id, true));
    assert!(!callback_notice(&mut state, &id, true));
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
    let mailbox = state.runtime.runs[0].attempts[0].turn_mailbox.as_ref().unwrap();
    let receipt = resolve(&mailbox.deliveries, second["receipt"].as_str().unwrap());
    assert!(mailbox.deliveries[&receipt].gap);
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
    let response = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(())).unwrap();
    assert!(
        response["events"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Preserve the public API")
    );
    assert_eq!(state.runtime.runs[0].status, RunStatus::Running);
    assert!(state.chats[0].continuity.as_ref().unwrap().working.is_none());
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
    assert!(handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(())).is_err());
    let second = handle(
        &mut state,
        &AttemptRef { nonce: "page-2", ..cx },
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
            &AttemptRef { nonce: "draft", ..cx },
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
        &AttemptRef { nonce: "draft", ..cx },
        "control_turn_handoff",
        &handoff,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        state.chats[0].continuity.as_ref().unwrap().working.is_none(),
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
#[test]
fn rejected_handoffs_are_remembered_for_the_recovery_wake() {
    let (mut state, id, generation) = fixture();
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "draft",
    };
    let rejected = |state: &State| {
        state.chats[0]
            .continuity
            .as_ref()
            .unwrap()
            .checkpointing
            .rejected
            .clone()
    };
    // Invalid notes are the host's refusal, kept with its attempt. (An unread
    // receipt is no longer one: it is left pending and reported.)
    let handoff = json!({"base_revision":0,"payload":{"subgoal":" ","findings":[],"tried":[],"pending":[],"next":[]},"processed":["never-delivered"],"disposition":{"kind":"continue"},"progress":null});
    let error = handle(&mut state, &cx, "control_turn_handoff", &handoff, |_| Ok(()))
        .unwrap_err()
        .to_string();
    let rejection = rejected(&state).unwrap();
    assert_eq!(rejection.generation, generation);
    assert_eq!(rejection.error, error);
    // A storage failure is not a rejected receipt, and reads record nothing.
    state.chats[0].continuity.as_mut().unwrap().checkpointing = Default::default();
    let page = handle(&mut state, &cx, "control_turn_context", &json!({}), |_| Ok(())).unwrap();
    let valid = json!({"base_revision":0,"payload":{"subgoal":"Next useful action","findings":[],"tried":[],"pending":[],"next":[]},"processed":[page["receipt"]],"disposition":{"kind":"continue"},"progress":null});
    assert!(
        handle(&mut state, &cx, "control_turn_handoff", &valid, |_| Err(err(
            "disk full"
        )))
        .is_err()
    );
    assert!(
        handle(
            &mut state,
            &cx,
            "control_turn_context",
            &json!({"source":"bogus"}),
            |_| Ok(())
        )
        .is_err()
    );
    assert_eq!(rejected(&state), None);
    // A stale attempt cannot write another attempt's record.
    let stale = AttemptRef {
        generation: "stale",
        ..cx
    };
    assert!(handle(&mut state, &stale, "control_turn_handoff", &handoff, |_| Ok(())).is_err());
    assert_eq!(rejected(&state), None);
    // Long errors are bounded.
    reject(&mut state, &cx, &"x".repeat(5_000));
    assert_eq!(rejected(&state).unwrap().error.chars().count(), REJECTION + 1);
}
fn hand_off(state: &mut State, id: &str, generation: &str, nonce: &str, args: Value) -> io::Result<Value> {
    handle(
        state,
        &AttemptRef {
            run: id,
            generation,
            nonce,
        },
        "control_turn_handoff",
        &args,
        |_| Ok(()),
    )
}
fn read_room(state: &mut State, id: &str, generation: &str, nonce: &str) -> Value {
    handle(
        state,
        &AttemptRef {
            run: id,
            generation,
            nonce,
        },
        "control_turn_context",
        &json!({"source":"room"}),
        |_| Ok(()),
    )
    .unwrap()
}
#[test]
fn only_a_disposition_is_required_and_wait_topics_are_normalised_not_refused() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    crate::bots::command(&mut state, &chat, "on").unwrap();
    let all = crate::bots::wait_topics(&state, &chat);
    assert!(
        all.contains(&"room:general".to_owned()) && all.contains(&format!("conversation:{chat}")),
        "{all:?}"
    );
    // A bare string disposition, with no receipts, notes or revision.
    let done = hand_off(&mut state, &id, &generation, "done", json!({"disposition":"done"})).unwrap();
    assert_eq!(done["disposition"]["kind"], "done");
    assert_eq!(done["adjusted"].as_array().unwrap().len(), 1, "{done}"); // the placeholder notes
    // A #name and a bare name are read as subscriptions; unknown ones are dropped.
    let wait = hand_off(
        &mut state,
        &id,
        &generation,
        "wait",
        json!({"disposition":{"kind":"wait","topics":["#general","nope"],"recheck_seconds":0}}),
    )
    .unwrap();
    assert_eq!(wait["disposition"]["topics"], json!(["room:general"]), "{wait}");
    assert!(wait["disposition"]["recheck_seconds"].is_null());
    let adjusted = wait["adjusted"].to_string();
    assert!(
        adjusted.contains("read #general as room:general")
            && adjusted.contains("dropped nope")
            && adjusted.contains("recheck_seconds 0"),
        "{adjusted}"
    );
    // A recheck is a timer: dropped, and said so, unless the team selected the gene.
    let dropped = hand_off(
        &mut state,
        &id,
        &generation,
        "no-gene",
        json!({"disposition":{"kind":"wait","topics":[],"recheck_seconds":300}}),
    )
    .unwrap();
    assert!(dropped["disposition"]["recheck_seconds"].is_null(), "{dropped}");
    assert!(
        dropped["adjusted"]
            .to_string()
            .contains("did not select the timers gene"),
        "{dropped}"
    );
    state.chats[0].lenses = Some(vec!["timers".into()]);
    let kept = hand_off(
        &mut state,
        &id,
        &generation,
        "gene",
        json!({"disposition":{"kind":"wait","topics":[],"recheck_seconds":300}}),
    )
    .unwrap();
    assert!(!kept.to_string().contains("timers gene"), "kept as asked: {kept}");
    state.chats[0].lenses = None;
    // Nothing matches, or nothing is named: keep every subscription.
    for (nonce, disposition) in [
        ("none", json!({"kind":"wait","topics":["elsewhere"]})),
        ("omitted", json!({"kind":"wait"})),
    ] {
        let widest = hand_off(&mut state, &id, &generation, nonce, json!({"disposition":disposition})).unwrap();
        let topics: Vec<String> = serde_json::from_value(widest["disposition"]["topics"].clone()).unwrap();
        assert_eq!(topics.len(), all.len(), "{widest}");
        assert!(
            widest["adjusted"].to_string().contains("every current subscription"),
            "{widest}"
        );
    }
    // An explicit empty selection keeps its meaning: urgent failures and direct messages only.
    let quiet = hand_off(
        &mut state,
        &id,
        &generation,
        "quiet",
        json!({"disposition":{"kind":"wait","topics":[]}}),
    )
    .unwrap();
    // Nothing was adjusted, so the result does not echo the request back.
    assert!(
        quiet.get("disposition").is_none() && quiet.get("adjusted").is_none(),
        "{quiet}"
    );
    let staged = &state.runtime.runs[0].attempts[0]
        .turn_mailbox
        .as_ref()
        .unwrap()
        .accepted()
        .unwrap()
        .disposition;
    assert_eq!(json!(staged)["topics"], json!([]));
    // A stale base revision cannot race this attempt: used and reported.
    let stale = hand_off(
        &mut state,
        &id,
        &generation,
        "stale",
        json!({"base_revision":7,"disposition":{"kind":"continue"}}),
    )
    .unwrap();
    assert!(
        stale["adjusted"]
            .to_string()
            .contains("base_revision 7 is not the committed revision; used 0"),
        "{stale}"
    );
    // What cannot be honoured is refused with a callable fix.
    let error = hand_off(
        &mut state,
        &id,
        &generation,
        "sleep",
        json!({"disposition":{"kind":"sleep"}}),
    )
    .unwrap_err()
    .to_string();
    assert!(crate::worker_tools::fixes::names_fix(&error), "{error}");
    // Corrections never exhaust a budget; the last accepted draft stands.
    for index in 0..12 {
        hand_off(
            &mut state,
            &id,
            &generation,
            &format!("correction-{index}"),
            json!({"disposition":"continue","progress":format!("try {index}")}),
        )
        .unwrap();
    }
    let mailbox = state.runtime.runs[0].attempts[0].turn_mailbox.as_ref().unwrap();
    assert!(mailbox.drafts.len() <= 8);
    assert_eq!(mailbox.accepted().unwrap().progress.as_deref(), Some("try 11"));
}
#[test]
fn a_wait_on_a_pending_port_request_sleeps_until_its_decision() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    crate::bots::command(&mut state, &chat, "on").unwrap();
    let port = |status: &str| {
        serde_json::from_value::<crate::ports::Port>(json!({
        "id":"port-1","service":"svc-a","generation":1,"container":"b".repeat(64),"node":"node-a","chat":chat,
        "actor":{"type":"local_operator"},"origin_run":id,"origin_attempt":generation,"name":"download","direction":"egress",
        "service_port":3128,"target":{"host":"files.example","port":443},"status":status,"approval":null,"diagnostic":""})).unwrap()
    };
    state.ports.push(port("pending"));
    // Only the port is named: an explicit wait, not every subscription.
    let wait = hand_off(
        &mut state,
        &id,
        &generation,
        "port",
        json!({"disposition":{"kind":"wait","topics":["port:port-1"]}}),
    )
    .unwrap();
    assert_eq!(wait["disposition"]["topics"], json!([]), "{wait}");
    assert!(
        wait["adjusted"]
            .to_string()
            .contains("port:port-1: the operator's decision starts this chat's next turn"),
        "{wait}"
    );
    // With other topics it narrows to them; an unknown request is dropped.
    let both = hand_off(
        &mut state,
        &id,
        &generation,
        "both",
        json!({"disposition":{"kind":"wait","topics":["port:port-1","port:nope","#general"]}}),
    )
    .unwrap();
    assert_eq!(both["disposition"]["topics"], json!(["room:general"]), "{both}");
    assert!(both["adjusted"].to_string().contains("dropped port:nope"), "{both}");
    // Already decided: act on it now.
    state.ports[0] = port("approved");
    let decided = hand_off(
        &mut state,
        &id,
        &generation,
        "decided",
        json!({"disposition":{"kind":"wait","topics":["port:port-1"]}}),
    )
    .unwrap();
    assert_eq!(decided["disposition"]["kind"], "continue", "{decided}");
    assert!(
        decided["adjusted"].to_string().contains("already approved"),
        "{decided}"
    );
}
#[test]
fn without_continuation_a_wait_on_a_decided_port_keeps_the_turn_going() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    assert!(!crate::bots::continuation_enabled(&state, &chat));
    let port = |status: &str| {
        serde_json::from_value::<crate::ports::Port>(json!({
        "id":"port-1","service":"svc-a","generation":1,"container":"b".repeat(64),"node":"node-a","chat":chat,
        "actor":{"type":"local_operator"},"origin_run":id,"origin_attempt":generation,"name":"download","direction":"egress",
        "service_port":3128,"target":{"host":"files.example","port":443},"status":status,"approval":null,"diagnostic":""})).unwrap()
    };
    // A pending request sleeps: its decision starts the next turn.
    state.ports.push(port("pending"));
    hand_off(
        &mut state,
        &id,
        &generation,
        "pending",
        json!({"disposition":{"kind":"wait","topics":["port:port-1"]}}),
    )
    .unwrap();
    // Decided already: a recorded continue would start no turn, and the
    // decision may have been delivered, so the agent is told to act now.
    state.ports[0] = port("approved");
    let refused = hand_off(
        &mut state,
        &id,
        &generation,
        "stranded",
        json!({"disposition":{"kind":"wait","topics":["port:port-1","#general"]}}),
    )
    .unwrap_err()
    .to_string();
    assert!(
        refused.contains("already approved") && refused.contains("act on it now in this turn"),
        "{refused}"
    );
    assert!(crate::worker_tools::fixes::names_fix(&refused), "{refused}");
}
#[test]
fn malformed_handoffs_name_the_field_to_fix() {
    let (mut state, id, generation) = fixture();
    let missing = hand_off(&mut state, &id, &generation, "missing", json!({"progress":"x"}))
        .unwrap_err()
        .to_string();
    assert!(missing.contains("Expected a handoff with a disposition"), "{missing}");
    // A payload missing its required subgoal is named rather than blamed on the disposition.
    let payload = hand_off(
        &mut state,
        &id,
        &generation,
        "payload",
        json!({"disposition":"continue","payload":{"findings":[]}}),
    )
    .unwrap_err()
    .to_string();
    assert!(
        payload.contains("payload") && payload.contains("missing field") && !payload.contains("Expected a handoff"),
        "{payload}"
    );
    let unknown = hand_off(
        &mut state,
        &id,
        &generation,
        "unknown",
        json!({"disposition":"continue","summary":"x"}),
    )
    .unwrap_err()
    .to_string();
    assert!(
        unknown.contains("arguments did not match") && unknown.contains("summary"),
        "{unknown}"
    );
    for error in [missing, payload, unknown] {
        assert!(crate::worker_tools::fixes::names_fix(&error), "{error}");
    }
}
#[test]
fn omitted_receipts_process_what_was_delivered_and_explicit_ones_exactly_those() {
    let (mut state, id, generation) = fixture();
    post(&mut state, "first");
    post(&mut state, "second");
    let first = read_room(&mut state, &id, &generation, "r1");
    post(&mut state, "third");
    let second = read_room(&mut state, &id, &generation, "r2");
    let publish = |state: &State| {
        state.runtime.runs[0].attempts[0]
            .turn_mailbox
            .as_ref()
            .unwrap()
            .publication(None, &id, &generation, "cp", |_| true)
            .unwrap()
            .unwrap()
            .processed
    };
    let room = |processed: Vec<Range>| {
        processed.iter().find_map(|r| match r {
            Range::Room { through, .. } => Some(*through),
            _ => None,
        })
    };
    // Partial: exactly the listed receipt.
    // Results show receipt handles; the handle names the delivery.
    assert_eq!(first["receipt"].as_str().unwrap().len(), RECEIPT_HANDLE);
    let partial = hand_off(
        &mut state,
        &id,
        &generation,
        "partial",
        json!({"processed":[first["receipt"]],"disposition":"continue"}),
    )
    .unwrap();
    assert_eq!(partial["processed"], 1);
    assert_eq!(room(publish(&state)), Some(2));
    // The full receipt is accepted too and processes the same delivery.
    let full = state.runtime.runs[0].attempts[0]
        .turn_mailbox
        .as_ref()
        .unwrap()
        .deliveries
        .keys()
        .find(|key| key.starts_with(first["receipt"].as_str().unwrap()))
        .unwrap()
        .clone();
    assert_eq!(full.len(), 64);
    let by_full = hand_off(
        &mut state,
        &id,
        &generation,
        "partial-full",
        json!({"processed":[full],"disposition":"continue"}),
    )
    .unwrap();
    assert_eq!(
        (by_full["processed"].as_u64(), by_full.get("left_pending")),
        (Some(1), None),
        "{by_full}"
    );
    assert_eq!(room(publish(&state)), Some(2));
    // A receipt that would skip unseen events stays pending instead of failing the handoff.
    let skip = hand_off(
        &mut state,
        &id,
        &generation,
        "skip",
        json!({"processed":[second["receipt"]],"disposition":"continue"}),
    )
    .unwrap();
    assert_eq!(skip["processed"], 0, "{skip}");
    assert_eq!(skip["left_pending"][0]["receipt"], second["receipt"]);
    assert!(
        skip["left_pending"][0]["reason"].is_string() && skip["adjusted"].is_array(),
        "{skip}"
    );
    assert_eq!(room(publish(&state)), None);
    // Omitted: everything delivered in this attempt, in order.
    let all = hand_off(&mut state, &id, &generation, "all", json!({"disposition":"continue"})).unwrap();
    assert!(all.get("left_pending").is_none(), "{all}");
    assert_eq!(
        (all["staged"].as_bool(), all["processed"].as_u64()),
        (Some(true), Some(2)),
        "{all}"
    );
    assert_eq!(room(publish(&state)), Some(3));
}

#[test]
fn context_reads_carry_events_and_position_not_the_notes_the_prompt_already_shows() {
    let (mut state, id, generation) = fixture();
    hand_off(&mut state, &id, &generation, "notes", json!({"disposition":"continue","payload":{"subgoal":"ship it","findings":[],"tried":[],"pending":[],"next":[]}})).unwrap();
    post(&mut state, "first");
    let read = read_room(&mut state, &id, &generation, "plain");
    for absent in ["working_state", "guidance"] {
        assert!(read.get(absent).is_none(), "{absent}: {read}");
    }
    assert_eq!(
        read["draft"],
        json!({"disposition":{"kind":"continue"}}),
        "a staged draft is summarised"
    );
    assert_eq!(read["events"][0]["text"], "first");
    assert!(
        read["receipt"].is_string() && read["through"].is_u64() && read["budget"]["elapsed_seconds"].is_u64(),
        "{read}"
    );
    let cx = AttemptRef {
        run: &id,
        generation: &generation,
        nonce: "with-notes",
    };
    let notes = handle(
        &mut state,
        &cx,
        "control_turn_context",
        &json!({"source":"room","notes":true}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        notes.get("working_state").is_some_and(Value::is_null) || notes["working_state"].is_object(),
        "{notes}"
    );
}
#[test]
fn the_inbox_delivers_new_conversation_once_and_cut_entries_point_to_their_full_text() {
    let (mut state, id, generation) = fixture();
    assert!(inbox(&mut state, &id, |_| Ok(())).is_none(), "nothing new");
    let chat = state.chats[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer Keep the public API", "test").unwrap();
    let long = format!("Plan: {}", "step ".repeat(400));
    state.chats[0].messages.push(crate::chat::Message {
        speaker: Default::default(),
        text: long.clone(),
        run_id: None,
        reply: String::new(),
    });
    // A failed save delivers nothing.
    assert!(inbox(&mut state, &id, |_| Err(io::Error::other("disk"))).is_none());
    let block = inbox(&mut state, &id, |_| Ok(())).unwrap();
    assert!(
        block.starts_with("Inbox (2 new, shown once)") && block.contains("[instruction] /steer Keep the public API"),
        "{block}"
    );
    assert!(
        !block.contains(&long) && block.contains("[cut; full text: control_turn_context"),
        "{block}"
    );
    assert!(inbox(&mut state, &id, |_| Ok(())).is_none(), "shown once");
    assert!(
        !callback_notice(&mut state, &id, true),
        "no hint for what the inbox delivered"
    );
    let cx = |nonce| AttemptRef {
        run: &id,
        generation: &generation,
        nonce,
    };
    let read = handle(
        &mut state,
        &cx("after-inbox"),
        "control_turn_context",
        &json!({}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        read["events"].as_array().unwrap().is_empty(),
        "already delivered: {read}"
    );
    // The pointer rereads the cut entry in full.
    let pointer = block
        .split("\"after_receipt\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let full = handle(
        &mut state,
        &cx("full"),
        "control_turn_context",
        &json!({"after_receipt":pointer}),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(full["events"][0]["text"], long.as_str());
    // A handoff that omits processed acknowledges what the inbox delivered.
    let staged = handle(
        &mut state,
        &cx("handoff"),
        "control_turn_handoff",
        &json!({"disposition":"done"}),
        |_| Ok(()),
    )
    .unwrap();
    assert!(staged.get("left_pending").is_none(), "{staged}");
    crate::chat::send(&mut state, &chat, "/steer Too late", "test").unwrap();
    assert!(
        inbox(&mut state, &id, |_| Ok(())).is_none(),
        "nothing after a staged handoff"
    );
}
/// Run E: agents started a short transient routine, then named it in the same
/// turn's wait. It had already exited, so its subscription was retired with
/// its exit queued for the chat. The drop says so instead of implying the
/// wait tried to create a subscription; a never-subscribed name keeps the
/// general reason.
#[test]
fn a_wait_on_a_finished_routine_says_its_queued_exit_wakes_the_chat() {
    let (mut state, id, generation) = fixture();
    let chat = state.chats[0].id.clone();
    crate::bots::command(&mut state, &chat, "on").unwrap();
    let watch: crate::watches::Watch = serde_json::from_value(json!({
        "id":"w-1","name":"dig","chat":chat,"service":"svc-a","generation":1,"permanent":false,"command":"node dig.js","stopped":true
    })).unwrap();
    state.watches.push(watch);
    assert!(
        !crate::bots::wait_topics(&state, &chat)
            .iter()
            .any(|t| t.starts_with("watch:"))
    );
    let wait = hand_off(
        &mut state,
        &id,
        &generation,
        "finished",
        json!({"disposition":{"kind":"wait","topics":["watch:w-1","w-2"]}}),
    )
    .unwrap();
    let adjusted = wait["adjusted"].to_string();
    assert!(
        adjusted.contains("dropped watch:w-1: that routine already finished")
            && adjusted.contains("wake this chat without a wait topic"),
        "{adjusted}"
    );
    assert!(
        adjusted.contains("dropped w-2: not a current subscription"),
        "{adjusted}"
    );
    // Nothing named remains, so the wait keeps every subscription; the queued
    // exit is urgent and releases it.
    assert!(adjusted.contains("every current subscription"), "{adjusted}");
}
#[test]
fn an_operator_turn_handoff_replies_with_the_agents_last_message() {
    let (mut state, id, generation) = fixture();
    state.runtime.runs[0]
        .attempts
        .last_mut()
        .unwrap()
        .activity
        .push(crate::usage::Activity {
            id: "m1".into(),
            kind: "message".into(),
            text: "Root cause and diff.".into(),
            output: String::new(),
            status: "completed".into(),
        });
    // The operator is told the last message is the reply, not that none is needed.
    let staged = hand_off(&mut state, &id, &generation, "done", json!({"disposition":"done"})).unwrap();
    assert!(
        staged["next"].as_str().unwrap().contains("end with your full answer"),
        "{staged}"
    );
    let attempt = state.runtime.runs[0].attempts.last().unwrap().clone();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(state));
    let poll = |shared: &std::sync::Arc<std::sync::Mutex<State>>| {
        crate::attempt_control::poll(Some((shared, &id)), &attempt, std::time::Duration::ZERO)
    };
    assert_eq!(
        poll(&shared),
        crate::attempt_control::Decision::Finish("Root cause and diff.".into())
    );
    // A wake keeps the placeholder and the stop instruction.
    shared.lock().unwrap().runtime.runs[0].kind = crate::runtime::RunKind::BotWake;
    assert_eq!(
        poll(&shared),
        crate::attempt_control::Decision::Finish("Agent yielded with a committed handoff.".into())
    );
}
#[test]
fn a_wake_handoff_still_needs_no_further_response() {
    let (mut state, id, generation) = fixture();
    state.runtime.runs[0].kind = crate::runtime::RunKind::BotWake;
    let staged = hand_off(&mut state, &id, &generation, "done", json!({"disposition":"done"})).unwrap();
    assert!(staged["next"].as_str().unwrap().starts_with("Stop:"), "{staged}");
}
