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
    /// The account reached its usage or rate limit; when to try again, if
    /// the provider's message says.
    UsageLimit {
        retry_at: Option<StatedRetry>,
    },
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
        (
            &["quota", "usage limit", "rate limit"],
            Cause::UsageLimit { retry_at: None },
        ),
        (
            &["stream disconnected", "error sending request", "connection"],
            Cause::Connection,
        ),
    ]
    .into_iter()
    .find(|(needles, _)| any(needles))
    .map(|(_, cause)| match cause {
        Cause::UsageLimit { .. } => Cause::UsageLimit {
            retry_at: stated_retry(&message),
        },
        other => other,
    })
}

/// When a provider's limit message says to try again, as written. Codex
/// writes it in the CLI's local zone (`format_retry_timestamp` in
/// codex-rs/protocol/src/error.rs); Control's pinned worker images carry no
/// zone data and set no `TZ`, so that zone is UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatedRetry {
    /// `Nov 26th, 2026 12:24 PM`: a full date, as seconds since the epoch.
    On(u64),
    /// `12:24 PM`: a time on the day the message was written, as minutes
    /// after midnight. Which day that was is the host's to decide.
    Today(u16),
}

/// The retry time after "try again at" in the two forms Codex 0.164.0 writes
/// (`%b %-d{st|nd|rd|th}, %Y %-I:%M %p` for another day, `%-I:%M %p` for the
/// same day); `None` for anything else.
pub fn stated_retry(message: &str) -> Option<StatedRetry> {
    let message = message.to_ascii_lowercase();
    let (_, rest) = message.split_once("try again at ")?;
    let words: Vec<&str> = rest.split_whitespace().take(5).collect();
    match words.as_slice() {
        [month, day, year, time, meridiem] => on(month, day, year, time, meridiem),
        _ => None,
    }
    .or_else(|| match words.as_slice() {
        [time, meridiem, ..] => clock(time, meridiem).map(StatedRetry::Today),
        _ => None,
    })
}
/// Minutes after midnight of `12:24` `pm`. Whatever follows the meridiem
/// (`.`, or the `"}` closing a JSON event line) is not part of it.
fn clock(time: &str, meridiem: &str) -> Option<u16> {
    let (hour, minute) = time.split_once(':')?;
    let (hour, minute) = (hour.parse::<u16>().ok()?, minute.parse::<u16>().ok()?);
    let word = meridiem
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or_default();
    let afternoon = match word {
        "am" => 0,
        "pm" => 12,
        _ => return None,
    };
    ((1..=12).contains(&hour) && minute < 60).then_some(((hour % 12) + afternoon) * 60 + minute)
}
/// `nov` `26th,` `2026` `12:24` `pm.` as seconds since the epoch.
fn on(month: &str, day: &str, year: &str, time: &str, meridiem: &str) -> Option<StatedRetry> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let month = MONTHS.iter().position(|m| *m == month)? as i64 + 1;
    let day = day
        .strip_suffix(',')?
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .parse::<i64>()
        .ok()?;
    let year = year.parse::<i64>().ok().filter(|year| *year >= 1970)?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let length = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    let minutes = clock(time, meridiem)?;
    (1..=length)
        .contains(&day)
        .then(|| days_from_civil(year, month, day) * 86_400 + i64::from(minutes) * 60)
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map(StatedRetry::On)
}
/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The command that signs a turn's account in again, exactly as the
/// operator types it (`control claude login --account second`). The host
/// knows the account; a backend never does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignIn(String);
impl From<String> for SignIn {
    fn from(command: String) -> Self {
        Self(command)
    }
}
impl SignIn {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Cause {
    /// The fixed sentence for `agent`, the backend's display name; a sign-in
    /// failure names the one command that recovers this account.
    pub fn message(self, agent: &str, sign_in: &SignIn) -> String {
        let again = sign_in.as_str();
        match self {
            Self::UnsupportedModel => {
                format!("The signed-in account does not support the requested {agent} model or runtime feature.")
            }
            Self::ReadOnlySession => format!(
                "{agent} could not use or refresh its account session. Sign in again with {again} and submit a new run."
            ),
            Self::PermissionDenied => format!(
                "{agent} encountered a filesystem permission denial. Check credential ownership, ephemeral state, and sandbox compatibility."
            ),
            Self::AuthRejected => {
                format!("{agent} account authentication was rejected. Sign in again with {again} and submit a new run.")
            }
            Self::Tls => format!(
                "{agent} could not validate the provider TLS connection. Check the Nix worker certificate configuration."
            ),
            Self::RejectedConfig => format!(
                "The pinned {agent} worker rejected its runtime configuration. Check CLI and permission-profile compatibility."
            ),
            Self::UsageLimit { .. } => format!("The signed-in {agent} account reached a usage or rate limit."),
            Self::Connection => format!(
                "{agent} could not complete its provider connection. Check container network connectivity and account availability."
            ),
            Self::SignIn => {
                format!(
                    "{agent}'s login is missing, expired or was rejected. Sign in again with {again} and submit a new run."
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
    /// The user-facing wording; `sign_in` recovers the turn's account.
    pub fn message(&self, sign_in: &SignIn) -> String {
        match self {
            Self::Blocked(text)
            | Self::Failed(text)
            | Self::Ended(text)
            | Self::UnexpectedTools(text)
            | Self::Interrupted(text) => text.clone(),
            Self::Provider { agent, cause } => cause.message(agent, sign_in),
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
