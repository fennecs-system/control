
use super::*;
#[test]
fn defaults_are_additive_bounded_and_preserve_local_definitions() {
    let mut library = Library::default();
    library
        .definitions
        .retain(|d| d.id != "autonomy" && d.id != "investigation" && d.id != "coordination");
    let captured = library.resolve(&["engineering".into()], &[]).unwrap();
    library.definitions.push(Definition {
        id: "autonomy".into(),
        name: "Local policy".into(),
        revision: 7,
        instructions: "Operator's existing guidance".into(),
        ..Default::default()
    });
    assert!(library.merge_defaults());
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
    assert!(library.get("coordination").is_some());
    assert!(!library.merge_defaults());
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
        builtin: Default::default(),
    };
    assert!(!full.merge_defaults());
    full.validate(&[]).unwrap();
}
#[test]
fn optional_procedures_compose_without_adding_tools_or_authority() {
    let library = Library::default();
    let ordinary = library.resolve(&["engineering".into()], &[]).unwrap();
    let selected = library
        .resolve(
            &[
                "engineering".into(),
                "autonomy".into(),
                "investigation".into(),
                "investigation".into(),
                "coordination".into(),
                "coordination".into(),
            ],
            &[],
        )
        .unwrap();
    assert_eq!(selected.tools, ordinary.tools);
    assert!(selected.documents.is_empty());
    assert!(!ordinary.lenses.iter().any(|lens| lens.id == "coordination"));
    assert_eq!(
        selected.lenses.iter().filter(|lens| lens.id == "coordination").count(),
        1
    );
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
#[test]
fn newer_built_in_revisions_reach_nodes_unless_the_lens_was_edited_locally() {
    let shipped = Library::default().get("coordination").unwrap().clone();
    let older = Definition { revision: shipped.revision - 1, instructions: "Earlier wording".into(), ..shipped.clone() };
    let node = |definitions: Vec<Definition>, installed: Option<u64>| {
        let mut library = Library::default();
        library.definitions.retain(|d| d.id != "coordination");
        library.definitions.extend(definitions);
        match installed {
            Some(revision) => { library.builtin.insert("coordination".into(), revision); }
            None => { library.builtin.remove("coordination"); }
        }
        library
    };

    let mut upgraded = node(vec![older.clone()], Some(older.revision));
    assert!(upgraded.merge_defaults());
    assert_eq!(upgraded.get("coordination").unwrap(), &shipped);
    assert!(upgraded.definitions.contains(&older), "earlier revisions stay for captured runs");
    assert!(!upgraded.merge_defaults());

    let edited = Definition { revision: shipped.revision + 4, instructions: "Operator wording".into(), ..shipped.clone() };
    let mut local = node(vec![older.clone(), edited.clone()], Some(older.revision));
    assert!(!local.merge_defaults());
    assert_eq!(local.get("coordination").unwrap(), &edited);

    // Nodes stored before this record adopt an exact built-in copy.
    let mut legacy = node(vec![shipped.clone()], None);
    assert!(legacy.merge_defaults());
    assert_eq!(legacy.builtin.get("coordination"), Some(&shipped.revision));
    assert_eq!(legacy.definitions.iter().filter(|d| d.id == "coordination").count(), 1);
    let mut unknown = node(vec![older.clone()], None);
    assert!(!unknown.merge_defaults(), "an unrecognised copy may be a local edit");
    assert_eq!(unknown.get("coordination").unwrap(), &older);
}
