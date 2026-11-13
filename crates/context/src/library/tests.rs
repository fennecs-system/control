
use super::*;
#[test]
fn defaults_are_additive_bounded_and_preserve_local_definitions() {
    let mut library = Library::default();
    library
        .definitions
        .retain(|d| d.id != "autonomy" && d.id != "investigation");
    let captured = library.resolve(&["engineering".into()], &[]).unwrap();
    library.definitions.push(Definition {
        id: "autonomy".into(),
        name: "Local policy".into(),
        revision: 7,
        instructions: "Operator's existing guidance".into(),
        ..Default::default()
    });
    assert!(library.add_missing_defaults());
    assert_eq!(library.get("autonomy").unwrap().revision, 7);
    assert_eq!(
        library.get("autonomy").unwrap().instructions,
        "Operator's existing guidance"
    );
    assert_eq!(
        library.resolve(&["engineering".into()], &[]).unwrap(),
        captured
    );
    assert!(library.get("investigation").is_some());
    assert!(!library.add_missing_defaults());
    library.validate(&[]).unwrap();
    let mut full = Library {
        definitions: (0..128)
            .map(|n| Definition {
                id: format!("local-{n}"),
                name: format!("Local {n}"),
                revision: 1,
                ..Default::default()
            })
            .collect(),
    };
    assert!(!full.add_missing_defaults());
    full.validate(&[]).unwrap();
}
#[test]
fn investigation_is_opt_in_and_composition_adds_guidance_without_authority() {
    let library = Library::default();
    let ordinary = library.resolve(&["engineering".into()], &[]).unwrap();
    let selected = library
        .resolve(
            &[
                "engineering".into(),
                "autonomy".into(),
                "investigation".into(),
                "investigation".into(),
            ],
            &[],
        )
        .unwrap();
    assert_eq!(selected.tools, ordinary.tools);
    assert!(selected.documents.is_empty());
    assert!(
        !ordinary
            .instructions
            .iter()
            .any(|s| s.contains("three distinct probes"))
    );
    assert_eq!(
        selected
            .instructions
            .iter()
            .filter(|s| s.contains("three distinct probes"))
            .count(),
        1
    );
    assert_eq!(
        selected
            .lenses
            .iter()
            .filter(|lens| lens.id == "evidence")
            .count(),
        1
    );
    assert!(
        selected
            .instructions
            .iter()
            .any(|s| s.contains("enforced limits"))
    );
    assert!(
        selected
            .instructions
            .iter()
            .any(|s| s.contains("not a host-enforced experiment counter"))
    );
}
#[test]
fn presets_resolve_nested_lenses_once_and_are_pure() {
    let library = Library::default();
    library.validate(&[]).unwrap();
    let a = library
        .resolve(&["engineering".into(), "research".into()], &[])
        .unwrap();
    let b = library
        .resolve(
            &["research".into(), "engineering".into(), "research".into()],
            &[],
        )
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.tools.len(), 5);
    assert_eq!(a.lenses.len(), 8);
}
#[test]
fn cycles_missing_references_and_unknown_tools_fail_closed() {
    let mut library = Library::default();
    let mut d = library.get("engineering").unwrap().clone();
    d.revision = 2;
    d.includes.push("engineering".into());
    library.definitions.push(d);
    assert!(library.validate(&[]).is_err());
    assert!(
        Library::default()
            .resolve(&["missing".into()], &[])
            .is_err()
    );
    let mut library = Library::default();
    library.definitions[0]
        .tools
        .push("arbitrary.execute".into());
    assert!(library.validate(&[]).is_err());
}
#[test]
fn document_inputs_are_explicit_and_snapshots_survive_definition_changes() {
    let mut library = Library::default();
    let mut d = library.get("architecture-notes").unwrap().clone();
    d.revision = 2;
    d.documents.push("note-1".into());
    library.definitions.push(d);
    assert!(library.resolve(&["engineering".into()], &[]).is_err());
    let inputs = vec![DocumentInput {
        id: "note-1".into(),
        title: "Architecture.md".into(),
        revision: 3,
        body: "A fixed fact".into(),
    }];
    let snapshot = library.resolve(&["engineering".into()], &inputs).unwrap();
    assert_eq!(snapshot.documents[0].revision, 3);
    let mut d = library.get("architecture-notes").unwrap().clone();
    d.revision = 3;
    d.documents.clear();
    library.definitions.push(d);
    assert!(
        library
            .resolve(&["engineering".into()], &inputs)
            .unwrap()
            .documents
            .is_empty()
    );
    assert_eq!(snapshot.documents[0].body, "A fixed fact");
}
