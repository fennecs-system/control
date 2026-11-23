use super::*;
use crate::config::unix_seconds as now;
use crate::runtime::RunStatus;
use serde_json::json;
use std::sync::{Arc, Mutex};

/// A running managed attempt in a fresh chat, started at Unix second 1000.
fn running(timeout: Option<u64>) -> (State, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Steer the team", "test").unwrap();
    crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let run = &mut state.runtime.runs[0];
    assert_eq!(run.status, RunStatus::Running);
    let attempt = run.attempts.last_mut().unwrap();
    assert!(attempt.turn_mailbox.is_some());
    (attempt.started_unix, attempt.explicit_timeout_seconds) = (1000, timeout);
    let id = run.id().to_string();
    (state, chat, id)
}
/// A member chat spawned from `parent`, with its latest turn in `status`.
fn member(state: &mut State, parent: &str, status: &str) -> String {
    let child = crate::chat::create(state);
    state
        .chats
        .iter_mut()
        .find(|session| session.id == child)
        .unwrap()
        .parent_chat = Some(parent.into());
    crate::chat::send(state, &child, "Member work", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = RunStatus::named(status);
    child
}

#[test]
fn deadline_threshold_is_ninety_seconds_or_fifteen_percent() {
    assert_eq!(deadline_threshold(600), 90);
    assert_eq!(deadline_threshold(3600), 540);
    assert_eq!(deadline_threshold(300), 90);
    // A short limit is not warned about on its first tool call.
    assert_eq!(deadline_threshold(120), 60);
    assert_eq!(deadline_threshold(0), 0);
}

#[test]
fn deadline_warning_is_sent_once_per_attempt_below_the_threshold() {
    // The default 600 s limit of an ordinary attempt applies.
    let (mut state, _, run) = running(None);
    assert!(
        notes(&mut state, &run, "control_room_read", 1000 + 500).is_empty(),
        "100 s left is above the threshold"
    );
    let sent = notes(&mut state, &run, "control_room_read", 1000 + 520);
    assert_eq!(sent, [deadline_text(80)]);
    assert_eq!(
        sent[0],
        "80 s left in this turn: publish your checkpoint (control_turn_handoff) now and continue in a routine or the next turn. Record your key findings (facts, diagnoses) and current plan in its working notes, payload.findings and payload.next, so they survive compaction; an omitted payload keeps the older notes."
    );
    assert!(
        notes(&mut state, &run, "control_room_read", 1000 + 560).is_empty(),
        "once per attempt"
    );
    // A new attempt warns again.
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        started_unix: 2000,
        ..Default::default()
    });
    assert_eq!(
        notes(&mut state, &run, "control_room_read", 2000 + 595),
        [deadline_text(5)]
    );
}

#[test]
fn explicit_limits_replace_the_default_and_finished_runs_are_not_nudged() {
    let (mut state, _, run) = running(None);
    // An explicit operator limit replaces the default.
    state.runtime.runs[0]
        .attempts
        .last_mut()
        .unwrap()
        .explicit_timeout_seconds = Some(7200);
    assert!(notes(&mut state, &run, "control_room_read", 1000 + 600).is_empty());
    assert_eq!(
        notes(&mut state, &run, "control_room_read", 1000 + 7200 - 1000),
        [deadline_text(1000)]
    );
    // Finished runs are not nudged.
    let (mut state, _, run) = running(None);
    state.runtime.runs[0].status = RunStatus::Failed;
    assert!(notes(&mut state, &run, "control_room_read", 1000 + 590).is_empty());
}

#[test]
fn approved_bots_are_warned_before_their_captured_turn_limit() {
    let (mut state, chat, run) = running(None);
    state.chats[0].room = Some("general".into());
    crate::bots::command(&mut state, &chat, "on").unwrap();
    assert!(crate::bots::continuation_enabled(&state, &chat));
    // Captured at admission: the default 1200 s bot limit, threshold 180 s.
    state.runtime.runs[0].attempts.last_mut().unwrap().bot_timeout_seconds = Some(1200);
    assert!(
        notes(&mut state, &run, "control_room_read", 1000 + 600).is_empty(),
        "the ordinary 600 s limit does not apply"
    );
    assert_eq!(
        notes(&mut state, &run, "control_room_read", 1000 + 1100),
        [deadline_text(100)]
    );
    // A bot limit configured as unlimited gives no warning.
    let (mut state, chat, run) = running(None);
    state.chats[0].room = Some("general".into());
    crate::bots::command(&mut state, &chat, "on").unwrap();
    state.runtime.runs[0].attempts.last_mut().unwrap().bot_timeout_seconds = None;
    assert!(notes(&mut state, &run, "control_room_read", 1000 + 86_000).is_empty());
}

#[test]
fn context_notes_belong_to_the_local_loop_and_never_reach_hosted_sessions() {
    // Codex and Claude manage their own context: their tool results carry only
    // the deadline and steering notes, however long the attempt runs.
    let (mut state, _, run) = running(Some(3600));
    for second in (0..3600).step_by(60) {
        assert!(
            notes(&mut state, &run, "control_workspace_run", 1000 + second)
                .iter()
                .all(|note| !note.contains("% full"))
        );
    }
    // The note asks for findings and the plan in the working notes (the
    // handoff payload) before compaction loses them, or marked lines without one.
    let note = context_text(81, Next::Turn);
    assert!(
        note.starts_with(
            "Context 81% full: publish your checkpoint (control_turn_handoff) and continue in the next turn."
        ),
        "{note}"
    );
    assert!(
        note.contains("working notes, payload.findings and payload.next") && note.contains("Finding:"),
        "{note}"
    );
    assert_eq!(
        context_text(81, Next::Report),
        format!("Context 81% full: finish with a brief final report soon. {MARKS}")
    );
    assert!(steps_text(4, Next::Turn).contains("payload.findings and payload.next"));
    // A chat with the handoff tool but no persistent bot has no next turn, so
    // the note names what would give it one and never promises a continuation.
    assert!(
        matches!(Next::of(true, true, false), Next::Turn)
            && matches!(Next::of(true, false, false), Next::Report)
            && matches!(Next::of(false, true, true), Next::Report)
    );
    for note in [
        context_text(81, Next::of(true, false, true)),
        steps_text(4, Next::of(true, false, true)),
    ] {
        assert!(
            note.contains("this chat has no next turn")
                && note.contains("control_bot_request")
                && !note.contains("continue"),
            "{note}"
        );
    }
}
#[test]
fn local_model_attempts_are_warned_before_their_captured_local_limit() {
    // A local chat and a local bot both use the captured local limit (default
    // 3600 s, threshold 540 s), not the 600 s or bot defaults.
    for bot in [false, true] {
        let (mut state, chat, run) = running(None);
        if bot {
            state.chats[0].room = Some("general".into());
            crate::bots::command(&mut state, &chat, "on").unwrap();
        }
        let attempt = state.runtime.runs[0].attempts.last_mut().unwrap();
        (
            attempt.backend,
            attempt.bot_timeout_seconds,
            attempt.local_timeout_seconds,
        ) = (crate::backend::Backend::Local, Some(1200), Some(3600));
        assert!(
            notes(&mut state, &run, "control_room_read", 1000 + 1100).is_empty(),
            "neither 600 s nor the bot limit applies"
        );
        assert!(
            notes(&mut state, &run, "control_room_read", 1000 + 3000).is_empty(),
            "600 s left is above the threshold"
        );
        assert_eq!(
            notes(&mut state, &run, "control_room_read", 1000 + 3100),
            [deadline_text(500)]
        );
    }
    // A local limit configured as unlimited gives no warning.
    let (mut state, _, run) = running(None);
    let attempt = state.runtime.runs[0].attempts.last_mut().unwrap();
    (attempt.backend, attempt.local_timeout_seconds) = (crate::backend::Backend::Local, None);
    assert!(notes(&mut state, &run, "control_room_read", 1000 + 86_000).is_empty());
}

#[test]
fn steering_detection_counts_only_active_descendants() {
    let (mut state, lead, _) = running(None);
    assert_eq!(steered(&state, &lead), 0, "no members");
    let idle = member(&mut state, &lead, "completed");
    assert_eq!(steered(&state, &lead), 0, "a settled member is not active");
    member(&mut state, &lead, "running");
    member(&mut state, &lead, "queued");
    // Members spawned by a member are steered transitively.
    member(&mut state, &idle, "waiting");
    assert_eq!(steered(&state, &lead), 3);
    assert_eq!(steered(&state, &idle), 1, "a sub-lead steers its own members");
    // An unrelated running chat is not a member.
    let other = crate::chat::create(&mut state);
    member(&mut state, &other, "running");
    assert_eq!(steered(&state, &lead), 3);
}

#[test]
fn steering_suggestion_after_long_turn_or_many_execs_once() {
    let (mut state, lead, run) = running(Some(86_400));
    // Below both thresholds, or without members, nothing is suggested.
    assert!(
        notes(&mut state, &run, "control_room_read", 1000 + 300).is_empty(),
        "no members to delegate to"
    );
    for _ in 0..2 {
        member(&mut state, &lead, "running");
    }
    // The run under test must stay the latest running turn of the lead.
    assert!(notes(&mut state, &run, "control_room_read", 1000 + 60).is_empty());
    let sent = notes(&mut state, &run, "control_room_read", 1000 + 240);
    assert_eq!(
        sent,
        ["You are steering 2 active members: delegate this work to a member and end your turn."]
    );
    assert!(
        notes(&mut state, &run, "control_room_read", 1000 + 400).is_empty(),
        "once per attempt"
    );

    // Several command executions trigger it before the time threshold.
    let (mut state, lead, run) = running(Some(86_400));
    member(&mut state, &lead, "running");
    state.runtime.execution.steering_exec_calls = 3;
    for _ in 0..2 {
        assert!(notes(&mut state, &run, "control_service_exec", 1000 + 10).is_empty());
    }
    assert!(
        notes(&mut state, &run, "control_room_post", 1000 + 10).is_empty(),
        "other tools are not executions"
    );
    assert_eq!(
        notes(&mut state, &run, "control_workspace_run", 1000 + 10),
        [steering_text(1)]
    );
    assert_eq!(
        state.runtime.runs[0].attempts.last().unwrap().nudges,
        Nudges {
            execs: 3,
            deadline: false,
            steering: true
        }
    );
}

#[test]
fn nudges_are_appended_to_tool_results_through_the_broker() {
    let (mut state, lead, run) = running(None);
    member(&mut state, &lead, "running");
    // An attempt that started long ago is near its default deadline and long.
    let started = now().saturating_sub(560);
    state.runtime.runs[0].attempts.last_mut().unwrap().started_unix = started;
    let shared = Arc::new(Mutex::new(state));
    let call = |nonce: &str| {
        super::super::rpc_at(
            &shared,
            &run,
            &json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_tool_search","arguments":{}}}}),
            None,
        )
    };
    let response = call("1");
    let texts = response["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>();
    assert!(
        texts
            .iter()
            .any(|text| text.contains("s left in this turn: publish your checkpoint (control_turn_handoff)")),
        "{response}"
    );
    assert!(texts.contains(&steering_text(1).as_str()), "{response}");
    // Clients that show only structuredContent see the same notes.
    let structured = response["result"]["structuredContent"]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(structured, texts[1..], "{response}");
    let again = call("2");
    let texts = again["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !texts
            .iter()
            .any(|text| text.contains("left in this turn") || text.contains("You are steering")),
        "{again}"
    );
    assert!(again["result"]["structuredContent"].get("notes").is_none(), "{again}");
}

#[test]
fn nudges_serialise_only_when_set() {
    assert_eq!(
        serde_json::to_value(crate::usage::Attempt::default())
            .unwrap()
            .get("nudges"),
        None
    );
    let nudges = Nudges {
        execs: 2,
        deadline: true,
        steering: false,
    };
    assert_eq!(
        serde_json::to_value(&nudges).unwrap(),
        json!({"execs":2,"deadline":true})
    );
    assert_eq!(
        serde_json::from_value::<Nudges>(json!({"execs":2,"deadline":true})).unwrap(),
        nudges
    );
}
