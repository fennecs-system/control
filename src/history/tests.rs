
use super::*;

#[test]
fn missing_activity_is_reported_in_chat_views_and_prompt_without_changing_state() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build it", "test").unwrap();
    let chat = own(&mut state, &chat, "missing-activity");
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].attempts.push(Attempt {
        journal: Some(42),
        ..Default::default()
    });
    crate::chat::send(&mut state, &chat, "Continue", "test").unwrap();
    let next = state.runtime.runs[1].id.clone();
    let before = persisting(|| serde_json::to_vec(&state)).unwrap();
    let detail = crate::chat::detail(&state, &chat).unwrap();
    let attempt = &detail["messages"][0]["attempts"][0];
    assert_eq!(attempt["journal"], 42);
    let error = attempt["history_error"]
        .as_str()
        .expect("missing history must be visible");
    assert!(error.contains("missing record") && error.contains("42") && error.contains(&chat));
    assert!(crate::chat::transcript(&state, &chat)
        .unwrap()
        .contains(error));
    assert!(crate::chat::prompt(&state, &next).unwrap().contains(error));
    assert_eq!(persisting(|| serde_json::to_vec(&state)).unwrap(), before);
}

#[test]
fn empty_invalid_and_unavailable_activity_records_remain_distinct() {
    for (name, bytes, expected) in [
        (
            "empty",
            b"{\"run\":\"r\",\"attempt\":0,\"activity\":[]}\n".as_slice(),
            None,
        ),
        (
            "invalid",
            b"{\"run\":\"r\",\"attempt\":0,\"activity\":\"private-payload\"}\n".as_slice(),
            Some(ReadCause::Invalid),
        ),
        (
            "malformed",
            b"{private-payload\n".as_slice(),
            Some(ReadCause::Invalid),
        ),
    ] {
        let mut state = crate::State::default();
        let chat = crate::chat::create(&mut state);
        let chat = own(&mut state, &chat, &format!("read-{name}"));
        let path = path(Some(&chat)).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let attempt = Attempt {
            journal: Some(0),
            ..Default::default()
        };
        match (activity(Some(&chat), &attempt), expected) {
            (Ok(events), None) => assert!(events.is_empty()),
            (Err(error), Some(expected)) => {
                assert_eq!(error.cause, expected);
                assert_eq!(error.journal, 0);
                assert!(!error.to_string().contains("private-payload"));
            }
            _ => panic!("missing or corrupt history cannot be successful empty activity"),
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "reads preserve evidence"
        );
    }

    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    let chat = own(&mut state, &chat, "read-locked");
    let path = path(Some(&chat)).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _other_owner = FileJournal::open(&path).unwrap();
    let error = activity(
        Some(&chat),
        &Attempt {
            journal: Some(0),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.cause, ReadCause::Io(io::ErrorKind::WouldBlock));
}

#[test]
fn failed_hydration_keeps_its_reference_and_can_read_a_repaired_record() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    let chat = own(&mut state, &chat, "read-repaired");
    let attempts = [Attempt {
        journal: Some(0),
        ..Default::default()
    }];
    let failed = serde_json::to_value(hydrated(Some(&chat), &attempts)).unwrap();
    assert_eq!(failed[0]["journal"], 0);
    assert!(failed[0]["history_error"].is_string());
    with(Some(&chat), |journal| {
        journal.append(
            &serde_json::to_vec(&Entry {
                run: "r".into(),
                attempt: 0,
                activity: Cow::Owned(vec![event("repaired")]),
            })
            .unwrap(),
        )
    })
    .unwrap();
    let repaired = serde_json::to_value(hydrated(Some(&chat), &attempts)).unwrap();
    assert!(repaired[0].get("history_error").is_none());
    assert!(repaired[0].get("journal").is_none());
    assert_eq!(repaired[0]["activity"][0]["id"], "repaired");
    assert_eq!(
        attempts[0].journal,
        Some(0),
        "hydration is only a read view"
    );
}

#[test]
fn delivery_warns_about_missing_activity_without_blocking_other_reports() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Missing", "test").unwrap();
    let chat = own(&mut state, &chat, "read-delivery");
    state.runtime.runs[0].status = "completed".into();
    *state.runtime.runs[0].result = "Finished task".into();
    state.runtime.runs[0].attempts.push(Attempt {
        journal: Some(42),
        ..Default::default()
    });
    crate::chat::send(&mut state, &chat, "Healthy", "test").unwrap();
    state.runtime.runs[1].status = "completed".into();
    *state.runtime.runs[1].result = "Healthy report".into();
    emit(&mut state, |_| Ok(())).unwrap();
    let deliveries =
        control::outbox::read(&outbox_path().unwrap(), control::outbox::Cursor::default())
            .unwrap()
            .into_iter()
            .map(|(event, _)| event)
            .filter(|event| event.chat.as_deref() == Some(&chat))
            .collect::<Vec<_>>();
    assert_eq!(deliveries.len(), 2);
    assert!(deliveries[0].report.starts_with("Finished task"));
    assert!(deliveries[0].report.contains("missing record"));
    assert!(deliveries[0].change.is_none());
    assert_eq!(deliveries[1].report, "Healthy report");
    assert_eq!(*state.runtime.runs[0].result, "Finished task");
    assert!(state
        .runtime
        .runs
        .iter()
        .all(|run| run.emitted.as_deref() == Some("completed")));
}

#[test]
fn space_focus_and_edge_summaries_survive_handoff_journal_reload() {
    use crate::conversation::{Compactor, Conversation, Lens, Summaries};
    use control_comonad::{Block, Space, Store};
    use control_context::{
        turn_mailbox::{Disposition, Handoff, Mailbox},
        working_state::Payload,
    };
    struct Edges;
    impl Summaries for Edges {
        fn summary(&self, block: Block) -> Option<String> {
            Some(format!("retained block {}:{}", block.start, block.level))
        }
    }
    let snapshot = |state: &crate::State| {
        let raw = Conversation::of(state, &state.chats[0]);
        let task = Conversation::of(state, &state.chats[0]).task_view();
        let focus = Store::new(&task, 7).unwrap();
        let focused = focus.seek(31).unwrap().extract().message.text.clone();
        let summaries = Lens {
            verbatim: 8,
            summaries: &Edges,
        }
        .window(&task, task.len())
        .render(&task);
        (
            raw.positions().collect::<Vec<_>>(),
            task.positions().collect::<Vec<_>>(),
            focused,
            summaries,
            (0..task.len())
                .map(|i| task.entry(i).unwrap().render())
                .collect::<Vec<_>>(),
        )
    };
    let mut state = crate::State::default();
    let id = crate::chat::create(&mut state);
    let chat = own(&mut state, &id, "review-space-reload");
    for index in 0..64 {
        crate::chat::send(&mut state, &chat, &format!("event {index}"), "test").unwrap();
        let run = state.runtime.runs.last_mut().unwrap();
        run.status = "completed".into();
        if index % 2 == 1 {
            run.kind = crate::runtime::RunKind::BotWake;
        }
        let mut mailbox = Mailbox::default();
        mailbox
            .stage(
                "note".into(),
                Handoff {
                    base_revision: 0,
                    payload: Payload {
                        subgoal: "Keep working".into(),
                        ..Default::default()
                    },
                    processed: vec![],
                    resynchronised: vec![],
                    disposition: Disposition::Continue,
                    progress: Some(format!("verified progress {index}")),
                },
                0,
            )
            .unwrap();
        run.attempts.push(Attempt {
            turn_mailbox: Some(Box::new(mailbox)).into(),
            activity: vec![event(&index.to_string())],
            ..Default::default()
        });
        archive_run(run).unwrap();
    }
    let before = snapshot(&state);
    let saved = persisting(|| serde_json::to_vec(&state)).unwrap();
    let mut restored: crate::State = serde_json::from_slice(&saved).unwrap();
    recover(&restored).unwrap();
    hydrate_text(&mut restored).unwrap();
    assert_eq!(before, snapshot(&restored));
    assert_eq!(before.0, before.1, "projection cannot renumber the space");
    assert_eq!(before.2, "event 31");
    assert!(before.3.iter().any(|s| s.contains("retained block")));
    assert!(before.4.iter().any(|s| s.contains("verified progress 63")));
}

/// A chat whose journal no parallel test shares (clock-based ids can collide).
fn own(state: &mut crate::State, chat: &str, name: &str) -> String {
    let id = format!("c-test-{name}");
    for session in state.chats.iter_mut().filter(|s| s.id == chat) {
        session.id = id.clone();
    }
    for run in state
        .runtime
        .runs
        .iter_mut()
        .filter(|r| r.chat_id.as_deref() == Some(chat))
    {
        run.chat_id = Some(id.clone());
    }
    let _ = std::fs::remove_file(path(Some(&id)).unwrap());
    id
}

fn event(id: &str) -> Activity {
    Activity {
        id: id.into(),
        kind: "command_execution".into(),
        text: "cargo test".into(),
        output: "ok".repeat(50),
        status: "completed".into(),
    }
}

#[test]
fn finished_handoffs_spill_with_activity_and_failed_saves_restore_the_inline_value() {
    use control_context::turn_mailbox::Mailbox;
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Retain a handoff", "test").unwrap();
    own(&mut state, &chat, "handoff-spill");
    let mut mailbox = Mailbox::default();
    mailbox
        .deliver(
            "receipt".into(),
            control_context::turn_mailbox::Delivery {
                ranges: vec![],
                gap: false,
                request: "x".repeat(8000),
            },
        )
        .unwrap();
    let run = &mut state.runtime.runs[0];
    run.attempts.push(Attempt {
        turn_mailbox: Some(Box::new(mailbox.clone())).into(),
        ..Default::default()
    });
    run.status = "completed".into();
    let inline = persisting(|| serde_json::to_vec(&state)).unwrap();
    let archived = archive_run(&mut state.runtime.runs[0]).unwrap();
    let compact = persisting(|| serde_json::to_vec(&state)).unwrap();
    assert!(
        compact.len() + 7000 < inline.len(),
        "finished receipts must not be rewritten on every checkpoint"
    );
    let mut restored: crate::State = serde_json::from_slice(&compact).unwrap();
    recover(&restored).unwrap();
    hydrate_text(&mut restored).unwrap();
    assert_eq!(
        restored.runtime.runs[0].attempts[0].turn_mailbox.as_deref(),
        Some(&mailbox)
    );
    assert_eq!(
        persisting(|| serde_json::to_vec(&restored)).unwrap(),
        compact
    );
    undo(&mut state.runtime.runs[0], archived);
    assert_eq!(persisting(|| serde_json::to_vec(&state)).unwrap(), inline);
}
#[test]
fn finished_attempts_move_to_journals_and_read_back_unchanged() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build it", "test").unwrap();
    let chat = own(&mut state, &chat, "attempts");
    let run = &mut state.runtime.runs[0];
    let mut attempt = Attempt::new();
    attempt.activity = vec![event("a"), event("b")];
    run.attempts.push(attempt);
    run.status = "completed".into();
    archive_run(&mut state.runtime.runs[0]).unwrap();
    let attempt = &state.runtime.runs[0].attempts[0];
    assert!(attempt.activity.is_empty() && attempt.journal.is_some());
    let saved = serde_json::to_string(&state).unwrap();
    assert!(
        !saved.contains("cargo test") && saved.contains("\"journal\""),
        "state carries only the position"
    );
    let read = activity(Some(&chat), attempt).unwrap();
    assert_eq!(
        read.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(
        hydrated(Some(&chat), &state.runtime.runs[0].attempts)[0]
            .attempt.activity
            .len(),
        2
    );
    let line = std::fs::read_to_string(path(Some(&chat)).unwrap()).unwrap();
    let entry: serde_json::Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
    assert_eq!(
        (entry["run"].as_str(), entry["attempt"].as_u64()),
        (Some(state.runtime.runs[0].id.as_str()), Some(0)),
        "inspectable JSON lines"
    );
}

#[test]
fn finished_run_text_saves_as_a_position_and_edits_save_in_full() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Write the long report", "test").unwrap();
    own(&mut state, &chat, "text");
    *state.runtime.runs[0].result = "A long finished answer. ".repeat(200);
    state.runtime.runs[0].status = "completed".into();
    archive_run(&mut state.runtime.runs[0]).unwrap();
    let saved = persisting(|| serde_json::to_string(&state).unwrap());
    let runs = persisting(|| serde_json::to_string(&state.runtime.runs).unwrap());
    assert!(
        !runs.contains("long finished answer") && !runs.contains("Write the long report"),
        "a save carries positions, not run text"
    );
    assert!(
        serde_json::to_string(&state)
            .unwrap()
            .contains("long finished answer"),
        "views still see the full value"
    );
    let mut loaded: crate::State = serde_json::from_str(&saved).unwrap();
    assert!(
        loaded.runtime.runs[0].result.is_empty(),
        "markers load empty"
    );
    hydrate_text(&mut loaded).unwrap();
    assert_eq!(
        *loaded.runtime.runs[0].result,
        *state.runtime.runs[0].result
    );
    assert_eq!(
        *loaded.runtime.runs[0].objective,
        *state.runtime.runs[0].objective
    );
    let copy = loaded.runtime.runs[0].clone();
    assert!(
        persisting(|| serde_json::to_string(&copy).unwrap()).contains("long finished answer"),
        "a copy never borrows another run's journal position"
    );
    loaded.runtime.runs[0].result.push_str(" Edited.");
    let resaved = persisting(|| serde_json::to_string(&loaded.runtime.runs).unwrap());
    assert!(
        resaved.contains("Edited.") && !resaved.contains("Write the long report"),
        "only the edited field is saved in full"
    );
}

#[test]
fn deliveries_are_emitted_once_per_status_and_retried_after_a_failed_save() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Tell me when done", "test").unwrap();
    let chat = own(&mut state, &chat, "outbox");
    let outbox = outbox_path().unwrap();
    let events = || {
        control::outbox::read(&outbox, control::outbox::Cursor::default())
            .unwrap()
            .into_iter()
            .map(|(e, _)| e)
            .filter(|e| e.chat.as_deref() == Some(chat.as_str()))
            .collect::<Vec<_>>()
    };
    emit(&mut state, |_| Ok(())).unwrap();
    assert!(events().is_empty(), "queued runs are not deliverable");
    state.runtime.runs[0].status = "waiting".into();
    emit(&mut state, |_| Err(io::Error::other("disk"))).unwrap_err();
    assert_eq!(
        state.runtime.runs[0].emitted, None,
        "a failed save re-emits next pass"
    );
    emit(&mut state, |_| Ok(())).unwrap();
    emit(&mut state, |_| Ok(())).unwrap();
    state.runtime.runs[0].status = "completed".into();
    *state.runtime.runs[0].result = "Finished.\n\nReview workspace: x".into();
    emit(&mut state, |_| Ok(())).unwrap();
    let statuses: Vec<_> = events()
        .iter()
        .map(|e| (e.status.clone(), e.report.clone()))
        .collect();
    assert_eq!(
        statuses.iter().filter(|(s, _)| s == "waiting").count(),
        2,
        "at least once: the failed save's event and its retry"
    );
    assert_eq!(
        statuses.last(),
        Some(&("completed".to_owned(), "Finished.".to_owned())),
        "one event per new status, without the review footer"
    );
}

#[test]
fn finishing_a_run_is_one_transaction_with_its_save() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Finish atomically", "test").unwrap();
    let chat = own(&mut state, &chat, "transaction");
    let mut attempt = Attempt::new();
    attempt.activity = vec![event("x")];
    state.runtime.runs[0].attempts.push(attempt);
    state.runtime.runs[0].status = "completed".into();
    let before = serde_json::to_value(&state.runtime.runs[0]).unwrap();
    let len = || with(Some(&chat), |j| Ok(j.len())).unwrap();
    let start = path(Some(&chat)).unwrap().exists().then(len).unwrap_or(0);
    // A failed save: the run is as it was in memory; the unreferenced line is
    // dropped by recovery from the state that was actually saved.
    let saved = persisting(|| serde_json::to_string(&state).unwrap());
    let archived = archive_run(&mut state.runtime.runs[0]).unwrap();
    assert_eq!(len(), start + 1);
    undo(&mut state.runtime.runs[0], archived);
    assert_eq!(
        serde_json::to_value(&state.runtime.runs[0]).unwrap(),
        before
    );
    recover(&serde_json::from_str(&saved).unwrap()).unwrap();
    assert_eq!(len(), start);
    // A crash after the append but before the save: recovery drops the line.
    archive_run(&mut state.runtime.runs[0]).unwrap();
    assert_eq!(len(), start + 1);
    let restarted: crate::State = serde_json::from_str(&saved).unwrap();
    recover(&restarted).unwrap();
    assert_eq!(
        len(),
        start,
        "an append whose save never happened is not history"
    );
}
