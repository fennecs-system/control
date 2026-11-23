use super::*;
use crate::{
    backend::Backend,
    delegation::{self, Role},
    identity::{Action, Actor},
    runtime::RunKind,
    usage::Attempt,
};

/// A root DM with one running turn that may spawn.
fn active() -> (State, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    state
        .runtime
        .enqueue_for(
            "Build a shop".into(),
            RunKind::Chat,
            "web",
            Actor::LocalOperator,
            Action::Task,
        )
        .unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.chat_id = Some(chat.clone());
    run.status = "running".into();
    run.backend = Backend::Codex;
    run.model = Some("captured-model".into());
    run.attempts.push(Attempt {
        workspace_id: Some("parent-attempt".into()),
        parent_artifact: Some("baseline".into()),
        ..Default::default()
    });
    let id = run.id.clone();
    (state, chat, id)
}
fn spawn(state: &mut State, id: &str, nonce: &str, input: Value) -> Result<Value, &'static str> {
    crate::spawning::request(
        state,
        id,
        "parent-attempt",
        nonce,
        serde_json::from_value(input).unwrap(),
        |_| Ok(()),
    )
}
fn bundle(run: &crate::runtime::Run) -> &Captured {
    run.persona
        .as_ref()
        .and_then(|p| p.bundle.as_deref())
        .expect("captured persona")
}
fn edit(state: &mut State, id: &str, change: impl FnOnce(&mut Definition)) -> Definition {
    let mut draft = state.personas.get(id).unwrap().clone();
    change(&mut draft);
    save_with(state, draft, |_| Ok(())).unwrap()
}

#[test]
fn spawn_by_persona_captures_dna_standing_and_body_and_edits_never_change_it() {
    let (mut state, _, id) = active();
    state.runtime.models.push(crate::providers::Model::captured(
        Backend::Claude,
        "worker-model".into(),
    ));
    edit(&mut state, "worker", |d| d.model = "claude:worker-model".into());
    spawn(
        &mut state,
        &id,
        "w",
        json!({"objectives":["Stock the shelves"],"persona":"worker"}),
    )
    .unwrap();
    let child = state.runtime.runs.last().unwrap().clone();
    assert_eq!(
        child.lenses.as_deref(),
        Some(&["swarm".to_owned()][..]),
        "DNA from the persona, not the parent"
    );
    assert!(
        child
            .context
            .as_ref()
            .as_ref()
            .unwrap()
            .lenses
            .iter()
            .any(|lens| lens.id == "coordination"),
        "genes resolved and captured"
    );
    assert_eq!(
        (child.backend, child.model.as_deref()),
        (Backend::Claude, Some("worker-model")),
        "the persona's body"
    );
    let captured = bundle(&child);
    assert_eq!((captured.id.as_str(), captured.revision), ("worker", 2));
    assert!(!delegation::is_judge(&child));
    assert_eq!(
        of(&state.bots, &child),
        Effective {
            id: "worker".into(),
            standing: captured.standing.clone(),
            approved: false,
            captured: true
        }
    );
    // The prompt's first instruction is the persona.
    let prompt = crate::chat::prompt(&state, &child.id).unwrap();
    assert!(
        prompt.starts_with("Persona worker@2 (Worker): Complete the work you claimed."),
        "{}",
        &prompt[..200]
    );
    // Editing the persona appends a revision; the running child keeps its capture.
    edit(&mut state, "worker", |d| d.purpose = "Changed purpose.".into());
    assert_eq!(state.personas.get("worker").unwrap().revision, 3);
    assert_eq!(
        bundle(state.runtime.runs.iter().find(|r| r.id == child.id).unwrap()).revision,
        2
    );
    assert_eq!(
        state.personas.revision("worker", 2).unwrap().purpose,
        captured.purpose,
        "earlier revisions stay readable"
    );
    // A continuation of the child keeps the same capture (lineage carries it).
    let follow = crate::chat::FollowUp::from(&state, &child);
    assert_eq!(follow.persona.bundle.as_deref().map(|b| b.revision), Some(2));
}

#[test]
fn explicit_genes_and_model_override_the_persona_and_roles_must_agree() {
    let (mut state, _, id) = active();
    state
        .runtime
        .models
        .push(crate::providers::Model::captured(Backend::Claude, "reviewer".into()));
    spawn(
        &mut state,
        &id,
        "o",
        json!({"objectives":["Research"],"persona":"worker","lenses":["research"],"model":"claude:reviewer"}),
    )
    .unwrap();
    let child = state.runtime.runs.last().unwrap();
    assert_eq!(child.lenses.as_deref(), Some(&["research".to_owned()][..]));
    assert_eq!(child.model.as_deref(), Some("reviewer"));
    assert_eq!(
        bundle(child).lenses,
        ["swarm"],
        "the persona's own DNA is still recorded"
    );
    let runs = state.runtime.runs.len();
    assert!(
        spawn(
            &mut state,
            &id,
            "r",
            json!({"objectives":["x"],"persona":"worker","role":"judge"})
        )
        .is_err(),
        "standing decides the role"
    );
    assert!(spawn(&mut state, &id, "u", json!({"objectives":["x"],"persona":"nobody"})).is_err());
    assert!(
        spawn(
            &mut state,
            &id,
            "g",
            json!({"objectives":["x"],"lenses":["no-such-gene"]})
        )
        .is_err()
    );
    assert_eq!(state.runtime.runs.len(), runs, "refusals create nothing");
    // Without a persona, children inherit the parent's captured context exactly as before.
    spawn(&mut state, &id, "plain", json!({"objectives":["x"]})).unwrap();
    let plain = state.runtime.runs.last().unwrap();
    assert!(plain.persona.as_ref().unwrap().bundle.is_none());
    assert_eq!(plain.model.as_deref(), Some("captured-model"));
}

#[test]
fn observer_personas_spawn_as_judges_behind_their_own_opt_in_gene() {
    let (mut state, chat, id) = active();
    assert_eq!(
        spawn(
            &mut state,
            &id,
            "a0",
            json!({"objectives":["Patrol"],"persona":"antibody"})
        )
        .unwrap_err(),
        crate::bots::ADVISOR_OFF
    );
    state.chats.iter_mut().find(|c| c.id == chat).unwrap().lenses = Some(vec!["advisor".into()]);
    assert_eq!(
        spawn(
            &mut state,
            &id,
            "a1",
            json!({"objectives":["Patrol"],"persona":"antibody"})
        )
        .unwrap_err(),
        crate::bots::ANTIBODY_OFF
    );
    spawn(
        &mut state,
        &id,
        "adv",
        json!({"objectives":["Advise"],"persona":"advisor"}),
    )
    .unwrap();
    let advisor = state.runtime.runs.last().unwrap();
    assert!(delegation::is_judge(advisor) && advisor.resource_grants.is_empty());
    assert_eq!(advisor.lenses.as_deref(), Some(&["advisor".to_owned()][..]));
    let standing = of(&state.bots, advisor);
    assert!(
        standing.read_only() && !standing.approved && !standing.may_call("control_agent_message"),
        "notes need operator approval"
    );
    assert!(standing.may_call("control_room_read") && !standing.may_call("control_service_start"));
}

#[test]
fn behaviours_apply_on_spawn_through_existing_primitives() {
    let (mut state, chat, id) = active();
    state.personas.definitions.push(Definition {
        id: "shopkeeper".into(),
        name: "Shopkeeper".into(),
        revision: 1,
        purpose: "Keep the shop stocked.".into(),
        lenses: vec!["ganglia".into()],
        standing: persona::builtin("worker").unwrap().standing,
        behaviours: vec![
            Behaviour::Schedule { every_minutes: 30 },
            Behaviour::Observe {
                command: "cat stock.json".into(),
            },
            Behaviour::Watch {
                name: "restock".into(),
                command: "./restock.sh".into(),
                salience: "{\"min_change\":5}".into(),
            },
        ],
        ..Default::default()
    });
    state.personas.validate().unwrap();
    let refused = crate::spawning::request(
        &mut state,
        &id,
        "parent-attempt",
        "fail",
        serde_json::from_value(json!({"objectives":["Run the shop"],"persona":"shopkeeper"})).unwrap(),
        |_| Err(io::Error::other("disk")),
    );
    assert!(refused.is_err());
    assert_eq!(
        crate::bots::pending_json(&state, &chat),
        json!([]),
        "a refused batch files no bot request"
    );
    spawn(
        &mut state,
        &id,
        "shop",
        json!({"objectives":["Run the shop"],"persona":"shopkeeper"}),
    )
    .unwrap();
    let child = state.runtime.runs.last().unwrap().clone();
    let child_chat = child.chat_id.clone().unwrap();
    assert_eq!(bundle(&child).behaviours.len(), 3, "captured and visible");
    // The schedule is an ordinary pending bot request for the operator.
    let pending = crate::bots::pending_json(&state, &chat);
    let request = pending
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["chat_id"] == child_chat.as_str())
        .expect("a pending bot request");
    assert_eq!(request["every_minutes"], 30);
    // Service-bound behaviours are standing orders naming the existing tools.
    let prompt = crate::chat::prompt(&state, &child.id).unwrap();
    assert!(
        prompt.contains("control_observe_set {command:\"cat stock.json\"}"),
        "{prompt}"
    );
    assert!(
        prompt
            .contains("control_watch_start {name:\"restock\", command:\"./restock.sh\", salience:{\"min_change\":5}}"),
        "{prompt}"
    );
    // A later bot request for it defaults to the persona's schedule.
    assert_eq!(schedule(&state.runtime.runs, &child_chat), Some(30));
}

#[test]
fn built_in_standing_matches_pre_persona_behaviour() {
    let (mut state, _, id) = active();
    let root = state.runtime.runs[0].clone();
    assert_eq!(of(&state.bots, &root).id, "lead");
    spawn(&mut state, &id, "w", json!({"objectives":["x"]})).unwrap();
    let member = state.runtime.runs.last().unwrap().clone();
    assert_eq!(of(&state.bots, &member).id, "worker");
    let mut judge = member.clone();
    judge.persona.as_mut().unwrap().role = Role::Judge;
    let unapproved = of(&state.bots, &judge);
    assert_eq!(unapproved.id, "judge");
    // Every catalogued tool: the standing allows exactly what the judge policy did.
    for tool in control_context::TOOLS.iter().flat_map(|t| t.mcp_names) {
        assert_eq!(
            unapproved.may_call(tool.name),
            control_context::judge_may_call(tool.name, false),
            "{}",
            tool.name
        );
        let approved = Effective::builtin("advisor", true);
        assert_eq!(
            approved.may_call(tool.name),
            control_context::judge_may_call(tool.name, true),
            "{}",
            tool.name
        );
        let antibody = Effective::builtin("antibody", true);
        assert_eq!(
            antibody.may_call(tool.name),
            control_context::judge_may_call(tool.name, true),
            "{}",
            tool.name
        );
        assert!(of(&state.bots, &member).may_call(tool.name) && of(&state.bots, &root).may_call(tool.name));
    }
    assert!(of(&state.bots, &root).notifies() && !of(&state.bots, &member).notifies() && !unapproved.notifies());
    assert!(Effective::builtin("antibody", true).antibody() && !Effective::builtin("antibody", false).antibody());
    assert!(Effective::builtin("advisor", true).advisor() && !Effective::builtin("advisor", true).notifies());
}

#[test]
fn a_deny_list_hides_and_refuses_tools() {
    let mut standing = Effective::builtin("worker", false);
    standing.standing.acts = Acts::Deny(vec!["control_service_start".into(), "control.agent.spawn".into()]);
    assert!(!standing.may_call("control_service_start"));
    assert!(!standing.may_call("control_agent_spawn"), "either spelling");
    assert!(standing.may_call("control_room_read"));
}

#[test]
fn a_root_dm_selects_a_persona_and_each_message_captures_it() {
    let (mut state, chat, _) = active();
    state.runtime.runs.clear();
    select(&mut state, &chat, Some("lead")).unwrap();
    let session = state.chats.iter().find(|c| c.id == chat).unwrap();
    assert_eq!(
        (session.persona.as_deref(), session.lenses.as_deref()),
        (Some("lead"), Some(&["swarm".to_owned()][..]))
    );
    crate::chat::send(&mut state, &chat, "Plan the shop", "test").unwrap();
    let run = state.runtime.runs.last().unwrap();
    let lineage = run.persona.as_ref().unwrap();
    assert_eq!(
        (lineage.depth, lineage.parent.as_deref(), lineage.root.as_str()),
        (0, None, run.id.as_str()),
        "still the root"
    );
    assert_eq!(bundle(run).id, "lead");
    assert_eq!(of(&state.bots, run).id, "lead");
    assert!(
        select(&mut state, &chat, Some("antibody")).is_err(),
        "observers are spawned, not DMs"
    );
    let tool = tool(&mut state, "control.chat.persona", &json!({"id":chat,"persona":null})).unwrap();
    assert_eq!(tool["persona"], Value::Null);
    crate::chat::send(&mut state, &chat, "Carry on", "test").unwrap();
    assert!(
        state.runtime.runs.last().unwrap().persona.is_none(),
        "no persona: unchanged behaviour"
    );
}

#[test]
fn soul_documents_become_the_first_instruction() {
    let (mut state, _, _) = active();
    state.documents.push(crate::knowledge::Document {
        id: "shop-soul".into(),
        title: "SOUL.md".into(),
        body: "I keep the shop stocked and honest.".into(),
        revision: 4,
        saved_at: None,
    });
    let document = state.documents.last().cloned().unwrap();
    let saved = edit(&mut state, "worker", |d| d.soul = document.id.clone());
    let captured = capture(&state.personas, &state.documents, "worker").unwrap();
    assert_eq!(captured.soul, Some((document.id.clone(), document.revision)));
    assert_eq!(saved.revision, 2);
    assert!(captured.prompt().starts_with(&format!(
        "Persona worker@2 (Worker): {}",
        document.body.trim().chars().take(20).collect::<String>()
    )));
}

#[test]
fn the_personas_page_lists_built_ins_and_saves_revisions() {
    let (mut state, _, _) = active();
    let html = render(&state, "lead", None, None);
    for id in ["lead", "worker", "advisor", "antibody", "judge"] {
        assert!(html.contains(&format!("href='/personas?id={id}'")), "{id}");
    }
    assert!(html.contains("First instruction") && html.contains("href='/dna?id=swarm'"));
    assert!(crate::navigation::primary("persona-page").contains("href='/personas' aria-current='page'"));
    let mut fields = HashMap::new();
    for (key, value) in [
        ("id", "scout"),
        ("name", "Scout"),
        ("revision", "0"),
        ("purpose", "Find things."),
        ("sees", "team"),
        ("steers", "none"),
        ("answers_to", "lead"),
        ("acts", "deny"),
        ("deny", "control_service_start"),
        ("gene_research", "on"),
        (
            "behaviours",
            "[{\"kind\":\"score\",\"name\":\"found\",\"command\":\"./count\"}]",
        ),
    ] {
        fields.insert(key.to_owned(), value.to_owned());
    }
    let saved = save_with(&mut state, draft(&fields).unwrap(), |_| Ok(())).unwrap();
    assert_eq!(
        (saved.revision, saved.standing.acts.clone()),
        (1, Acts::Deny(vec!["control_service_start".into()]))
    );
    assert!(
        save_with(&mut state, draft(&fields).unwrap(), |_| Ok(())).is_err(),
        "a stale revision is a conflict"
    );
    fields.insert("revision".into(), "1".into());
    fields.insert("sees".into(), "node".into());
    assert!(
        save_with(&mut state, draft(&fields).unwrap(), |_| Ok(())).is_err(),
        "acting personas never see the node"
    );
    assert!(
        tool(&mut state, "control.personas.list", &json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "scout")
    );
}

/// Every header of the persona page, so a change to how it responds cannot
/// move a byte; the body is the rendered page, passed through unchanged.
#[test]
fn page_response_head_is_pinned() {
    let mut state = State::default();
    let mut out = Vec::new();
    assert!(handle(&mut out, &mut state, "GET", "/personas", &HashMap::new(), b""));
    let text = String::from_utf8(out).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(body.starts_with("<!doctype html>"), "{body}");
    assert_eq!(
        head,
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
            body.len()
        )
    );
}
