
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
    let planning = library.resolve(&["planning".into()], &[]).unwrap();
    assert!(planning.tools.is_empty() && planning.documents.is_empty());
    assert_eq!(planning.lenses.len(), 1);
    let swarm = library.resolve(&["swarm".into(), "planning".into()], &[]).unwrap();
    assert_eq!(swarm.lenses.iter().filter(|lens| lens.id == "planning").count(), 1);
    assert_eq!(swarm.guidance.iter().filter(|g| planning.guidance.contains(g)).count(), 1);
    let swarm_only = library.resolve(&["swarm".into()], &[]).unwrap();
    assert!(swarm_only.lenses.iter().any(|lens| lens.id == "planning"));
    // Ganglia is opt-in so runs with and without it can be compared.
    assert!(!swarm_only.lenses.iter().any(|lens| lens.id == "ganglia"));
    let ganglia = library.resolve(&["swarm".into(), "ganglia".into()], &[]).unwrap();
    assert_eq!(ganglia.tools, swarm_only.tools);
    assert!(ganglia.documents.is_empty() && ganglia.packages.is_empty());
    assert_eq!(ganglia.lenses.len(), swarm_only.lenses.len() + 1);
    assert!(swarm_only.guidance.contains(&planning.guidance[0]));
    assert_eq!(selected.lenses.iter().filter(|lens| lens.id == "planning").count(), 1);
    assert!(!ordinary.lenses.iter().any(|lens| lens.id == "coordination"));
    assert_eq!(
        selected.lenses.iter().filter(|lens| lens.id == "coordination").count(),
        1
    );
    assert!(
        !ordinary
            .guidance.iter().map(|g| &g.text)
            .any(|s| s.contains("three distinct probes"))
    );
    assert_eq!(
        selected
            .guidance.iter().map(|g| &g.text)
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
            .guidance.iter().map(|g| &g.text)
            .any(|s| s.contains("enforced limits"))
    );
    assert!(
        selected
            .guidance.iter().map(|g| &g.text)
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
    // ... and a copy of an earlier shipped revision upgrades.
    let previous: Vec<Definition> = serde_json::from_str(include_str!("../builtin-lens-history.json")).unwrap();
    let mut shipped_earlier = node(vec![previous[0].clone()], None);
    assert!(shipped_earlier.merge_defaults());
    assert_eq!(shipped_earlier.get("coordination").unwrap(), &shipped);
    let mut unknown = node(vec![older.clone()], None);
    assert!(!unknown.merge_defaults(), "an unrecognised copy may be a local edit");
    assert_eq!(unknown.get("coordination").unwrap(), &older);
}
#[test]
fn every_shipped_lens_revision_upgrades_to_the_current_built_in() {
    let investigation = Library::default().get("investigation").unwrap().clone();
    assert_eq!(investigation.revision, 3);
    assert_eq!(investigation.includes, ["evidence"]);
    assert!(investigation.tools.is_empty());
    let autonomy = Library::default().get("autonomy").unwrap().clone();
    assert_eq!(autonomy.revision, 9);
    assert!(autonomy.instructions.contains("routine steps need no evidence files, read-backs or proof records"));
    assert!(autonomy.instructions.contains("Never claim an outcome you have not observed"));
    assert!(autonomy.instructions.contains("when tooling work stops advancing the goal, stop and use what exists"));
    assert!(autonomy.instructions.contains("the model decides what to do, when and by whom"));
    assert!(autonomy.instructions.contains("a setback means recover, not quit"));
    assert!(autonomy.instructions.contains("two consecutive routine cycles"));
    assert!(autonomy.instructions.contains("Weigh risk by reversibility"));
    assert!(autonomy.instructions.contains("Guards should bound damage"));
    let coordination = Library::default().get("coordination").unwrap().clone();
    assert_eq!(coordination.revision, 14);
    assert!(coordination.instructions.contains("the outcome, not evidence paths or measurements"));
    assert!(coordination.instructions.chars().count() <= 4000);
    assert!(coordination.instructions.contains("keeping its turns short to stay responsive: it delegates implementation, long work and ongoing infrastructure to members, spawned or assigned"));
    assert_eq!(coordination.includes, ["planning"]);
    assert!(coordination.instructions.starts_with("Whoever steers a team posts the milestone backlog"));
    assert!(coordination.instructions.contains("Wait on a claim only for an output you cannot reasonably produce yourself"));
    assert!(coordination.instructions.contains("briefs each worker with the shared goal and its first milestone"));
    assert!(coordination.instructions.contains("room post or note awaiting direction is not a question"));
    assert!(coordination.instructions.contains("routine and repeated outcomes stay in your own notes"));
    assert!(coordination.instructions.contains("ask once per blocker, not per member"));
    assert!(coordination.instructions.contains("refreshing the backlog is not a response"));
    assert!(coordination.instructions.contains("Coordinate only over consumable or exclusive resources"));
    assert!(coordination.instructions.contains("never invent locks or turn-taking"));
    let planning = Library::default().get("planning").unwrap().clone();
    assert_eq!(planning.revision, 5);
    assert!(planning.instructions.contains("observable outcome cheap to check"));
    assert!(planning.instructions.contains("Check it once when claiming the milestone done"));
    assert!(planning.instructions.contains("treat reaching its done-condition as the tool's test"));
    let advisor = Library::default().get("advisor").unwrap().clone();
    assert_eq!(advisor.revision, 7);
    assert!(advisor.instructions.contains("members marked claims blocked"));
    assert!(advisor.instructions.contains("several members report a similar blocker"));
    assert!(advisor.instructions.contains("(busy but stalled)"));
    assert!(advisor.instructions.contains("putting more members on the bottleneck, splitting it, or re-routing dependents around it"));
    let swarm = Library::default().get("swarm").unwrap().clone();
    assert_eq!(swarm.revision, 2);
    assert!(swarm.includes.contains(&"planning".to_string()));
    let ganglia = Library::default().get("ganglia").unwrap().clone();
    assert_eq!((ganglia.name.as_str(), ganglia.revision), ("Ganglia", 2));
    assert!(ganglia.instructions.starts_with("Reflexes and routines run locally; turns are strategy."));
    assert!(ganglia.instructions.contains("long-running work on systems that change faster than a turn, not one-off tasks"));
    // Reuse an existing package first; build or extend only for a demonstrated gap.
    assert!(ganglia.instructions.contains("Reuse first: when an admitted or available tool package already provides reflexes or skills, use it"));
    assert!(ganglia.instructions.contains("Build or extend a layer only for a gap a run has demonstrated"));
    assert!(!ganglia.instructions.contains("always-on reflex layer first") && !ganglia.instructions.contains("builds it early"));
    // Reflexes commit (hysteresis) rather than thrash: the run -128 lesson.
    assert!(ganglia.instructions.contains("hold until the trigger stays clear a while"));
    assert!(ganglia.instructions.contains("weigh every active condition, preempt only for urgent ones"));
    assert!(ganglia.instructions.contains("pause routines rather than cancel them"));
    assert!(ganglia.instructions.contains("escalate only milestones, failures and conditions they cannot handle"));
    assert!(ganglia.instructions.contains("A layer that blocks the task is bypassed or removed, not patched again and again"));
    assert!(ganglia.instructions.contains("save as a tool package"));
    assert!(ganglia.instructions.chars().count() <= 1200);
    // Domain-independent: examples belong in docs, not in the lens.
    let lowered = format!("{} {}", ganglia.instructions, ganglia.summary).to_lowercase();
    for word in ["game", "player", "robot", "minecraft", "mob", "threat", "health", "hostile", "manoeuvre", "scrap", "price", "combat", "enemy"] {
        assert!(!lowered.contains(word), "Ganglia lens mentions {word}");
    }
    // On-demand lenses carry one index line; the advisor procedure is scoped to lead and advisor.
    for lens in [&investigation, &autonomy, &coordination, &planning, &advisor, &ganglia] {
        assert!(!lens.summary.is_empty() && lens.summary.chars().count() <= MAX_SUMMARY, "{}", lens.id);
    }
    assert_eq!((advisor.roles.as_slice(), advisor.inline.as_slice()), ([Audience::Lead, Audience::Advisor].as_slice(), [Audience::Advisor].as_slice()));
    assert!(advisor.instructions.contains("For a turn stopped at its time limit, suggest splitting the work into smaller turns or, for the lead, delegating it to a member."));
    let previous: Vec<Definition> = serde_json::from_str(include_str!("../builtin-lens-history.json")).unwrap();
    assert!(previous.iter().any(|d| d.id == "investigation" && d.revision == 2));
    assert!(previous.iter().any(|d| d.id == "ganglia" && d.revision == 1));
    for revision in 1..=8 {
        assert!(previous.iter().any(|d| d.id == "autonomy" && d.revision == revision));
    }
    for revision in 1..=13 {
        assert!(previous.iter().any(|d| d.id == "coordination" && d.revision == revision));
    }
    for revision in 1..=4 {
        assert!(previous.iter().any(|d| d.id == "planning" && d.revision == revision));
    }
    assert!(previous.iter().any(|d| d.id == "swarm" && d.revision == 1));
    for revision in 1..=4 {
        assert!(previous.iter().any(|d| d.id == "advisor" && d.revision == revision));
    }
    for earlier in previous {
        let shipped = Library::default().get(&earlier.id).unwrap().clone();
        assert!(earlier.revision < shipped.revision, "{} history must predate the shipped revision", earlier.id);
        let mut node = Library::default();
        node.definitions.retain(|d| d.id != earlier.id);
        node.definitions.push(earlier.clone());
        node.builtin.remove(&earlier.id);
        assert!(node.merge_defaults());
        assert_eq!(node.get(&earlier.id).unwrap(), &shipped);
        assert!(node.definitions.contains(&earlier), "earlier revisions stay for captured runs");
    }
}
#[test]
fn packages_resolve_pinned_or_latest_and_validate_without_admission() {
    let package = |version: u64| PackageInput {
        name: "mc263-client".into(),
        version,
        commit: format!("{version:040x}"),
        description: "Client".into(),
    };
    let mut library = Library::default();
    for (id, reference) in [("latest", "mc263-client"), ("pinned", "mc263-client@1"), ("other", "mc263-client@2")] {
        library.definitions.push(Definition {
            id: id.into(),
            name: id.into(),
            revision: 1,
            packages: vec![reference.into()],
            ..Default::default()
        });
    }
    let admitted = [package(1), package(2)];
    let latest = library.resolve_with(&["latest".into()], &[], &admitted).unwrap();
    assert_eq!(latest.packages, vec![package(2)]);
    let pinned = library.resolve_with(&["pinned".into()], &[], &admitted).unwrap();
    assert_eq!(pinned.packages, vec![package(1)]);
    // Two lenses naming the same version share one entry; different versions conflict.
    assert_eq!(library.resolve_with(&["latest".into(), "other".into()], &[], &admitted).unwrap().packages.len(), 1);
    assert!(library.resolve_with(&["pinned".into(), "other".into()], &[], &admitted).unwrap_err().contains("different versions"));
    // Revoked or unknown versions are not inputs: pinned fails closed, latest falls back.
    assert!(library.resolve_with(&["pinned".into()], &[], &[package(2)]).unwrap_err().contains("not admitted"));
    assert_eq!(library.resolve_with(&["latest".into()], &[], &[package(1)]).unwrap().packages, vec![package(1)]);
    assert!(library.resolve(&["latest".into()], &[]).is_err());
    // Library validation checks syntax only, so revocation never blocks other edits.
    library.validate(&[]).unwrap();
    library.definitions.push(Definition { id: "bad".into(), name: "Bad".into(), revision: 1, packages: vec!["Bad Name@0".into()], ..Default::default() });
    assert!(library.validate(&[]).is_err());
    // Lenses without packages keep their serialized form.
    assert!(!serde_json::to_string(library.get("engineering").unwrap()).unwrap().contains("packages"));
    assert!(!serde_json::to_string(&pinned).unwrap().is_empty());
    assert!(!serde_json::to_string(&library.resolve(&["engineering".into()], &[]).unwrap()).unwrap().contains("packages"));
}
#[test]
fn summaries_and_roles_make_lenses_on_demand_and_role_scoped() {
    let mut library = Library::default();
    let resolved = library.resolve(&["swarm".into(), "advisor".into()], &[]).unwrap();
    // Short lenses without a summary stay inline for every role.
    assert_eq!(resolved.instructions, ["Support factual claims with evidence. Distinguish findings from assumptions."]);
    let ids = |role| resolved.on_demand(role).map(|g| g.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids(Audience::Member), ["planning", "coordination", "autonomy", "investigation"]);
    assert_eq!(ids(Audience::Lead), ["advisor", "planning", "coordination", "autonomy", "investigation"]);
    assert_eq!(ids(Audience::Advisor), ["planning", "coordination", "autonomy", "investigation"]);
    assert_eq!(resolved.inline(Audience::Advisor).count(), 2);
    assert_eq!(resolved.inline(Audience::Member).count(), 1);
    assert!(resolved.guidance_for("advisor", Audience::Member).is_none());
    assert_eq!(resolved.guidance_for("Team advisor", Audience::Lead).unwrap().revision, 7);
    // Role-scoped lenses without a summary are inline only for their roles.
    library.definitions.push(Definition { id: "lead-only".into(), name: "Lead only".into(), revision: 1, instructions: "Lead text".into(), roles: vec![Audience::Lead], ..Default::default() });
    let scoped = library.resolve(&["lead-only".into()], &[]).unwrap();
    assert!(scoped.instructions.is_empty());
    assert_eq!(scoped.inline(Audience::Lead).collect::<Vec<_>>(), ["Lead text"]);
    assert_eq!(scoped.inline(Audience::Member).count() + scoped.on_demand(Audience::Member).count(), 0);
    library.validate(&[]).unwrap();
    for summary in ["two\nlines".to_owned(), "x".repeat(MAX_SUMMARY + 1)] {
        let mut invalid = library.clone();
        invalid.definitions.push(Definition { id: "bad".into(), name: "Bad".into(), revision: 1, summary, ..Default::default() });
        assert!(invalid.validate(&[]).is_err());
    }
    // Contexts captured before this field still deserialize, with everything inline.
    let mut old = serde_json::to_value(&resolved).unwrap();
    old.as_object_mut().unwrap().remove("guidance");
    assert!(serde_json::from_value::<Resolved>(old).unwrap().guidance.is_empty());
}
