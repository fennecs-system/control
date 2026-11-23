//! Run records and construction without execution, rendering or I/O.
//! Constructor contracts and usage fixtures live in runs/tests.rs.
use crate::identity::{Action, Actor};
use control_context::agent_policy::RunKind;
use serde::{Deserialize, Serialize};
/// A run's lifecycle status, saved as its lowercase name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Queued,
    Running,
    Cancelling,
    Waiting,
    Completed,
    Blocked,
    Failed,
    Interrupted,
    Cancelled,
    Continued,
    /// A saved status no current code writes, kept verbatim so old state and
    /// history still load and save unchanged. It is finished and inactive.
    #[serde(untagged)]
    Unrecognised(String),
}
impl RunStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Continued => "continued",
            Self::Unrecognised(status) => status,
        }
    }
    /// Executing, queued or awaiting input: the run will still change.
    pub fn is_active(&self) -> bool {
        match self {
            Self::Queued | Self::Running | Self::Cancelling | Self::Waiting => true,
            Self::Completed
            | Self::Blocked
            | Self::Failed
            | Self::Interrupted
            | Self::Cancelled
            | Self::Continued
            | Self::Unrecognised(_) => false,
        }
    }
    /// Every other status. A finished run does not execute again, though a
    /// resumable one can be continued by a new run, and a finished run's
    /// records (delivery, links) may still be updated.
    pub fn is_finished(&self) -> bool {
        !self.is_active()
    }
    /// Finished without success: a continuation can resume from it.
    pub fn is_resumable(&self) -> bool {
        match self {
            Self::Blocked | Self::Failed | Self::Interrupted | Self::Cancelled => true,
            Self::Queued
            | Self::Running
            | Self::Cancelling
            | Self::Waiting
            | Self::Completed
            | Self::Continued
            | Self::Unrecognised(_) => false,
        }
    }
    /// Test fixtures name statuses as text; an unknown name is a typo there.
    #[cfg(test)]
    pub(crate) fn named(status: &str) -> Self {
        match serde_json::from_value(serde_json::Value::String(status.into())) {
            Ok(Self::Unrecognised(_)) | Err(_) => panic!("not a run status: {status}"),
            Ok(known) => known,
        }
    }
    /// Active and not being cancelled: queued, running or awaiting input.
    pub fn is_proceeding(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Waiting)
    }
    /// Not stopped: still proceeding, or ended by success or a continuation.
    /// Cancelling, blocked, failed, interrupted and cancelled work is stopped.
    pub fn is_unstopped(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::Waiting | Self::Completed | Self::Continued
        )
    }
    /// Stopped work: being cancelled, or ended without success. An
    /// unrecognised status is neither stopped nor unstopped.
    pub fn is_stopped(&self) -> bool {
        matches!(
            self,
            Self::Cancelling | Self::Blocked | Self::Failed | Self::Interrupted | Self::Cancelled
        )
    }
    /// Holds a worker slot: executing, or stopping an execution.
    pub fn occupies_slot(&self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
}
impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
/// A failed or paused turn's outcome becomes its run's status.
impl From<control_agent::Status> for RunStatus {
    fn from(status: control_agent::Status) -> Self {
        use control_agent::Status;
        match status {
            Status::Blocked => Self::Blocked,
            Status::Failed => Self::Failed,
            Status::Cancelled => Self::Cancelled,
            Status::Waiting => Self::Waiting,
            Status::Interrupted => Self::Interrupted,
        }
    }
}
/// A run as saved. Code uses [`Run`], which remembers its saved form between
/// saves (see `history::Cached`); finished runs then cost a copy, not a
/// re-serialization.
pub type Run = crate::history::Cached<Record>;
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_turn: Option<Box<crate::bots::Turn>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_grants: Vec<crate::services::Grant>,
    #[serde(default)]
    pub spawns: Vec<crate::spawning::Batch>,
    #[serde(default)]
    pub room_wake_used: bool,
    /// The last status sent to frontends through the outbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted: Option<RunStatus>,
    #[serde(default)]
    pub persona: Option<crate::delegation::Persona>,
    #[serde(default)]
    pub delegation: Option<crate::delegation::Delegation>,
    #[serde(default)]
    pub question: Option<crate::design::Question>,
    #[serde(default)]
    pub continuation: crate::history::Spilled<Option<crate::design::Continuation>>,
    /// Backend, captured account and model, saved as the flat `backend`,
    /// `account` and `model` fields of older records.
    #[serde(flatten)]
    pub target: crate::providers::Target,
    #[serde(default)]
    pub queue_order: u64,
    #[serde(default)]
    pub context: crate::history::Spilled<Option<control_context::Resolved>>,
    #[serde(default)]
    pub lenses: Option<Vec<String>>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub attempts: Vec<crate::usage::Attempt>,
    #[serde(default)]
    pub actor: Actor,
    #[serde(default)]
    pub action: Action,
    // Private: the run ledger (`Runs`) indexes runs by id, so no `&mut Run`
    // may change it. Read it with `id()`.
    id: RunId,
    pub objective: crate::history::Spilled<String>,
    pub kind: RunKind,
    pub status: RunStatus,
    pub source: String,
    pub result: crate::history::Spilled<String>,
}
/// A run's input, independent of clock, authorization, storage and wakeups.
/// Drivers authorize/admit it before publishing the constructed run.
pub(crate) struct Submission {
    pub objective: String,
    pub kind: RunKind,
    pub source: String,
    pub actor: Actor,
    pub action: Action,
}
impl Submission {
    /// Construct a fresh continuation from captured settings. The host supplies
    /// current grants, authorization/admission, identity and queue position.
    /// Never clone the origin's attempts, pending requests or execution state.
    pub(crate) fn into_continuation(
        self,
        origin: &Run,
        chat: &str,
        id: impl Into<RunId>,
        queue_order: u64,
        grants: Vec<crate::services::Grant>,
    ) -> Run {
        let mut run = self.into_run(id, queue_order, origin.target.clone());
        run.chat_id = Some(chat.into());
        run.context = origin.context.clone();
        run.lenses = origin.lenses.clone();
        run.persona = Some(crate::delegation::lineage(origin));
        run.resource_grants = grants;
        run
    }

    pub(crate) fn into_run(self, id: impl Into<RunId>, queue_order: u64, target: crate::providers::Target) -> Run {
        Record {
            bot_turn: None,
            resource_grants: Vec::new(),
            spawns: Vec::new(),
            room_wake_used: false,
            emitted: None,
            persona: None,
            delegation: None,
            question: None,
            continuation: None.into(),
            target,
            queue_order,
            context: None.into(),
            lenses: None,
            chat_id: None,
            attempts: Vec::new(),
            actor: self.actor,
            action: self.action,
            id: id.into(),
            objective: self.objective.into(),
            kind: self.kind,
            source: self.source,
            status: RunStatus::Queued,
            result: String::new().into(),
        }
        .into()
    }
}

#[cfg(test)]
mod tests;
impl Record {
    /// Milliseconds since the epoch when the run was queued, from its identifier.
    pub(crate) fn created_ms(&self) -> Option<u64> {
        let nanos = u128::from_str_radix(self.id.split('-').next()?, 16).ok()?;
        u64::try_from(nanos / 1_000_000).ok()
    }
    /// Retires this queued turn: the active run `active` of its chat already
    /// delivered and processed its message.
    pub(crate) fn absorb(&mut self, active: &str) {
        self.status = RunStatus::Completed;
        self.result = format!("{ABSORBED}{active}; see that session's response.").into();
    }
    /// The run that processed this turn's message while it was active, when
    /// this turn never ran itself.
    pub(crate) fn absorbed_by(&self) -> Option<&str> {
        if !self.attempts.is_empty() || self.status != RunStatus::Completed {
            return None;
        }
        self.result
            .strip_prefix(ABSORBED)?
            .split_once(';')
            .map(|(active, _)| active)
    }
}
/// The result prefix of a queued turn that an active run processed instead.
const ABSORBED: &str = "Processed in active run ";
mod ledger;
pub(crate) use ledger::Runs;

/// A run's identifier: `<creation stamp in hex>-<sequence>`, saved as a string.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RunId(String);
impl RunId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::ops::Deref for RunId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl std::borrow::Borrow<str> for RunId {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl AsRef<str> for RunId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<String> for RunId {
    fn from(id: String) -> Self {
        Self(id)
    }
}
impl From<&str> for RunId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}
impl From<RunId> for String {
    fn from(id: RunId) -> Self {
        id.0
    }
}
impl PartialEq<str> for RunId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}
impl PartialEq<&str> for RunId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}
impl PartialEq<String> for RunId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}
impl Record {
    pub(crate) fn id(&self) -> &RunId {
        &self.id
    }
}
#[cfg(test)]
impl Run {
    /// The same run under another id, for fixtures, before it joins a ledger;
    /// a run inside `Runs` cannot be renamed.
    pub(crate) fn with_id(mut self, id: impl Into<RunId>) -> Self {
        self.id = id.into();
        self
    }
}
// Comparisons as they read at call sites: `run.id() == chat.run_id` and the like.
impl PartialEq<String> for &RunId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}
