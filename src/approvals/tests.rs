
use super::*;
mod profiling;

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
    let message = state.chats[0].messages.last().unwrap();
    assert_eq!(message.speaker(Some(run)), control_context::agent_policy::MessageSpeaker::Control, "a continuation speaks for Control");
    assert!(!message.is_notice(Some(run)), "it renders with its run, not as a bare notice");
    let html = crate::chat::transcript(&state, &chat).unwrap();
    assert!(html.contains("<p class='message-status' aria-label='Control update'>Port decisions\nrelay: approved</p>"), "{html}");
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
    while crate::scheduler::admit(&state.runtime).is_ok() {
        state.runtime.enqueue("Other queued work".into(), RunKind::Task, "test");
    }
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

/// Records a decision note: the requesting turn completed, then a later
/// automatic turn failed, so no continuation starts.
fn noted() -> (State, String, String) {
    let (mut state, chat, origin) = requested();
    state.runtime.runs[0].status = "completed".into();
    let mut wake = state.runtime.runs[0].clone();
    wake.id = "wake".into();
    wake.kind = RunKind::BotWake;
    wake.status = "failed".into();
    state.runtime.runs.push(wake);
    assert_eq!(decide(&mut state, &chat, &origin), Outcome::Noted);
    (state, chat, origin)
}

#[test]
fn a_noted_decision_is_a_control_notice_in_every_view() {
    use control_context::agent_policy::MessageSpeaker;
    let (state, chat, _) = noted();
    let note = state.chats[0].messages.last().unwrap();
    assert_eq!((note.run_id.as_deref(), note.speaker), (None, MessageSpeaker::Control));
    assert_eq!(serde_json::to_value(note).unwrap()["speaker"], "control");

    let html = crate::chat::transcript(&state, &chat).unwrap();
    assert!(html.contains("<p class='message-status preserve' aria-label='Control notice'>Control · Port decisions: relay: approved</p>"), "{html}");
    assert!(!html.contains("<p class='preserve'>Port decisions"), "not shown as operator input");
    assert_eq!(html.matches("aria-label='You'").count(), 1, "only the operator's own message");
    assert!(html.contains("<article class='user-message' aria-label='You'><p class='preserve'>Run the server</p></article>"));

    let detail = crate::chat::detail(&state, &chat).unwrap();
    assert_eq!(detail["messages"][1]["notice"], true);
    assert!(detail["messages"][0].get("notice").is_none(), "user messages are unchanged");
    assert!(serde_json::to_value(&state.chats[0].messages[0]).unwrap().get("speaker").is_none(), "user records keep their shape");

    let conversation = crate::conversation::Conversation::of(&state, &state.chats[0]);
    assert_eq!(conversation.entry(1).unwrap().render(), "Control event: Port decisions\nrelay: approved\n");
    assert!(conversation.entry(0).unwrap().render().starts_with("User: Run the server\n"));
}

#[test]
fn a_decision_note_is_not_a_human_turn() {
    let (mut state, chat, origin) = noted();
    // A later automatic turn completes; the note did not steer the task, so the
    // next decision for the same request resumes it.
    state.runtime.runs[1].status = "completed".into();
    assert!(matches!(decide(&mut state, &chat, &origin), Outcome::Resumed(_)));
    let conversation = crate::conversation::Conversation::of(&state, &state.chats[0]);
    let current = conversation.len() - 1;
    let window = crate::conversation::Window { items: Vec::new(), omitted: current };
    let carried = conversation.carry(current, &window).instructions;
    assert_eq!(carried, ["Run the server"], "notes are never retained as user instructions");
}

#[test]
fn notes_stored_before_speakers_are_recognised_by_their_heading() {
    use crate::application::approval_resume::HEADINGS;
    use control_context::agent_policy::MessageSpeaker;
    let message = |text: &str, run_id: Option<&str>| serde_json::from_value::<crate::chat::Message>(serde_json::json!({"text":text,"run_id":run_id,"reply":""})).unwrap();
    for heading in HEADINGS {
        let old = message(&format!("{heading}\nrelay (p-1): approved."), None);
        assert_eq!((old.speaker, old.speaker(None)), (MessageSpeaker::User, MessageSpeaker::Control));
        assert!(old.is_notice(None));
        assert!(!message(&format!("{heading}\nx"), Some("r-1")).is_notice(None), "linked messages are not legacy notes");
        assert!(!message(&format!("{heading} for the relay?"), None).is_notice(None), "the heading must be a line of its own");
    }
    assert!(!message("Please review the port decisions\nthanks", None).is_notice(None));
    let (mut state, chat, _) = requested();
    state.runtime.runs[0].status = "completed".into();
    state.chats[0].messages.push(message("Bot decisions\nhelper (c-1): denied", None));
    let html = crate::chat::transcript(&state, &chat).unwrap();
    assert!(html.contains("aria-label='Control notice'>Control · Bot decisions: helper (c-1): denied</p>"));
}
