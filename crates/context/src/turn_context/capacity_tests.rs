use super::*;
#[test]
fn whole_input_fits_preserving_required_sections_and_tracing_omissions() {
    let sections = [
        Section {
            kind: SectionKind::Instructions,
            text: "keep this constraint",
            required: true,
            reference: None,
        },
        Section {
            kind: SectionKind::History,
            text: &"old evidence ".repeat(400),
            required: false,
            reference: None,
        },
        Section {
            kind: SectionKind::History,
            text: "recent verified finding",
            required: false,
            reference: None,
        },
        Section {
            kind: SectionKind::Request,
            text: "continue",
            required: true,
            reference: None,
        },
    ];
    let capacity = Capacity {
        context_tokens: 4096,
        output_tokens: 1024,
        overhead_tokens: 2048,
    };
    let context = fit("policy", &sections, capacity).unwrap();
    assert!(context.text.len() <= 1024);
    assert!(context.text.contains("keep this constraint"));
    assert!(context.text.contains("recent verified finding"));
    assert!(!context.sections[1].included);
    assert_eq!(context.capacity, Some(capacity));
    assert!(fit(&"mandatory".repeat(400), &sections, capacity).is_err());
    assert!(
        Capacity {
            context_tokens: 100,
            output_tokens: u32::MAX,
            overhead_tokens: 10
        }
        .input_bytes()
        .is_none()
    );
}
