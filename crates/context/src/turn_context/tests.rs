
use super::*;
#[test]
fn assembly_preserves_the_selected_chronology() {
    let sections = ["earlier", "later"].map(|text| Section {
        kind: SectionKind::History,
        text,
        required: false,
        reference: None,
    });
    let prepared = compose("", &sections, 12).unwrap();
    assert_eq!(prepared.text, "earlierlater");
    assert!(prepared.sections.iter().all(|section| section.included));
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
    let value = compose("Policy\n", &sections, 130).unwrap();
    assert!(
        value.text.len() > 130,
        "selected history has its own compaction policy"
    );
    assert!(value.text.contains("Observe only.") && value.text.contains("Current event"));
    assert!(
        value.sections[0].included && value.sections[0].reference.as_ref().unwrap().run == "old"
    );
    assert!(value.text.contains(&old));
}
#[test]
fn required_input_fails_instead_of_silent_truncation() {
    let sections = [Section {
        kind: SectionKind::Instructions,
        text: "λ constraints",
        required: true,
        reference: None,
    }];
    let error = compose("adapter framing ", &sections, 16).unwrap_err();
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
    let prepared = compose("frame\n", &sections, MAX_REQUIRED_BYTES).unwrap();
    let bytes = serde_json::to_vec(&prepared).unwrap();
    assert_eq!(prepared, serde_json::from_slice(&bytes).unwrap());
    assert_eq!(prepared.text, "frame\nbuild");
}
