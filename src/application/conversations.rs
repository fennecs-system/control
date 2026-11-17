//! Conversation records shared by application operations and presentation.
use crate::identity::Actor;
use control_context::agent_policy::MessageSpeaker;
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub text: String,
    pub run_id: Option<String>,
    #[serde(default)]
    pub reply: String,
    /// Who wrote a message with no answering run. Omitted for the user, so
    /// older records (and older binaries reading newer ones) keep their shape.
    #[serde(default, skip_serializing_if = "MessageSpeaker::is_user")]
    pub speaker: MessageSpeaker,
}
impl Message {
    /// A Control notice recorded without a run, such as operator decisions.
    pub fn notice(text: impl Into<String>, run_id: Option<String>) -> Self {
        Self { text: text.into(), run_id, reply: String::new(), speaker: MessageSpeaker::Control }
    }
    /// Who speaks for this message. The answering run's kind decides when it
    /// exists; otherwise the recorded speaker, and finally decision notes stored
    /// before speakers were recorded, recognised by their exact heading.
    pub fn speaker(&self, run: Option<&super::runs::Run>) -> MessageSpeaker {
        if let Some(run) = run {
            return run.kind.speaker();
        }
        if !self.speaker.is_user() || self.legacy_notice() {
            MessageSpeaker::Control
        } else {
            MessageSpeaker::User
        }
    }
    /// A Control notice with no run, which presentation shows as one line.
    pub fn is_notice(&self, run: Option<&super::runs::Run>) -> bool {
        run.is_none() && self.speaker(None) == MessageSpeaker::Control
    }
    fn legacy_notice(&self) -> bool {
        self.run_id.is_none()
            && super::approval_resume::HEADINGS.iter().any(|heading| {
                self.text.strip_prefix(heading).is_some_and(|rest| rest.starts_with('\n'))
            })
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub model: Option<crate::providers::Model>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<Box<crate::turn_context::Managed>>,
    #[serde(default)]
    pub activity_order: u64,
    #[serde(default)]
    pub project: Option<crate::projects::Project>,
    #[serde(default)]
    pub parent_chat: Option<String>,
    #[serde(default)]
    pub fork_context: Option<String>,
    #[serde(default)]
    pub improve_mode: bool,
    #[serde(default)]
    pub improve_workspace: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub lenses: Option<Vec<String>>,
    #[serde(default)]
    pub design_context: Option<String>,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub lens: Option<String>,
    #[serde(default)]
    pub revision: u64,
    pub id: String,
    pub actor: Actor,
    /// Legacy free text (first message or objective); clients show a
    /// callsign or `name` instead (`crate::callsigns`). Kept for older builds.
    pub title: String,
    /// The operator's `/rename`. Absent: the chat's derived callsign.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The highest run `queue_order` when the operator last cancelled work in
    /// this chat (`/cancel` here, or `/cancel all` here or in an ancestor).
    /// Operator decisions on requests made by those runs never resume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_through: Option<u64>,
    /// The operator's MemoryTree edits: versioned summary revisions, pinned
    /// entries and ranges dropped from the agent's view. Absent until edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Box<crate::compaction::Edits>>,
    pub messages: Vec<Message>,
}
