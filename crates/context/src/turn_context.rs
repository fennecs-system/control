//! Pure input assembly and trace. Selection belongs to the configured compactor.
use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;
// Reserve room for both the original assignment and current 20,000-scalar
// request (up to four UTF-8 bytes each), plus policy and recovery notes.
pub const MAX_REQUIRED_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Version;
impl TryFrom<u8> for Version {
    type Error = &'static str;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == VERSION {
            Ok(Self)
        } else {
            Err("Unsupported continuity version")
        }
    }
}
impl From<Version> for u8 {
    fn from(_: Version) -> Self {
        VERSION
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    Policy,
    Instructions,
    WorkingState,
    Events,
    History,
    Lenses,
    Request,
}

/// References identify retained evidence; content never implies authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub run: String,
    pub section: SectionKind,
}

pub struct Section<'a> {
    pub kind: SectionKind,
    pub text: &'a str,
    pub required: bool,
    pub reference: Option<Reference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionTrace {
    pub kind: SectionKind,
    pub bytes: usize,
    pub included: bool,
    pub reference: Option<Reference>,
}

/// Exact text, including adapter framing. Persist before launch; send this value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedContext {
    pub version: u8,
    pub text: String,
    pub sections: Vec<SectionTrace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<Capacity>,
}

/// Declared capacity, with explicit allowance for provider framing/tool schemas.
/// UTF-8 byte accounting is conservative estimation, not an exact tokenizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capacity {
    pub context_tokens: u32,
    pub output_tokens: u32,
    pub overhead_tokens: u32,
}
impl Capacity {
    pub fn input_bytes(self) -> Option<usize> {
        let reserved = self.output_tokens.checked_add(self.overhead_tokens)?;
        let available = self.context_tokens.checked_sub(reserved)?;
        (available > 0 && self.output_tokens > 0).then_some(available as usize)
    }
}

/// Required instructions remain intact. Optional evidence is selected newest
/// first; every omitted section stays explicit in the persisted trace.
pub fn fit(
    prefix: &str,
    sections: &[Section<'_>],
    capacity: Capacity,
) -> Result<PreparedContext, BudgetExceeded> {
    let limit = capacity.input_bytes().unwrap_or(0);
    let required = sections
        .iter()
        .filter(|s| s.required)
        .fold(prefix.len(), |n, s| n.saturating_add(s.text.len()));
    if required > limit || limit == 0 {
        return Err(BudgetExceeded {
            required_bytes: required,
            limit_bytes: limit,
        });
    }
    let mut selected = vec![false; sections.len()];
    let mut used = required;
    for (at, section) in sections.iter().enumerate().rev() {
        if section.required {
            selected[at] = true;
        } else if used.saturating_add(section.text.len()) <= limit {
            selected[at] = true;
            used += section.text.len();
        }
    }
    let mut text = String::with_capacity(used);
    text.push_str(prefix);
    let trace = sections
        .iter()
        .zip(selected)
        .map(|(section, included)| {
            if included {
                text.push_str(section.text);
            }
            SectionTrace {
                kind: section.kind,
                bytes: section.text.len(),
                included,
                reference: section.reference.clone(),
            }
        })
        .collect();
    Ok(PreparedContext {
        version: VERSION,
        text,
        sections: trace,
        capacity: Some(capacity),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub required_bytes: usize,
    pub limit_bytes: usize,
}
impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Required Control input uses {} bytes; limit is {}. Reduce required instructions or lenses; no instructions were truncated.",
            self.required_bytes, self.limit_bytes
        )
    }
}
impl std::error::Error for BudgetExceeded {}

/// Preserve the selected history exactly and in order. Required Control
/// framing has a separate guard; this is not a second history compactor.
/// In particular, passthrough leaves history compression to the provider.
pub fn compose(
    prefix: &str,
    sections: &[Section<'_>],
    limit: usize,
) -> Result<PreparedContext, BudgetExceeded> {
    let required = sections
        .iter()
        .filter(|s| s.required)
        .fold(prefix.len(), |n, s| n.saturating_add(s.text.len()));
    if required > limit {
        return Err(BudgetExceeded {
            required_bytes: required,
            limit_bytes: limit,
        });
    }
    let total = sections
        .iter()
        .fold(prefix.len(), |n, s| n.saturating_add(s.text.len()));
    let mut text = String::with_capacity(total);
    text.push_str(prefix);
    let mut trace = Vec::with_capacity(sections.len());
    for section in sections {
        text.push_str(section.text);
        trace.push(SectionTrace {
            kind: section.kind,
            bytes: section.text.len(),
            included: true,
            reference: section.reference.clone(),
        });
    }
    Ok(PreparedContext {
        version: VERSION,
        text,
        sections: trace,
        capacity: None,
    })
}

#[cfg(test)]
mod capacity_tests;
#[cfg(test)]
mod tests;
