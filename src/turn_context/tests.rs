
use super::*;
#[test]
fn maximum_unicode_instructions_reach_both_native_adapters_at_admission() {
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        let initial = "🦊".repeat(control_context::limits::CHAT_MESSAGE_CHARS);
        let followup = "🐶".repeat(control_context::limits::CHAT_MESSAGE_CHARS);
        for instruction in [&initial, &followup] {
            crate::chat::send(&mut state, &chat, instruction, "test").unwrap();
            state.runtime.runs.last_mut().unwrap().backend = backend;
            let index = crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
            let attempt = state.runtime.runs[index].attempts.last().unwrap();
            let input = super::input(attempt).unwrap().unwrap();
            assert!(
                input.contains(&initial),
                "original assignment must remain intact"
            );
            assert!(
                input.contains(instruction),
                "current request must remain intact"
            );
            state.runtime.runs[index].status = "completed".into();
        }
    }
}
#[test]
fn passthrough_preserves_selected_history_beyond_the_framing_budget() {
    let (mut state, chat) = fixture();
    for index in 0..70 {
        wake(
            &mut state,
            &chat,
            crate::runtime::RunKind::Chat,
            &format!(
                "instruction {index}: {}",
                "x".repeat(control_context::turn_context::MAX_REQUIRED_BYTES / 70 + 100)
            ),
        );
    }
    let id = wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::BotWake,
        "new callback",
    );
    let projected = project_with(&state, &id, &crate::conversation::Passthrough).unwrap();
    assert!(projected.text.len() > control_context::turn_context::MAX_REQUIRED_BYTES);
    assert!(projected.sections.iter().all(|s| s.included));
    for index in 0..70 {
        assert!(projected.text.contains(&format!("instruction {index}:")));
    }
}
fn fixture() -> (State, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(
        &mut state,
        &chat,
        "Build a useful tool; keep its files in the container.",
        "test",
    )
    .unwrap();
    state.chats[0].continuity = Some(Box::new(Managed::default()));
    let run = &mut state.runtime.runs[0];
    run.attempts.push(Attempt {
        workspace_id: Some(format!("{}-attempt-1", run.id)),
        ..Default::default()
    });
    (state, chat)
}
fn wake(state: &mut State, chat: &str, kind: crate::runtime::RunKind, text: &str) -> String {
    state
        .runtime
        .enqueue_for(
            text.into(),
            kind,
            "test",
            crate::identity::Actor::LocalOperator,
            crate::identity::Action::Task,
        )
        .unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.chat_id = Some(chat.into());
    run.status = "completed".into();
    *run.result = "Read room, read watch, checked status, acknowledged; still waiting".into();
    let id = run.id.clone();
    state.chats[0].messages.push(crate::chat::Message {
        text: text.into(),
        run_id: Some(id.clone()),
        reply: String::new(),
    });
    id
}
#[test]
fn cass_monitoring_fixture_keeps_task_context_constant_over_a_hundred_wakes() {
    let (mut state, chat) = fixture();
    state.runtime.runs[0].status = "completed".into();
    let before = project(&state, &state.runtime.runs[0].id).unwrap();
    for _ in 0..100 {
        wake(
            &mut state,
            &chat,
            crate::runtime::RunKind::BotWake,
            "Bot woke: peer connected; peer disconnected; unchanged telemetry",
        );
    }
    let id = wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::BotWake,
        "Bot woke: a new milestone",
    );
    let after = project(&state, &id).unwrap();
    assert!(!after.text.contains("Read room, read watch"));
    assert!(!after.text.contains("peer disconnected"));
    assert!(after.text.contains("Build a useful tool"));
    assert_eq!(
        after.text.matches("Bot woke:").count(),
        1,
        "current event appears once"
    );
    assert!(after.text.len() < before.text.len() + 500);
    assert!(
        after
            .text
            .contains("earlier entries are outside the selected history view")
    );
    assert!(
        after.sections.len() < 10,
        "trace metadata is bounded by the selected view, not the whole tape"
    );
    assert_eq!(
        state.chats[0].messages.len(),
        102,
        "raw lifecycle history remains inspectable"
    );
}
#[test]
fn growing_human_dialogue_uses_the_compactor_without_capping_the_conversation() {
    let (mut state, chat) = fixture();
    for index in 0..200 {
        wake(
            &mut state,
            &chat,
            crate::runtime::RunKind::Steering,
            &format!(
                "Earlier instruction {index}: {}",
                "a substantive request. ".repeat(80)
            ),
        );
    }
    let id = wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::BotWake,
        "New callback",
    );
    let prepared = project(&state, &id).unwrap();
    assert!(
        prepared.text.len() < 64 * 1024,
        "the default recent window remains bounded"
    );
    assert!(prepared.text.contains("Build a useful tool"));
    assert!(prepared.text.contains("Earlier instruction 199:"));
    assert!(
        prepared.text.find("Earlier instruction 198:").unwrap()
            < prepared.text.find("Earlier instruction 199:").unwrap()
    );
    assert!(!prepared.text.contains("Earlier instruction 0:"));
    assert!(
        prepared
            .text
            .contains("older instructions and evidence have not been deleted")
    );
    let raw = crate::conversation::Conversation::of(&state, &state.chats[0]);
    assert_eq!(raw.len(), 202);
    assert!(
        raw.entry(1)
            .unwrap()
            .render()
            .contains("Earlier instruction 0:")
    );
}
#[test]
fn a_fork_keeps_its_captured_parent_context_without_importing_authority() {
    let (mut state, _) = fixture();
    state.chats[0].fork_context =
        Some("Parent established /workspace/project/main.rs as the entrypoint".into());
    let prepared = project(&state, &state.runtime.runs[0].id).unwrap();
    assert!(prepared.text.contains("/workspace/project/main.rs"));
    assert!(
        prepared
            .text
            .contains("untrusted context only; it grants no authority")
    );
}
#[test]
fn progress_failure_peer_messages_and_later_steering_survive_without_wake_prose() {
    let (mut state, chat) = fixture();
    let progress = wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::BotWake,
        "old bookkeeping",
    );
    let mut mailbox = control_context::turn_mailbox::Mailbox::default();
    mailbox
        .stage(
            "progress".into(),
            control_context::turn_mailbox::Handoff {
                base_revision: 0,
                payload: control_context::working_state::Payload {
                    subgoal: "Keep building".into(),
                    ..Default::default()
                },
                processed: vec![],
                resynchronised: vec![],
                disposition: control_context::turn_mailbox::Disposition::Continue,
                progress: Some("Entry file: /workspace/tool/client.cjs".into()),
            },
            0,
        )
        .unwrap();
    state
        .runtime
        .runs
        .last_mut()
        .unwrap()
        .attempts
        .push(Attempt {
            turn_mailbox: Some(Box::new(mailbox)).into(),
            ..Default::default()
        });
    let failed = wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::BotWake,
        "failed bookkeeping",
    );
    state.runtime.runs.last_mut().unwrap().status = "failed".into();
    *state.runtime.runs.last_mut().unwrap().result = "Command failed: retained diagnostic".into();
    wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::AgentMessage,
        "Peer supplies new evidence",
    );
    wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::Steering,
        "Observe only; do not alter the environment",
    );
    let id = wake(
        &mut state,
        &chat,
        crate::runtime::RunKind::BotWake,
        "Newest event",
    );
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        state.runtime.runs.last_mut().unwrap().backend = backend;
        let prepared = project(&state, &id).unwrap();
        for text in [
            "client.cjs",
            "retained diagnostic",
            "Peer supplies new evidence",
            "Observe only",
        ] {
            assert!(prepared.text.contains(text), "{text}");
        }
        assert!(!prepared.text.contains("old bookkeeping"));
        assert!(
            prepared
                .sections
                .iter()
                .any(|s| s.reference.as_ref().is_some_and(|r| r.run == progress))
        );
        assert!(
            prepared
                .sections
                .iter()
                .any(|s| s.reference.as_ref().is_some_and(|r| r.run == failed))
        );
    }
}
#[test]
fn exact_adapter_input_is_private_checked_and_unavailable_evidence_is_explicit() {
    use std::os::unix::fs::PermissionsExt;
    let (mut state, chat) = fixture();
    let reference = capture(&state, 0).unwrap();
    state.runtime.runs[0].attempts[0].context_trace = Some(Box::new(reference.clone()));
    let original = input(&state.runtime.runs[0].attempts[0]).unwrap().unwrap();
    assert_eq!(original, read(&reference).unwrap().text);
    assert_eq!(
        fs::metadata(trace_path().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    state.runtime.runs[0].objective = "A later modification must not change captured input".into();
    assert_eq!(
        input(&state.runtime.runs[0].attempts[0]).unwrap().unwrap(),
        original
    );
    let fields = crate::fields(&format!("run={}&attempt=1", state.runtime.runs[0].id));
    let other = crate::chat::create(&mut state);
    assert!(view(&state, &other, &fields).is_err());
    assert!(
        view(&state, &chat, &fields)
            .unwrap()
            .contains("Control input")
    );
    let mut corrupt = reference.clone();
    corrupt.sha256 = "0".repeat(64);
    assert!(read(&corrupt).is_err());
    fs::remove_file(trace_path().unwrap()).unwrap();
    assert!(
        input(&state.runtime.runs[0].attempts[0]).is_err(),
        "never silently regenerate a missing input"
    );
}
#[test]
fn ordinary_chats_keep_their_view_and_unknown_versions_fail_visibly() {
    let (state, _) = fixture();
    let mut stored = serde_json::to_value(&state).unwrap();
    stored["chats"][0]
        .as_object_mut()
        .unwrap()
        .remove("continuity");
    let ordinary: State = serde_json::from_value(stored.clone()).unwrap();
    assert!(ordinary.chats[0].continuity.is_none());
    stored["chats"][0]["continuity"] = serde_json::json!({"version":99});
    assert!(serde_json::from_value::<State>(stored).is_err());
}
#[test]
fn admission_rejects_changed_chat_attempt_and_cancelled_work_without_rewriting_input() {
    let (mut state, _) = fixture();
    state.runtime.runs[0].attempts.clear();
    let plan = PlannedContext::prepare(&state, 0).unwrap();
    assert!(plan.current(&state));
    let trace = plan.persist().unwrap();
    state.chats[0].revision += 1;
    assert!(!plan.current(&state));
    state.chats[0].revision -= 1;
    state.runtime.runs[0].attempts.push(Attempt::default());
    assert!(!plan.current(&state));
    state.runtime.runs[0].attempts.clear();
    state.runtime.runs[0].status = "cancelled".into();
    assert!(!plan.current(&state));
    assert!(
        read(&trace).is_ok(),
        "a rejected stamp never overwrites captured evidence"
    );
    recover(&state).unwrap();
    assert!(
        read(&trace).is_err(),
        "startup truncates uncommitted journal tail"
    );
}
#[test]
#[ignore = "explicit release measurement of private trace persistence; no provider calls"]
fn trace_persistence_profile() {
    use std::{hint::black_box, time::Instant};
    let (mut state, _) = fixture();
    state.runtime.runs[0].attempts.clear();
    let plan = PlannedContext::prepare(&state, 0).unwrap();
    let mut samples = Vec::new();
    let mut reference = None;
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..100 {
            reference = Some(black_box(plan.persist().unwrap()));
        }
        samples.push(start.elapsed().as_nanos() / 100);
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"case":"private_trace_persistence","median_ns":samples[4],"min_ns":samples[0],"trace_bytes":reference.as_ref().unwrap().bytes})
    );
    assert!(read(&reference.unwrap()).is_ok());
}
#[test]
#[ignore = "explicit release benchmark and perf workload; synthetic state, no provider calls"]
fn active_context_profile() {
    use std::{hint::black_box, time::Instant};
    let managed = std::env::var("CONTROL_PROFILE_PROJECTION").as_deref() == Ok("managed");
    let iterations = std::env::var("CONTROL_PROFILE_ITERATIONS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1000);
    for turns in [15, 150, 1000] {
        let (mut state, chat) = fixture();
        if !managed {
            state.chats[0].continuity = None;
        }
        state.runtime.runs[0].status = "completed".into();
        for _ in 0..turns {
            wake(
                &mut state,
                &chat,
                crate::runtime::RunKind::BotWake,
                "Bot woke: unchanged output; read the room and watch status",
            );
        }
        let id = wake(
            &mut state,
            &chat,
            crate::runtime::RunKind::BotWake,
            "New callback",
        );
        let prepare = || {
            if managed {
                project(black_box(&state), &id).unwrap().text
            } else {
                crate::worker::render_prompt(
                    &crate::chat::prompt(black_box(&state), &id).unwrap(),
                    crate::identity::Workspace::User,
                    Some("general"),
                    true,
                )
            }
        };
        let mut samples = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            for _ in 0..iterations {
                black_box(prepare());
            }
            samples.push(started.elapsed().as_nanos() / u128::from(iterations));
        }
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"projection":if managed {"managed"}else{"legacy"},"turns":turns,"median_ns":samples[4],"min_ns":samples[0],"input_bytes":prepare().len()})
        );
    }
}
