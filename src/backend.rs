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
    /// Local endpoints are chosen per chat or spawn from the endpoint registry;
    /// they are never the saved node default.
    pub fn default_capable(self) -> bool {
        !matches!(self, Self::Local)
    }
    /// The pinned image for the attempt's own offline tools. Local models run
    /// their loop in Control and use the Codex worker image only for
    /// credential-free workspace commands.
    pub fn image(self) -> Option<String> {
        match self {
            Self::Codex | Self::Local => crate::runtime::worker_image(),
            Self::Claude => {
                std::fs::read_to_string(crate::config::directory().ok()?.join("claude-image-id"))
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| crate::runtime::pinned_image(s))
            }
        }
    }
}
/// Refusal shared by selection and launch for endpoints without declared tool calls.
pub const LOCAL_TEXT_ONLY: &str = "This local endpoint is declared text-only: use control_model_generate for inference, or edit the entry in Manage → Models and enable \"Serves tool calls\" for a tool-capable server (llama.cpp --jinja) to run agent turns. No run was started and no hosted model was substituted.";
