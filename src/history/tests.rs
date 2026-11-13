
use super::*;
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
    let read = activity(Some(&chat), attempt);
    assert_eq!(
        read.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(
        hydrated(Some(&chat), &state.runtime.runs[0].attempts)[0]
            .activity
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
