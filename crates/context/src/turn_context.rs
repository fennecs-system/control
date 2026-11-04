//! Pure, byte-bounded Control input projection. No provider, storage or runtime effects.
use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

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

/// Required sections are reserved first. Optional sections are whole units, in
/// priority order. Retained evidence stays addressable through the trace.
pub fn prepare(
    prefix: &str,
    sections: &[Section<'_>],
    limit: usize,
) -> Result<PreparedContext, BudgetExceeded> {
    prepare_ordered(prefix, sections, 0..sections.len(), limit)
}

/// Select optional sections by priority, then render in source order. Recency
/// can decide what fits without reversing the chronology of user instructions.
pub fn prepare_ordered(
    prefix: &str,
    sections: &[Section<'_>],
    priority: impl IntoIterator<Item = usize>,
    limit: usize,
) -> Result<PreparedContext, BudgetExceeded> {
    let limit = limit.min(MAX_INPUT_BYTES);
    // A fixed notice reservation makes omissions explicit without a second
    // selection pass or any byte/UTF-8 truncation of instructions/evidence.
    const NOTICE: &str =
        "\nSome optional context was omitted; inspect the retained context trace for references.\n";
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
    let mut remaining = limit - required;
    if total > limit {
        remaining = remaining.checked_sub(NOTICE.len()).ok_or(BudgetExceeded {
            required_bytes: required.saturating_add(NOTICE.len()),
            limit_bytes: limit,
        })?;
    }
    let mut text = String::with_capacity(total.min(limit));
    text.push_str(prefix);
    let mut trace = Vec::with_capacity(sections.len());
    let mut omitted = false;
    // Priority chooses what survives pressure; it is not an inclusion filter.
    // If everything fits, avoid omissions (and an unreserved omission notice)
    // even when the caller supplies a partial priority order.
    let mut selected = vec![total <= limit; sections.len()];
    for index in priority {
        let Some(section) = sections.get(index) else {
            continue;
        };
        if !section.required && !selected[index] && section.text.len() <= remaining {
            selected[index] = true;
            remaining -= section.text.len();
        }
    }
    for (index, section) in sections.iter().enumerate() {
        let included = section.required || selected[index];
        if included {
            text.push_str(section.text);
        } else {
            omitted = true;
        }
        trace.push(SectionTrace {
            kind: section.kind,
            bytes: section.text.len(),
            included,
            reference: section.reference.clone(),
        });
    }
    if omitted {
        text.push_str(NOTICE);
    }
    Ok(PreparedContext {
        version: VERSION,
        text,
        sections: trace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_priority_cannot_overflow_the_budget_with_an_omission_notice() {
        let sections = ["earlier", "later"].map(|text| Section {
            kind: SectionKind::History,
            text,
            required: false,
            reference: None,
        });
        let prepared = prepare_ordered("", &sections, [1], 12).unwrap();
        assert_eq!(prepared.text, "earlierlater");
        assert!(prepared.sections.iter().all(|section| section.included));
    }
    #[test]
    fn selection_priority_never_reverses_instruction_chronology() {
        let sections = ["earlier", "later"].map(|text| Section {
            kind: SectionKind::History,
            text,
            required: false,
            reference: None,
        });
        let prepared = prepare_ordered("", &sections, [1, 0], MAX_INPUT_BYTES).unwrap();
        assert_eq!(prepared.text, "earlierlater");
    }
    #[test]
    fn required_instructions_survive_unicode_history_and_framing_counts() {
        let old = "λ".repeat(100);
        let sections = [
            Section {
                kind: SectionKind::History,
                text: &old,
                required: false,
                reference: Some(Reference {
                    run: "old".into(),
                    section: SectionKind::History,
                }),
            },
            Section {
                kind: SectionKind::Instructions,
                text: "Observe only.\n",
                required: true,
                reference: None,
            },
            Section {
                kind: SectionKind::Request,
                text: "Current event\n",
                required: true,
                reference: None,
            },
        ];
        let value = prepare("Policy\n", &sections, 130).unwrap();
        assert!(value.text.len() <= 130);
        assert!(value.text.contains("Observe only.") && value.text.contains("Current event"));
        assert!(
            !value.sections[0].included
                && value.sections[0].reference.as_ref().unwrap().run == "old"
        );
        assert!(value.text.contains("omitted"));
    }
    #[test]
    fn required_input_fails_instead_of_silent_truncation() {
        let sections = [Section {
            kind: SectionKind::Instructions,
            text: "λ constraints",
            required: true,
            reference: None,
        }];
        let error = prepare("adapter framing ", &sections, 16).unwrap_err();
        assert!(error.required_bytes > error.limit_bytes);
    }
    #[test]
    fn full_projection_and_trace_roundtrip_exactly() {
        let sections = [Section {
            kind: SectionKind::Request,
            text: "build",
            required: true,
            reference: None,
        }];
        let prepared = prepare("frame\n", &sections, MAX_INPUT_BYTES).unwrap();
        let bytes = serde_json::to_vec(&prepared).unwrap();
        assert_eq!(prepared, serde_json::from_slice(&bytes).unwrap());
        assert_eq!(prepared.text, "frame\nbuild");
    }
}
