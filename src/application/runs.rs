//! Run records and construction without execution, rendering or I/O.
//! Constructor contracts and usage fixtures live in runs/tests.rs.
use crate::identity::{Action, Actor};
use control_context::agent_policy::RunKind;
use serde::{Deserialize, Serialize};
/// Run statuses, decided in one place.
pub(crate) mod status {
    /// Executing, queued or awaiting input: the run will still change.
    pub(crate) fn active(status: &str) -> bool {
        matches!(status, "queued" | "running" | "cancelling" | "waiting")
    }
    /// Every other status. A finished run does not execute again, though a
    /// resumable one can be continued by a new run, and a finished run's
    /// records (delivery, links) may still be updated.
    pub(crate) fn finished(status: &str) -> bool {
        !active(status)
    }
    /// Finished without success: a continuation can resume from it.
    pub(crate) fn resumable(status: &str) -> bool {
        matches!(status, "blocked" | "failed" | "interrupted" | "cancelled")
    }
    /// Finished because a continuation took over: its outcome is the continuation's.
    pub(crate) fn superseded(status: &str) -> bool {
        status == "continued"
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
    pub emitted: Option<String>,
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
    pub id: String,
    pub objective: crate::history::Spilled<String>,
    pub kind: RunKind,
    pub status: String,
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
        id: String,
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

    pub(crate) fn into_run(self, id: String, queue_order: u64, target: crate::providers::Target) -> Run {
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
            id,
            objective: self.objective.into(),
            kind: self.kind,
            source: self.source,
            status: "queued".into(),
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
        self.status = "completed".into();
        self.result = format!("{ABSORBED}{active}; see that session's response.").into();
    }
    /// The run that processed this turn's message while it was active, when
    /// this turn never ran itself.
    pub(crate) fn absorbed_by(&self) -> Option<&str> {
        if !self.attempts.is_empty() || self.status != "completed" {
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
