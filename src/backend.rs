//! Closed backend selection, captured with each run and attempt.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Codex,
    Claude,
}
impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "OpenAI / Codex",
            Self::Claude => "Anthropic / Claude Code",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    pub fn default_model(self) -> String {
        match self {
            Self::Codex => crate::usage::default_model(),
            Self::Claude => "sonnet".into(),
        }
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
        }
    }
}
