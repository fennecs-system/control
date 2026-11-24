//! Attempt lifecycle projections. Providers interpret these decisions; workload
//! health and progress remain tool/service concerns, not supervisor heuristics.
use crate::runtime::RunStatus;
use crate::store::Store;
use crate::{State, usage::Attempt};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Why an attempt ended before publishing. `StepLimit`: a local-model turn reached
/// its model-step cap (`local_agent_steps`) without a final report or handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    TimedOut,
    Cancelled,
    Restarted,
    ProviderError,
    PublicationFailed,
    StepLimit,
}

/// The run failed after being cut off mid-work before it checkpointed: stopped
/// at its time limit or model-step cap, or ended by a provider failure. Its host
/// checkpoint lets the next turn resume.
pub fn cut_off(run: &crate::runtime::Run) -> bool {
    run.status == RunStatus::Failed
        && run.attempts.last().is_some_and(|attempt| {
            matches!(
                attempt.termination,
                Some(Termination::TimedOut | Termination::ProviderError | Termination::StepLimit)
            )
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Continue,
    Finish(String),
    TimedOut(u64),
}

/// The attempt time limit. An explicit operator limit wins for every attempt.
/// Otherwise an attempt on the local-model backend uses the local turn limit
/// captured at admission (chats and bots alike; local steps are ~10× slower),
/// an approved persistent bot uses the bot turn limit captured at admission,
/// and every other attempt has the 600-second default. None: unlimited.
pub fn limit(attempt: &Attempt, persistent: bool) -> Option<u64> {
    if let Some(explicit) = attempt.explicit_timeout_seconds {
        return Some(explicit);
    }
    if attempt.backend.runs_control_loop() {
        return attempt.local_timeout_seconds;
    }
    if persistent {
        attempt.bot_timeout_seconds
    } else {
        Some(600)
    }
}
pub fn deadline(state: &State, run: &crate::runtime::Run, attempt: &Attempt) -> Option<u64> {
    limit(
        attempt,
        run.chat_id
            .as_ref()
            .is_some_and(|chat| crate::bots::continuation_enabled(state, chat)),
    )
}
fn decide(limit: Option<u64>, elapsed: Duration, handoff: Option<String>) -> Decision {
    // An already accepted handoff is a host finish request, even at the deadline.
    // Cancellation/authority are checked by the caller and again at publication.
    if let Some(summary) = handoff {
        Decision::Finish(summary)
    } else if let Some(seconds) = limit.filter(|seconds| elapsed >= Duration::from_secs(*seconds)) {
        Decision::TimedOut(seconds)
    } else {
        Decision::Continue
    }
}
pub fn poll(bridge: Option<(&Store, &str)>, attempt: &Attempt, elapsed: Duration) -> Decision {
    let Some((shared, id)) = bridge else {
        return decide(limit(attempt, false), elapsed, None);
    };
    shared
        .read(|state| {
            let Some(run) = state.runtime.runs.get(id) else {
                return Decision::Continue;
            };
            let handoff = run
                .attempts
                .last()
                .filter(|live| live.workspace_id == attempt.workspace_id)
                .and_then(|live| Some((live, live.turn_mailbox.as_ref()?.accepted()?)))
                .map(|(live, h)| reply(requested(run), &live.activity, h.progress.as_deref()));
            decide(deadline(state, run, attempt), elapsed, handoff)
        })
        .unwrap_or(Decision::Continue)
}
/// An operator asked for this turn and reads its reply in the chat; a wake,
/// delegation or agent message has no one waiting on its wording.
pub fn requested(run: &crate::runtime::Run) -> bool {
    use crate::runtime::RunKind;
    matches!(run.kind, RunKind::Chat | RunKind::Steering | RunKind::DesignAnswer)
}
/// The reply an accepted handoff publishes. A requested turn answers with the
/// agent's last message; any other turn keeps its progress line.
pub fn reply(requested: bool, activity: &[crate::usage::Activity], progress: Option<&str>) -> String {
    let said = activity
        .iter()
        .rev()
        .filter(|_| requested)
        .find(|a| a.kind == "message" && !a.text.trim().is_empty());
    said.map(|a| a.text.as_str())
        .or(progress)
        .unwrap_or("Agent yielded with a committed handoff.")
        .to_owned()
}
/// Stop all attempt-owned writers using current durable job identities. Persistent
/// services are separate resources and intentionally survive a model yield.
pub fn stop(
    isolation: &dyn control_isolation::Isolation,
    bridge: Option<(&Store, &str)>,
    attempt: &Attempt,
) -> Result<(), &'static str> {
    let mut identity = attempt.clone();
    if let Some((shared, id)) = bridge {
        shared
            .read(|state| -> Result<(), &str> {
                let live = state
                    .runtime
                    .runs
                    .get(id)
                    .and_then(|r| r.attempts.last())
                    .filter(|a| a.workspace_id == attempt.workspace_id)
                    .ok_or("Attempt generation changed while stopping")?;
                identity.command_jobs = live
                    .command_jobs
                    .iter()
                    .map(crate::command_jobs::Job::identity)
                    .collect();
                Ok(())
            })
            .map_err(|_| "State unavailable while stopping attempt")??;
    }
    crate::lifecycle::stop_attempt(isolation, &identity)
}

#[cfg(test)]
mod tests;
