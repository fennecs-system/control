//! Model-authored working notes. Identity, instructions, grants and checkpoint
//! provenance are supplied separately by Control, never by these notes.
use serde::{Deserialize, Serialize};

pub const MAX_PAYLOAD_BYTES: usize = 12 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Unknown,
    Tentative,
    Supported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub claim: String,
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
    pub findings: Vec<Finding>,
    pub tried: Vec<Tried>,
    pub pending: Vec<Pending>,
    pub next: Vec<Next>,
}

fn text(value: &str) -> Result<(), &'static str> {
    if value.chars().count() > 1000 {
        Err("Each working-state text field must contain at most 1000 characters")
    } else {
        Ok(())
    }
}
impl Payload {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.subgoal.trim().is_empty() {
            return Err("Working state requires a current subgoal");
        }
        if self.findings.len() > 16
            || self.tried.len() > 16
            || self.pending.len() > 16
            || self.next.len() > 8
        {
            return Err(
                "Working state permits 16 findings, 16 tried approaches, 16 pending actions and 8 next actions",
            );
        }
        if serde_json::to_vec(self)
            .map_err(|_| "Working state could not be encoded")?
            .len()
            > MAX_PAYLOAD_BYTES
        {
            return Err("Working state must fit in 12 KiB UTF-8 JSON; remove redundant notes");
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
