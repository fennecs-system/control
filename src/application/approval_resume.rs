//! Approval eligibility over borrowed run records; no policy lookup or effects.
//! The host authorizes a candidate and checks admission before constructing it.
use super::runs::Run;
use control_context::agent_policy::{MessageSpeaker, RunKind};

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
