//! Run records and construction without execution, rendering or I/O.
use crate::identity::{Action, Actor};
use control_context::agent_policy::RunKind;
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
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
    #[serde(default)]
    pub backend: crate::backend::Backend,
    #[serde(default)]
    pub queue_order: u64,
    #[serde(default)]
    pub model: Option<String>,
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
    pub(crate) fn into_run(
        self,
        id: String,
        queue_order: u64,
        backend: crate::backend::Backend,
        model: Option<String>,
    ) -> Run {
        Run {
            bot_turn: None,
            resource_grants: Vec::new(),
            spawns: Vec::new(),
            room_wake_used: false,
            emitted: None,
            persona: None,
            delegation: None,
            question: None,
            continuation: None.into(),
            backend,
            queue_order,
            model,
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
    }
}
impl Run {
    /// Milliseconds since the epoch when the run was queued, from its identifier.
    pub(crate) fn created_ms(&self) -> Option<u64> {
        let nanos = u128::from_str_radix(self.id.split('-').next()?, 16).ok()?;
        u64::try_from(nanos / 1_000_000).ok()
    }
}
