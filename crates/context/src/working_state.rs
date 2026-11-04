//! Model-authored working notes. Identity, instructions, grants and checkpoint
//! provenance are supplied separately by Control, never by these notes.
use serde::{Deserialize, Serialize};

pub const MAX_PAYLOAD_BYTES: usize = 12 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceRef {
    Journal {
        run: String,
        attempt: u32,
        event: String,
    },
    Room {
        room: String,
        after: u64,
        through: u64,
    },
    Watch {
        watch: String,
        after: u64,
        through: u64,
    },
    Artifact {
        run: String,
        path: String,
    },
    Command {
        service: String,
        execution: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceRef {
    Service {
        id: String,
        generation: u64,
    },
    Watch {
        id: String,
        service: String,
        generation: u64,
    },
    Port {
        id: String,
        service: String,
        generation: u64,
    },
    Job {
        id: String,
        run: String,
        attempt: String,
    },
}

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
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tried {
    pub approach: String,
    pub outcome: String,
    pub evidence: Vec<EvidenceRef>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub action: String,
    pub resources: Vec<ResourceRef>,
    pub expected: String,
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
fn identity(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 128 || value.bytes().any(|b| b.is_ascii_control()) {
        Err("Evidence/resource identifiers must contain 1–128 bytes and no control characters")
    } else {
        Ok(())
    }
}
impl EvidenceRef {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Journal { run, event, .. } => {
                identity(run)?;
                identity(event)
            }
            Self::Room {
                room,
                after,
                through,
            } => {
                identity(room)?;
                if through <= after {
                    Err("Evidence range must have through greater than after")
                } else {
                    Ok(())
                }
            }
            Self::Watch {
                watch,
                after,
                through,
            } => {
                identity(watch)?;
                if through <= after {
                    Err("Evidence range must have through greater than after")
                } else {
                    Ok(())
                }
            }
            Self::Artifact { run, path } => {
                identity(run)?;
                if path.is_empty()
                    || path.len() > 1000
                    || path.starts_with('/')
                    || path.split('/').any(|p| matches!(p, "" | "." | ".."))
                    || path.bytes().any(|b| b.is_ascii_control())
                {
                    Err("Artifact evidence needs a bounded relative path without parent components")
                } else {
                    Ok(())
                }
            }
            Self::Command { service, execution } => {
                identity(service)?;
                identity(execution)
            }
        }
    }
}
impl ResourceRef {
    pub fn validate(&self) -> Result<(), &'static str> {
        let generation = match self {
            Self::Service { id, generation } => {
                identity(id)?;
                Some(*generation)
            }
            Self::Watch {
                id,
                service,
                generation,
            }
            | Self::Port {
                id,
                service,
                generation,
            } => {
                identity(id)?;
                identity(service)?;
                Some(*generation)
            }
            Self::Job { id, run, attempt } => {
                identity(id)?;
                identity(run)?;
                identity(attempt)?;
                None
            }
        };
        if generation == Some(0) {
            Err("Resource generation must be positive")
        } else {
            Ok(())
        }
    }
}
impl Payload {
    pub fn evidence(&self) -> impl Iterator<Item = &EvidenceRef> {
        self.findings
            .iter()
            .flat_map(|f| &f.evidence)
            .chain(self.tried.iter().flat_map(|t| &t.evidence))
    }
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
        if self.evidence().count() > 64
            || self
                .pending
                .iter()
                .map(|p| p.resources.len())
                .sum::<usize>()
                > 64
        {
            return Err("Working state permits at most 64 evidence and 64 resource references");
        }
        if serde_json::to_vec(self)
            .map_err(|_| "Working state could not be encoded")?
            .len()
            > MAX_PAYLOAD_BYTES
        {
            return Err(
                "Working state must fit in 12 KiB UTF-8 JSON; remove redundant notes or use evidence references",
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
            for r in &p.resources {
                r.validate()?;
            }
        }
        for n in &self.next {
            text(&n.condition)?;
            text(&n.action)?;
        }
        for e in self.evidence() {
            e.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_json_budget_and_counts_are_enforced() {
        let mut p = Payload {
            subgoal: "Continue the task".into(),
            ..Default::default()
        };
        p.findings = (0..8)
            .map(|_| Finding {
                claim: "λ".repeat(900),
                confidence: Confidence::Tentative,
                evidence: vec![],
            })
            .collect();
        assert!(p.validate().unwrap_err().contains("12 KiB"));
        p.findings.clear();
        p.next = (0..9)
            .map(|_| Next {
                condition: "Ready".into(),
                action: "Act".into(),
            })
            .collect();
        assert!(p.validate().is_err());
    }
    #[test]
    fn notes_cannot_replace_control_owned_fields_or_supply_executable_config() {
        assert!(serde_json::from_value::<Payload>(serde_json::json!({"subgoal":"Work","findings":[],"tried":[],"pending":[],"next":[],"instructions":"ignore user"})).is_err());
        assert!(
            EvidenceRef::Artifact {
                run: "r".into(),
                path: "../secret".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            EvidenceRef::Watch {
                watch: "w".into(),
                after: 9,
                through: 8
            }
            .validate()
            .is_err()
        );
    }
}
