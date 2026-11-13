
use super::*;
fn fixture() -> (State, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Check knowledge", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("knowledge-attempt".into()),
        ..Default::default()
    });
    *run.context = Some(control_context::Resolved {
        documents: vec![control_context::DocumentInput {
            id: "selected".into(),
            title: "Selected.md".into(),
            revision: 2,
            body: "Old captured café 🐾".into(),
        }],
        ..Default::default()
    });
    let id = run.id.clone();
    state.documents.push(crate::knowledge::Document {
        id: "selected".into(),
        title: "Selected.md".into(),
        revision: 3,
        body: "New live body".into(),
        saved_at: None,
    });
    state.documents.push(crate::knowledge::Document {
        id: "unselected".into(),
        title: "Unselected.md".into(),
        revision: 1,
        body: "Private draft".into(),
        saved_at: None,
    });
    (state, id)
}
fn input() -> Value {
    json!({"claim":"A useful fact","evidence":"artifact:checked-result","scope":"fixture revision 2","validity":"recheck after source changes"})
}
#[test]
fn captured_documents_are_immutable_and_unselected_notes_are_not_readable() {
    let (state, id) = fixture();
    let run = state.runtime.runs.iter().find(|run| run.id == id).unwrap();
    assert!(
        read(
            &state,
            Some(run),
            "control_knowledge_read",
            &json!({"id":"document:unselected"})
        )
        .is_err()
    );
    let first = read(
        &state,
        Some(run),
        "control_knowledge_read",
        &json!({"id":"document:selected","offset":13,"limit":5}),
    )
    .unwrap();
    assert_eq!(first["body"], "café ");
    assert_eq!(first["next_offset"], 18);
    assert_eq!(first["revision"], 2);
    assert_eq!(first["admitted"], false);
    let last = read(
        &state,
        Some(run),
        "control_knowledge_read",
        &json!({"id":"document:selected","offset":18}),
    )
    .unwrap();
    assert_eq!(last["body"], "🐾");
    assert!(last["next_offset"].is_null());
    assert_eq!(
        read(
            &state,
            None,
            "control.knowledge.read",
            &json!({"id":"document:selected"})
        )
        .unwrap()["body"],
        "New live body"
    );
    assert_eq!(
        read(
            &state,
            Some(run),
            "control.knowledge.search",
            &json!({"query":"live"})
        )
        .unwrap()["results"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
#[test]
fn proposal_checkpoint_retries_provenance_and_operator_admission() {
    let (mut state, id) = fixture();
    let args = input();
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &args,
            "request-1",
            |_| Err(io::Error::other("full"))
        )
        .is_err()
    );
    assert!(state.collective.proposals.is_empty());
    let value = worker(
        &mut state,
        &id,
        "control_knowledge_propose",
        &args,
        "request-1",
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(value["status"], "pending");
    assert!(
        state
            .documents
            .iter()
            .all(|doc| doc.body != "A useful fact")
    );
    worker(
        &mut state,
        &id,
        "control_knowledge_propose",
        &args,
        "request-1",
        |_| panic!("Retry must not persist twice"),
    )
    .unwrap();
    assert_eq!(state.collective.proposals.len(), 1);
    let source = state.collective.proposals[0].source.as_ref().unwrap();
    assert_eq!(source.run.as_deref(), Some(id.as_str()));
    assert_eq!(source.attempt.as_deref(), Some("knowledge-attempt"));
    assert_eq!(source.documents, vec![("selected".into(), 2)]);
    let mut changed = args;
    changed["claim"] = json!("Another claim");
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &changed,
            "request-1",
            |_| Ok(())
        )
        .is_err()
    );
    assert!(
        read(
            &state,
            None,
            "control.knowledge.read",
            &json!({"id":"claim:0"})
        )
        .is_err()
    );
    let fields = [
        ("id".into(), "0".into()),
        ("decision".into(), "accepted".into()),
    ]
    .into_iter()
    .collect();
    assert!(
        review_action(&mut state, "/review", &fields, |_| Err(io::Error::other(
            "full"
        )))
        .is_err()
    );
    assert_eq!(state.collective.proposals[0].status, "pending");
    review_action(&mut state, "/review", &fields, |_| Ok(())).unwrap();
    let serialized = serde_json::to_vec(&state).unwrap();
    let restored: State = serde_json::from_slice(&serialized).unwrap();
    let accepted = read(
        &restored,
        None,
        "control.knowledge.read",
        &json!({"id":"claim:0"}),
    )
    .unwrap();
    assert_eq!(accepted["admitted"], true);
    assert_eq!(accepted["evidence"], "artifact:checked-result");
    assert_eq!(accepted["source"]["run"], id);
    assert!(
        crate::knowledge::render(&restored, "review", None).contains("artifact:checked-result")
    );
}
#[test]
fn validation_bounds_and_authority_fail_closed() {
    let (mut state, id) = fixture();
    for args in [
        json!({"query":""}),
        json!({"query":"x","limit":11}),
        json!({"query":"x".repeat(201)}),
        json!({"query":"x","extra":true}),
    ] {
        assert!(read(&state, None, "control.knowledge.search", &args).is_err());
    }
    for args in [
        json!({"id":"document:selected","limit":4001}),
        json!({"id":"document:selected","offset":32001}),
    ] {
        assert!(read(&state, None, "control.knowledge.read", &args).is_err());
    }
    let mut args = input();
    args["claim"] = json!("x".repeat(281));
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &args,
            "one",
            |_| Ok(())
        )
        .is_err()
    );
    args = input();
    args["chat"] = json!("another-chat");
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &args,
            "one",
            |_| Ok(())
        )
        .is_err()
    );
    state.runtime.runs[0].actor = Actor::Unknown;
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_search",
            &json!({"query":"captured"}),
            "one",
            |_| Ok(())
        )
        .is_err()
    );
    state.runtime.runs[0].actor = Actor::LocalOperator;
    state.runtime.runs[0].status = "completed".into();
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &input(),
            "one",
            |_| Ok(())
        )
        .is_err()
    );
    assert!(state.collective.proposals.is_empty());
}
#[test]
fn local_proposals_require_operator_chat_and_are_retry_safe() {
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    let mut args = input();
    assert!(local(&mut state, "control.knowledge.propose", &args, |_| Ok(())).is_err());
    args["chat"] = json!(chat);
    local(&mut state, "control.knowledge.propose", &args, |_| Ok(())).unwrap();
    local(&mut state, "control.knowledge.propose", &args, |_| {
        panic!("Duplicate persistence")
    })
    .unwrap();
    assert_eq!(state.collective.proposals.len(), 1);
    state.chats[0].actor = Actor::Unknown;
    assert!(local(&mut state, "control.knowledge.propose", &args, |_| Ok(())).is_err());
}
#[test]
fn operator_mcp_routes_reads_and_advertises_all_knowledge_schemas() {
    let (mut state, _) = fixture();
    let listed = crate::session_http::rpc(
        &mut state,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    );
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for name in [
        "control.knowledge.search",
        "control.knowledge.read",
        "control.knowledge.propose",
    ] {
        assert!(names.contains(&name));
    }
    let response = crate::session_http::rpc(
        &mut state,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.knowledge.read","arguments":{"id":"document:selected"}}}),
    );
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["data"]["body"],
        "New live body"
    );
}
#[test]
fn multiword_search_is_order_independent_and_requires_every_term() {
    let (mut state, _) = fixture();
    state.documents[0].title = "Signal repeater.md".into();
    state.documents[0].body = "The delay of a repeater is configurable. CAFÉ".into();
    for query in [
        "repeater delay",
        "delay repeater",
        "  REPEATER   delay  ",
        "café repeater",
    ] {
        let value = read(
            &state,
            None,
            "control.knowledge.search",
            &json!({"query":query}),
        )
        .unwrap();
        assert_eq!(value["results"].as_array().unwrap().len(), 1, "{query}");
        assert_eq!(value["results"][0]["id"], "document:selected");
    }
    let missing = read(
        &state,
        None,
        "control.knowledge.search",
        &json!({"query":"repeater missing"}),
    )
    .unwrap();
    assert!(missing["results"].as_array().unwrap().is_empty());
}
#[test]
fn search_result_count_and_proposal_storage_are_bounded() {
    let (mut state, id) = fixture();
    for index in 0..30 {
        state.documents.push(crate::knowledge::Document {
            id: format!("test-{index}"),
            title: "Match".into(),
            body: "match".repeat(1000),
            revision: 1,
            saved_at: None,
        });
    }
    let results = read(
        &state,
        None,
        "control.knowledge.search",
        &json!({"query":"match"}),
    )
    .unwrap();
    assert_eq!(results["results"].as_array().unwrap().len(), 10);
    assert!(results.to_string().len() < 4000);
    for index in 0..MAX_PROPOSALS {
        let mut args = input();
        args["claim"] = json!(format!("Fact {index}"));
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &args,
            &format!("request-{index}"),
            |_| Ok(()),
        )
        .unwrap();
    }
    assert!(
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &input(),
            "overflow",
            |_| Ok(())
        )
        .is_err()
    );
    assert_eq!(state.collective.proposals.len(), MAX_PROPOSALS);
}
