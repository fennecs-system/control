//! Advisory host notes appended to a managed session's tool results, each at
//! most once per attempt: the attempt deadline is near, or a chat that steers
//! active members has spent a long turn doing work itself. They inform; they
//! never stop a tool, end a turn or change authority. Typed host records
//! decide; the model's prose is never parsed.
use crate::State;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Per-attempt nudge state, kept on the attempt so each note is sent once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Nudges {
    /// Command executions requested through Control tools in this attempt.
    #[serde(skip_serializing_if = "is_zero")]
    pub execs: u32,
    /// The deadline warning was sent.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deadline: bool,
    /// The steering suggestion was sent.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub steering: bool,
}
impl Nudges {
    pub fn is_clear(&self) -> bool {
        *self == Self::default()
    }
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// The deadline warning fires once remaining time is at most this many
/// seconds, or `DEADLINE_SHARE_PERCENT` of the limit when that is larger.
/// For limits shorter than twice the floor, half the limit is the floor, so
/// a short limit is not warned about on its first tool call.
const DEADLINE_FLOOR_SECONDS: u64 = 90;
const DEADLINE_SHARE_PERCENT: u64 = 15;
/// Tools that run commands. Counting them is cheap and needs no job lookup.
const EXECUTIONS: [&str; 3] = ["control_service_exec", "control_workspace_run", "control_workspace_start"];
/// Parent links followed from one chat; delegation depth is far smaller.
const DEPTH: usize = 64;

pub(super) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
/// Remaining seconds at or below which an attempt with `limit` is warned.
pub(crate) fn deadline_threshold(limit: u64) -> u64 {
    (limit.saturating_mul(DEADLINE_SHARE_PERCENT) / 100).max(DEADLINE_FLOOR_SECONDS.min(limit / 2))
}
/// What a checkpoint must carry so findings outlive the turn and its compaction:
/// the working notes are `control_turn_handoff`'s payload, and an omitted
/// payload keeps the older notes.
pub(crate) const NOTES: &str = "Record your key findings (facts, diagnoses) and current plan in its working notes, payload.findings and payload.next, so they survive compaction; an omitted payload keeps the older notes.";
/// How a turn without a handoff keeps findings through in-turn compaction.
pub(crate) const MARKS: &str = "Write key facts on lines starting Finding: and choices on lines starting Decision:; Control keeps those lines when it summarizes older steps.";
pub(crate) fn deadline_text(remaining: u64) -> String {
    format!("{remaining} s left in this turn: publish your checkpoint (control_turn_handoff) now and continue in a routine or the next turn. {NOTES}")
}
/// What follows a turn that stops now. Only a persistent bot has a next turn:
/// a chat that hands off without one simply ends, so the note must not promise
/// a continuation.
#[derive(Clone, Copy)]
pub(crate) enum Next {
    /// A persistent bot with the handoff tool: checkpoint and continue.
    Turn,
    /// The handoff tool without a bot yet, and one can be requested.
    Unscheduled,
    /// Nothing follows: finish.
    Report,
}
impl Next {
    pub(crate) fn of(handoff: bool, continues: bool, requestable: bool) -> Self {
        match (handoff, continues, requestable) {
            (true, true, _) => Self::Turn,
            (true, false, true) => Self::Unscheduled,
            _ => Self::Report,
        }
    }
}
const UNSCHEDULED: &str = "this chat has no next turn: before you hand off, request your persistent bot (control_bot_request) or delegate what remains; otherwise finish with a brief final report.";
/// The once-per-turn note when a turn's context passes its threshold (local
/// endpoints, where Control owns the transcript and measures its size).
pub(crate) fn context_text(percent: u32, next: Next) -> String {
    match next {
        Next::Turn => format!("Context {percent}% full: publish your checkpoint (control_turn_handoff) and continue in the next turn. {NOTES} {MARKS}"),
        Next::Unscheduled => format!("Context {percent}% full and {UNSCHEDULED} {NOTES} {MARKS}"),
        Next::Report => format!("Context {percent}% full: finish with a brief final report soon. {MARKS}"),
    }
}
/// The once-per-turn note when a local-model turn nears its model-step cap.
pub(crate) fn steps_text(left: usize, next: Next) -> String {
    match next {
        Next::Turn => format!("{left} model steps left in this turn: publish your checkpoint (control_turn_handoff) and continue next turn. {NOTES}"),
        Next::Unscheduled => format!("{left} model steps left in this turn and {UNSCHEDULED} {NOTES}"),
        Next::Report => format!("{left} model steps left in this turn: finish with a brief final report now."),
    }
}
pub(crate) fn steering_text(members: usize) -> String {
    format!("You are steering {members} active members: delegate this work to a member and end your turn.")
}

/// Members `chat` currently steers: chats spawned from it, directly or
/// through other members, that have a turn queued, running or waiting, or
/// are approved persistent bots.
pub(crate) fn steered(state: &State, chat: &str) -> usize {
    let parents = state.chats.iter()
        .filter_map(|session| session.parent_chat.as_deref().map(|parent| (session.id.as_str(), parent)))
        .collect::<std::collections::HashMap<_, _>>();
    if !parents.values().any(|parent| *parent == chat) {
        return 0;
    }
    let members = parents.keys().copied().filter(|member| {
        let mut at = *member;
        for _ in 0..DEPTH {
            match parents.get(at) {
                Some(&parent) if parent == chat => return true,
                Some(&parent) => at = parent,
                None => return false,
            }
        }
        false
    }).collect::<HashSet<_>>();
    let mut active = members.iter().copied().filter(|member| crate::bots::continuation_enabled(state, member)).collect::<HashSet<_>>();
    for run in &state.runtime.runs {
        if let Some(member) = run.chat_id.as_deref().filter(|member| members.contains(member))
            && matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling") {
            active.insert(member);
        }
    }
    active.len()
}

/// Notes for one tool call `tool` of the running attempt of `run_id` at Unix
/// second `now`, recording which were sent. Nothing once a handoff is staged.
pub(crate) fn notes(state: &mut State, run_id: &str, tool: &str, now: u64) -> Vec<String> {
    let Some(index) = state.runtime.runs.iter().position(|run| run.id == run_id && run.status == "running") else {
        return Vec::new();
    };
    let run = &state.runtime.runs[index];
    let Some(attempt) = run.attempts.last() else { return Vec::new() };
    if attempt.turn_mailbox.as_ref().and_then(|mailbox| mailbox.accepted()).is_some() {
        return Vec::new();
    }
    let mut nudges = attempt.nudges.clone();
    if EXECUTIONS.contains(&tool) {
        nudges.execs = nudges.execs.saturating_add(1);
    }
    let elapsed = now.saturating_sub(attempt.started_unix);
    let mut notes = Vec::new();
    if !nudges.deadline
        && let Some(limit) = crate::attempt_control::deadline(state, run, attempt)
        && limit.saturating_sub(elapsed) <= deadline_threshold(limit)
    {
        nudges.deadline = true;
        notes.push(deadline_text(limit.saturating_sub(elapsed)));
    }
    let limits = &state.runtime.execution;
    if !nudges.steering
        && (elapsed >= limits.steering_turn_seconds || nudges.execs as usize >= limits.steering_exec_calls)
        && let Some(chat) = run.chat_id.as_deref()
    {
        let members = steered(state, chat);
        if members > 0 {
            nudges.steering = true;
            notes.push(steering_text(members));
        }
    }
    let attempt = state.runtime.runs[index].attempts.last_mut().unwrap();
    if attempt.nudges != nudges {
        attempt.nudges = nudges;
    }
    notes
}

#[cfg(test)]
mod tests;
