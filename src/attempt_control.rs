//! Attempt lifecycle projections. Providers interpret these decisions; workload
//! health and progress remain tool/service concerns, not supervisor heuristics.
use crate::{State, usage::Attempt};
use serde::{Deserialize, Serialize};
use std::{sync::{Arc, Mutex}, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Termination { TimedOut, Cancelled, Restarted, ProviderError, PublicationFailed }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision { Continue, Finish(String), TimedOut }

pub fn limit(explicit: Option<u64>, persistent: bool) -> Option<u64> {
    explicit.or_else(|| (!persistent).then_some(600))
}
pub fn deadline(state: &State, run: &crate::runtime::Run, attempt: &Attempt) -> Option<u64> {
    limit(attempt.explicit_timeout_seconds, run.chat_id.as_ref()
        .is_some_and(|chat| crate::bots::continuation_enabled(state, chat)))
}
fn decide(limit: Option<u64>, elapsed: Duration, handoff: Option<String>) -> Decision {
    // An already accepted handoff is a host finish request, even at the deadline.
    // Cancellation/authority are checked by the caller and again at publication.
    if let Some(summary) = handoff { Decision::Finish(summary) }
    else if limit.is_some_and(|seconds| elapsed >= Duration::from_secs(seconds)) { Decision::TimedOut }
    else { Decision::Continue }
}
pub fn poll(bridge: Option<(&Arc<Mutex<State>>, &str)>, attempt: &Attempt, elapsed: Duration) -> Decision {
    let Some((shared, id)) = bridge else {
        return decide(limit(attempt.explicit_timeout_seconds, false), elapsed, None);
    };
    let Ok(state) = shared.lock() else { return Decision::Continue; };
    let Some(run) = state.runtime.runs.iter().find(|r| r.id == id) else { return Decision::Continue; };
    let handoff = run.attempts.last().filter(|live| live.workspace_id == attempt.workspace_id)
        .and_then(|live| live.turn_mailbox.as_ref()).and_then(|m| m.accepted())
        .map(|h| h.progress.clone().unwrap_or_else(|| "Agent yielded with a committed handoff.".into()));
    decide(deadline(&state, run, attempt), elapsed, handoff)
}
/// Stop all attempt-owned writers using current durable job identities. Persistent
/// services are separate resources and intentionally survive a model yield.
pub fn stop(bridge: Option<(&Arc<Mutex<State>>, &str)>, attempt: &Attempt) -> Result<(), &'static str> {
    let mut identity = attempt.clone();
    if let Some((shared, id)) = bridge {
        let state = shared.lock().map_err(|_| "State unavailable while stopping attempt")?;
        let live = state.runtime.runs.iter().find(|r| r.id == id)
            .and_then(|r| r.attempts.last()).filter(|a| a.workspace_id == attempt.workspace_id)
            .ok_or("Attempt generation changed while stopping")?;
        identity.command_jobs = live.command_jobs.iter().map(crate::command_jobs::Job::identity).collect();
    }
    crate::lifecycle::stop_attempt(&identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn productive_persistent_attempts_have_no_default_wall_clock_cliff() {
        assert_eq!(decide(limit(None, true), Duration::from_secs(7*86400), None), Decision::Continue);
        assert_eq!(decide(limit(None, false), Duration::from_secs(600), None), Decision::TimedOut);
        assert_eq!(decide(limit(Some(1200), true), Duration::from_secs(1199), None), Decision::Continue);
        assert_eq!(decide(limit(Some(1200), true), Duration::from_secs(1200), None), Decision::TimedOut);
    }
    #[test]
    fn accepted_handoff_finishes_even_at_explicit_deadline_without_model_reply() {
        assert_eq!(decide(Some(600), Duration::from_secs(600), Some("saved".into())), Decision::Finish("saved".into()));
    }
}
