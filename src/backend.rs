//! Closed backend selection, captured with each run and attempt.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Codex,
    Claude,
    /// An operator-configured private inference endpoint, named by its text
    /// model ID. Text generation is available; agent turns are not yet.
    Local,
}
impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "OpenAI / Codex",
            Self::Claude => "Anthropic / Claude Code",
            Self::Local => "Local endpoint",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Local => "local",
        }
    }
    pub fn default_model(self) -> String {
        match self {
            Self::Codex => crate::usage::default_model(),
            Self::Claude => "sonnet".into(),
            Self::Local => String::new(),
        }
    }
    /// Whether Control can run an agent attempt (tool loop, recovery, cancellation)
    /// on this backend. Selection refuses others before any effect.
    pub fn runs_agents(self) -> bool {
        !matches!(self, Self::Local)
    }
    pub fn image(self) -> Option<String> {
        match self {
            Self::Codex => crate::runtime::worker_image(),
            Self::Claude => {
                std::fs::read_to_string(crate::config::directory().ok()?.join("claude-image-id"))
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| crate::runtime::pinned_image(s))
            }
            Self::Local => None,
        }
    }
}
/// Refusal shared by selection and launch until the local tool loop lands.
pub const LOCAL_AGENT_PENDING: &str = "Local endpoint models are text-only in Control today: use control_model_generate for inference. Local agent turns (tool calls, cancellation, recovery) are not implemented yet, so no run was started and no hosted model was substituted.";
