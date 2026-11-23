//! Closed backend selection, captured with each run and attempt. What each
//! backend supplies is `control_agent::Agent`, implemented in its own crate.
pub(crate) use control_agent::{Agent, TextCall};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Codex,
    Claude,
    Grok,
    Antigravity,
    /// An operator-configured private inference endpoint, named by its text
    /// model ID. Text generation is available; agent turns are not yet.
    Local,
}
impl Backend {
    pub(crate) const ALL: [Self; 5] = [Self::Codex, Self::Claude, Self::Grok, Self::Antigravity, Self::Local];
    /// The backends that run a pinned CLI image and can be the node default.
    pub(crate) fn hosted() -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(|backend| backend.default_capable())
    }
    /// The only dispatch from the persisted tag to a backend.
    pub(crate) fn agent(self) -> &'static dyn Agent {
        match self {
            Self::Codex => &control_codex::Codex,
            Self::Claude => &control_claude::Claude,
            Self::Grok => &control_grok::Grok,
            Self::Antigravity => &control_antigravity::Antigravity,
            Self::Local => &crate::local_agent::Local,
        }
    }
    pub(crate) fn named(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|backend| backend.id() == id)
    }
    pub fn label(self) -> &'static str {
        self.agent().label()
    }
    pub fn id(self) -> &'static str {
        self.agent().id()
    }
    /// The backend's default model, unless its variable names a valid one.
    pub fn default_model(self) -> String {
        let agent = self.agent();
        agent
            .model_variable()
            .and_then(|name| std::env::var(name).ok())
            .filter(|model| crate::usage::valid_model(model))
            .unwrap_or_else(|| agent.default_model().to_owned())
    }
    pub fn default_capable(self) -> bool {
        self.agent().default_capable()
    }
    /// The operator's pinned image, from its variable or configuration file.
    pub fn image(self) -> Option<String> {
        let pin = self.agent().image();
        pin.variable
            .and_then(|name| std::env::var(name).ok())
            .or_else(|| std::fs::read_to_string(crate::config::directory().ok()?.join(pin.file)).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| crate::runtime::pinned_image(s))
    }
    /// Difference: Control runs this backend's tool loop itself (local endpoints).
    pub(crate) fn runs_control_loop(self) -> bool {
        matches!(self.agent().runs(), control_agent::Runs::Control)
    }
}
/// Whether any backend keeps `name` private in a workspace.
pub(crate) fn private_name(name: &str) -> bool {
    Backend::ALL
        .iter()
        .any(|backend| backend.agent().private_names().contains(&name))
}
/// Refusal shared by selection and launch for endpoints without declared tool calls.
pub const LOCAL_TEXT_ONLY: &str = "This local endpoint is declared text-only: use control_model_generate for inference, or edit the entry in Manage → Models and enable \"Serves tool calls\" for a tool-capable server (llama.cpp --jinja) to run agent turns. No run was started and no hosted model was substituted.";

#[cfg(test)]
mod tests;
