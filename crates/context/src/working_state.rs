//! Model-authored working notes. Identity, instructions, grants and checkpoint
//! provenance are supplied separately by Control, never by these notes.
use serde::{Deserialize, Serialize};

pub const MAX_PAYLOAD_BYTES: usize = 12 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    #[default]
    Unknown,
    Tentative,
    Supported,
}
/// Models write confidence their own way ("high", 0.95): read what they mean
/// instead of refusing the whole handoff, and anything else as unknown.
impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Word(String),
            Number(f64),
            Other(serde::de::IgnoredAny),
        }
        Ok(match Written::deserialize(deserializer)? {
            Written::Word(word) => match word.trim().to_ascii_lowercase().as_str() {
                "supported" | "observed" | "verified" | "confirmed" | "certain" | "high" => Self::Supported,
                "tentative" | "reported" | "likely" | "medium" | "low" => Self::Tentative,
                _ => Self::Unknown,
            },
            Written::Number(number) if number >= 0.8 => Self::Supported,
            Written::Number(number) if number > 0.0 => Self::Tentative,
            _ => Self::Unknown,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub claim: String,
    #[serde(default)]
    pub confidence: Confidence,
    /// Read #133's empty placeholder without exposing a reference language.
    #[serde(default, skip_serializing)]
    pub evidence: [(); 0],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tried {
    pub approach: String,
    pub outcome: String,
    #[serde(default, skip_serializing)]
    pub evidence: [(); 0],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub action: String,
    pub expected: String,
    #[serde(default, skip_serializing)]
    pub resources: [(); 0],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Next {
    pub condition: String,
    pub action: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub subgoal: String,
    // Lists an agent has nothing for may be omitted rather than failing the handoff.
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub tried: Vec<Tried>,
    #[serde(default)]
    pub pending: Vec<Pending>,
    #[serde(default)]
    pub next: Vec<Next>,
}

fn text(value: &str) -> Result<(), &'static str> {
    if value.chars().count() > 1000 {
        Err(
            "Each working-state text field must contain at most 1000 characters: shorten the longest notes, or retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without payload to keep the committed notes",
        )
    } else {
        Ok(())
    }
}
impl Payload {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.subgoal.trim().is_empty() {
            return Err(
                "Working state requires a current subgoal: set payload.subgoal, or retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without payload to keep the committed notes",
            );
        }
        if self.findings.len() > 16
            || self.tried.len() > 16
            || self.pending.len() > 16
            || self.next.len() > 8
        {
            return Err(
                "Working state permits 16 findings, 16 tried approaches, 16 pending actions and 8 next actions: merge or drop older entries, or retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without payload to keep the committed notes",
            );
        }
        if serde_json::to_vec(self)
            .map_err(|_| {
                "Working state could not be encoded: ask the operator via control_design_ask"
            })?
            .len()
            > MAX_PAYLOAD_BYTES
        {
            return Err(
                "Working state must fit in 12 KiB UTF-8 JSON: remove redundant notes, or retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without payload to keep the committed notes",
            );
        }
        text(&self.subgoal)?;
        for f in &self.findings {
            text(&f.claim)?;
        }
        for t in &self.tried {
            text(&t.approach)?;
            text(&t.outcome)?;
        }
        for p in &self.pending {
            text(&p.action)?;
            text(&p.expected)?;
        }
        for n in &self.next {
            text(&n.condition)?;
            text(&n.action)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
