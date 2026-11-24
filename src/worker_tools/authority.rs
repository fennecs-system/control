//! A worker request's authority as a value: the one place that finds the
//! caller's run and checks it may act.
use super::deferred::Facts;
use crate::store::Guard;
use crate::{State, identity, runtime::RunStatus};
use std::path::Path;

/// Proof that a request's run is running, its actor is authorized and the
/// mailbox belongs to the run's current attempt. Only [`admit`] builds one,
/// from the state behind the held lock. Handlers take it instead of finding
/// and checking the run again; no handler step that runs after the lock is
/// released is given one.
pub(super) struct Caller<'r> {
    run: &'r str,
    /// The run's ledger position at admission. The lock stays held until the
    /// handler that receives this returns, and runs are only appended (or
    /// rolled back to their earlier length) meanwhile, so it stays valid.
    index: usize,
    facts: Facts,
}
impl Caller<'_> {
    pub(super) fn id(&self) -> &str {
        self.run
    }
    pub(super) fn run<'s>(&self, state: &'s State) -> &'s crate::runtime::Run {
        let run = &state.runtime.runs[self.index];
        debug_assert_eq!(run.id().as_str(), self.run);
        run
    }
    /// What the run's session may see and call, captured at admission.
    pub(super) fn facts(&self) -> &Facts {
        &self.facts
    }
    /// The current attempt's identity (its workspace id), if it has one.
    pub(super) fn generation<'s>(&self, state: &'s State) -> Option<&'s str> {
        self.run(state).attempts.last()?.workspace_id.as_deref()
    }
    pub(super) fn chat<'s>(&self, state: &'s State) -> Option<&'s str> {
        self.run(state).chat_id.as_deref()
    }
}

/// Admits a request for `run` arriving through `workspace` (the mailbox's
/// workspace; `None` for callers without one). Refusals keep their order:
/// an inactive run, then actor authority, then a mailbox from another attempt.
pub(super) fn admit<'r>(state: &Guard<'_>, run: &'r str, workspace: Option<&Path>) -> Result<Caller<'r>, &'static str> {
    let index = state
        .runtime
        .runs
        .position(run)
        .filter(|&index| state.runtime.runs[index].status == RunStatus::Running)
        .ok_or("Run is not active")?;
    let record = &state.runtime.runs[index];
    identity::authorize(&record.actor, record.action)?;
    // Every request is fenced before dispatch, including room writes and search.
    // A late mailbox from a previous attempt cannot acquire the retry's authority.
    if let Some(workspace) = workspace {
        let expected = record
            .attempts
            .last()
            .and_then(|a| a.workspace_id.as_deref())
            .and_then(|id| crate::runtime::workspace(id).ok())
            .and_then(|p| p.canonicalize().ok());
        if expected.as_deref() != Some(workspace) {
            return Err("Mailbox belongs to an inactive attempt");
        }
    }
    // Enabling a bot affects the next admission, not the protocol of an
    // already-running provider session whose input was captured earlier.
    let managed = crate::turn_context::managed(state, record)
        && record
            .attempts
            .last()
            .is_some_and(|attempt| attempt.turn_mailbox.is_some());
    Ok(Caller {
        run,
        index,
        facts: Facts::of(state, record, managed),
    })
}
