//! How a turn ends without a result, as data. The host turns a [`Failure`]
//! into the attempt's status and its user-facing wording here, in one place;
//! backends classify, they do not word.
use std::time::Duration;

/// A provider failure recognised in bounded diagnostics. Raw text is never shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    UnsupportedModel,
    /// The read-only session could not be used or refreshed.
    ReadOnlySession,
    PermissionDenied,
    AuthRejected,
    Tls,
    /// The pinned CLI rejected its runtime configuration.
    RejectedConfig,
    UsageLimit,
    Connection,
    /// The CLI's own sign-in is missing, expired or rejected (Antigravity).
    SignIn,
}

/// Classifies captured diagnostics; `None` when nothing known matches.
pub fn classify(bytes: &[u8]) -> Option<Cause> {
    let message = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|needle| message.contains(needle));
    [
        (&["not supported", "model_not_found"][..], Cause::UnsupportedModel),
        (
            &["read-only file system", "refresh token", "token refresh"],
            Cause::ReadOnlySession,
        ),
        (&["permission denied"], Cause::PermissionDenied),
        (
            &[
                "401",
                "unauthorized",
                "not logged in",
                "invalid api key",
                "authentication_failed",
            ],
            Cause::AuthRejected,
        ),
        (&["certificate", "ssl"], Cause::Tls),
        (
            &[
                "unexpected argument",
                "error parsing",
                "unknown variant",
                "unknown permission",
            ],
            Cause::RejectedConfig,
        ),
        (&["quota", "usage limit", "rate limit"], Cause::UsageLimit),
        (
            &["stream disconnected", "error sending request", "connection"],
            Cause::Connection,
        ),
    ]
    .into_iter()
    .find(|(needles, _)| any(needles))
    .map(|(_, cause)| cause)
}

impl Cause {
    /// The fixed sentence for `agent`, the backend's display name.
    pub fn message(self, agent: &str) -> String {
        match self {
            Self::UnsupportedModel => {
                format!("The signed-in account does not support the requested {agent} model or runtime feature.")
            }
            Self::ReadOnlySession => format!(
                "{agent} could not use or refresh the read-only account session. Refresh the host login and submit a new run. The credential mount was not made writable."
            ),
            Self::PermissionDenied => format!(
                "{agent} encountered a filesystem permission denial. Check credential ownership, ephemeral state, and sandbox compatibility."
            ),
            Self::AuthRejected => {
                format!("{agent} account authentication was rejected. Refresh the host login and submit a new run.")
            }
            Self::Tls => format!(
                "{agent} could not validate the provider TLS connection. Check the Nix worker certificate configuration."
            ),
            Self::RejectedConfig => format!(
                "The pinned {agent} worker rejected its runtime configuration. Check CLI and permission-profile compatibility."
            ),
            Self::UsageLimit => format!("The signed-in {agent} account reached a usage or rate limit."),
            Self::Connection => format!(
                "{agent} could not complete its provider connection. Check container network connectivity and account availability."
            ),
            Self::SignIn => {
                let command = agent.to_ascii_lowercase();
                format!(
                    "{agent}'s login is missing, expired or was rejected. Run control {command} check, then sign in again with control {command} login or replace the API key."
                )
            }
        }
    }
}

/// Where a cancelled turn stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cancelled {
    /// Before the provider started; the backend's display name when it says so.
    BeforeLaunch(Option<&'static str>),
    Running,
}

/// A turn that ended without a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Nothing was started: a login, probe, input or mount problem, in the
    /// backend's fixed wording.
    Blocked(String),
    /// The turn could not finish, in fixed wording.
    Failed(String),
    /// A provider failure the classifier recognised.
    Provider {
        agent: &'static str,
        cause: Cause,
    },
    /// The CLI's own bounded ending.
    Ended(String),
    /// The CLI listed or called a tool outside Control's; its own ending.
    UnexpectedTools(String),
    /// Nothing recognised: the backend's fixed sentence.
    Unclassified(&'static str),
    /// The attempt reached its time limit.
    TimedOut(Duration),
    Cancelled(Cancelled),
    /// The agent asked the operator a design question.
    Waiting,
    /// Stopped in a state that needs inspection before a retry.
    Interrupted(String),
    /// The provider ended inside a Control tool call: the effect is unconfirmed.
    UnfinishedTool,
}

/// The attempt status a failure leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Blocked,
    Failed,
    Cancelled,
    Waiting,
    Interrupted,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Waiting => "waiting",
            Self::Interrupted => "interrupted",
        }
    }
}

pub const WAITING: &str = "Waiting for a design decision.";
pub const CANCELLED: &str = "Cancelled. The previous published workspace is unchanged.";
pub const UNFINISHED_TOOL: &str = "The provider ended with an unfinished MCP tool call. Inspect pending requests and service status before retrying: the tool's outcome is unconfirmed. The published checkpoint is unchanged; check recovery evidence for unpublished work.";

/// The message for a turn stopped at its time limit.
pub fn timeout_message(limit: Duration) -> String {
    format!(
        "Worker reached its {} s time limit and was stopped. Files are retained in the run directory; the previous published checkpoint is unchanged. Hand off before the limit and continue in the next turn; operators can raise it with worker-timeout-secs (all turns), local-turn-timeout-secs (local-model turns) or bot-turn-timeout-secs (approved persistent bots) in the node configuration.",
        limit.as_secs()
    )
}

impl Failure {
    pub fn status(&self) -> Status {
        match self {
            Self::Blocked(_) => Status::Blocked,
            Self::Failed(_)
            | Self::Provider { .. }
            | Self::Ended(_)
            | Self::UnexpectedTools(_)
            | Self::Unclassified(_)
            | Self::TimedOut(_) => Status::Failed,
            Self::Cancelled(_) => Status::Cancelled,
            Self::Waiting => Status::Waiting,
            Self::Interrupted(_) | Self::UnfinishedTool => Status::Interrupted,
        }
    }
    /// The user-facing wording.
    pub fn message(&self) -> String {
        match self {
            Self::Blocked(text)
            | Self::Failed(text)
            | Self::Ended(text)
            | Self::UnexpectedTools(text)
            | Self::Interrupted(text) => text.clone(),
            Self::Provider { agent, cause } => cause.message(agent),
            Self::Unclassified(text) => (*text).to_owned(),
            Self::TimedOut(limit) => timeout_message(*limit),
            Self::Cancelled(Cancelled::BeforeLaunch(Some(agent))) => format!("Cancelled before {agent} launch."),
            Self::Cancelled(Cancelled::BeforeLaunch(None)) => "Cancelled before launch.".to_owned(),
            Self::Cancelled(Cancelled::Running) => CANCELLED.to_owned(),
            Self::Waiting => WAITING.to_owned(),
            Self::UnfinishedTool => UNFINISHED_TOOL.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests;
