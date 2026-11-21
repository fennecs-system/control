use super::*;
use control_context::{Definition, Library};

fn swarm() -> Resolved {
    Library::default().resolve(&["swarm".into(), "advisor".into(), "ganglia".into()], &[]).unwrap()
}

#[test]
fn lists_topics_for_the_role_and_reads_the_captured_text() {
    let context = swarm();
    let member = read(Some(&context), Audience::Member, None).unwrap();
    let topics = member["topics"].as_array().unwrap().iter().map(|t| t["topic"].as_str().unwrap()).collect::<Vec<_>>();
    for topic in ["coordination", "planning", "autonomy", "investigation", "ganglia", "session", "rooms"] {
        assert!(topics.contains(&topic), "{topic} in {topics:?}");
    }
    assert!(!topics.contains(&"advisor"), "members do not see lead/advisor procedure");
    let coordination = read(Some(&context), Audience::Member, Some("Coordination")).unwrap();
    assert_eq!(coordination["revision"], 15);
    assert!(coordination["text"].as_str().unwrap().starts_with("Whoever steers a team posts the milestone backlog"));
    // Lookup by display name, any case.
    assert_eq!(read(Some(&context), Audience::Member, Some("task autonomy")).unwrap()["topic"], "autonomy");
    assert!(read(Some(&context), Audience::Member, Some("advisor")).is_err());
    let lead = read(Some(&context), Audience::Lead, Some("advisor")).unwrap();
    assert!(lead["text"].as_str().unwrap().contains("an advisor is a background helper"));
    assert!(read(Some(&context), Audience::Lead, Some("unknown")).is_err());
    let session = read(None, Audience::Member, Some("session")).unwrap();
    assert_eq!(session["text"], agent_policy::SESSION_GUIDANCE_DETAIL);
    assert_eq!(read(None, Audience::Member, None).unwrap()["topics"].as_array().unwrap().len(), BUILT_IN.len());
}

#[test]
fn a_run_reads_the_revision_it_captured_not_the_current_library() {
    let mut library = Library::default();
    let captured = library.resolve(&["swarm".into()], &[]).unwrap();
    let mut edited: Definition = library.get("coordination").unwrap().clone();
    edited.revision += 1;
    edited.instructions = "Local coordination edit".into();
    library.definitions.push(edited);
    let current = library.resolve(&["swarm".into()], &[]).unwrap();
    assert_eq!(read(Some(&current), Audience::Member, Some("coordination")).unwrap()["text"], "Local coordination edit");
    let old = read(Some(&captured), Audience::Member, Some("coordination")).unwrap();
    assert_eq!(old["revision"], 15);
    assert_ne!(old["text"], "Local coordination edit");
    // A context captured before guidance on demand inlines everything and lists only built-ins.
    let mut legacy = captured.clone();
    legacy.instructions.extend(legacy.guidance.drain(..).map(|g| g.text));
    let legacy: Resolved = serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
    assert!(legacy.guidance.is_empty());
    assert!(read(Some(&legacy), Audience::Member, Some("coordination")).is_err());
}

#[test]
fn the_tool_is_read_only_and_survives_a_staged_handoff() {
    let definition = &Tools.definitions()[0];
    assert_eq!(definition["name"], "control_guidance");
    assert_eq!(definition["annotations"]["readOnlyHint"], true);
    assert!(control_context::tool_policy::after_handoff("control_guidance"));
    assert!(crate::tool_registry::attempt_tools("control_guidance").is_some());
}
