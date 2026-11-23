//! What a backend may ask of the host, as small traits over typed values. The
//! host implements them over its own state; a backend never sees that state.
use control_effect::sync::CancelToken;
use control_isolation::Isolation;
use std::{io, path::Path, path::PathBuf, sync::Arc, time::Duration};

/// The host's decision for a running turn, polled between waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Continue,
    /// A handoff was accepted; the reply it publishes.
    Finish(String),
    /// The attempt reached its time limit.
    TimedOut(Duration),
}

/// Which of the attempt's containers a stop covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The containers the attempt was admitted with.
    Admitted,
    /// Also the command jobs it started since, as the host now records them.
    Live,
}

/// A running turn's view of its attempt.
pub trait Host {
    /// The node the turn runs on.
    fn node(&self) -> &dyn Node;
    /// The run is no longer running, or Control is stopping.
    fn cancelled(&self) -> bool;
    /// The agent asked the operator a design question.
    fn design_requested(&self) -> bool;
    /// The host can no longer serve this turn.
    fn unavailable(&self) -> bool;
    /// Serves the tool requests waiting in `mailbox`.
    fn pump(&mut self, mailbox: &Path);
    /// `elapsed` counts from the attempt's admission.
    fn poll(&mut self, elapsed: Duration) -> Control;
    /// Stops the attempt's containers; the error is the operator's next step.
    fn stop(&mut self, scope: Stop) -> Result<(), String>;
    /// `prompt` with the chat's declared observation placed last.
    fn observed(&mut self, prompt: String) -> String;
    /// Where the turn's stream reports what it sees, from another thread.
    fn events(&self) -> Arc<dyn Events>;
    /// Someone waits on this turn's own reply (a chat message, not a wake).
    fn requested(&self) -> bool;
}

/// Records a running turn's decoded stream; safe to call from a reader thread.
pub trait Events: Send + Sync {
    fn activity(&self, activity: control_ai::Activity);
    /// The usage so far, for live totals; the finished attempt replaces it.
    fn usage(&self, usage: &control_ai::Usage);
    /// The provider changed files: record the workspace's diff.
    fn changed(&self);
}

/// The node's rules for a backend's own containers, outside any turn.
pub trait Node: Sync {
    fn id(&self) -> Result<String, String>;
    /// A private state directory for offline checks, by name.
    fn check_root(&self, name: &str) -> Result<PathBuf, String>;
    /// Control is stopping: start nothing, stop what runs.
    fn stopping(&self) -> bool;
    fn stop_token(&self) -> CancelToken;
    /// Runs `command` in the host's credential-free offline shell container
    /// over `workspace`; a command stopped at `deadline` fails with
    /// [`io::ErrorKind::TimedOut`].
    #[allow(clippy::too_many_arguments)]
    fn shell(
        &self,
        isolation: &dyn Isolation,
        image: &str,
        name: &str,
        node: &str,
        workspace: &Path,
        command: &str,
        deadline: Duration,
    ) -> io::Result<serde_json::Value>;
    /// Stops a container this node owns, after verifying its labels.
    fn stop_container(
        &self,
        isolation: &dyn Isolation,
        name: &str,
        node: &str,
        image: &str,
        workspace: &Path,
    ) -> Result<(), &'static str>;
}
