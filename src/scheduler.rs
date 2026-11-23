//! Bounded node execution with deterministic per-conversation ownership.
use crate::runtime::RunStatus;
use crate::runtime::Runtime;
use control_core::scheduling::{self, AttemptStamp, Capacity, Phase};
use std::io;

pub use control_core::scheduling::MAX_PARALLEL;
#[cfg(test)]
pub use control_core::scheduling::MAX_PENDING;
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_parallel_runs: usize,
    pub max_agents_per_root: usize,
    pub max_pending_agent_intents: usize,
    /// Read-only team advisors per root task, separate from its one blocking
    /// judge review. 0 disables advisors.
    pub max_advisors_per_root: usize,
    /// Consecutive failed or blocked turns of one member before its advisor
    /// hears of it. Single failures, provider errors included, are not facts.
    pub advisor_failure_streak: usize,
    /// Settled team turns while claims stay open and none advances (becomes
    /// done, or resumes working after being blocked) before the advisor hears
    /// the team is busy but stalled. Counted in turns, not elapsed time.
    pub advisor_stall_turns: usize,
    /// Distinct members, other than the owner, whose claims wait on one live
    /// claim directly or transitively (through `blocked_on`) before the
    /// advisor hears that claim is a bottleneck.
    pub advisor_bottleneck_members: usize,
    /// Immediate recovery turns a persistent bot gets after its handoff is
    /// rejected, counted since its last accepted handoff. Beyond it the bot
    /// waits on its subscriptions with a backoff recheck. 0 never retries.
    pub bot_recovery_continues: usize,
    /// First recheck of a bot that could not checkpoint, in seconds; it
    /// doubles with each further failed turn, up to 64 times or one day.
    pub bot_recovery_recheck_seconds: u64,
    /// Seconds a turn of a chat steering active members may run before its
    /// tool results suggest delegating the work and ending the turn. Advisory.
    pub steering_turn_seconds: u64,
    /// Command executions in one turn of a chat steering active members before
    /// the same advisory suggestion.
    pub steering_exec_calls: usize,
    /// Seconds a non-urgent agent message may wait for another wake of its
    /// recipient before it is delivered as a digest turn of its own.
    pub peer_digest_seconds: u64,
    /// Model steps one local-model turn may take before Control stops it and
    /// writes a host checkpoint; the agent is warned `STEP_NOTE_LEFT` steps before.
    pub local_agent_steps: usize,
    /// Bytes of a declared observation placed at the top of each turn.
    pub observation_max_bytes: usize,
    /// Pathology facts (for advisors and antibodies): file- or tool-building
    /// commands a team runs while its claims and score probes do not advance
    /// before it is reported as harness churn.
    pub pathology_churn_commands: usize,
    /// Failures of one member's same command (digits folded) among its
    /// retained commands before it is reported as a repetition loop.
    pub pathology_repeat_failures: usize,
    /// Status-only commands of one member among its retained commands before
    /// it is reported as polling.
    pub pathology_poll_commands: usize,
    /// Tokens a team spends while its claims and score probes do not advance
    /// before it is reported as cost burn.
    pub pathology_burn_tokens: u64,
    /// Operator notices (`control_operator_notify`) the node forwards per
    /// hour; further notices are refused until the hour passes. 0 disables them.
    pub operator_notices_per_hour: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parallel_runs: 3,
            max_agents_per_root: 64,
            max_pending_agent_intents: 256,
            max_advisors_per_root: 3,
            advisor_failure_streak: 3,
            advisor_stall_turns: 24,
            advisor_bottleneck_members: 2,
            bot_recovery_continues: 2,
            bot_recovery_recheck_seconds: 300,
            steering_turn_seconds: 240,
            steering_exec_calls: 6,
            peer_digest_seconds: 600,
            observation_max_bytes: 2048,
            local_agent_steps: 64,
            pathology_churn_commands: 12,
            pathology_repeat_failures: 3,
            pathology_poll_commands: 6,
            pathology_burn_tokens: 2_000_000,
            operator_notices_per_hour: 4,
        }
    }
}
impl Limits {
    fn validate(self) -> io::Result<Self> {
        if !(1..=MAX_PARALLEL).contains(&self.max_parallel_runs) {
            return Err(io::Error::other("max_parallel_runs must be between 1 and 32"));
        }
        if !(1..=65536).contains(&self.max_agents_per_root) || !(1..=65536).contains(&self.max_pending_agent_intents) {
            return Err(io::Error::other(
                "Agent root and pending-intent budgets must be positive integers up to 65536",
            ));
        }
        if self.max_advisors_per_root > 8
            || !(2..=64).contains(&self.advisor_failure_streak)
            || !(4..=4096).contains(&self.advisor_stall_turns)
            || !(2..=64).contains(&self.advisor_bottleneck_members)
        {
            return Err(io::Error::other(
                "max_advisors_per_root must be 0–8, advisor_failure_streak 2–64, advisor_stall_turns 4–4096 and advisor_bottleneck_members 2–64",
            ));
        }
        if self.bot_recovery_continues > 16 || !(30..=86_400).contains(&self.bot_recovery_recheck_seconds) {
            return Err(io::Error::other(
                "bot_recovery_continues must be 0–16 and bot_recovery_recheck_seconds 30–86400",
            ));
        }
        if !(30..=86_400).contains(&self.steering_turn_seconds) || !(1..=1024).contains(&self.steering_exec_calls) {
            return Err(io::Error::other(
                "steering_turn_seconds must be 30–86400 and steering_exec_calls 1–1024",
            ));
        }
        if !(30..=86_400).contains(&self.peer_digest_seconds) || !(256..=16_384).contains(&self.observation_max_bytes) {
            return Err(io::Error::other(
                "peer_digest_seconds must be 30–86400 and observation_max_bytes 256–16384",
            ));
        }
        if !(16..=1024).contains(&self.local_agent_steps) {
            return Err(io::Error::other("local_agent_steps must be 16–1024"));
        }
        if !(2..=1024).contains(&self.pathology_churn_commands)
            || !(2..=16).contains(&self.pathology_repeat_failures)
            || !(2..=16).contains(&self.pathology_poll_commands)
            || !(10_000..=1_000_000_000).contains(&self.pathology_burn_tokens)
            || self.operator_notices_per_hour > 60
        {
            return Err(io::Error::other(
                "pathology_churn_commands must be 2–1024, pathology_repeat_failures and pathology_poll_commands 2–16, pathology_burn_tokens 10000–1000000000 and operator_notices_per_hour 0–60",
            ));
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
fn load_limits_in(path: &std::path::Path, override_limit: Option<std::ffi::OsString>) -> io::Result<Limits> {
    let mut limits = match crate::config::read_regular(path, 4096, true) {
        Ok(bytes) => {
            serde_json::from_slice::<Limits>(&bytes).map_err(|_| io::Error::other("Invalid execution.json"))?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Limits::default(),
        Err(error) => return Err(error),
    };
    if let Some(value) = override_limit {
        limits.max_parallel_runs = value
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| io::Error::other("CONTROL_MAX_PARALLEL_RUNS must be an integer from 1 to 32"))?;
    }
    limits.validate()
}

#[inline]
pub(crate) fn phase(run: &crate::runtime::Run) -> Phase {
    match run.status {
        RunStatus::Queued => Phase::Queued,
        RunStatus::Running => Phase::Running,
        RunStatus::Cancelling => Phase::Cancelling,
        RunStatus::Waiting => Phase::Waiting {
            reserves_chat: run.delegation.is_some() || run.question.as_ref().is_some_and(|q| q.pending()),
        },
        RunStatus::Completed
        | RunStatus::Blocked
        | RunStatus::Failed
        | RunStatus::Interrupted
        | RunStatus::Cancelled
        | RunStatus::Continued
        | RunStatus::Unrecognised(_) => Phase::Inactive,
    }
}
#[inline]
pub(crate) fn stamp(attempt: &crate::usage::Attempt) -> AttemptStamp<'_> {
    AttemptStamp {
        workspace: attempt.workspace_id.as_deref(),
        started_unix: attempt.started_unix,
    }
}
#[inline]
pub fn admit(runtime: &Runtime) -> Result<(), &'static str> {
    if scheduling::has_capacity(runtime.runs.iter()) {
        Ok(())
    } else {
        Err("Run queue is full (32 pending tasks). Wait for work to finish or cancel a task.")
    }
}
/// Delegated and spawned agents and automatic bot turns run in the background
/// lane, whatever their ancestry.
fn background(run: &crate::runtime::Run) -> bool {
    matches!(
        run.kind,
        crate::runtime::RunKind::BotWake | crate::runtime::RunKind::BotDecision
    ) || run.persona.as_ref().is_some_and(|persona| persona.parent.is_some())
}
/// Only each chat's oldest queued turn is a candidate, keeping chat FIFO. Among
/// candidates, conversation turns start before background agents, and background
/// agents use at most all but one slot, so a person is not queued behind them.
#[cfg(test)]
pub fn next(runtime: &Runtime) -> Option<usize> {
    next_where(runtime, |_| true)
}
/// Application admission applies current bot intent without moving that state
/// into the scheduling core. Held bot turns, and turns held by their account's cap or cooldown, do not block
/// other conversations.
pub(crate) fn next_in(state: &crate::State) -> Option<usize> {
    let holds = crate::providers::accounts::Holds::of(&state.runtime);
    next_where(&state.runtime, |run| {
        crate::bots::may_start(state, run) && holds.hold(run).is_none()
    })
}
fn next_where(runtime: &Runtime, permitted: impl Fn(&crate::runtime::Run) -> bool) -> Option<usize> {
    let capacity = Capacity::new(runtime.execution.max_parallel_runs.min(MAX_PARALLEL))?;
    if runtime.paused.is_some() {
        return None;
    }
    let mut heads = std::collections::HashMap::new();
    for (index, run) in runtime
        .runs
        .iter()
        .enumerate()
        .filter(|(_, run)| run.status == RunStatus::Queued && permitted(run))
    {
        if let Some(chat) = run.chat_id.as_deref() {
            heads
                .entry(chat)
                .and_modify(|head: &mut (u64, usize)| *head = (*head).min((run.queue_order, index)))
                .or_insert((run.queue_order, index));
        }
    }
    let candidate = |index: usize, run: &crate::runtime::Run| {
        run.status == RunStatus::Queued
            && permitted(run)
            && run
                .chat_id
                .as_deref()
                .is_none_or(|chat| heads.get(chat).is_some_and(|head| head.1 == index))
    };
    let mut lanes = runtime
        .runs
        .iter()
        .enumerate()
        .map(|(index, run)| Lane {
            run,
            queued: candidate(index, run) && !background(run),
        })
        .collect::<Vec<_>>();
    if let Some(index) = scheduling::next(lanes.iter(), capacity, false) {
        return Some(index);
    }
    let busy = runtime
        .runs
        .iter()
        .filter(|run| background(run) && scheduling::RunView::occupies_slot(*run))
        .count();
    if busy + usize::from(capacity.slots() > 1) >= capacity.slots() {
        return None;
    }
    for (index, lane) in lanes.iter_mut().enumerate() {
        lane.queued = candidate(index, lane.run) && background(lane.run);
    }
    scheduling::next(lanes.iter(), capacity, false)
}
/// One lane's view of the ledger: other lanes' queued runs are hidden, while
/// every run's slot and chat reservations still count.
struct Lane<'a> {
    run: &'a crate::runtime::Run,
    queued: bool,
}
impl scheduling::RunView for Lane<'_> {
    fn phase(&self) -> Phase {
        if self.run.status == RunStatus::Queued && !self.queued {
            Phase::Inactive
        } else {
            phase(self.run)
        }
    }
    fn chat(&self) -> Option<&str> {
        self.run.chat_id.as_deref()
    }
    fn queue_order(&self) -> u64 {
        self.run.queue_order
    }
    fn occupies_slot(&self) -> bool {
        scheduling::RunView::occupies_slot(self.run)
    }
    fn is_queued(&self) -> bool {
        self.queued
    }
    fn is_pending(&self) -> bool {
        scheduling::RunView::is_pending(self.run)
    }
    fn reserves_chat(&self) -> bool {
        scheduling::RunView::reserves_chat(self.run)
    }
}
impl scheduling::RunView for crate::runtime::Run {
    #[inline]
    fn phase(&self) -> Phase {
        phase(self)
    }
    #[inline]
    fn chat(&self) -> Option<&str> {
        self.chat_id.as_deref()
    }
    #[inline]
    fn queue_order(&self) -> u64 {
        self.queue_order
    }
    #[inline]
    fn occupies_slot(&self) -> bool {
        self.status.occupies_slot()
    }
    #[inline]
    fn is_queued(&self) -> bool {
        self.status == RunStatus::Queued
    }
    #[inline]
    fn is_pending(&self) -> bool {
        self.status.is_active()
    }
    #[inline]
    fn reserves_chat(&self) -> bool {
        self.occupies_slot()
            || (self.status == RunStatus::Waiting
                && (self.delegation.is_some() || self.question.as_ref().is_some_and(|q| q.pending())))
    }
}

#[cfg(test)]
#[path = "scheduler/tests/qa.rs"]
mod qa;

#[cfg(test)]
mod tests;
