//! Operator decisions that resume the agent which asked, shared by port and bot
//! requests. Callers keep their own request records and acknowledge them once
//! `resume` returns anything but `Wait`. Checkpoint that receipt with the edits
//! before waking the dispatcher; this preparation step performs no I/O.
use crate::{State, runtime::RunKind};
use crate::application::approval_resume::{self, Eligibility, Edit};
pub(crate) use approval_resume::Outcome;

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

/// Resumes the requester when its latest turn completed and no human turn
/// followed the latest request. An answered design question continues the
/// same turn rather than steering it.
pub(crate) fn resume(state: &mut State, decisions: Decisions<'_>) -> Outcome {
    let chat = decisions.chat;
    let latest = match approval_resume::decide(&state.runtime.runs, chat, decisions.origins) {
        Eligibility::Wait => return Outcome::Wait,
        Eligibility::Note => None,
        Eligibility::Candidate(at) => {
            let run = &state.runtime.runs[at];
            crate::identity::authorize(&run.actor, run.action).is_ok().then_some(at)
        }
    };
    let mut continuation = None;
    if let Some(latest) = latest {
        if crate::scheduler::admit(&state.runtime).is_err() {
            return Outcome::Wait;
        }
        let objective = format!("Operator {}:\n{}\n{} Original task:\n{}", decisions.heading.to_lowercase(), decisions.lines, decisions.instruction, decisions.task).chars().take(2000).collect();
        if let Ok(mut run) = crate::chat::continuation(state, chat, latest, objective, decisions.kind, decisions.source) {
            for grant in decisions.grants {
                if !run.resource_grants.contains(&grant) {
                    run.resource_grants.push(grant);
                }
            }
            continuation = Some(run);
        }
    }
    approval_resume::record(Edit {
        runs: &mut state.runtime.runs, notice: &mut state.runtime.notice, chats: &mut state.chats,
    }, chat, continuation, format!("{}\n{}", decisions.heading, decisions.lines))
}

#[cfg(test)]
mod tests;
