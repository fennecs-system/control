//! Operator decisions that resume the agent which asked, shared by port and bot
//! requests. Callers keep their own request records and acknowledge them once
//! `resume` returns anything but `Wait`.
use crate::{State, runtime::RunKind};
use control_context::agent_policy::MessageSpeaker;

pub(crate) struct Decisions<'a> {
    /// Requester chat and the runs that made the requests.
    pub chat: &'a str,
    pub origins: &'a [&'a str],
    /// e.g. "Port decisions"; also the chat note's heading.
    pub heading: &'a str,
    pub lines: &'a str,
    /// What the resumed agent should do next.
    pub instruction: &'a str,
    /// The task the requester was working on.
    pub task: &'a str,
    pub kind: RunKind,
    pub source: &'a str,
    pub grants: Vec<crate::services::Grant>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The chat is busy or there is no capacity; try again later.
    Wait,
    Resumed(String),
    /// A newer human turn steers the task, or the request's turn did not
    /// finish; the decisions are recorded in the chat without a new turn.
    Noted,
}

/// Resumes the requester when its latest turn completed and no human turn
/// followed the latest request. An answered design question continues the
/// same turn rather than steering it.
pub(crate) fn resume(state: &mut State, decisions: Decisions<'_>) -> Outcome {
    let chat = decisions.chat;
    let in_chat = |run: &&crate::runtime::Run| run.chat_id.as_deref() == Some(chat);
    if state.runtime.runs.iter().filter(in_chat).any(|run| matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling")) {
        return Outcome::Wait;
    }
    let requested = state.runtime.runs.iter().rposition(|run| decisions.origins.contains(&run.id.as_str()));
    let latest = state.runtime.runs.iter().rposition(|run| in_chat(&run));
    let steered = requested.is_some_and(|at| state.runtime.runs[at + 1..].iter().filter(in_chat)
        .any(|run| run.kind.speaker() == MessageSpeaker::User && run.kind != RunKind::DesignAnswer));
    let resumable = requested.is_some() && !steered && latest.is_some_and(|at| state.runtime.runs[at].status == "completed");
    let mut run_id = None;
    if let (true, Some(latest)) = (resumable, latest) {
        if crate::scheduler::admit(&state.runtime).is_err() {
            return Outcome::Wait;
        }
        let objective = format!("Operator {}:\n{}\n{} Original task:\n{}", decisions.heading.to_lowercase(), decisions.lines, decisions.instruction, decisions.task).chars().take(2000).collect();
        let Ok(id) = crate::chat::enqueue_continuation(state, chat, latest, objective, decisions.kind, decisions.source) else { return Outcome::Wait };
        let run = state.runtime.runs.last_mut().unwrap();
        for grant in decisions.grants {
            if !run.resource_grants.contains(&grant) {
                run.resource_grants.push(grant);
            }
        }
        run_id = Some(id);
    }
    if let Some(session) = state.chats.iter_mut().find(|session| session.id == chat) {
        session.messages.push(crate::chat::Message { text: format!("{}\n{}", decisions.heading, decisions.lines), run_id: run_id.clone(), reply: String::new() });
        session.revision = session.revision.saturating_add(1);
    }
    run_id.map_or(Outcome::Noted, Outcome::Resumed)
}

#[cfg(test)]
mod tests {
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
        resume(state, Decisions { chat, origins: &[origin], heading: "Port decisions", lines: "relay: approved", instruction: "Continue.", task: "Run the server", kind: RunKind::PortDecision, source: "port-decision", grants: Vec::new() })
    }

    #[test]
    fn decisions_wait_for_the_turn_then_resume_with_its_context() {
        let (mut state, chat, origin) = requested();
        assert_eq!(decide(&mut state, &chat, &origin), Outcome::Wait, "the requesting turn is still running");
        state.runtime.runs[0].status = "completed".into();
        let Outcome::Resumed(id) = decide(&mut state, &chat, &origin) else { panic!("resumes") };
        let run = state.runtime.runs.last().unwrap();
        assert_eq!((run.id.as_str(), run.kind, run.chat_id.as_deref(), run.model.as_deref()), (id.as_str(), RunKind::PortDecision, Some(chat.as_str()), Some("captured-model")));
        assert!(run.objective.starts_with("Operator port decisions:\nrelay: approved\nContinue. Original task:\nRun the server"));
        assert_eq!(state.chats[0].messages.last().unwrap().run_id.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn human_turns_after_the_request_get_a_note_and_design_answers_do_not_count() {
        let (mut state, chat, origin) = requested();
        state.runtime.runs[0].status = "completed".into();
        let mut answer = state.runtime.runs[0].clone();
        answer.id = "answer".into();
        answer.kind = RunKind::DesignAnswer;
        state.runtime.runs.push(answer);
        assert!(matches!(decide(&mut state, &chat, &origin), Outcome::Resumed(_)), "an answered design question continues the same turn");
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        crate::chat::send(&mut state, &chat, "Stop the playtest", "test").unwrap();
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        let runs = state.runtime.runs.len();
        assert_eq!(decide(&mut state, &chat, &origin), Outcome::Noted);
        assert_eq!(state.runtime.runs.len(), runs);
        assert_eq!(state.chats[0].messages.last().unwrap().text, "Port decisions\nrelay: approved");
    }

    #[test]
    fn a_full_scheduler_defers_the_resume() {
        let (mut state, chat, origin) = requested();
        state.runtime.runs[0].status = "completed".into();
        while crate::scheduler::admit(&state.runtime).is_ok() {
            state.runtime.enqueue("Queued".into(), RunKind::Task, "test");
        }
        assert_eq!(decide(&mut state, &chat, &origin), Outcome::Wait);
    }
}
