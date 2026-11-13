//! Approval eligibility and publication over borrowed records; no I/O or policy.
//! The host admits candidates and checkpoints publication with request receipts.
use super::runs::Run;
use control_context::agent_policy::{MessageSpeaker, RunKind};
use super::conversations::{Message, Session};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The chat is busy or there is no capacity; try again later.
    Wait,
    Resumed(String),
    /// Only record the decisions; a newer human turn or failed authorization
    /// or completion check prevents starting another turn.
    Noted,
}

pub(crate) struct Edit<'a> {
    pub runs: &'a mut Vec<Run>,
    pub notice: &'a mut String,
    pub chats: &'a mut [Session],
}

/// Publish an already-admitted continuation and its note over borrowed records.
/// No I/O, IDs, clock, policy lookup or dispatcher notification. The driver must
/// checkpoint these edits with its request receipts before waking a dispatcher.
/// Keep exclusive ownership through acknowledgement; this is not an async plan.
#[inline]
pub(crate) fn record(edit: Edit<'_>, chat: &str, run: Option<Run>, text: String) -> Outcome {
    let run_id = run.as_ref().map(|run| run.id.clone());
    if let Some(run) = run {
        edit.runs.push(run);
        *edit.notice = "Run queued.".into();
    }
    if let Some(session) = edit.chats.iter_mut().find(|session| session.id == chat) {
        session.messages.push(Message { text, run_id: run_id.clone(), reply: String::new() });
        session.revision = session.revision.saturating_add(1);
    }
    run_id.map_or(Outcome::Noted, Outcome::Resumed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Eligibility {
    Wait,
    Note,
    Candidate(usize),
}

/// Request IDs name the originating turns. Later design answers continue their
/// task; later human turns steer it. Any live turn in this chat defers decisions.
/// Candidate indices are valid only under the caller's existing exclusive state
/// ownership; they are not plans to retain across another writer or an await.
pub(crate) fn decide(runs: &[Run], chat: &str, origins: &[&str]) -> Eligibility {
    let in_chat = |run: &&Run| run.chat_id.as_deref() == Some(chat);
    if runs.iter().filter(in_chat).any(|run| {
        matches!(
            run.status.as_str(),
            "queued" | "running" | "waiting" | "cancelling"
        )
    }) {
        return Eligibility::Wait;
    }
    let requested = runs
        .iter()
        .rposition(|run| origins.contains(&run.id.as_str()));
    let latest = runs.iter().rposition(|run| in_chat(&run));
    let steered = requested.is_some_and(|at| {
        runs[at + 1..].iter().filter(in_chat).any(|run| {
            run.kind.speaker() == MessageSpeaker::User && run.kind != RunKind::DesignAnswer
        })
    });
    match (requested, latest) {
        (Some(_), Some(at)) if !steered && runs[at].status == "completed" => {
            Eligibility::Candidate(at)
        }
        _ => Eligibility::Note,
    }
}

#[cfg(test)]
mod tests;
