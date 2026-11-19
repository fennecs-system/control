use super::*;
use crate::State;
use std::sync::{Arc, Mutex};

fn facts(role: Audience) -> Facts {
    Facts { role, improve: false, managed: true, judge: role == Audience::Advisor, advisor: false, shell: false, local: false, rooms: true, standing: crate::personas::Effective::builtin("worker", false) }
}
fn names(tools: &[Value]) -> Vec<&str> {
    tools.iter().map(name).collect()
}
fn bytes(tools: &[Value]) -> usize {
    serde_json::to_string(tools).unwrap().len()
}

#[test]
fn role_core_sets_follow_catalog_metadata_and_stay_below_ten_kilobytes() {
    for role in Audience::ALL {
        for (shell, local) in [(false, false), (true, false), (true, true)] {
            let f = Facts { shell, local, ..facts(role) };
            let listed = listed(&f);
            let expected = offered(&f).into_iter().filter(|t| control_context::loaded(name(t), role)).collect::<Vec<_>>();
            let mut want = names(&expected);
            want.push(SEARCH);
            if !local {
                want.push(CALL);
            }
            assert_eq!(names(&listed), want, "{role:?}");
            assert!(bytes(&listed) < 9_600, "{role:?} shell={shell}: {}", bytes(&listed));
            // Every listed schema is callable through the same host path.
            assert!(listed.iter().all(|t| t["inputSchema"]["type"] == "object"));
        }
    }
    let member = names(&listed(&facts(Audience::Member))).into_iter().map(str::to_owned).collect::<Vec<_>>();
    let lead = names(&listed(&facts(Audience::Lead))).into_iter().map(str::to_owned).collect::<Vec<_>>();
    for core in ["control_turn_context", "control_turn_handoff", "control_service_start", "control_service_status", "control_service_exec",
        "control_service_exec_status", "control_watch_start", "control_watch_events", "control_room_read", "control_room_post",
        "control_room_claim", "control_agent_message", "control_agent_spawn", "control_bot_request", "control_guidance"] {
        assert!(member.iter().any(|n| n == core) && lead.iter().any(|n| n == core), "{core}");
    }
    assert_eq!(lead.iter().filter(|n| !member.contains(n)).collect::<Vec<_>>(), ["control_design_ask", "control_room_plan"]);
    let advisor = Facts { advisor: true, ..facts(Audience::Advisor) };
    assert_eq!(names(&listed(&advisor)), ["control_workspace_start", "control_workspace_status", "control_agent_message", "control_guidance",
        "control_agent_result", "control_room_read", SEARCH, CALL]);
    // Codex enabled_tools and Claude --allowedTools come from the catalogue.
    assert!([SEARCH, CALL].iter().all(|n| control_context::worker_mcp_names().contains(n)));
    // A judge's list is exactly what judge policy lets it call.
    assert!(offered(&advisor).iter().all(|t| control_context::judge_may_call(name(t), true)));
    assert!(!names(&offered(&facts(Audience::Advisor))).contains(&"control_agent_message"));
}

#[test]
fn core_schemas_are_compacted_and_search_returns_the_full_ones() {
    let f = facts(Audience::Member);
    let listed = listed(&f);
    let handoff = listed.iter().find(|t| t["name"] == "control_turn_handoff").unwrap();
    assert_eq!(handoff["description"], control_context::mcp("control_turn_handoff").unwrap().brief);
    assert_eq!(handoff["inputSchema"]["required"], json!(["disposition"]));
    assert_eq!(handoff["inputSchema"]["properties"]["payload"]["description"], "see control_tool_search");
    assert!(listed.iter().all(|t| t.get("annotations").is_none()));
    // Neither the closed-object flag nor a type-sized bound is listed; the host enforces both.
    let text = serde_json::to_string(&listed).unwrap();
    assert!(listed.iter().all(|t| t["inputSchema"].get("additionalProperties").is_none()));
    assert!(!text.contains("4294967295"), "{text}");
    // A listed tool asked for by name returns only what the listing left out.
    let found = search(&f, &json!({"names":["turn_handoff"]})).unwrap();
    assert!(found["tools"].as_array().unwrap().is_empty());
    let full = offered(&f).into_iter().find(|t| t["name"] == "control_turn_handoff").unwrap();
    let rest = &found["already_listed"][0];
    assert_eq!(rest["name"], "control_turn_handoff");
    assert_eq!(rest["about"], control_context::mcp("control_turn_handoff").unwrap().phrase);
    assert!(rest.get("description").is_none(), "the listing has its brief");
    assert_eq!(rest["properties"]["payload"], full["inputSchema"]["properties"]["payload"]);
    assert!(rest["properties"].get("disposition").is_none(), "already in the listing");
    assert!(rest["properties"]["payload"]["properties"]["subgoal"].is_object());
}

#[test]
fn search_returns_full_schemas_only_for_tools_not_already_listed() {
    let f = facts(Audience::Member);
    let core = ["control_service_status", "control_service_exec", "control_room_read"];
    let schemas = offered(&f).into_iter().filter(|t| core.contains(&name(t))).collect::<Vec<_>>();
    let before = json!({"tools":schemas}).to_string().len();
    let by_name = search(&f, &json!({"names":core})).unwrap();
    assert!(by_name["tools"].as_array().unwrap().is_empty());
    assert_eq!(by_name["already_listed"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), core);
    let after = by_name.to_string().len();
    eprintln!("tool search for listed tools: before {before} after {after} bytes");
    assert!(after * 2 < before, "before {before} after {after}");
    // A query names listed hits by phrase and returns schemas for the rest.
    let query = search(&f, &json!({"query":"service status watch stop"})).unwrap();
    for hit in query["already_listed"].as_array().unwrap() {
        assert_eq!(hit.as_object().unwrap().keys().collect::<Vec<_>>(), ["about", "name"], "{hit}");
        assert!(control_context::loaded(hit["name"].as_str().unwrap(), f.role));
    }
    for tool in query["tools"].as_array().unwrap() {
        assert!(!control_context::loaded(name(tool), f.role) && tool["inputSchema"].is_object(), "{tool}");
    }
    assert!(names(query["tools"].as_array().unwrap()).contains(&"control_watch_stop"));
}

#[test]
fn the_index_names_every_deferred_tool_once_and_no_core_tool() {
    for role in Audience::ALL {
        for local in [false, true] {
            let f = Facts { local, shell: local, ..facts(role) };
            let text = index(&f);
            for tool in deferred(&f) {
                let short = name(&tool).trim_start_matches("control_");
                assert_eq!(text.matches(&format!(" {short} (")).count(), 1, "{role:?} {short}\n{text}");
            }
            for tool in listed(&f) {
                assert!(!text.contains(&format!(" {} (", name(&tool).trim_start_matches("control_"))), "{role:?} {}", name(&tool));
            }
            assert_eq!(text.contains("control_call"), !local);
            assert!(text.lines().skip(1).all(|line| line.starts_with("- ")));
        }
    }
    // Family titles were about 300 bytes of a 1.9 KB index; names and phrases remain.
    let member = index(&facts(Audience::Member));
    assert!(member.len() <= 1_700 && !member.contains("- Watches:") && member.contains("- watch_status (inspect watches); "), "{member}");
    let local = index(&Facts { local: true, shell: true, ..facts(Audience::Member) });
    assert!(local.starts_with("More tools (names omit control_; call by full name; the schema loads on first use):\n"), "{local}");
}

#[test]
fn search_finds_by_name_or_query_within_bounds_and_names_a_fix() {
    let f = facts(Audience::Member);
    let by_name = search(&f, &json!({"names":["watch_stop","control_port_status","control_nope"]})).unwrap();
    assert_eq!(names(by_name["tools"].as_array().unwrap()), ["control_watch_stop", "control_port_status"]);
    assert_eq!(by_name["unknown"], json!(["control_nope"]));
    assert!(by_name["fix"].as_str().unwrap().contains("control_tool_search {}"));
    assert!(by_name["next"].as_str().unwrap().contains("control_call"));
    let query = search(&f, &json!({"query":"stop a watch"})).unwrap();
    assert_eq!(query["tools"][0]["name"], "control_watch_stop");
    assert!(query["tools"].as_array().unwrap().len() <= QUERY_RESULTS);
    assert!(search(&f, &json!({"query":"zzzz qqqq"})).unwrap()["fix"].is_string());
    let listing = search(&f, &json!({})).unwrap();
    assert_eq!(listing["deferred"].as_array().unwrap().len(), deferred(&f).len());
    assert!(search(&f, &json!({"names":vec!["control_watch_stop"; NAMED_RESULTS + 1]})).is_err());
    assert!(search(&f, &json!({"other":1})).is_err());
    // A judge's search sees only what judge policy permits.
    let judge = search(&facts(Audience::Advisor), &json!({"names":["control_service_start"]})).unwrap();
    assert!(judge["tools"].as_array().unwrap().is_empty() && judge["unknown"] == json!(["control_service_start"]));
    let local = search(&Facts { local: true, ..f }, &json!({"names":["control_watch_stop"]})).unwrap();
    assert!(local["next"].as_str().unwrap().contains("callable directly"));
}

#[test]
fn control_call_unwraps_to_the_direct_request_or_names_a_fix() {
    let envelope = json!({"nonce":"7","request":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":CALL,"arguments":{"tool":"watch_stop","arguments":{"watch_id":"w"}}}}});
    assert_eq!(unwrap(&envelope).unwrap().unwrap(),
        json!({"nonce":"7","request":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"control_watch_stop","arguments":{"watch_id":"w"}}}}));
    let direct = json!({"request":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"control_watch_stop"}}});
    assert!(unwrap(&direct).unwrap().is_none());
    let with = |arguments: Value| unwrap(&json!({"request":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":CALL,"arguments":arguments}}}));
    assert_eq!(with(json!({"tool":"control_room_read"})).unwrap().unwrap()["request"]["params"]["arguments"], json!({}));
    for (arguments, fix) in [
        (json!({"tool":"control_nope"}), "control_tool_search {\"query\""),
        (json!({"tool":"control.chat.send"}), "control_tool_search {\"query\""),
        (json!({"tool":CALL,"arguments":{}}), "control_call {\"tool\""),
        (json!({"tool":"control_room_read","arguments":[1]}), "control_tool_search {\"names\""),
        (json!({"name":"control_room_read"}), "control_tool_search {\"names\""),
    ] {
        assert!(with(arguments.clone()).unwrap_err().contains(fix), "{arguments}");
    }
}

/// A managed member turn on a fresh copy of the same state.
fn member_state() -> (String, Value) {
    let mut state = State::default();
    let lead = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    crate::chat::send(&mut state, &lead, "Lead the team", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    let chat = crate::chat::create(&mut state);
    let session = state.chats.iter_mut().find(|c| c.id == chat).unwrap();
    (session.parent_chat, session.room) = (Some(lead), Some("general".into()));
    crate::chat::send(&mut state, &chat, "Own member 2", "test").unwrap();
    let index = state.runtime.runs.len() - 1;
    state.runtime.runs[index].persona = Some(crate::delegation::Persona { role: crate::delegation::Role::Worker, root: "root".into(), depth: 1, parent: None, bundle: None });
    crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[index].id.clone();
    assert_eq!(state.runtime.runs[index].status, "running");
    (id, serde_json::to_value(&state).unwrap())
}
fn fresh(snapshot: &Value) -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(serde_json::from_value(snapshot.clone()).unwrap()))
}
fn rpc(shared: &Arc<Mutex<State>>, run: &str, name: &str, arguments: Value) -> Value {
    super::super::rpc_at(shared, run, &json!({"nonce":"11-0","request":{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":name,"arguments":arguments}}}), None)
}
fn through_call(shared: &Arc<Mutex<State>>, run: &str, name: &str, arguments: Value) -> Value {
    rpc(shared, run, CALL, json!({"tool":name,"arguments":arguments}))
}

#[test]
fn every_callable_tool_answers_identically_through_control_call() {
    let (run, snapshot) = member_state();
    let shared = fresh(&snapshot);
    let facts = {
        let state = shared.lock().unwrap();
        let run = state.runtime.runs.iter().find(|r| r.id == run).unwrap();
        Facts::of(&state, run, true)
    };
    assert_eq!(facts.role, Audience::Member);
    let tools = offered(&facts);
    assert!(tools.len() > 40);
    // Arguments every schema rejects, so no tool reaches an engine or network;
    // plus one valid read and one valid write, compared on separate copies.
    let mut cases = tools.iter().map(|t| (name(t).to_owned(), json!({"not_a_parameter":true}))).collect::<Vec<_>>();
    cases.push(("control_room_read".into(), json!({})));
    cases.push(("control_room_post".into(), json!({"message":"Milestone 2 claimed","delivery":"record"})));
    cases.push(("control_guidance".into(), json!({"topic":"rooms"})));
    for (tool, arguments) in cases {
        // Stopping a preview ignores its arguments and reaches the engine.
        if tool == "control_preview_stop" { continue; }
        let (direct_state, call_state) = (fresh(&snapshot), fresh(&snapshot));
        let direct = rpc(&direct_state, &run, &tool, arguments.clone());
        let called = through_call(&call_state, &run, &tool, arguments.clone());
        assert_eq!(direct, called, "{tool} {arguments}");
        assert_eq!(serde_json::to_value(&*direct_state.lock().unwrap()).unwrap(), serde_json::to_value(&*call_state.lock().unwrap()).unwrap(), "{tool}");
    }
    // The valid post took effect once through either spelling.
    let posted = fresh(&snapshot);
    through_call(&posted, &run, "control_room_post", json!({"message":"Milestone 2 claimed","delivery":"record"}));
    assert_eq!(posted.lock().unwrap().collective.posts.len(), 1);
}

#[test]
fn judges_handoff_fences_and_unknown_tools_apply_to_control_call() {
    let (run, snapshot) = member_state();
    // Judge policy refuses the same tools whichever spelling forges them.
    let judge = {
        let mut value = snapshot.clone();
        let runs = value["runtime"]["runs"].as_array_mut().unwrap();
        let index = runs.iter().position(|r| r["id"] == run).unwrap();
        runs[index]["persona"]["role"] = json!("judge");
        value
    };
    for tool in ["control_service_start", "control_agent_spawn", "control_room_claim", "control_design_ask", "control_agent_message"] {
        let direct = rpc(&fresh(&judge), &run, tool, json!({}));
        assert!(crate::toolbox::failure_text(&direct).unwrap().contains("Judge policy"), "{tool}");
        assert_eq!(direct, through_call(&fresh(&judge), &run, tool, json!({})), "{tool}");
    }
    let read = through_call(&fresh(&judge), &run, "control_room_read", json!({}));
    assert_eq!(read["result"]["isError"], false, "{read}");
    // A staged handoff fences mutators but not reads or the search.
    let staged = fresh(&snapshot);
    let handoff = rpc(&staged, &run, "control_turn_handoff", json!({"disposition":{"kind":"continue"}}));
    assert_eq!(handoff["result"]["isError"], false, "{handoff}");
    let staged = serde_json::to_value(&*staged.lock().unwrap()).unwrap();
    for tool in ["control_service_start", "control_room_post", "control_watch_stop"] {
        let direct = rpc(&fresh(&staged), &run, tool, json!({}));
        assert!(crate::toolbox::failure_text(&direct).unwrap().contains("handoff is staged"), "{tool}");
        assert_eq!(direct, through_call(&fresh(&staged), &run, tool, json!({})));
    }
    let found = rpc(&fresh(&staged), &run, SEARCH, json!({"names":["control_watch_stop"]}));
    assert_eq!(found["result"]["isError"], false);
    assert_eq!(through_call(&fresh(&staged), &run, "control_room_read", json!({}))["result"]["isError"], false);
    // Unknown or operator-only names are refused with a callable next step.
    for tool in ["control_nope", "control.chat.send"] {
        let refused = through_call(&fresh(&snapshot), &run, tool, json!({}));
        assert!(crate::toolbox::failure_text(&refused).unwrap().contains("control_tool_search {"), "{refused}");
    }
    let direct = rpc(&fresh(&snapshot), &run, "control_nope", json!({}));
    assert!(crate::toolbox::failure_text(&direct).unwrap().contains("control_tool_search {}"), "{direct}");
}

/// A scripted run -128-style member turn: the tools members used, in order.
/// Core tools are called directly; every deferred one costs a lookup.
#[test]
fn a_member_turn_needs_few_lookups() {
    let turn = ["control_turn_context", "control_room_read", "control_room_claim", "control_service_status", "control_service_start",
        "control_service_copy", "control_service_exec", "control_service_exec_status", "control_watch_start", "control_watch_events",
        "control_watch_status", "control_agent_message", "control_room_post", "control_guidance", "control_score_probe", "control_turn_handoff"];
    let f = facts(Audience::Member);
    let deferred = turn.iter().filter(|t| !control_context::loaded(t, Audience::Member)).collect::<Vec<_>>();
    // One search by names covers every deferred tool of the turn.
    let found = search(&f, &json!({"names":deferred})).unwrap();
    assert_eq!(found["tools"].as_array().unwrap().len(), deferred.len());
    println!("{}", json!({"tools_used":turn.len(),"deferred":deferred,"lookups_one_per_tool":deferred.len(),"lookups_batched":1}));
    assert_eq!(deferred, [&"control_watch_status", &"control_score_probe"]);
}
