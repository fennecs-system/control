//! Conversation records shared by application operations and presentation.
use crate::identity::Actor;
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub text: String,
    pub run_id: Option<String>,
    #[serde(default)]
    pub reply: String,
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
    pub title: String,
    pub messages: Vec<Message>,
}
