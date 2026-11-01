//! Shared prompt policy and attribution for harness-generated conversation events.

pub const CODE_LANGUAGE_GUIDANCE: &str = "Control's maintained source and integrations use Rust/WASM, Haskell, Lean and Nix. Task-specific programs outside Control's repository may use other languages inside explicitly granted tool containers, including Node or Python. Keep those programs and their dependencies inside their private containers; do not create host services or modify shared base images. This restriction on Control's source does not prohibit third-party tools or task-specific container code.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSpeaker {
    User,
    Control,
}

impl MessageSpeaker {
    pub fn for_run_kind(kind: Option<&str>) -> Self {
        match kind {
            Some("delegation-result" | "agent-completion" | "room-notification" | "port-decision") => Self::Control,
            _ => Self::User,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::User => "You",
            Self::Control => "Control",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_automatic_events_are_attributed_to_control() {
        for kind in ["delegation-result", "agent-completion", "room-notification", "port-decision"] {
            assert_eq!(MessageSpeaker::for_run_kind(Some(kind)), MessageSpeaker::Control);
        }
        for kind in [None, Some("chat"), Some("design-answer"), Some("steering"), Some("spawn")] {
            assert_eq!(MessageSpeaker::for_run_kind(kind), MessageSpeaker::User);
        }
    }
}
