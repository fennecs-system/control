use super::*;

#[test]
fn builtins_are_valid_and_name_existing_genes() {
    let library = Library::default();
    library.validate().unwrap();
    let ids = library.current().iter().map(|d| d.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["acceptance", "advisor", "antibody", "judge", "lead", "worker"]);
    let (acceptance, advisor) = (builtin("acceptance").unwrap(), builtin("advisor").unwrap());
    assert_eq!((acceptance.standing, acceptance.lenses), (advisor.standing, vec!["acceptance".to_owned()]));
    assert!(acceptance.purpose.chars().count() <= 600);
    let genes = crate::Library::default();
    for persona in library.current() {
        for gene in &persona.lenses {
            assert!(genes.get(gene).is_some(), "{} names unknown gene {gene}", persona.id);
        }
    }
    let antibody = builtin("antibody").unwrap();
    assert_eq!(antibody.standing, Standing { sees: Sees::Node, steers: Steers::Leads, answers_to: AnswersTo::Operator, acts: Acts::ReadOnly });
}

#[test]
fn edits_are_new_revisions_and_builtins_never_replace_them() {
    let mut library = Library::default();
    let mut edited = library.get("worker").unwrap().clone();
    edited.revision += 1;
    edited.model = "local:qwen".into();
    library.definitions.push(edited);
    library.validate().unwrap();
    assert_eq!(library.get("worker").unwrap().model, "local:qwen");
    assert_eq!(library.revision("worker", 1).unwrap().model, "", "earlier revisions stay for captured runs");
    assert_eq!(library.history("worker").len(), 2);
    assert!(!library.merge_defaults(), "a local edit is never replaced");
    assert_eq!(library.get("worker").unwrap().model, "local:qwen");
    // A node missing a built-in gets it without touching the others.
    library.definitions.retain(|d| d.id != "judge");
    library.builtin.remove("judge");
    assert!(library.merge_defaults());
    assert!(library.get("judge").is_some());
}

#[test]
fn standing_keeps_observers_read_only_and_actors_in_their_team() {
    let mut definition = builtin("worker").unwrap();
    definition.standing.sees = Sees::Node;
    assert!(!definition.valid(), "only read-only observers see the node");
    let mut definition = builtin("worker").unwrap();
    definition.standing.steers = Steers::Lead;
    assert!(!definition.valid(), "lead notes are an observer's standing");
    let mut definition = builtin("worker").unwrap();
    definition.standing.acts = Acts::Deny(vec!["control_service_start".into()]);
    assert!(definition.valid());
    let mut definition = builtin("advisor").unwrap();
    definition.behaviours.push(Behaviour::Schedule { every_minutes: 5 });
    assert!(!definition.valid(), "an advisor takes no timer");
    let mut definition = builtin("antibody").unwrap();
    definition.behaviours.push(Behaviour::Schedule { every_minutes: 30 });
    assert!(definition.valid(), "an antibody may patrol");
    definition.behaviours.push(Behaviour::Observe { command: "status".into() });
    assert!(!definition.valid(), "read-only personas start no routines");
}

#[test]
fn capture_copies_the_soul_and_renders_standing_and_behaviours_first() {
    let mut definition = builtin("worker").unwrap();
    definition.soul = "soul-doc".into();
    definition.behaviours = vec![
        Behaviour::Schedule { every_minutes: 10 },
        Behaviour::Watch { name: "price".into(), command: "./watch.sh".into(), salience: "{\"min_change\":0.02}".into() },
    ];
    assert!(Captured::of(&definition, &[]).is_err(), "a missing SOUL.md fails capture");
    let documents = [crate::DocumentInput { id: "soul-doc".into(), title: "SOUL.md".into(), revision: 3, body: "I keep the shop stocked.".into() }];
    let captured = Captured::of(&definition, &documents).unwrap();
    assert_eq!(captured.purpose, "I keep the shop stocked.");
    assert_eq!(captured.soul, Some(("soul-doc".into(), 3)));
    let prompt = captured.prompt();
    assert!(prompt.starts_with("Persona worker@1 (Worker): I keep the shop stocked.\n"), "{prompt}");
    assert!(prompt.contains("answers to its lead"));
    assert!(prompt.contains("control_watch_start {name:\"price\""), "{prompt}");
    assert!(!prompt.contains("every 10 min"), "host-applied schedules are not prompt orders");
    let long = [crate::DocumentInput { body: "x".repeat(MAX_SOUL + 1), ..documents[0].clone() }];
    assert!(Captured::of(&definition, &long).is_err(), "SOUL.md stays short");
}

#[test]
fn behaviours_round_trip_as_tagged_json() {
    let json = r#"[{"kind":"schedule","every_minutes":15},{"kind":"room"},{"kind":"observe","command":"cat status"},{"kind":"score","name":"items","command":"./score"}]"#;
    let behaviours: Vec<Behaviour> = serde_json::from_str(json).unwrap();
    assert_eq!(serde_json::to_string(&behaviours).unwrap(), json);
    assert!(serde_json::from_str::<Behaviour>(r#"{"kind":"cron","at":"*"}"#).is_err());
    let acts: Acts = serde_json::from_str(r#"{"deny":["control_agent_spawn"]}"#).unwrap();
    assert_eq!(acts, Acts::Deny(vec!["control_agent_spawn".into()]));
}
