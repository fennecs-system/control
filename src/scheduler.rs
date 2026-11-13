//! Bounded node execution with deterministic per-conversation ownership.
use crate::runtime::Runtime;
use std::io;
use control_core::scheduling::{self, AttemptStamp, Capacity, Phase};

#[cfg(test)]
pub use control_core::scheduling::MAX_PENDING;
pub use control_core::scheduling::MAX_PARALLEL;
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_parallel_runs: usize,
    pub max_agents_per_root: usize,
    pub max_pending_agent_intents: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parallel_runs: 3,
            max_agents_per_root: 64,
            max_pending_agent_intents: 256,
        }
    }
}
impl Limits {
    fn validate(self) -> io::Result<Self> {
        if !(1..=MAX_PARALLEL).contains(&self.max_parallel_runs) {
            return Err(io::Error::other(
                "max_parallel_runs must be between 1 and 8",
            ));
        }
        if !(1..=65536).contains(&self.max_agents_per_root)
            || !(1..=65536).contains(&self.max_pending_agent_intents) {
            return Err(io::Error::other("Agent root and pending-intent budgets must be positive integers up to 65536"));
        }
        Ok(self)
    }
}
pub fn load_limits() -> io::Result<Limits> {
    load_limits_in(
        &crate::config::directory()
            .map_err(io::Error::other)?
            .join("execution.json"),
        std::env::var_os("CONTROL_MAX_PARALLEL_RUNS"),
    )
}
fn load_limits_in(
    path: &std::path::Path,
    override_limit: Option<std::ffi::OsString>,
) -> io::Result<Limits> {
    let mut limits = match crate::config::read_regular(path, 4096, true) {
        Ok(bytes) => serde_json::from_slice::<Limits>(&bytes)
            .map_err(|_| io::Error::other("Invalid execution.json"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Limits::default(),
        Err(error) => return Err(error),
    };
    if let Some(value) = override_limit {
        limits.max_parallel_runs =
            value.to_str().and_then(|s| s.parse().ok()).ok_or_else(|| {
                io::Error::other("CONTROL_MAX_PARALLEL_RUNS must be an integer from 1 to 8")
            })?;
    }
    limits.validate()
}

#[inline]
pub(crate) fn phase(run: &crate::runtime::Run) -> Phase {
    match run.status.as_str() {
        "queued" => Phase::Queued,
        "running" => Phase::Running,
        "cancelling" => Phase::Cancelling,
        "waiting" => Phase::Waiting {reserves_chat:run.delegation.is_some() || run.question.as_ref().is_some_and(|q|q.answer.is_none())},
        _ => Phase::Inactive,
    }
}
#[inline]
pub(crate) fn stamp(attempt: &crate::usage::Attempt) -> AttemptStamp<'_> {
    AttemptStamp {workspace:attempt.workspace_id.as_deref(),started_unix:attempt.started_unix}
}
#[inline]
pub fn admit(runtime: &Runtime) -> Result<(), &'static str> {
    if scheduling::has_capacity(runtime.runs.iter()) { Ok(()) }
    else { Err("Run queue is full (32 pending tasks). Wait for work to finish or cancel a task.") }
}
/// Delegated and spawned agents and automatic bot turns run in the background
/// lane, whatever their ancestry.
fn background(run: &crate::runtime::Run) -> bool {
    matches!(run.kind, crate::runtime::RunKind::BotWake | crate::runtime::RunKind::BotDecision) || run.persona.as_ref().is_some_and(|persona| persona.parent.is_some())
}
/// Only each chat's oldest queued turn is a candidate, keeping chat FIFO. Among
/// candidates, conversation turns start before background agents, and background
/// agents use at most all but one slot, so a person is not queued behind them.
#[cfg(test)]
pub fn next(runtime: &Runtime) -> Option<usize> {
    next_where(runtime, |_| true)
}
/// Application admission applies current bot intent without moving that state
/// into the scheduling core. Held bot turns do not block other conversations.
pub(crate) fn next_in(state: &crate::State) -> Option<usize> {
    next_where(&state.runtime, |run| crate::bots::may_start(state, run))
}
fn next_where(runtime: &Runtime, permitted: impl Fn(&crate::runtime::Run) -> bool) -> Option<usize> {
    let capacity=Capacity::new(runtime.execution.max_parallel_runs.min(MAX_PARALLEL))?;
    if runtime.paused.is_some() {
        return None;
    }
    let mut heads=std::collections::HashMap::new();
    for (index, run) in runtime.runs.iter().enumerate().filter(|(_, run)| run.status == "queued" && permitted(run)) {
        if let Some(chat)=run.chat_id.as_deref() {
            heads.entry(chat).and_modify(|head: &mut (u64, usize)| *head=(*head).min((run.queue_order, index))).or_insert((run.queue_order, index));
        }
    }
    let candidate=|index: usize, run: &crate::runtime::Run| run.status == "queued" && permitted(run)
        && run.chat_id.as_deref().is_none_or(|chat| heads.get(chat).is_some_and(|head| head.1 == index));
    let mut lanes=runtime.runs.iter().enumerate()
        .map(|(index, run)| Lane {run, queued: candidate(index, run) && !background(run)})
        .collect::<Vec<_>>();
    if let Some(index)=scheduling::next(lanes.iter(),capacity,false) {
        return Some(index);
    }
    let busy=runtime.runs.iter().filter(|run| background(run) && scheduling::RunView::occupies_slot(*run)).count();
    if busy + usize::from(capacity.slots() > 1) >= capacity.slots() {
        return None;
    }
    for (index, lane) in lanes.iter_mut().enumerate() {
        lane.queued=candidate(index, lane.run) && background(lane.run);
    }
    scheduling::next(lanes.iter(),capacity,false)
}
/// One lane's view of the ledger: other lanes' queued runs are hidden, while
/// every run's slot and chat reservations still count.
struct Lane<'a> {
    run: &'a crate::runtime::Run,
    queued: bool,
}
impl scheduling::RunView for Lane<'_> {
    fn phase(&self) -> Phase {
        if self.run.status == "queued" && !self.queued { Phase::Inactive } else { phase(self.run) }
    }
    fn chat(&self) -> Option<&str> { self.run.chat_id.as_deref() }
    fn queue_order(&self) -> u64 { self.run.queue_order }
    fn occupies_slot(&self) -> bool { scheduling::RunView::occupies_slot(self.run) }
    fn is_queued(&self) -> bool { self.queued }
    fn is_pending(&self) -> bool { scheduling::RunView::is_pending(self.run) }
    fn reserves_chat(&self) -> bool { scheduling::RunView::reserves_chat(self.run) }
}
impl scheduling::RunView for crate::runtime::Run {
    #[inline]
    fn phase(&self) -> Phase { phase(self) }
    #[inline]
    fn chat(&self) -> Option<&str> { self.chat_id.as_deref() }
    #[inline]
    fn queue_order(&self) -> u64 { self.queue_order }
    #[inline]
    fn occupies_slot(&self) -> bool { matches!(self.status.as_str(), "running"|"cancelling") }
    #[inline]
    fn is_queued(&self) -> bool { self.status == "queued" }
    #[inline]
    fn is_pending(&self) -> bool { matches!(self.status.as_str(),"queued"|"running"|"cancelling"|"waiting") }
    #[inline]
    fn reserves_chat(&self) -> bool { self.occupies_slot() || (self.status=="waiting" && (self.delegation.is_some() || self.question.as_ref().is_some_and(|q|q.answer.is_none()))) }
}

#[cfg(test)]
#[path = "scheduler/tests/qa.rs"]
mod qa;

#[cfg(test)]
mod tests;
