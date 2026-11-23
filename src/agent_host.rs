//! The host side of `control_agent`: this node's container rules, and a
//! running attempt's view for one backend turn. Backends see only these
//! traits; state, locks and attempt bookkeeping stay here.
use crate::{State, usage::Attempt};
use control_agent::{Control, Events, Failure, Host, Node, Outcome, Stop};
use control_effect::sync::CancelToken;
use control_isolation::Isolation;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// This node.
pub(crate) struct ThisNode;
pub(crate) const NODE: ThisNode = ThisNode;
impl Node for ThisNode {
    fn id(&self) -> Result<String, String> {
        crate::config::node_id().map_err(str::to_owned)
    }
    fn check_root(&self, name: &str) -> Result<PathBuf, String> {
        crate::config::state(name).map_err(str::to_owned)
    }
    fn stopping(&self) -> bool {
        crate::shutdown::requested()
    }
    fn stop_token(&self) -> CancelToken {
        crate::shutdown::token()
    }
    fn shell(
        &self,
        isolation: &dyn Isolation,
        image: &str,
        name: &str,
        node: &str,
        workspace: &Path,
        command: &str,
        deadline: Duration,
    ) -> io::Result<serde_json::Value> {
        crate::host_shell::shell_bounded(isolation, image, name, node, workspace, command, false, deadline)
    }
    fn stop_container(
        &self,
        isolation: &dyn Isolation,
        name: &str,
        node: &str,
        image: &str,
        workspace: &Path,
    ) -> Result<(), &'static str> {
        crate::lifecycle::stop_resource(isolation, name, node, image, workspace)
    }
}

type Bridge<'a> = Option<(&'a Arc<Mutex<State>>, &'a str)>;

/// One attempt's turn, as its backend sees it.
pub(crate) struct Attempted<'a> {
    /// The attempt's workspace identity: its generation.
    pub id: &'a str,
    pub isolation: &'a dyn Isolation,
    pub bridge: Bridge<'a>,
    pub attempt: &'a mut Attempt,
    /// The prepared workspace, canonical.
    pub workspace: &'a Path,
}
impl Host for Attempted<'_> {
    fn node(&self) -> &dyn Node {
        &NODE
    }
    fn cancelled(&self) -> bool {
        crate::lifecycle::cancelled(self.bridge)
    }
    fn design_requested(&self) -> bool {
        crate::design::requested(self.bridge)
    }
    fn unavailable(&self) -> bool {
        self.bridge.is_some_and(|(state, _)| state.is_poisoned())
    }
    fn pump(&mut self, mailbox: &Path) {
        if let Some((state, run)) = self.bridge {
            crate::worker_tools::pump_at(
                state,
                run,
                mailbox,
                self.workspace,
                self.attempt.context_trace.is_some(),
            );
        }
    }
    fn poll(&mut self, elapsed: Duration) -> Control {
        match crate::attempt_control::poll(self.bridge, self.attempt, elapsed) {
            crate::attempt_control::Decision::Continue => Control::Continue,
            crate::attempt_control::Decision::Finish(summary) => Control::Finish(summary),
            crate::attempt_control::Decision::TimedOut(seconds) => {
                self.attempt.termination = Some(crate::attempt_control::Termination::TimedOut);
                Control::TimedOut(Duration::from_secs(seconds))
            }
        }
    }
    fn stop(&mut self, scope: Stop) -> Result<(), String> {
        match scope {
            Stop::Admitted => crate::lifecycle::stop_attempt(self.isolation, self.attempt),
            Stop::Live => crate::attempt_control::stop(self.isolation, self.bridge, self.attempt),
        }
        .map_err(str::to_owned)
    }
    fn observed(&mut self, prompt: String) -> String {
        crate::observe::append(self.bridge, prompt)
    }
    fn events(&self) -> Arc<dyn Events> {
        Arc::new(Live {
            target: self.bridge.map(|(state, run)| (Arc::clone(state), run.to_owned())),
            generation: self.id.to_owned(),
            workspace: self.workspace.to_path_buf(),
        })
    }
    fn requested(&self) -> bool {
        self.bridge.is_some_and(|(state, run)| {
            state.lock().is_ok_and(|s| {
                s.runtime
                    .runs
                    .iter()
                    .any(|r| r.id == run && crate::attempt_control::requested(r))
            })
        })
    }
}

/// A turn's stream recorded into the live attempt, fenced by its generation.
struct Live {
    target: Option<(Arc<Mutex<State>>, String)>,
    generation: String,
    workspace: PathBuf,
}
impl Events for Live {
    fn activity(&self, activity: control_ai::Activity) {
        if let Some((state, run)) = &self.target
            && let Ok(mut state) = state.lock()
        {
            crate::worker::record_activity_for(&mut state, run, &self.generation, activity);
        }
    }
    fn usage(&self, usage: &control_ai::Usage) {
        if let Some((state, run)) = &self.target
            && let Ok(mut state) = state.lock()
        {
            crate::worker::record_usage_for(&mut state, run, &self.generation, usage.clone());
        }
    }
    fn changed(&self) {
        if let Some((state, run)) = &self.target {
            crate::worker::observe_workspace_diff(state, run, &self.generation, &self.workspace, "observed");
        }
    }
}

/// A failure as the attempt records it: its status and wording.
pub(crate) fn status(failure: &Failure) -> (&'static str, String) {
    (failure.status().as_str(), failure.message())
}

/// Applies a finished turn to the attempt: launch, usage, and the activity
/// and termination a failure implies.
pub(crate) fn finish(
    outcome: Outcome,
    attempt: &mut Attempt,
    bridge: Bridge<'_>,
    id: &str,
) -> Result<String, (&'static str, String)> {
    let result = match outcome {
        Outcome::NotLaunched(failure) => Err(failure),
        Outcome::Launched { result, usage } => {
            attempt.accounting = crate::usage::Accounting::Unknown;
            if let Some(usage) = usage {
                attempt.record_usage(usage);
            }
            result
        }
    };
    result.map_err(|failure| {
        if let Failure::Provider { cause, .. } = failure {
            attempt.cause = Some(cause);
        }
        if failure == Failure::UnfinishedTool {
            crate::worker::interrupt_unfinished_tools(bridge, id);
            attempt.termination = Some(crate::attempt_control::Termination::ProviderError);
        }
        status(&failure)
    })
}

#[cfg(test)]
mod tests;
