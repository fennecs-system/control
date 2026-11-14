
use super::*;
use crate::runtime::RunKind;

#[test]
fn codex_tool_outcome_survives_activity_truncation_and_drains_final_results() {
    for completion in [None, Some("completed"), Some("failed")] {
        let mut lines = String::new();
        let mut append = |id: &str, kind: &str, status: &str| {
            lines.push_str(
                &serde_json::json!({"type":"item.updated", "item":{
                    "id":id, "type":kind, "tool":"control_room_post", "command":"test",
                    "status":status
                }})
                .to_string(),
            );
            lines.push('\n');
        };
        append("mcp1", "mcp_tool_call", "in_progress");
        for i in 0..100 {
            append(&format!("cmd{i}"), "command_execution", "completed");
        }
        if let Some(status) = completion {
            append("mcp1", "mcp_tool_call", status);
        }
        let mut tail = std::collections::VecDeque::new();
        let (_, unfinished) = capture_codex(lines.as_bytes(), |event| {
            tail.push_back(event);
            if tail.len() > 64 {
                tail.pop_front();
            }
        });
        assert_eq!(tail.len(), 64);
        assert_eq!(unfinished, completion.is_none());
        if completion.is_none() {
            assert!(tail.iter().all(|event| event.kind != "mcp_tool_call"));
        }
    }
}

#[test]
fn unfinished_codex_tool_prevents_success_without_publishing_or_retrying() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].workspace = Some("previous-output".into());
    crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    let attempt = state.runtime.runs[0].attempts[0].clone();
    let generation = attempt.workspace_id.clone().unwrap();
    record_activity_for(
        &mut state,
        &id,
        &generation,
        crate::usage::Activity {
            id: "item_1".into(),
            kind: "mcp_tool_call".into(),
            text: "control_room_post".into(),
            status: "in_progress".into(),
            output: String::new(),
        },
    );
    let shared = Arc::new(Mutex::new(state));
    let mut attempt = attempt;
    let result = finish_codex_tools(&mut attempt, Some((&shared, &id)), &generation, false, true);
    assert!(matches!(&result, Err(("interrupted", _))));
    assert_eq!(
        attempt.termination,
        Some(crate::attempt_control::Termination::ProviderError)
    );
    let mut state = shared.lock().unwrap();
    commit(
        &mut state,
        &id,
        Action::Task,
        attempt,
        result.map(|()| "done".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.runtime.runs.len(), 1);
    assert_eq!(state.runtime.runs[0].status, "interrupted");
    assert_eq!(state.chats[0].workspace.as_deref(), Some("previous-output"));
    let events = crate::history::activity(Some(&chat), &state.runtime.runs[0].attempts[0]);
    assert_eq!(events[0].status, "interrupted");
    assert!(events[0]
        .output
        .contains("effect may already have happened"));
}

#[test]
fn codex_completion_distinguishes_finished_tools_and_host_handoffs() {
    for (kind, status, host_finished) in [
        ("mcp_tool_call", "completed", false),
        ("mcp_tool_call", "failed", false),
        ("command_execution", "in_progress", false),
        ("mcp_tool_call", "in_progress", true),
    ] {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
        checkpoint_next(&mut state, |_| Ok(())).unwrap();
        let id = state.runtime.runs[0].id.clone();
        let mut attempt = state.runtime.runs[0].attempts[0].clone();
        let generation = attempt.workspace_id.clone().unwrap();
        let event = crate::usage::Activity {
            id: "item_1".into(),
            kind: kind.into(),
            text: "tool".into(),
            status: "in_progress".into(),
            output: String::new(),
        };
        record_activity_for(&mut state, &id, &generation, event.clone());
        // The reader has drained its final completion before the exit guard runs.
        record_activity_for(
            &mut state,
            &id,
            &generation,
            crate::usage::Activity {
                status: status.into(),
                ..event
            },
        );
        let shared = Arc::new(Mutex::new(state));
        assert!(finish_codex_tools(
            &mut attempt,
            Some((&shared, &id)),
            &generation,
            host_finished,
            kind == "mcp_tool_call" && status == "in_progress"
        )
        .is_ok());
        assert!(attempt.termination.is_none());
        assert_eq!(
            shared.lock().unwrap().runtime.runs[0].attempts[0].activity[0].status,
            status
        );
    }
}

#[test]
fn stale_codex_exit_cannot_interrupt_another_attempt() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    let attempt = state.runtime.runs[0].attempts[0].clone();
    let generation = attempt.workspace_id.clone().unwrap();
    record_activity_for(
        &mut state,
        &id,
        &generation,
        crate::usage::Activity {
            id: "item_1".into(),
            kind: "mcp_tool_call".into(),
            text: "tool".into(),
            status: "in_progress".into(),
            output: String::new(),
        },
    );
    let shared = Arc::new(Mutex::new(state));
    interrupt_unfinished_tools(Some((&shared, &id)), "stale-generation");
    assert_eq!(
        shared.lock().unwrap().runtime.runs[0].attempts[0].activity[0].status,
        "in_progress"
    );
}

#[test]
fn both_adapters_can_commit_host_finished_handoff_without_a_final_model_reply() {
    use control_context::{
        turn_mailbox::{Disposition, Handoff, Mailbox},
        working_state::Payload,
    };
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        for cancelled in [false, true] {
            let mut state = State::default();
            let chat = crate::chat::create(&mut state);
            crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
            state.runtime.runs[0].backend = backend;
            checkpoint_next(&mut state, |_| Ok(())).unwrap();
            let id = state.runtime.runs[0].id.clone();
            let mut mailbox = Mailbox::default();
            mailbox
                .stage(
                    "yield".into(),
                    Handoff {
                        base_revision: 0,
                        payload: Payload {
                            subgoal: "Wait for dependency".into(),
                            ..Default::default()
                        },
                        processed: vec![],
                        resynchronised: vec![],
                        disposition: Disposition::Done,
                        progress: Some("Checkpoint ready".into()),
                    },
                    0,
                )
                .unwrap();
            state.runtime.runs[0].attempts[0].turn_mailbox = Some(Box::new(mailbox)).into();
            let mut attempt = state.runtime.runs[0].attempts[0].clone();
            attempt.explicit_timeout_seconds = Some(600);
            let shared = Arc::new(Mutex::new(state));
            let decision = crate::attempt_control::poll(
                Some((&shared, &id)),
                &attempt,
                Duration::from_secs(600),
            );
            let crate::attempt_control::Decision::Finish(summary) = decision else {
                panic!("host must finish accepted handoff");
            };
            let mut state = shared.lock().unwrap();
            if cancelled {
                state.runtime.runs[0].status = "cancelling".into();
            }
            attempt.artifact = Some("stopped-writer-checkpoint".into());
            commit(
                &mut state,
                &id,
                Action::Task,
                attempt,
                Ok(summary),
                false,
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(state.chats[0].workspace.is_some(), !cancelled);
            assert_eq!(
                state.chats[0]
                    .continuity
                    .as_ref()
                    .unwrap()
                    .working
                    .is_some(),
                !cancelled
            );
            assert_eq!(
                state.runtime.runs[0].status,
                if cancelled { "cancelled" } else { "completed" }
            );
        }
    }
}
#[test]
fn consumed_steering_does_not_drop_requested_continuation() {
    use control_context::{
        turn_mailbox::{Delivery, Disposition, Handoff, Range},
        working_state::Payload,
    };
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    crate::chat::send(&mut state, &chat, "Build the tool", "test").unwrap();
    crate::bots::command(&mut state, &chat, "on").unwrap();
    let trace = crate::turn_context::PlannedContext::prepare(&state, 0)
        .unwrap()
        .persist()
        .unwrap();
    checkpoint_prepared(&mut state, Some(trace), |_| Ok(())).unwrap();
    crate::chat::send(
        &mut state,
        &chat,
        "/steer Keep the public API stable",
        "test",
    )
    .unwrap();
    let mailbox = state.runtime.runs[0].attempts[0]
        .turn_mailbox
        .as_mut()
        .unwrap();
    mailbox
        .deliver(
            "steering".into(),
            Delivery {
                ranges: vec![Range::Conversation {
                    chat: chat.clone(),
                    after: 1,
                    through: 2,
                }],
                gap: false,
                request: String::new(),
            },
        )
        .unwrap();
    mailbox
        .stage(
            "handoff".into(),
            Handoff {
                base_revision: 0,
                payload: Payload {
                    subgoal: "Finish the implementation".into(),
                    ..Default::default()
                },
                processed: vec!["steering".into()],
                resynchronised: vec![],
                disposition: Disposition::Continue,
                progress: None,
            },
            0,
        )
        .unwrap();
    let id = state.runtime.runs[0].id.clone();
    let mut attempt = state.runtime.runs[0].attempts[0].clone();
    attempt.artifact = Some("checkpoint".into());
    commit(
        &mut state,
        &id,
        Action::Task,
        attempt,
        Ok("Made progress; more work remains".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.runtime.runs[1].status, "completed");
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(
        state
            .runtime
            .runs
            .iter()
            .any(|r| r.kind == RunKind::BotWake && r.status == "queued"),
        "A committed continue was lost after consuming steering"
    );
}
#[test]
fn bot_activation_keeps_the_live_protocol_and_captures_only_the_next_admission() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    crate::chat::send(
        &mut state,
        &chat,
        "Build a small tool and keep improving it",
        "test",
    )
    .unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let first = state.runtime.runs[0].id.clone();
    // Simulate an already running pre-continuity provider process.
    state.runtime.runs[0].attempts[0].context_trace = None;
    state.runtime.runs[0].attempts[0].turn_mailbox = None.into();
    state.collective.posts.push(crate::rooms::Post {
        room: "general".into(),
        text: "Old room history".into(),
        chat: Some("peer".into()),
        run: None,
        key: "old".into(),
        delivery: Default::default(),
    });
    crate::bots::command(&mut state, &chat, "on").unwrap();
    assert!(state.runtime.runs[0].attempts[0].context_trace.is_none());
    assert!(state.runtime.runs[0].attempts[0].turn_mailbox.is_none());
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(
        crate::scheduler::next(&state.runtime).is_none(),
        "one writer while the original session is active"
    );
    let mut attempt = state.runtime.runs[0].attempts[0].clone();
    attempt.artifact = Some("first-checkpoint".into());
    commit(
        &mut state,
        &first,
        Action::Task,
        attempt,
        Ok("Created /workspace/tool.rs".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    let queued = crate::scheduler::next(&state.runtime).unwrap();
    let plan = crate::turn_context::PlannedContext::prepare(&state, queued).unwrap();
    let trace = plan.persist().unwrap();
    assert_eq!(
        checkpoint_prepared(&mut state, Some(trace), |_| Ok(())),
        Some(queued)
    );
    let attempt = state.runtime.runs[queued].attempts.last().unwrap();
    assert!(attempt.turn_mailbox.as_ref().unwrap().baseline.iter().any(|range|matches!(range,control_context::turn_mailbox::Range::Room{room,through:1,..} if room=="general")));
    let input = crate::turn_context::input(attempt).unwrap().unwrap();
    assert!(!input.contains("Old room history"));
    assert!(input.contains("Created /workspace/tool.rs"));
    assert!(input.contains("no periodic handoff target"));
}
#[test]
fn project_io_activity_is_live_without_holding_state_lock_and_retains_failures() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Read history", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let run = state.runtime.runs[0].id.clone();
    let generation = state.runtime.runs[0].attempts[0]
        .workspace_id
        .clone()
        .unwrap();
    let shared = Arc::new(Mutex::new(state));
    let result = project_operation(
        Some((&shared, &run)),
        &generation,
        "project-prepare",
        "Preparing workspace",
        || {
            let state = shared
                .try_lock()
                .expect("Project I/O must not hold the state lock");
            assert_eq!(
                state.runtime.runs[0].attempts[0].activity[0].status,
                "in_progress"
            );
            Ok("ready")
        },
    )
    .unwrap();
    assert_eq!(result, "ready");
    assert_eq!(
        shared.lock().unwrap().runtime.runs[0].attempts[0].activity[0].status,
        "completed"
    );
    let error = project_operation(
        Some((&shared, &run)),
        &generation,
        "project-checkpoint",
        "Saving checkpoint",
        || Err::<(), _>(std::io::Error::other("disk full")),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "disk full");
    let state = shared.lock().unwrap();
    let activity = &state.runtime.runs[0].attempts[0].activity;
    assert_eq!(activity.len(), 2);
    assert_eq!(activity[1].status, "failed");
    assert!(activity[1].output.lines().next().unwrap().ends_with('s'));
    assert!(activity[1].output.contains("disk full"));
    drop(state);
    project_operation(
        Some((&shared, &run)),
        &generation,
        "project-prepare",
        "Preparing workspace",
        || {
            shared.lock().unwrap().runtime.runs[0].status = "cancelling".into();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        shared.lock().unwrap().runtime.runs[0].attempts[0].activity[0].status,
        "cancelled"
    );
    shared.lock().unwrap().runtime.runs[0].status = "running".into();
    let error = project_operation(
        Some((&shared, &run)),
        &generation,
        "project-checkpoint",
        "Saving checkpoint",
        || {
            shared.lock().unwrap().runtime.runs[0].status = "cancelling".into();
            Err::<(), _>(std::io::Error::other("disk full"))
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "disk full");
    assert_eq!(
        shared.lock().unwrap().runtime.runs[0].attempts[0].activity[1].status,
        "failed"
    );
}
#[test]
fn result_rollback_preserves_attempt_history_and_live_diagnostics() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Work", "test").unwrap();
    let historical = crate::usage::Attempt {
        status: "failed".into(),
        workspace_id: Some("historical".into()),
        ..Default::default()
    };
    state.runtime.runs[0].attempts.push(historical.clone());
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    record_activity(
        &mut state,
        &id,
        crate::usage::Activity {
            id: "command".into(),
            kind: "command_execution".into(),
            text: "cargo test".into(),
            output: "retained diagnostics".into(),
            status: "completed".into(),
        },
    );
    let mut attempt = state.runtime.runs[0].attempts.last().unwrap().clone();
    attempt.artifact = Some("published".into());
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        commit(
            &mut state,
            &id,
            Action::Task,
            attempt.clone(),
            Ok("done".into()),
            false,
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    commit(
        &mut state,
        &id,
        Action::Task,
        attempt,
        Ok("done".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&state.runtime.runs[0].attempts[0]).unwrap(),
        serde_json::to_value(historical).unwrap()
    );
    assert_eq!(
        crate::history::activity(
            state.runtime.runs[0].chat_id.as_deref(),
            &state.runtime.runs[0].attempts[1]
        )[0]
        .output,
        "retained diagnostics"
    );
    assert_eq!(state.chats[0].workspace.as_deref(), Some("published"));
}
#[test]
fn working_state_and_processed_receipts_publish_with_the_matching_workspace_only() {
    use control_context::{
        turn_mailbox::{Delivery, Disposition, Handoff, Mailbox, Range},
        working_state::Payload,
    };
    for failure in [None, Some("failed"), Some("cancelled"), Some("interrupted")] {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Keep working toward the goal", "test").unwrap();
        state.chats[0].room = Some("general".into());
        crate::rooms::ensure_inbox(&mut state, &chat).unwrap();
        checkpoint_next(&mut state, |_| Ok(())).unwrap();
        for i in 0..8 {
            state.collective.posts.push(crate::rooms::Post {
                room: "general".into(),
                text: format!("event {i}"),
                chat: None,
                run: None,
                key: String::new(),
                delivery: Default::default(),
            });
        }
        state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed::default()));
        let run = &mut state.runtime.runs[0];
        let id = run.id.clone();
        let mut attempt = run.attempts[0].clone();
        attempt.artifact = Some("matching-workspace".into());
        let mut mailbox = Mailbox::default();
        mailbox
            .deliver(
                "receipt".into(),
                Delivery {
                    ranges: vec![Range::Room {
                        room: "general".into(),
                        after: 0,
                        through: 8,
                    }],
                    gap: false,
                    request: String::new(),
                },
            )
            .unwrap();
        mailbox
            .stage(
                "handoff".into(),
                Handoff {
                    base_revision: 0,
                    payload: Payload {
                        subgoal: "Continue building".into(),
                        ..Default::default()
                    },
                    processed: vec!["receipt".into()],
                    resynchronised: vec![],
                    disposition: Disposition::Continue,
                    progress: Some("Verified a useful milestone".into()),
                },
                0,
            )
            .unwrap();
        run.attempts[0].turn_mailbox = Some(Box::new(mailbox)).into();
        let before = serde_json::to_value(&state).unwrap();
        let outcome = || {
            failure.map_or_else(
                || Ok("Progress".into()),
                |status| Err((status, "diagnostics".into())),
            )
        };
        assert!(
            commit(
                &mut state,
                &id,
                Action::Task,
                attempt.clone(),
                outcome(),
                false,
                |_| Err(std::io::Error::other("disk full"))
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            before,
            "failed save restores draft, receipts and workspace together"
        );
        commit(
            &mut state,
            &id,
            Action::Task,
            attempt,
            outcome(),
            false,
            |_| Ok(()),
        )
        .unwrap();
        let working = state.chats[0].continuity.as_ref().unwrap().working.as_ref();
        if failure.is_none() {
            assert_eq!(working.unwrap().checkpoint, "matching-workspace");
            assert_eq!(working.unwrap().processed[0].through(), 8);
            assert_eq!(
                state.runtime.runs[0].attempts[0].task_progress(),
                Some("Verified a useful milestone")
            );
            assert_eq!(crate::rooms::inbox_cursor(&state, &chat), Some(8));
        } else {
            assert!(
                working.is_none(),
                "unsuccessful attempts retain a draft without processing receipts"
            );
            assert!(state.chats[0].workspace.is_none());
            assert!(
                state.runtime.runs[0].attempts[0]
                    .turn_mailbox
                    .as_ref()
                    .unwrap()
                    .accepted()
                    .is_some()
            );
            assert_eq!(crate::rooms::inbox_cursor(&state, &chat), Some(0));
        }
        crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
        assert!(!crate::bots::defined(&state, &chat));
        assert_eq!(
            state.runtime.runs.len(),
            1,
            "agent notes cannot enable unattended scheduling"
        );
    }
}
#[test]
fn consumed_steering_does_not_launch_twice_and_racing_input_stays_queued() {
    use control_context::{
        turn_mailbox::{Delivery, Disposition, Handoff, Mailbox, Range},
        working_state::Payload,
    };
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build the tool", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed::default()));
    crate::chat::send(&mut state, &chat, "/steer Keep the API stable", "test").unwrap();
    let mut mailbox = Mailbox::default();
    mailbox.baseline.push(Range::Conversation {
        chat: chat.clone(),
        after: 0,
        through: 1,
    });
    mailbox
        .deliver(
            "steering".into(),
            Delivery {
                ranges: vec![Range::Conversation {
                    chat: chat.clone(),
                    after: 1,
                    through: 2,
                }],
                gap: false,
                request: String::new(),
            },
        )
        .unwrap();
    mailbox
        .stage(
            "handoff".into(),
            Handoff {
                base_revision: 0,
                payload: Payload {
                    subgoal: "Continue the goal".into(),
                    ..Default::default()
                },
                processed: vec!["steering".into()],
                resynchronised: vec![],
                disposition: Disposition::Continue,
                progress: None,
            },
            0,
        )
        .unwrap();
    state.runtime.runs[0].attempts[0].turn_mailbox = Some(Box::new(mailbox)).into();
    crate::chat::send(&mut state, &chat, "/steer Add a useful test too", "test").unwrap();
    let id = state.runtime.runs[0].id.clone();
    let mut attempt = state.runtime.runs[0].attempts[0].clone();
    attempt.artifact = Some("checkpoint".into());
    let before = serde_json::to_value(&state).unwrap();
    assert!(
        commit(
            &mut state,
            &id,
            Action::Task,
            attempt.clone(),
            Ok("Progress".into()),
            false,
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    commit(
        &mut state,
        &id,
        Action::Task,
        attempt,
        Ok("Progress".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.runtime.runs[1].status, "completed");
    assert!(state.runtime.runs[1].result.contains(&id));
    assert_eq!(
        state.runtime.runs[2].status, "queued",
        "unseen steering must still run"
    );
}
#[test]
fn reports_keep_valid_utf8_prefixes_but_reject_malformed_files_and_links() {
    let root = std::env::temp_dir().join(format!(
        "control-report-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("report");
    for character in ["é", "猫", "🐶"] {
        for cut in 1..character.len() {
            let prefix = "a".repeat(64000 - cut);
            fs::write(&path, format!("{prefix}{character}tail")).unwrap();
            assert_eq!(read_report(&path).unwrap(), prefix);
        }
    }
    fs::write(&path, "é".repeat(32001)).unwrap();
    assert_eq!(read_report(&path).unwrap(), "é".repeat(32000));
    for bytes in [vec![0xff], vec![0xc3], {
        let mut bytes = vec![b'a'; 63999];
        bytes.push(0xc3);
        bytes
    }] {
        fs::write(&path, bytes).unwrap();
        assert!(read_report(&path).is_err());
    }
    std::os::unix::fs::symlink(&path, root.join("link")).unwrap();
    assert!(read_report(&root.join("link")).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "run alone with isolated CONTROL_STATE_DIR and CONTROL_CONFIG_DIR; requests process-wide shutdown"]
fn isolated_supervisor_retains_intent_after_worker_panic() {
    fn panic_worker(_: Arc<Mutex<State>>, _: Task) {
        panic!("Injected worker failure");
    }
    assert!(
        std::env::var("CONTROL_STATE_DIR")
            .unwrap()
            .contains("control-execution-qa")
    );
    assert!(crate::backend::Backend::Codex.image().is_none());
    assert!(crate::backend::Backend::Claude.image().is_none());
    storage::initialize().unwrap();
    let mut initial = State::default();
    let chat = crate::chat::create(&mut initial);
    crate::chat::send(&mut initial, &chat, "Work", "test").unwrap();
    storage::save(&initial).unwrap();
    let state = Arc::new(Mutex::new(initial));
    let threads = start_with(state, panic_worker);
    let start = Instant::now();
    while !crate::shutdown::requested() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Panic was not supervised"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(threads.join().is_err());
    let saved: State =
        serde_json::from_slice(&fs::read(crate::config::state("state.json").unwrap()).unwrap())
            .unwrap();
    assert_eq!(saved.runtime.runs[0].status, "running");
    assert_eq!(saved.runtime.runs[0].attempts.len(), 1);
}
#[test]
fn overlapping_commits_rollback_only_their_own_conversation() {
    let mut state = State::default();
    for _ in 0..2 {
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Work", "test").unwrap();
    }
    assert_eq!(checkpoint_next(&mut state, |_| Ok(())), Some(0));
    assert_eq!(checkpoint_next(&mut state, |_| Ok(())), Some(1));
    let mut attempts: Vec<_> = state
        .runtime
        .runs
        .iter()
        .map(|r| r.attempts[0].clone())
        .collect();
    attempts[0].artifact = Some("output-a".into());
    attempts[1].artifact = Some("output-b".into());
    let ids: Vec<_> = state.runtime.runs.iter().map(|r| r.id.clone()).collect();
    commit(
        &mut state,
        &ids[1],
        Action::Task,
        attempts[1].clone(),
        Ok("B".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        commit(
            &mut state,
            &ids[0],
            Action::Task,
            attempts[0].clone(),
            Ok("A".into()),
            false,
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(state.chats[1].workspace.as_deref(), Some("output-b"));
    assert_eq!(state.runtime.runs[1].status, "completed");
    assert!(state.chats[0].workspace.is_none());
    assert_eq!(state.runtime.runs[0].status, "running");
    commit(
        &mut state,
        &ids[0],
        Action::Task,
        attempts[0].clone(),
        Ok("A".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.chats[0].workspace.as_deref(), Some("output-a"));
    assert_eq!(state.chats[1].workspace.as_deref(), Some("output-b"));
}

#[test]
#[ignore = "run alone with isolated CONTROL_STATE_DIR and CONTROL_CONFIG_DIR; requests process-wide shutdown"]
fn isolated_supervisor_overlaps_cancels_orders_and_joins() {
    // The real dispatcher, durable checkpoints and commit path, without model calls.
    fn execute(state: Arc<Mutex<State>>, task: Task) {
        {
            let mut s = state.lock().unwrap();
            s.runtime
                .runs
                .iter_mut()
                .find(|r| r.id == task.0)
                .unwrap()
                .result = "entered".into();
            storage::save(&s).unwrap();
        }
        let marker = crate::config::state(&format!("release-{}", task.0)).unwrap();
        let started = Instant::now();
        while !marker.exists() && !crate::lifecycle::cancelled(Some((&state, &task.0))) {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "Test worker timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        commit(
            &mut state.lock().unwrap(),
            &task.0,
            task.3,
            task.4,
            Ok("done".into()),
            crate::shutdown::requested(),
            storage::save,
        )
        .unwrap();
    }
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            crate::shutdown::request();
        }
    }
    let _stop = Stop;
    assert!(
        std::env::var("CONTROL_STATE_DIR")
            .unwrap()
            .contains("control-execution-qa")
    );
    assert!(crate::backend::Backend::Codex.image().is_none());
    assert!(crate::backend::Backend::Claude.image().is_none());
    storage::initialize().unwrap();
    let mut initial = State::default();
    let a = crate::chat::create(&mut initial);
    let b = crate::chat::create(&mut initial);
    let c = crate::chat::create(&mut initial);
    for chat in [&a, &a, &b, &c] {
        crate::chat::send(&mut initial, chat, "Work", "test").unwrap();
    }
    storage::save(&initial).unwrap();
    let ids: Vec<_> = initial.runtime.runs.iter().map(|r| r.id.clone()).collect();
    let state = Arc::new(Mutex::new(initial));
    let threads = start_with(state.clone(), execute);
    let wait = |expected: &[(usize, &str, Option<&str>)]| {
        let start = Instant::now();
        loop {
            let s = state.lock().unwrap();
            assert!(
                s.runtime
                    .runs
                    .iter()
                    .filter(|r| matches!(r.status.as_str(), "running" | "cancelling"))
                    .count()
                    <= 2
            );
            if expected.iter().all(|(i, status, result)| {
                s.runtime.runs[*i].status == *status
                    && result.is_none_or(|r| s.runtime.runs[*i].result == r)
            }) {
                break;
            }
            drop(s);
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Dispatcher did not progress"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    wait(&[
        (0, "running", Some("entered")),
        (1, "queued", None),
        (2, "running", Some("entered")),
        (3, "queued", None),
    ]);
    {
        let mut s = state.lock().unwrap();
        crate::lifecycle::request_cancel(&mut s.runtime, &ids[2], &Actor::LocalOperator).unwrap();
        storage::save(&s).unwrap();
    }
    wake();
    wait(&[
        (0, "running", Some("entered")),
        (1, "queued", None),
        (2, "cancelled", None),
        (3, "running", Some("entered")),
    ]);
    fs::write(
        crate::config::state(&format!("release-{}", ids[0])).unwrap(),
        "release",
    )
    .unwrap();
    wait(&[
        (0, "completed", None),
        (1, "running", Some("entered")),
        (3, "running", Some("entered")),
    ]);
    {
        let mut s = state.lock().unwrap();
        crate::chat::send(&mut s, &a, "Keep queued", "test").unwrap();
        storage::save(&s).unwrap();
    }
    crate::shutdown::request();
    threads.join().unwrap();
    let s = state.lock().unwrap();
    assert_eq!(s.runtime.runs[1].status, "interrupted");
    assert_eq!(s.runtime.runs[3].status, "interrupted");
    assert_eq!(s.runtime.runs[4].status, "queued");
    let attempts: std::collections::HashSet<_> = s
        .runtime
        .runs
        .iter()
        .flat_map(|r| &r.attempts)
        .map(|a| &a.workspace_id)
        .collect();
    assert_eq!(attempts.len(), 4);
    let saved: State =
        serde_json::from_slice(&fs::read(crate::config::state("state.json").unwrap()).unwrap())
            .unwrap();
    assert_eq!(saved.runtime.runs[4].status, "queued");
    assert!(
        saved
            .runtime
            .runs
            .iter()
            .all(|r| !matches!(r.status.as_str(), "running" | "cancelling"))
    );
}
#[test]
#[ignore = "starts an isolated server and one authenticated model task; requires pinned images and CONTROL_QA_URL/CONTROL_QA_SERVER_BIN"]
fn live_signal_stops_active_worker_and_preserves_queued_turn() {
    live_shutdown(false);
}
#[test]
#[ignore = "starts an isolated server and two authenticated model tasks; requires pinned images and CONTROL_QA_URL/CONTROL_QA_SERVER_BIN"]
fn live_parallel_workers_cancel_independently_and_shutdown_cleanly() {
    live_shutdown(true);
}
fn live_shutdown(parallel: bool) {
    use serde_json::json;
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            unsafe {
                libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
            }
            let start = Instant::now();
            while self.0.try_wait().ok().flatten().is_none()
                && start.elapsed() < Duration::from_secs(30)
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let url = std::env::var("CONTROL_QA_URL").expect("isolated server URL");
    let parsed = reqwest::Url::parse(&url).unwrap();
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    let addr = format!("127.0.0.1:{}", parsed.port().unwrap());
    assert!(
        std::net::TcpStream::connect(&addr).is_err(),
        "QA port is already in use"
    );
    let binary = std::env::var("CONTROL_QA_SERVER_BIN").expect("built server binary");
    if let Ok(bytes) = fs::read(crate::config::state("state.json").unwrap()) {
        let previous: State = serde_json::from_slice(&bytes).unwrap();
        assert!(
            previous.runtime.runs.is_empty(),
            "Use fresh QA state; an old queued turn must not dispatch during a repeated check"
        );
    }
    let mut server = Server(
        Command::new(binary)
            .arg("serve")
            .env("CONTROL_ADDR", addr)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let started = Instant::now();
    while !client
        .get(format!("{url}/health"))
        .send()
        .is_ok_and(|r| r.status().is_success())
    {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "QA server did not start"
        );
        assert!(server.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(50));
    }
    // Use the application's normal client; this test never reads or prints a credential.
    let mut hive = control::client::Hive::connect(&url).unwrap();
    let mut tool = |name: &str, arguments: serde_json::Value| {
        let value = hive
            .rpc("tools/call", json!({"name":name,"arguments":arguments}))
            .unwrap();
        assert_ne!(value["isError"], true, "QA request refused");
        value["structuredContent"]["data"].clone()
    };
    let chat = tool("control.chat.create", json!({}))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    tool(
        "control.chat.send",
        json!({"id":chat,"message":"Use one workspace shell command to run exactly: printf shutdown-proof > shutdown-proof.txt; sleep 90. Do not change other files or inspect credentials. Wait for the command."}),
    );
    let started = Instant::now();
    loop {
        let detail = tool("control.chat.read", json!({"id":chat}));
        let message = &detail["messages"][0];
        let executing = message["attempts"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|a| a["activity"].as_array().into_iter().flatten())
            .any(|event| {
                event["kind"] == "command_execution"
                    && ["running", "in_progress"].contains(&event["status"].as_str().unwrap_or(""))
                    && event["text"]
                        .as_str()
                        .is_some_and(|s| s.contains("sleep 90"))
            });
        let marker = message["attempts"]
            .as_array()
            .and_then(|a| a.last())
            .and_then(|a| a["workspace_id"].as_str())
            .and_then(|id| runtime::workspace(id).ok())
            .is_some_and(|workspace| workspace.join("shutdown-proof.txt").is_file());
        if executing && marker {
            break;
        }
        assert!(
            ["queued", "running"].contains(&message["status"].as_str().unwrap_or("")),
            "QA worker did not remain active"
        );
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "model did not start the requested command"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    if parallel {
        let sibling = tool("control.chat.create", json!({}))["id"]
            .as_str()
            .unwrap()
            .to_owned();
        tool(
            "control.chat.send",
            json!({"id":sibling,"message":"Use one workspace shell command to run exactly: printf overlap-proof > overlap-proof.txt; sleep 90. Do not change other files or inspect credentials. Wait for the command."}),
        );
        let started = Instant::now();
        loop {
            let detail = tool("control.chat.read", json!({"id":sibling}));
            let message = &detail["messages"][0];
            let marker = message["attempts"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(|a| a["workspace_id"].as_str())
                .and_then(|id| runtime::workspace(id).ok())
                .is_some_and(|workspace| workspace.join("overlap-proof.txt").is_file());
            assert_eq!(
                tool("control.chat.read", json!({"id":chat}))["messages"][0]["status"],
                "running",
                "First worker finished before overlap was established"
            );
            if marker && message["status"] == "running" {
                break;
            }
            assert!(
                ["queued", "running"].contains(&message["status"].as_str().unwrap_or("")),
                "Sibling worker failed"
            );
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "Workers did not overlap"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        tool(
            "control.chat.send",
            json!({"id":sibling,"message":"/cancel"}),
        );
        let started = Instant::now();
        loop {
            let detail = tool("control.chat.read", json!({"id":sibling}));
            if detail["messages"][0]["status"] == "cancelled" {
                let id = detail["messages"][0]["attempts"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["workspace_id"]
                    .as_str()
                    .unwrap();
                let names = crate::engine::query(&[
                    "ps",
                    "--all",
                    "--format",
                    "{{.Names}}",
                    "--filter",
                    &format!("name=control-{id}"),
                ])
                .unwrap();
                assert!(
                    names.trim().is_empty(),
                    "Cancelled sibling still has a container"
                );
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "Sibling cancellation timed out"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            tool("control.chat.read", json!({"id":chat}))["messages"][0]["status"],
            "running"
        );
    }
    tool(
        "control.chat.send",
        json!({"id":chat,"message":"Follow up after this task."}),
    );
    assert_eq!(
        unsafe { libc::kill(server.0.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let started = Instant::now();
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success(), "server shutdown failed");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "active worker shutdown timed out"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let state: State =
        serde_json::from_slice(&fs::read(crate::config::state("state.json").unwrap()).unwrap())
            .unwrap();
    let chat = state.chats.iter().find(|c| c.id == chat).unwrap();
    assert!(chat.workspace.is_none());
    assert_eq!(chat.messages.len(), 2);
    let run = state
        .runtime
        .runs
        .iter()
        .find(|r| Some(&r.id) == chat.messages[0].run_id.as_ref())
        .unwrap();
    assert_eq!(run.status, "interrupted");
    let attempt = run.attempts.last().unwrap();
    assert!(!attempt.activity.is_empty());
    let id = attempt.workspace_id.as_ref().unwrap();
    assert!(
        runtime::workspace(id)
            .unwrap()
            .join("shutdown-proof.txt")
            .is_file()
    );
    let next = state
        .runtime
        .runs
        .iter()
        .find(|r| Some(&r.id) == chat.messages[1].run_id.as_ref())
        .unwrap();
    assert_eq!(next.status, "queued");
    assert!(next.attempts.is_empty());
    let names = crate::engine::query(&[
        "ps",
        "--all",
        "--format",
        "{{.Names}}",
        "--filter",
        &format!("name=control-{id}"),
    ])
    .unwrap();
    assert!(names.trim().is_empty(), "attempt still has a container");
}
#[test]
fn failed_discord_design_answer_restores_question_and_delivery_checkpoint() {
    let (mut state, id) = crate::design::tests::paused();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        receive_with_save(
            &mut state,
            &id,
            "fixture-answer".into(),
            |state| crate::chat::send(state, &id, "2", "test"),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    assert!(
        receive_with_save(
            &mut state,
            &id,
            "fixture-answer".into(),
            |state| crate::chat::send(state, &id, "2", "test"),
            |_| Ok(())
        )
        .is_ok()
    );
    assert_eq!(state.runtime.runs[0].status, "continued");
    assert_eq!(state.discord_seen, vec!["fixture-answer"]);
}
#[test]
fn failed_discord_bot_commands_leave_no_bot_or_inbox() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    crate::chat::send(&mut state, &id, "Watch the farm", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        receive_with_save(
            &mut state,
            &id,
            "fixture-bot".into(),
            |state| crate::chat::send(state, &id, "/bot on", "test"),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    assert!(!crate::bots::defined(&state, &id));
    assert!(
        receive_with_save(
            &mut state,
            &id,
            "fixture-bot".into(),
            |state| crate::chat::send(state, &id, "/bot on", "test"),
            |_| Ok(())
        )
        .is_ok()
    );
    assert!(crate::bots::defined(&state, &id));
}
#[test]
fn shutdown_fences_success_preserves_diagnostics_queue_and_checkpoint() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].workspace = Some("previous-output".into());
    crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    let mut attempt = state.runtime.runs[0].attempts[0].clone();
    attempt.artifact = Some("uncommitted-output".into());
    let event = crate::usage::Activity {
        id: "command".into(),
        kind: "command_execution".into(),
        text: "build".into(),
        output: "retained diagnostics".into(),
        status: "completed".into(),
    };
    record_activity_for(
        &mut state,
        &id,
        attempt.workspace_id.as_deref().unwrap(),
        event,
    );
    crate::chat::send(&mut state, &chat, "Next turn", "test").unwrap();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        commit(
            &mut state,
            &id,
            Action::Task,
            attempt.clone(),
            Ok("done".into()),
            true,
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    commit(
        &mut state,
        &id,
        Action::Task,
        attempt,
        Ok("done".into()),
        true,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.runtime.runs[0].status, "interrupted");
    assert_eq!(
        crate::history::activity(
            state.runtime.runs[0].chat_id.as_deref(),
            &state.runtime.runs[0].attempts[0]
        )[0]
        .output,
        "retained diagnostics"
    );
    assert_eq!(state.runtime.runs[1].status, "queued");
    assert!(state.runtime.runs[1].attempts.is_empty());
    assert_eq!(state.chats[0].workspace.as_deref(), Some("previous-output"));
    assert_eq!(state.chats[0].messages.len(), 2);
}
#[test]
fn failed_result_checkpoint_restores_state_and_can_be_retried_or_cancelled() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    let mut attempt = state.runtime.runs[0].attempts[0].clone();
    attempt.artifact = Some("new-output".into());
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        commit(
            &mut state,
            &id,
            Action::Task,
            attempt.clone(),
            Ok("done".into()),
            false,
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    state.runtime.runs[0].status = "cancelling".into();
    assert!(
        commit(
            &mut state,
            &id,
            Action::Task,
            attempt,
            Ok("done".into()),
            false,
            |_| Ok(())
        )
        .is_ok()
    );
    assert_eq!(state.runtime.runs[0].status, "cancelled");
    assert!(state.chats[0].workspace.is_none());
}
#[test]
fn design_continuation_restores_pause_before_queued_followups_and_keeps_baseline() {
    let (mut state, chat) = crate::design::tests::paused();
    crate::chat::send(&mut state, &chat, "/steer Add tests", "test").unwrap();
    assert!(checkpoint_next(&mut state, |_| Ok(())).is_none());
    crate::chat::send(&mut state, &chat, "2", "test").unwrap();
    let index = checkpoint_next(&mut state, |_| Ok(())).unwrap();
    assert_eq!(index, 2);
    assert_eq!(
        state.runtime.runs[index].attempts[0]
            .parent_artifact
            .as_deref(),
        Some("paused-workspace")
    );
    assert_eq!(
        state.runtime.runs[index]
            .continuation
            .as_ref()
            .unwrap()
            .published_parent
            .as_deref(),
        Some("last-good")
    );
    assert_eq!(state.chats[0].workspace.as_deref(), Some("last-good"));
}
#[test]
fn failed_discord_cancel_checkpoint_preserves_run_and_retryable_delivery() {
    for status in ["queued", "running"] {
        let mut state = State::default();
        let id = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &id, "Keep my work", "test").unwrap();
        state.runtime.runs[0].status = status.into();
        state.runtime.runs[0].result = "Existing progress".into();
        let before = serde_json::to_vec(&state).unwrap();
        assert!(
            receive_with_save(
                &mut state,
                &id,
                "fixture-message".into(),
                |state| crate::chat::send(state, &id, "/cancel", "test"),
                |_| Err(std::io::Error::other("disk full"))
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        assert!(
            receive_with_save(
                &mut state,
                &id,
                "fixture-message".into(),
                |state| crate::chat::send(state, &id, "/cancel", "test"),
                |_| Ok(())
            )
            .is_ok()
        );
        assert_eq!(state.discord_seen, ["fixture-message"]);
        assert_eq!(
            state.runtime.runs[0].status,
            if status == "queued" {
                "cancelled"
            } else {
                "cancelling"
            }
        );
    }
}
#[test]
fn improvement_followups_and_ordinary_tasks_use_separate_workspace_lineages() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].workspace = Some("ordinary-files".into());
    state.chats[0].improve_workspace = Some("source-files".into());
    crate::chat::send(&mut state, &chat, "/improve discuss this design", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs[0].attempts[0].parent_artifact.as_deref(),
        Some("source-files")
    );
    state.runtime.runs[0].status = "completed".into();
    crate::chat::send(&mut state, &chat, "/improve off", "test").unwrap();
    crate::chat::send(&mut state, &chat, "Continue my project", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state.runtime.runs[1].attempts[0].parent_artifact.as_deref(),
        Some("ordinary-files")
    );
}
#[test]
fn child_uses_frozen_task_checkpoint_and_unique_writable_attempt() {
    let mut state = State::default();
    let parent = crate::chat::create(&mut state);
    state.chats[0].workspace = Some("published-task".into());
    state.chats[0].improve_workspace = Some("private-source".into());
    let child = crate::chat::fork(&mut state, &parent, "Inspect files", "test").unwrap();
    state.chats[0].workspace = Some("later-parent-output".into());
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let attempt = &state.runtime.runs[0].attempts[0];
    assert_eq!(attempt.parent_artifact.as_deref(), Some("published-task"));
    assert!(
        attempt
            .workspace_id
            .as_deref()
            .unwrap()
            .starts_with(&state.runtime.runs[0].id)
    );
    assert_eq!(
        state.runtime.runs[0].chat_id.as_deref(),
        Some(child.as_str())
    );
    assert_eq!(state.runtime.runs[0].action, Action::Task);
    assert_eq!(
        state.chats[0].workspace.as_deref(),
        Some("later-parent-output")
    );
    assert!(state.chats[1].improve_workspace.is_none());
}
#[test]
fn retry_cannot_rebind_to_an_added_folder_when_both_checkpoint_heads_are_empty() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state.chats[0].project = Some(crate::projects::Project {
        grant: Some("old-grant".into()),
        ..Default::default()
    });
    crate::chat::send(&mut state, &chat, "List files", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    state.runtime.runs[0].status = "queued".into();
    state.chats[0].project.as_mut().unwrap().grant = Some("new-grant".into());
    assert!(checkpoint_next(&mut state, |_| Ok(())).is_none());
    assert_eq!(state.runtime.runs[0].status, "blocked");
    assert_eq!(
        state.runtime.runs[0].attempts[0].project.as_deref(),
        Some("old-grant")
    );
}
#[test]
fn followups_capture_workspace_at_dispatch_and_retries_keep_their_parent() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "First", "test").unwrap();
    crate::chat::send(&mut state, &chat, "Second", "test").unwrap();
    assert_eq!(checkpoint_next(&mut state, |_| Ok(())), Some(0));
    assert!(state.runtime.runs[0].attempts[0].parent_artifact.is_none());
    state.runtime.runs[0].status = "completed".into();
    state.chats[0].workspace = Some("first-artifact".into());
    assert_eq!(checkpoint_next(&mut state, |_| Ok(())), Some(1));
    assert_eq!(
        state.runtime.runs[1].attempts[0].parent_artifact.as_deref(),
        Some("first-artifact")
    );
    state.runtime.runs[1].status = "failed".into();
    crate::chat::send(&mut state, &chat, "Third", "test").unwrap();
    let retry = state.runtime.runs[1].id.clone();
    state.runtime.action(
        "/retry-run",
        &std::collections::HashMap::from([("id".into(), retry)]),
    );
    assert_eq!(checkpoint_next(&mut state, |_| Ok(())), Some(2));
    state.runtime.runs[2].status = "completed".into();
    state.chats[0].workspace = Some("third-artifact".into());
    let bytes = serde_json::to_vec(&state).unwrap();
    let mut restored: State = serde_json::from_slice(&bytes).unwrap();
    // A later turn published: the retry is refused before any model run.
    assert_eq!(checkpoint_next(&mut restored, |_| Ok(())), None);
    assert_eq!(restored.runtime.runs[1].status, "blocked");
    assert!(
        restored.runtime.runs[1]
            .result
            .contains("Send a new message")
    );
    assert_eq!(restored.runtime.runs[1].attempts.len(), 1);
    restored.chats[0].workspace = Some("first-artifact".into());
    restored.runtime.runs[1].status = "queued".into();
    assert_eq!(checkpoint_next(&mut restored, |_| Ok(())), Some(1));
    assert_eq!(
        restored.runtime.runs[1].attempts[1]
            .parent_artifact
            .as_deref(),
        Some("first-artifact")
    );
}
#[test]
fn activity_updates_keep_attempt_identity_and_bounded_history() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Check build", "test").unwrap();
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let old_generation = state.runtime.runs[0].attempts[0]
        .workspace_id
        .clone()
        .unwrap();
    let run = state.runtime.runs[0].id.clone();
    let event = |id: usize, status: &str| crate::usage::Activity {
        id: id.to_string(),
        kind: "command_execution".into(),
        text: "cargo check".into(),
        output: String::new(),
        status: status.into(),
    };
    let revision = state.chats[0].revision;
    record_activity(&mut state, &run, event(0, "running"));
    record_activity(&mut state, &run, event(0, "completed"));
    assert_eq!(state.runtime.runs[0].attempts[0].activity.len(), 1);
    assert_eq!(
        state.runtime.runs[0].attempts[0].activity[0].status,
        "completed"
    );
    assert_eq!(state.chats[0].revision, revision + 2);
    for id in 1..70 {
        record_activity(&mut state, &run, event(id, "completed"));
    }
    assert_eq!(state.runtime.runs[0].attempts[0].activity.len(), 64);
    state.runtime.runs[0].status = "failed".into();
    state.runtime.action(
        "/retry-run",
        &std::collections::HashMap::from([("id".into(), run.clone())]),
    );
    assert!(!record_activity_for(
        &mut state,
        &run,
        &old_generation,
        event(99, "completed")
    ));
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let revision = state.chats[0].revision;
    assert!(!record_activity_for(
        &mut state,
        &run,
        &old_generation,
        event(99, "completed")
    ));
    assert_eq!(state.chats[0].revision, revision);
    assert!(state.runtime.runs[0].attempts[1].activity.is_empty());
    record_activity(&mut state, &run, event(0, "running"));
    assert_eq!(state.runtime.runs[0].attempts[0].activity.len(), 64);
    assert_eq!(state.runtime.runs[0].attempts[1].activity.len(), 1);
}
#[test]
fn failed_checkpoint_preserves_queue_and_dispatches_after_recovery() {
    let mut state = State::default();
    state.runtime.model = Some("queued-model".into());
    state
        .runtime
        .enqueue_for(
            "Check a file".into(),
            RunKind::Task,
            "website",
            Actor::LocalOperator,
            Action::Task,
        )
        .unwrap();
    state.runtime.model = Some("later-model".into());
    assert!(
        checkpoint_next(&mut state, |_| Err(std::io::Error::other(
            "simulated full disk"
        )))
        .is_none()
    );
    assert_eq!(state.runtime.runs[0].status, "queued");
    assert!(state.runtime.runs[0].attempts.is_empty());
    assert_eq!(checkpoint_next(&mut state, |_| Ok(())), Some(0));
    assert_eq!(state.runtime.runs[0].status, "running");
    assert_eq!(state.runtime.runs[0].attempts.len(), 1);
    assert_eq!(state.runtime.runs[0].attempts[0].model, "queued-model");
}
#[test]
fn user_workspace_has_no_control_source_or_configuration() {
    let root = std::env::temp_dir().join(format!(
        "control-user-scope-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let baseline = root.join("baseline");
    let workspace = root.join("workspace");
    prepare_workspace(Workspace::User, &baseline, &workspace).unwrap();
    assert_eq!(fs::read_dir(&baseline).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&workspace).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn source_workspace_snapshots_required_build_inputs_and_explains_failures() {
    let root =
        std::env::temp_dir().join(format!("control-source-workspace-{}", std::process::id()));
    let source = root.join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("hosting")).unwrap();
    fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(source.join("build.rs"), "fn main() {}").unwrap();
    fs::write(source.join("hosting/worker-seccomp.json"), "{}").unwrap();
    fs::write(source.join("hosting/.env"), "fake-secret").unwrap();
    fs::write(source.join(".env"), "fake-secret").unwrap();
    let baseline = root.join("baseline");
    let workspace = root.join("workspace");
    prepare_source_workspace(&source, &baseline, &workspace).unwrap();
    for directory in [&baseline, &workspace] {
        assert!(directory.join("src/main.rs").is_file());
        assert!(directory.join("build.rs").is_file());
        assert!(directory.join("hosting/worker-seccomp.json").is_file());
        assert!(!directory.join(".env").exists());
        assert!(!directory.join("hosting/.env").exists());
    }
    let blocked = root.join("blocked");
    fs::write(&blocked, "preserve me").unwrap();
    assert!(
        prepare_source_workspace(&source, &blocked, &root.join("never-created"))
            .unwrap_err()
            .to_string()
            .contains("snapshot the configured Control checkout")
    );
    assert!(!root.join("never-created").exists());
    assert_eq!(fs::read(&blocked).unwrap(), b"preserve me");
    assert!(
        prepare_source_workspace(&source, &root.join("second-baseline"), &blocked)
            .unwrap_err()
            .to_string()
            .contains("copy the source snapshot")
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn untrusted_jobs_are_rejected_before_container_or_credential_access() {
    let result = execute(
        "test",
        "modify Control",
        &Actor::Unknown,
        Action::Improve,
        &mut crate::usage::Attempt::new(),
        None,
    );
    assert!(matches!(result, Err(("blocked", _))));
}

#[test]
fn judge_dispatch_captures_readonly_policy_and_cannot_publish_a_development_head() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &id, "Review this source", "test").unwrap();
    let root = state.runtime.runs[0].id.clone();
    state.runtime.runs[0].persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: root.clone(),
        depth: 1,
        parent: Some("parent".into()),
    });
    state.chats[0].workspace = Some("immutable-source".into());
    let index = checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let mut attempt = state.runtime.runs[index].attempts[0].clone();
    assert!(attempt.review_only);
    assert_eq!(attempt.parent_artifact.as_deref(), Some("immutable-source"));
    // A successful review has no published source output.
    attempt.artifact = Some("must-not-publish".into());
    commit(
        &mut state,
        &root,
        Action::Task,
        attempt,
        Ok("Review evidence".into()),
        false,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        state.chats[0].workspace.as_deref(),
        Some("immutable-source")
    );
    assert!(state.chats[0].improve_workspace.is_none());
}

#[test]
#[ignore = "requires a locally pinned worker image and Linux container engine; no model calls"]
fn live_judge_source_is_readonly_in_both_execution_adapters() {
    use std::os::unix::fs::MetadataExt;
    let image =
        std::env::var("CONTROL_QA_IMAGE").expect("Set CONTROL_QA_IMAGE to a pinned image ID");
    assert!(runtime::pinned_image(&image));
    let root = std::env::temp_dir().join(format!(
        "control-judge-qa-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = root.join("source");
    let scratch = root.join("scratch");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir(&scratch).unwrap();
    fs::write(source.join("code.txt"), "immutable-source").unwrap();
    let meta = fs::metadata(&root).unwrap();
    let auth = Credentials {
        path: root.join("synthetic-auth.json"),
        uid: meta.uid(),
        gid: meta.gid(),
    };
    fs::write(&auth.path, b"{}").unwrap();
    let policy = root.join("seccomp.json");
    fs::write(&policy, include_bytes!("../../hosting/worker-seccomp.json")).unwrap();
    let name = format!("control-judge-codex-qa-{}", std::process::id());
    let mut command = container_command_scoped(
        &image,
        &name,
        &scratch,
        &auth,
        &policy,
        "synthetic-qa-node",
        Some(&source),
    )
    .unwrap();
    command.args(["codex", "sandbox", "-P", "control"]);
    permissions(&mut command);
    command.args(["--","bash","-c","set -eu; test \"$(cat /review/code.txt)\" = immutable-source; if printf poisoned > /review/code.txt 2>/dev/null; then exit 42; fi; printf scratch-ok > /workspace/result; printf tmp-ok > /tmp/result"]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "Synthetic judge mount probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(source.join("code.txt")).unwrap(),
        "immutable-source"
    );
    assert_eq!(
        fs::read_to_string(scratch.join("result")).unwrap(),
        "scratch-ok"
    );
    let result=crate::claude::shell_scoped(&image,&format!("control-judge-claude-qa-{}",std::process::id()),"synthetic-qa-node",&source,"set -eu; if printf poisoned > /workspace/code.txt 2>/dev/null; then exit 42; fi; printf tmp-ok > /tmp/result; cat /workspace/code.txt",true).unwrap();
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "immutable-source");
    assert_eq!(
        fs::read_to_string(source.join("code.txt")).unwrap(),
        "immutable-source"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn credentials_are_readonly_mounts_and_never_environment_values() {
    let auth = Credentials {
        path: PathBuf::from("/private/auth.json"),
        uid: 502,
        gid: 20,
    };
    let command = container_command(
        "sha256:test",
        "control-test",
        Path::new("/private/workspace"),
        &auth,
        Path::new("hosting/worker-seccomp.json"),
        "test-node",
    )
    .unwrap();
    let args: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
    assert!(
        args.contains(
            &"type=bind,src=/private/auth.json,dst=/home/agent/.codex/auth.json,readonly"
        )
    );
    assert!(
        !args
            .iter()
            .any(|s| s.contains("API_KEY") || s.contains("ACCESS_TOKEN"))
    );
    assert!(!command.get_envs().any(|(name, _)| name == "CODEX_API_KEY"
        || name == "OPENAI_API_KEY"
        || name == "CODEX_ACCESS_TOKEN"));
    assert!(!args.contains(&"--privileged"));
    assert!(args.contains(&"--interactive"));
    assert!(!args.contains(&"--security-opt=seccomp=unconfined"));
    assert!(PERMISSIONS.contains(&"approval_policy=\"never\""));
}

#[test]
fn diagnostics_return_fixed_messages_without_echoing_secret_text() {
    let failure = worker_failure(b"401 Unauthorized bearer fake-secret-for-test");
    assert!(failure.contains("authentication was rejected"));
    assert!(!failure.contains("fake-secret-for-test"));
    assert!(worker_failure(b"Invalid API key fake-secret-for-test").contains("authentication was rejected"));
    assert_eq!(
        worker_failure(b"unexpected raw text fake-secret-for-test"),
        UNKNOWN_FAILURE
    );
}

#[test]
fn mounts_reject_csv_injection_and_reports_reject_symlinks() {
    assert!(mount_path(Path::new("/tmp/auth,readonly=false")).is_err());
    assert!(mount_path(Path::new("relative/auth.json")).is_err());
    let root =
        std::env::temp_dir().join(format!("control-credentials-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let file = root.join("auth.json");
    fs::write(&file, b"harmless-canary").unwrap();
    use std::os::unix::fs::{PermissionsExt, symlink};
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Credentials::validate(&file).is_err());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Credentials::validate(&file).is_ok());
    let link = root.join("report-link");
    symlink(&file, &link).unwrap();
    assert!(Credentials::validate(&link).is_err());
    assert!(read_report(&link).is_err());
    fs::remove_dir_all(root).unwrap();
}
