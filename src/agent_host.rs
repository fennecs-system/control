//! The host side of `control_agent`: this node's container rules, and a
//! running attempt's view for one backend turn. Backends see only these
//! traits; state, locks and attempt bookkeeping stay here.
use crate::store::Store;
use crate::usage::Attempt;
use control_agent::{Control, Events, Failure, Host, Node, Outcome, Stop};
use control_effect::sync::CancelToken;
use control_isolation::Isolation;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
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

type Bridge<'a> = Option<(&'a Store, &'a str)>;

/// One attempt's turn, as its backend sees it.
pub(crate) struct Attempted<'a> {
    /// The attempt's workspace identity: its generation.
    pub id: &'a str,
    pub isolation: &'a dyn Isolation,
    pub bridge: Bridge<'a>,
    pub attempt: &'a mut Attempt,
    /// The prepared workspace, canonical.
    pub workspace: &'a Path,
    /// The chat's compactor, for a loop that compacts inside its turn.
    pub compactor: Option<crate::model_calls::Endpoint>,
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
        self.bridge.is_some_and(|(state, _)| state.is_unavailable())
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
    fn serve(&mut self, nonce: &str, request: serde_json::Value) -> serde_json::Value {
        let envelope = serde_json::json!({"nonce":nonce,"request":request});
        match self.bridge {
            Some((state, run)) => crate::worker_tools::local_rpc(
                state,
                run,
                &envelope,
                self.workspace,
                self.attempt.context_trace.is_some(),
            ),
            None => control_mcp::error(
                control_mcp::id(&envelope["request"]),
                -32603,
                "Worker tools need a live Control node",
            ),
        }
    }
    fn observation(&mut self, first: bool) -> Option<String> {
        self.bridge
            .and_then(|(state, run)| crate::observe::step(state, run, first))
    }
    fn inbox(&mut self) -> Option<String> {
        // Same transport choice as the CLI mailboxes: managed turns get callbacks.
        let (state, run) = self.bridge.filter(|_| self.attempt.context_trace.is_some())?;
        state
            .write(|state| crate::turn_context::inbox(state, run, crate::storage::save))
            .ok()
            .flatten()
    }
    fn continues(&self) -> bool {
        self.bridge.is_none_or(|(state, run)| {
            state
                .read(|state| {
                    let chat = state.runtime.runs.get(run).and_then(|run| run.chat_id.as_deref());
                    chat.is_some_and(|chat| crate::bots::continuation_enabled(state, chat))
                })
                .unwrap_or(true)
        })
    }
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String> {
        let compactor = self.compactor.as_ref().ok_or_else(|| io::Error::other(NO_COMPACTOR))?;
        crate::model_calls::call(compactor, &crate::compaction::system_within(words), evidence).map(|r| r.text)
    }
    fn retain(
        &mut self,
        block: control_comonad::Block,
        text: &str,
        children: Option<[String; 2]>,
        sources: Vec<String>,
    ) -> io::Result<String> {
        let compactor = self.compactor.as_ref().ok_or_else(|| io::Error::other(NO_COMPACTOR))?;
        let (state, run) = self
            .bridge
            .ok_or_else(|| io::Error::other("Recall needs a live Control node"))?;
        crate::compaction::turn::retain(state, run, self.id, compactor, block, text, children, sources)
    }
    fn reasoned(&mut self, reasoning: &str) {
        self.attempt.interrupted_reasoning = Some(crate::turn_context::carried_reasoning(reasoning));
    }
    fn events(&self) -> Arc<dyn Events> {
        Arc::new(Live {
            target: self.bridge.map(|(state, run)| (state.clone(), run.to_owned())),
            generation: self.id.to_owned(),
            workspace: self.workspace.to_path_buf(),
        })
    }
    fn requested(&self) -> bool {
        self.bridge.is_some_and(|(state, run)| {
            state
                .read(|s| s.runtime.runs.get(run).is_some_and(crate::attempt_control::requested))
                .unwrap_or(false)
        })
    }
}

/// A turn's stream recorded into the live attempt, fenced by its generation.
struct Live {
    target: Option<(Store, String)>,
    generation: String,
    workspace: PathBuf,
}
impl Events for Live {
    fn activity(&self, activity: control_ai::Activity) {
        if let Some((state, run)) = &self.target {
            let _ = state.write(|state| crate::worker::record_activity_for(state, run, &self.generation, activity));
        }
    }
    fn usage(&self, usage: &control_ai::Usage) {
        if let Some((state, run)) = &self.target {
            let _ = state.write(|state| crate::worker::record_usage_for(state, run, &self.generation, usage.clone()));
        }
    }
    fn changed(&self) {
        if let Some((state, run)) = &self.target {
            crate::worker::observe_workspace_diff(state, run, &self.generation, &self.workspace, "observed");
        }
    }
}

const NO_COMPACTOR: &str = "This turn has no compactor";

/// The operator's model-step cap, and whether the attempt resumes a cut-off
/// turn: the chat holds a host checkpoint, or the run already had an attempt
/// (the live one is its last).
pub(crate) fn continuity(bridge: Bridge<'_>) -> (usize, bool) {
    let default = || crate::scheduler::Limits::default().local_agent_steps;
    let Some((state, run)) = bridge else {
        return (default(), false);
    };
    state
        .read(|state| {
            let current = state.runtime.runs.get(run);
            let resumed = current
                .and_then(|r| r.chat_id.as_ref())
                .and_then(|chat| state.chats.iter().find(|c| &c.id == chat))
                .and_then(|s| s.continuity.as_ref())
                .is_some_and(|c| c.interrupted.is_some())
                || current.is_some_and(|r| r.attempts.len() > 1);
            (state.runtime.execution.local_agent_steps, resumed)
        })
        .unwrap_or_else(|_| (default(), false))
}

/// A failure as the attempt records it: its status and wording, which names
/// `sign_in` when the account must sign in again.
pub(crate) fn status(failure: &Failure, sign_in: &control_agent::SignIn) -> (control_agent::Status, String) {
    (failure.status(), failure.message(sign_in))
}

/// Applies a finished turn to the attempt: launch, usage, and the activity
/// and termination a failure implies.
pub(crate) fn finish(
    outcome: Outcome,
    attempt: &mut Attempt,
    bridge: Bridge<'_>,
    id: &str,
    sign_in: &control_agent::SignIn,
) -> Result<String, (control_agent::Status, String)> {
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
        // The next turn resumes from the host checkpoint this termination writes.
        if let Failure::StepLimit(_) = failure {
            attempt.termination = Some(crate::attempt_control::Termination::StepLimit);
        }
        if failure == Failure::UnfinishedTool {
            crate::worker::interrupt_unfinished_tools(bridge, id);
            attempt.termination = Some(crate::attempt_control::Termination::ProviderError);
        }
        status(&failure, sign_in)
    })
}

#[cfg(test)]
mod tests;
