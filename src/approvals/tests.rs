
use super::*;

fn requested() -> (State, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Run the server", "test").unwrap();
    state.runtime.runs[0].model = Some("captured-model".into());
    let origin = state.runtime.runs[0].id.clone();
    (state, chat, origin)
}
fn decide(state: &mut State, chat: &str, origin: &str) -> Outcome {
    resume(
        state,
        Decisions {
            chat,
            origins: &[origin],
            heading: "Port decisions",
            lines: "relay: approved",
            instruction: "Continue.",
            task: "Run the server",
            kind: RunKind::PortDecision,
            source: "port-decision",
            grants: Vec::new(),
        },
    )
}

#[test]
fn decisions_wait_for_the_turn_then_resume_with_its_context() {
    let (mut state, chat, origin) = requested();
    assert_eq!(
        decide(&mut state, &chat, &origin),
        Outcome::Wait,
        "the requesting turn is still running"
    );
    state.runtime.runs[0].status = "completed".into();
    let Outcome::Resumed(id) = decide(&mut state, &chat, &origin) else {
        panic!("resumes")
    };
    let run = state.runtime.runs.last().unwrap();
    assert_eq!(
        (
            run.id.as_str(),
            run.kind,
            run.chat_id.as_deref(),
            run.model.as_deref()
        ),
        (
            id.as_str(),
            RunKind::PortDecision,
            Some(chat.as_str()),
            Some("captured-model")
        )
    );
    assert!(run.objective.starts_with(
        "Operator port decisions:\nrelay: approved\nContinue. Original task:\nRun the server"
    ));
    assert_eq!(
        state.chats[0].messages.last().unwrap().run_id.as_deref(),
        Some(id.as_str())
    );
}

#[test]
fn human_turns_after_the_request_get_a_note_and_design_answers_do_not_count() {
    let (mut state, chat, origin) = requested();
    state.runtime.runs[0].status = "completed".into();
    let mut answer = state.runtime.runs[0].clone();
    answer.id = "answer".into();
    answer.kind = RunKind::DesignAnswer;
    state.runtime.runs.push(answer);
    assert!(
        matches!(decide(&mut state, &chat, &origin), Outcome::Resumed(_)),
        "an answered design question continues the same turn"
    );
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    crate::chat::send(&mut state, &chat, "Stop the playtest", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    let runs = state.runtime.runs.len();
    assert_eq!(decide(&mut state, &chat, &origin), Outcome::Noted);
    assert_eq!(state.runtime.runs.len(), runs);
    assert_eq!(
        state.chats[0].messages.last().unwrap().text,
        "Port decisions\nrelay: approved"
    );
}

#[test]
fn a_requester_without_authority_gets_a_note_not_a_retry() {
    let (mut state, chat, origin) = requested();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].actor = crate::identity::Actor::Discord {
        user_id: "1".parse().unwrap(),
        channel_id: "2".parse().unwrap(),
    };
    assert_eq!(decide(&mut state, &chat, &origin), Outcome::Noted);
}

#[test]
fn a_full_scheduler_defers_the_resume() {
    let (mut state, chat, origin) = requested();
    state.runtime.runs[0].status = "completed".into();
    while crate::scheduler::admit(&state.runtime).is_ok() {
        state
            .runtime
            .enqueue("Queued".into(), RunKind::Task, "test");
    }
    assert_eq!(decide(&mut state, &chat, &origin), Outcome::Wait);
}
