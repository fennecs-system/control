//! Wake selection belongs to the watch tool, not the generic supervisor.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WakePolicy {
    AnyLine {},
    ExitOnly {},
    Prefixes {
        #[serde(deserialize_with = "prefixes")]
        prefixes: Vec<String>,
    },
}
impl Default for WakePolicy {
    fn default() -> Self { Self::AnyLine {} }
}

fn prefixes<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<String>, D::Error> {
    let prefixes = Vec::<String>::deserialize(decoder)?;
    if prefixes.is_empty() || prefixes.len() > 8 || prefixes.iter().any(|p|
        p.is_empty() || p.len() > 128 || p.chars().any(char::is_control)
    ) {
        return Err(serde::de::Error::custom("Use 1–8 nonempty literal prefixes, each at most 128 bytes without control characters"));
    }
    Ok(prefixes)
}

impl WakePolicy {
    pub fn admits(&self, line: &[u8]) -> bool {
        match self {
            Self::AnyLine {} => true,
            Self::ExitOnly {} => false,
            Self::Prefixes { prefixes } => prefixes.iter().any(|p| line.starts_with(p.as_bytes())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounded_literal_selection_does_not_interpret_patterns() {
        let policy: WakePolicy = serde_json::from_value(json!({"kind":"prefixes","prefixes":["event:","[error]"]})).unwrap();
        assert!(policy.admits(b"event: done"));
        assert!(policy.admits(b"[error] disconnected"));
        assert!(!policy.admits(b"status: event: done"));
        assert!(!policy.admits(b"error disconnected"));
        assert!(!(WakePolicy::ExitOnly {}).admits(b"event: done"));
        for value in [
            json!({"kind":"prefixes","prefixes":[]}),
            json!({"kind":"prefixes","prefixes":[""]}),
            json!({"kind":"prefixes","prefixes":["x".repeat(129)]}),
            json!({"kind":"prefixes","prefixes":vec!["x"; 9]}),
            json!({"kind":"prefixes","prefixes":["x\ny"]}),
            json!({"kind":"exit_only","prefixes":["x"]}),
            json!({"kind":"unknown"}),
        ] {
            assert!(serde_json::from_value::<WakePolicy>(value.clone()).is_err(), "{value}");
        }
    }
}
