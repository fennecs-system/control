use super::*;
use crate::runtime::RunKind;

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and isolated CONTROL_STATE_DIR; real pinned MCP bridge, no models or credentials"]
fn live_mailbox_notifications_and_tool_calls_roundtrip() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::Stdio,
        time::{Duration, Instant},
    };
    let root = crate::config::state_directory().unwrap();
    assert!(root.to_string_lossy().starts_with("/private/tmp/control-mailbox-qa."));
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Isolated mailbox transport probe", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        image: Some(image.clone()),
        ..Default::default()
    });
    let run = state.runtime.runs[0].id.clone();
    let workspace = crate::runtime::workspace("attempt-a").unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let shared = Arc::new(Mutex::new(state));
    let name = format!("control-mailbox-qa-{}", std::process::id());
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = crate::engine::query(&["rm", "-f", &self.0]);
        }
    }
    let _cleanup = Cleanup(name.clone());
    let mut command = crate::engine::scrubbed();
    command.args([
        "run",
        "--rm",
        "--interactive",
        "--pull=never",
        "--name",
        &name,
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--memory=128m",
        "--cpus=1",
        "--pids-limit=32",
        "--user",
        &format!("{}:{}", unsafe { libc::getuid() }, unsafe { libc::getgid() }),
        "--mount",
        &format!("type=bind,src={},dst=/workspace", workspace.display()),
    ]);
    // Exercise a patched bridge without replacing any deployed worker image.
    if let Ok(bridge) = std::env::var("CONTROL_QA_BRIDGE") {
        command.args([
            "--mount",
            &format!("type=bind,src={bridge},dst=/opt/control-room-mcp,readonly"),
            "--entrypoint=/opt/control-room-mcp",
        ]);
    } else {
        command.arg("--entrypoint=/bin/control-room-mcp");
    }
    command.arg(&image);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut requests = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"qa","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    ];
    for i in 3..23 {
        requests.push(json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":format!("isolated-probe-{i}\nquoted: \"text\"; braces: }} ]; slash: \\; unicode: 🦊\r\nlast line"),"delivery":"record"}}}));
        requests.push(json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":i}}));
    }
    // A client may reject a well-formed but unrecognised MCP reply with an
    // id-less error. It has no request to answer and must not break later calls.
    requests.push(json!({"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid request"}}));
    let note = "first line\nquoted: \"note\"; braces: } ]; slash: \\; unicode: 🦊\r\nlast line";
    requests.push(json!({"jsonrpc":"2.0","id":"claim-note","method":"tools/call","params":{"name":"control_room_claim","arguments":{"room":"general","key":"frame-check","action":"acquire","status":"working","note":note}}}));
    // A tool's validation failure is a result Codex correlates with its call,
    // never a JSON-RPC error it rejects as an id-less Invalid Request.
    requests.push(json!({"jsonrpc":"2.0","id":"bad-post","method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":"never posted","delivery":"shout"}}}));
    requests.push(json!({"jsonrpc":"2.0","id":"room-read","method":"tools/call","params":{"name":"control_room_read","arguments":{"room":"general"}}}));
    requests.push(json!([
        {"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":"batch milestone one","delivery":"record"}}},
        {"jsonrpc":"2.0","method":"notifications/progress"},
        {"jsonrpc":"2.0","id":24,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":"batch milestone two","delivery":"record"}}},
        {"jsonrpc":"2.0","id":25,"method":"tools/call","params":{"name":"control_agent_spawn","arguments":{"objectives":["Own infrastructure","Own farming","Own building","Own mining","Own exploration"]}}}
    ]));
    let expected = requests
        .iter()
        .flat_map(|r| r.as_array().cloned().unwrap_or_else(|| vec![r.clone()]))
        .filter_map(|r| r.get("id").cloned())
        .collect::<Vec<_>>();
    for request in requests {
        let frame = format!("{request}\n");
        assert_eq!(frame.bytes().filter(|&byte| byte == b'\n').count(), 1);
        // Pipe reads may split anywhere, including inside a UTF-8 character or
        // an escape. Only the final delimiter ends a frame.
        for chunk in frame.as_bytes().chunks(7) {
            input.write_all(chunk).unwrap();
        }
    }
    drop(input);
    let (sender, replies) = std::sync::mpsc::channel();
    let output = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let _ = sender.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut observed = Vec::new();
    let mut room_read = None;
    let mut bad_post = None;
    while Instant::now() < deadline {
        pump(&shared, &run, &workspace, false);
        while let Ok(line) = replies.try_recv() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            for value in value.as_array().cloned().unwrap_or_else(|| vec![value]) {
                assert!(value.get("error").is_none(), "{value}");
                if value["id"] == "bad-post" {
                    bad_post = Some(value.clone());
                } else {
                    assert_ne!(value["result"]["isError"], true, "{value}");
                }
                if value["id"] == "room-read" {
                    room_read = Some(
                        serde_json::from_str::<Value>(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),
                    );
                }
                observed.push(value["id"].clone());
            }
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    reader.join().unwrap();
    while let Ok(line) = replies.try_recv() {
        let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
        for value in value.as_array().cloned().unwrap_or_else(|| vec![value]) {
            assert!(value.get("error").is_none(), "{value}");
            if value["id"] == "bad-post" {
                bad_post = Some(value.clone());
            } else {
                assert_ne!(value["result"]["isError"], true, "{value}");
            }
            if value["id"] == "room-read" {
                room_read = Some(
                    serde_json::from_str::<Value>(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),
                );
            }
            observed.push(value["id"].clone());
        }
    }
    assert!(status.success(), "MCP bridge did not complete; replies: {observed:?}");
    assert_eq!(observed, expected);
    let bad_post = bad_post.expect("the invalid post must be answered");
    assert_eq!(
        crate::toolbox::failure_text(&bad_post),
        Some("Expected delivery=record or notify."),
        "{bad_post}"
    );
    let room_read = room_read.expect("a room read must complete after the id-less peer error");
    assert_eq!(room_read["claims"][0]["note"], note);
    assert_eq!(
        room_read["posts"][0]["text"],
        "isolated-probe-22\nquoted: \"text\"; braces: } ]; slash: \\; unicode: 🦊\r\nlast line"
    );
    let state = shared.lock().unwrap();
    assert_eq!(state.collective.posts.len(), 22);
    assert_eq!(state.chats.len(), 6);
    assert!(
        state
            .chats
            .iter()
            .all(|chat| chat.room.as_deref().is_some_and(|room| room.starts_with("general/"))),
        "children inherit the room established before their spawn"
    );
    let children = state
        .chats
        .iter()
        .filter(|chat| chat.parent_chat.is_some())
        .map(|chat| chat.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 5);
    drop(state);
    for (index, child) in children.iter().enumerate() {
        let response = dispatch_at(
            &shared,
            &run,
            &json!({"nonce":format!("9-{index}"),"request":{"jsonrpc":"2.0","id":index,"method":"tools/call","params":{"name":"control_bot_request","arguments":{"chat_id":child}}}}),
            Some(&workspace),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["data"]["decision"], "pending",
            "{response}"
        );
    }
}
#[test]
fn ordinary_agents_receive_the_shared_continuity_mcp_without_bot_authority() {
    for backend in [crate::backend::Backend::Codex, crate::backend::Backend::Claude] {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Carry out the assigned work", "test").unwrap();
        state.runtime.runs[0].backend = backend;
        crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
        assert!(!crate::bots::defined(&state, &chat));
        let id = state.runtime.runs[0].id.clone();
        let attempt = &state.runtime.runs[0].attempts[0];
        assert!(attempt.turn_mailbox.is_some() && attempt.context_trace.is_some());
        let shared = Arc::new(Mutex::new(state));
        let listed = rpc_at(
            &shared,
            &id,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
            None,
        );
        let tools = listed["result"]["tools"].as_array().unwrap();
        for name in ["control_turn_context", "control_turn_handoff", "control_room_read"] {
            assert!(tools.iter().any(|t| t["name"] == name), "{backend:?}: {name}");
        }
        for name in ["control_room_ack", "control_room_notifications"] {
            assert!(!tools.iter().any(|t| t["name"] == name));
        }
    }
}
#[test]
fn staged_handoffs_fence_mutators_by_host_policy_and_legacy_chats_hide_continuity_tools() {
    use control_context::{
        turn_mailbox::{Disposition, Handoff, Mailbox},
        working_state::Payload,
    };
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Work", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    let id = state.runtime.runs[0].id.clone();
    let shared = Arc::new(Mutex::new(state));
    let list = json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}});
    let legacy = rpc_at(&shared, &id, &list, None);
    assert!(
        !legacy["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control_turn_context")
    );
    {
        let mut state = shared.lock().unwrap();
        state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed::default()));
        let mut mailbox = Mailbox::default();
        mailbox
            .stage(
                "draft".into(),
                Handoff {
                    base_revision: 0,
                    payload: Payload {
                        subgoal: "Keep the goal".into(),
                        ..Default::default()
                    },
                    processed: vec![],
                    resynchronised: vec![],
                    disposition: Disposition::Continue,
                    progress: None,
                },
                0,
            )
            .unwrap();
        state.runtime.runs[0].attempts.push(crate::usage::Attempt {
            workspace_id: Some(format!("{id}-attempt-1")),
            turn_mailbox: Some(Box::new(mailbox)).into(),
            ..Default::default()
        });
    }
    let managed = rpc_at(&shared, &id, &list, None);
    assert!(
        managed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control_turn_context")
    );
    let before = serde_json::to_value(&*shared.lock().unwrap()).unwrap();
    for name in [
        "control_agent_spawn",
        "control_agent_spawn_blocking",
        "control_design_ask",
        "control_service_start",
        "control_port_request",
        "control_room_post",
        "control_room_ack",
        "control_workspace_run",
        "unknown",
    ] {
        let denied = rpc_at(
            &shared,
            &id,
            &json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":{},"annotations":{"readOnlyHint":true}}}}),
            None,
        );
        assert!(
            crate::toolbox::failure_text(&denied)
                .unwrap()
                .contains("handoff is staged"),
            "{name}"
        );
    }
    assert_eq!(serde_json::to_value(&*shared.lock().unwrap()).unwrap(), before);
}
fn design_transport_envelope() -> Value {
    // Reproduce the recorded successful transport shape with generic task
    // arguments, not the literal user conversation. Identities are timestamp-count.
    json!({"nonce":"1793455807009221658-5","request":{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"control_design_ask","arguments":{
        "question":"How should we proceed with the local service? The checked environment lacks its required runtime, and no supported dependency download route is available.",
        "options":["Supply a tool container with the required runtime and dependencies, then continue implementation.","Prepare task-local service configuration and an implementation plan for later execution.","Stop the service task."],"recommended":0
    }}}})
}
#[test]
fn design_rpc_reports_fields_without_blaming_transport_and_retains_pause_fences() {
    let (mut state, _) = crate::design::tests::fixture();
    let id = state.runtime.runs[0].id.clone();
    let envelope = design_transport_envelope();
    let input = crate::design::parse_input(envelope["request"]["params"]["arguments"].clone()).unwrap();
    let admitted =
        crate::design::tests::ask_without_io(&mut state, &id, envelope["nonce"].as_str().unwrap(), input).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let before = serde_json::to_vec(&*shared.lock().unwrap()).unwrap();
    for (field, value, expected) in [
        ("question", json!(null), "question must be a string"),
        ("question", json!(" "), "question must contain"),
        ("options", json!("A or B"), "options must be an array"),
        ("options", json!(["Only one"]), "options must contain"),
        ("options", json!(["Same", " Same "]), "options must be distinct"),
        ("options", json!(["A", "x".repeat(201)]), "Each option must contain"),
        ("recommended", json!(3), "recommended must be a zero-based index"),
        ("recommended", json!(-1), "recommended must be a nonnegative"),
        ("recommended", json!("first"), "recommended must be a nonnegative"),
    ] {
        let mut malformed = envelope.clone();
        malformed["request"]["params"]["arguments"][field] = value;
        let response = rpc_at(&shared, &id, &malformed, None);
        let message = crate::toolbox::failure_text(&response).unwrap();
        assert!(message.starts_with(expected), "{message}");
        assert!(!message.contains("identity"));
        assert_eq!(serde_json::to_vec(&*shared.lock().unwrap()).unwrap(), before);
    }
    for _ in 0..2 {
        let replay = rpc_at(&shared, &id, &envelope, None);
        assert_eq!(
            serde_json::from_str::<Value>(replay["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),
            admitted
        );
    }
    let mut unknown = envelope.clone();
    unknown["request"]["params"]["arguments"]["extra"] = json!("do-not-echo-this-value");
    let response = rpc_at(&shared, &id, &unknown, None);
    let message = crate::toolbox::failure_text(&response).unwrap();
    assert!(message.contains("an object containing only"));
    assert!(!message.contains("do-not-echo-this-value"));
    for nonce in ["", "unsupported_identity", &"a".repeat(129)] {
        let mut bad_identity = envelope.clone();
        bad_identity["nonce"] = json!(nonce);
        assert_eq!(
            crate::toolbox::failure_text(&rpc_at(&shared, &id, &bad_identity, None)).unwrap(),
            "Invalid design request identity; retry the tool call."
        );
    }
    let mut different = envelope.clone();
    different["request"]["params"]["arguments"]["question"] = json!("A different valid question?");
    assert_eq!(
        crate::toolbox::failure_text(&rpc_at(&shared, &id, &different, None)).unwrap(),
        "This attempt already has a design question."
    );
    let write = json!({"nonce":"1793455807009221658-6","request":{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"control_workspace_start","arguments":{"command":"true"}}}});
    assert_eq!(
        crate::toolbox::failure_text(&rpc_at(&shared, &id, &write, None)).unwrap(),
        "This attempt is pausing for a design answer; no more tools may run."
    );
    assert_eq!(serde_json::to_vec(&*shared.lock().unwrap()).unwrap(), before);
}
#[test]
fn completed_child_summary_is_available_through_the_worker_broker() {
    let shared = Arc::new(Mutex::new(crate::delegated_results::tests::fixture()));
    let response = rpc_at(
        &shared,
        "resumed",
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_agent_result","arguments":{"child":"child","section":"summary","limit":10}}}}),
        None,
    );
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["summary"]["text"], "All reques");
    assert_eq!(value["summary"]["next_offset"], 10);
    assert_eq!(shared.lock().unwrap().runtime.runs.len(), 3);
}
#[test]
fn judge_tools_are_enforced_even_when_unadvertised_names_are_forged() {
    let mut s = State::default();
    let chat = crate::chat::create(&mut s);
    crate::chat::send(&mut s, &chat, "Review this", "test").unwrap();
    let run = &mut s.runtime.runs[0];
    let id = run.id.clone();
    run.status = "running".into();
    run.backend = crate::backend::Backend::Claude;
    run.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "root".into(),
        depth: 1,
        parent: Some("parent".into()),
        bundle: None,
    });
    let shared = Arc::new(Mutex::new(s));
    let callable = callable(&shared, &id);
    let names = callable.iter().map(String::as_str).collect::<Vec<_>>();
    assert!(names.contains(&"control_workspace_run"));
    assert!(!names.contains(&"control_agent_spawn_blocking"));
    assert!(!names.contains(&"control_agent_delegate"));
    assert!(names.contains(&"control_agent_result"));
    assert!(names.contains(&"control_knowledge_read"));
    assert!(names.contains(&"control_knowledge_search"));
    for name in [
        "control_agent_spawn_blocking",
        "control_agent_delegate",
        "control_agent_spawn",
        "control_agent_message",
        "control_project_request",
        "control_design_ask",
        "control_preview_start",
        "control_preview_stop",
        "control_github_pr",
        "control_service_start",
        "control_service_attach",
        "control_service_status",
        "control_service_stop",
        "control_service_exec",
        "control_service_exec_status",
        "control_service_copy",
        "control_knowledge_propose",
    ] {
        assert!(!names.contains(&name));
        let denied = rpc_at(
            &shared,
            &id,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{}}}}),
            None,
        );
        assert!(crate::toolbox::failure_text(&denied).unwrap().contains("Judge policy"));
    }
    assert_eq!(shared.lock().unwrap().runtime.runs.len(), 1);
    // Captured guidance is a read every role may make, judges included.
    assert!(names.contains(&"control_guidance"));
    let read = rpc_at(
        &shared,
        &id,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_guidance","arguments":{"topic":"session"}}}}),
        None,
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    assert_eq!(
        read["result"]["structuredContent"]["data"]["text"],
        control_context::agent_policy::SESSION_GUIDANCE_DETAIL
    );
}
#[test]
fn an_approved_advisor_judge_gains_only_evidence_notes() {
    let mut s = State::default();
    let lead = crate::chat::create(&mut s);
    s.chats[0].lenses = Some(vec!["advisor".into()]);
    crate::chat::send(&mut s, &lead, "Lead the team", "test").unwrap();
    s.runtime.runs[0].status = "completed".into();
    let chat = crate::chat::create(&mut s);
    let session = s.chats.iter_mut().find(|c| c.id == chat).unwrap();
    (session.parent_chat, session.room) = (Some(lead.clone()), Some("general".into()));
    crate::chat::send(&mut s, &chat, "Advise the team", "test").unwrap();
    let run = s.runtime.runs.last_mut().unwrap();
    let id = run.id.clone();
    run.status = "running".into();
    run.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "root".into(),
        depth: 1,
        parent: None,
        bundle: None,
    });
    let names = |shared: &Arc<Mutex<State>>| {
        rpc_at(
            shared,
            &id,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
            None,
        )["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let shared = Arc::new(Mutex::new(s));
    let judge = names(&shared);
    assert!(!judge.iter().any(|name| name == "control_agent_message"));
    crate::bots::command(&mut shared.lock().unwrap(), &chat, "on").unwrap();
    let advisor = names(&shared);
    assert_eq!(
        advisor.iter().filter(|name| !judge.contains(name)).collect::<Vec<_>>(),
        ["control_agent_message"]
    );
    for name in [
        "control_agent_spawn",
        "control_service_start",
        "control_design_ask",
        "control_room_watch",
    ] {
        let denied = rpc_at(
            &shared,
            &id,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{}}}}),
            None,
        );
        assert!(
            crate::toolbox::failure_text(&denied).unwrap().contains("Judge policy"),
            "{name}"
        );
    }
    let post = |delivery: &str| {
        rpc_at(
            &shared,
            &id,
            &json!({"nonce":if delivery == "record" { "1" } else { "2" },"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"message":"Build claim looks abandoned","delivery":delivery}}}}),
            None,
        )
    };
    for delivery in ["notify", "record"] {
        assert!(
            post(delivery).to_string().contains("does not post to rooms"),
            "an advisor only steers the lead: {delivery}"
        );
    }
    assert!(shared.lock().unwrap().collective.posts.is_empty());
}
#[test]
fn every_advertised_worker_tool_has_ui_registration() {
    for backend in [crate::backend::Backend::Codex, crate::backend::Backend::Claude] {
        for room in [None, Some("engineering")] {
            let mut state = State::default();
            let chat = crate::chat::create(&mut state);
            state.chats[0].room = room.map(str::to_owned);
            crate::chat::send(&mut state, &chat, "Inspect tools", "test").unwrap();
            let run = &mut state.runtime.runs[0];
            run.status = "running".into();
            run.backend = backend;
            let id = run.id.clone();
            let shared = Arc::new(Mutex::new(state));
            let response = rpc_at(
                &shared,
                &id,
                &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
                None,
            );
            for definition in response["result"]["tools"].as_array().unwrap() {
                let name = definition["name"].as_str().unwrap();
                let entry =
                    control_context::tool_for_mcp(name).unwrap_or_else(|| panic!("Missing worker tool in UI: {name}"));
                assert!(crate::tool_catalog::render(&shared.lock().unwrap(), name).contains(entry.title));
            }
        }
    }
}
#[test]
fn delegated_pause_fences_command_writers_and_github_before_dispatch() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/improve Check a fixture", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("pause-generation".into()),
        ..Default::default()
    });
    run.delegation = Some(crate::delegation::Delegation {
        input: crate::delegation::Input {
            model: None,
            objective: "Inspect".into(),
            role: crate::delegation::Role::Worker,
        },
        nonce: "delegate-fixture".into(),
        generation: "pause-generation".into(),
        backend: run.backend,
        model: "fixture".into(),
        child: None,
        continuation: None,
    });
    let id = run.id.clone();
    let shared = Arc::new(Mutex::new(state));
    for (name, args) in [
        ("control_workspace_start", json!({"command":"true"})),
        (
            "control_github_open_pr",
            json!({"run_id":"r-other","title":"Must not publish"}),
        ),
    ] {
        let response = rpc(
            &shared,
            &id,
            &json!({"nonce":"request-fixture","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}}),
        );
        assert!(
            crate::toolbox::failure_text(&response)
                .unwrap()
                .contains("pausing for delegated")
        );
    }
    let host = crate::tool_registry::LocalHost(&shared);
    let cx = crate::tool_registry::AttemptCall {
        attempt: crate::tool_registry::AttemptRef {
            run: &id,
            generation: "pause-generation",
            nonce: "request-fixture",
        },
        host: &host,
    };
    let response = crate::tool_registry::attempt_tools("control_workspace_start")
        .unwrap()
        .call(&cx, "control_workspace_start", &json!({"command":"true"}));
    assert!(response.unwrap_err().to_string().contains("pausing for delegated"));
    let state = shared.lock().unwrap();
    assert!(state.runtime.runs[0].attempts[0].command_jobs.is_empty());
    assert!(state.github_publications.is_empty());
}
#[test]
fn design_pause_blocks_further_tool_effects_until_user_answers() {
    let (mut state, _) = crate::design::tests::paused();
    state.runtime.runs[0].status = "running".into();
    let id = state.runtime.runs[0].id.clone();
    let state = Arc::new(Mutex::new(state));
    assert!(crate::design::requested(Some((&state, &id))));
    let response = rpc(
        &state,
        &id,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_rust_run","arguments":{"code":"fn main() {}"}}}}),
    );
    assert!(crate::toolbox::failure_text(&response).unwrap().contains("pausing"));
    state.lock().unwrap().runtime.runs[0].attempts[0].workspace_id = Some("retry-generation".into());
    assert!(!crate::design::requested(Some((&state, &id))));
}
fn rpc(state: &Arc<Mutex<State>>, run: &str, value: &Value) -> Value {
    rpc_at(state, run, value, None)
}
/// Every tool a session may call: its listed core plus its deferred index.
fn callable(state: &Arc<Mutex<State>>, run: &str) -> Vec<String> {
    let listed = rpc(
        state,
        run,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
    );
    let index = rpc(
        state,
        run,
        &json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control_tool_search","arguments":{}}}}),
    );
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .chain(
            index["result"]["structuredContent"]["data"]["deferred"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned()),
        )
        .collect()
}
/// Codex leaves a tools/call pending when its reply is a JSON-RPC error, so
/// every tool failure is a result; only protocol faults stay JSON-RPC errors.
#[test]
fn tool_failures_are_results_and_protocol_faults_stay_errors() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Tool error shapes", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    let run = state.runtime.runs[0].id.clone();
    let shared = Arc::new(Mutex::new(state));
    let call = |id: Value, name: &str, arguments: Value| {
        rpc(
            &shared,
            &run,
            &json!({"nonce":"7-1","request":{"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}}),
        )
    };
    for (id, name, arguments, expected) in [
        (
            json!(2),
            "control_room_post",
            json!({"room":"general","message":"never posted","delivery":"shout"}),
            Some("Expected delivery=record or notify."),
        ),
        (
            json!("bad-room"),
            "control_room_post",
            json!({"room":"no such room","message":"never posted"}),
            Some("Unknown room no such room. This chat's team room is general/"),
        ),
        (
            json!(3),
            "control_room_claim",
            json!({"room":"general","key":"k","action":"acquire","status":"released"}),
            None,
        ),
        (
            json!(4),
            "control_agent_spawn_blocking",
            json!({"unexpected":true}),
            Some("Expected a delegation objective and role"),
        ),
        // MCP names unknown tools a protocol error; a tool result keeps Codex's call
        // resolved and is indistinguishable from a policy denial of a known tool.
        (
            json!(5),
            "control_no_such_tool",
            json!({}),
            Some("Tool is not allowed for this worker"),
        ),
    ] {
        let response = call(id.clone(), name, arguments);
        assert_eq!(response["id"], id, "{response}");
        let message = crate::toolbox::failure_text(&response).unwrap_or_else(|| panic!("{name}: {response}"));
        if let Some(expected) = expected {
            assert!(message.starts_with(expected), "{message}");
        }
    }
    assert!(shared.lock().unwrap().collective.posts.is_empty());
    for (request, code) in [
        (
            json!({"jsonrpc":"1.0","id":6,"method":"tools/call","params":{"name":"control_room_list"}}),
            -32600,
        ),
        (
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"arguments":{}}}),
            -32602,
        ),
        (json!({"jsonrpc":"2.0","id":8,"method":"resources/list"}), -32601),
    ] {
        let response = rpc(&shared, &run, &json!({"nonce":"7-2","request":request}));
        assert_eq!(
            (response["id"].clone(), response["error"]["code"].clone()),
            (request["id"].clone(), json!(code)),
            "{response}"
        );
        assert!(response.get("result").is_none());
    }
    // A managed session's inbox joins a failed tool's content, and a
    // protocol error never gains a "result" member beside its "error".
    std::fs::create_dir_all(crate::config::state_directory().unwrap()).unwrap();
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Carry out the assigned work", "test").unwrap();
    crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let managed = state.runtime.runs[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer Keep the public API", "test").unwrap();
    let managed_state = Arc::new(Mutex::new(state));
    let failed = rpc(
        &managed_state,
        &managed,
        &json!({"nonce":"8-1","request":{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":"x","delivery":"shout"}}}}),
    );
    assert!(crate::toolbox::failure_text(&failed).is_some_and(|text| text.starts_with("Expected delivery=record or notify. Next: call control_room_read {}")), "{failed}");
    assert!(
        failed["result"]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("Keep the public API"),
        "{failed}"
    );
    let fault = rpc(
        &managed_state,
        &managed,
        &json!({"nonce":"8-2","request":{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{}}}),
    );
    assert_eq!(fault["error"]["code"], -32602);
    assert!(fault.get("result").is_none(), "{fault}");
    // Room-only methods keep their own protocol errors as well.
    let unsupported = crate::rooms::worker_rpc(
        &mut shared.lock().unwrap(),
        &run,
        &json!({"request":{"jsonrpc":"2.0","id":9,"method":"prompts/list"}}),
    );
    assert_eq!(unsupported["error"]["code"], -32601);
}
/// Every successful result leaves the worker boundary compact, in both the
/// structured data and the text a model reads, whichever tool produced it.
#[test]
fn successful_results_leave_the_boundary_compact_and_identical_in_both_forms() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Compact results", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    let run = state.runtime.runs[0].id.clone();
    let shared = Arc::new(Mutex::new(state));
    let call = |nonce: &str, name: &str, arguments: Value| {
        let response = rpc(
            &shared,
            &run,
            &json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}}),
        );
        let text = serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap_or_else(|_| panic!("{name}: {response}"));
        // Room tools reply in text only; others also carry structured data.
        if let Some(data) = response["result"].get("structuredContent") {
            assert_eq!(data["data"], text, "{response}");
        }
        text
    };
    let claim = call(
        "31-1",
        "control_room_claim",
        json!({"room":"general","key":"k","action":"acquire","note":"mine"}),
    );
    // No empty related list, no echo of the caller's own note, room or key.
    assert_eq!(claim["applied"], true, "{claim}");
    assert!(
        claim.get("related").is_none() && claim["claim"].get("note").is_none(),
        "{claim}"
    );
    assert!(claim["claim"]["revision"].is_u64(), "{claim}");
    let posted = call(
        "31-2",
        "control_room_post",
        json!({"room":"general","message":"hello","delivery":"record"}),
    );
    assert_eq!(posted["posted"], true, "{posted}");
    assert!(
        posted.get("delivery").is_none(),
        "the request's own delivery is not echoed: {posted}"
    );
    // Refusals keep their exact text and callable fix.
    let refused = rpc(
        &shared,
        &run,
        &json!({"nonce":"31-3","request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control_room_claim","arguments":{"room":"general","key":"k","action":"update","revision":999,"note":""}}}}),
    );
    let refusal = serde_json::from_str::<Value>(refused["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (refusal["applied"].as_bool(), refusal["claim"]["note"].as_str()),
        (Some(false), Some("mine")),
        "{refused}"
    );
}
#[test]
fn stale_workspace_is_rejected_for_room_posts_before_dispatch() {
    let mut state = State::default();
    state.runtime.enqueue("task".into(), RunKind::Task, "test");
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    let id = run.id.clone();
    let mut attempt = crate::usage::Attempt::new();
    attempt.workspace_id = Some("current-attempt".into());
    run.attempts.push(attempt);
    let state = Arc::new(Mutex::new(state));
    let stale = std::env::temp_dir().canonicalize().unwrap();
    let request = json!({"nonce":"late-proposal","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_knowledge_propose","arguments":{"claim":"Must not persist","evidence":"stale mailbox","scope":"fixture","validity":"always"}}}});
    let response = rpc_at(&state, &id, &request, Some(&stale));
    assert_eq!(
        crate::toolbox::failure_text(&response).unwrap(),
        "Mailbox belongs to an inactive attempt"
    );
    assert!(state.lock().unwrap().collective.proposals.is_empty());
    for (name, args) in [
        ("control_room_post", json!({"message":"late effect"})),
        ("control_room_watch", json!({"enabled":true,"wake":true})),
        ("control_room_ack", json!({"through":0})),
    ] {
        let request =
            json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}});
        let response = rpc_at(&state, &id, &request, Some(&stale));
        // Room refusals keep the host's message first and add a callable fix.
        let text = crate::toolbox::failure_text(&response).unwrap();
        assert!(
            text.starts_with("Mailbox belongs to an inactive attempt") && crate::worker_tools::fixes::names_fix(text),
            "{text}"
        );
    }
    assert!(state.lock().unwrap().collective.posts.is_empty());
    assert!(state.lock().unwrap().collective.room_watches.is_empty());
}
#[test]
fn workspace_execution_is_backend_scoped_and_rejects_model_supplied_mounts() {
    let mut state = State::default();
    state.runtime.enqueue("Test".into(), RunKind::Task, "test");
    state.runtime.runs[0].status = "running".into();
    let id = state.runtime.runs[0].id.clone();
    let state = Arc::new(Mutex::new(state));
    let request = json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_workspace_run","arguments":{"command":"true"}}}});
    assert!(crate::toolbox::failure_text(&rpc(&state, &id, &request)).is_some());
    state.lock().unwrap().runtime.runs[0].backend = crate::backend::Backend::Claude;
    let tools = rpc(
        &state,
        &id,
        &json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/list"}}),
    );
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control_workspace_run")
    );
    let mut forged = request.clone();
    forged["request"]["params"]["arguments"]["mount"] = json!("/Users/operator/.config/control");
    assert!(crate::toolbox::failure_text(&rpc(&state, &id, &forged)).is_some());
    assert!(crate::toolbox::failure_text(&rpc(&state, &id, &request)).is_some()); // no captured attempt
}
#[test]
fn workers_get_concrete_tools_without_operator_authority_or_room_access() {
    let mut state = State::default();
    state.runtime.enqueue("Test tools".into(), RunKind::Task, "test");
    let run = state.runtime.runs[0].id.clone();
    state.runtime.runs[0].status = "running".into();
    let state = Arc::new(Mutex::new(state));
    let callable = callable(&state, &run);
    let names: std::collections::BTreeSet<_> = callable.iter().map(String::as_str).collect();
    for name in [
        "control_agent_spawn_blocking",
        "control_agent_result",
        "control_workspace_start",
        "control_project_request",
    ] {
        assert!(names.contains(name));
    }
    assert!(!names.contains("control_agent_delegate"));
    let request = |name| json!({"nonce":"1-1","request":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":{"unexpected":true}}}});
    let response = rpc(&state, &run, &request("control_agent_spawn_blocking"));
    assert_eq!(
        crate::toolbox::failure_text(&response).unwrap(),
        "Expected a delegation objective and role"
    );
    let retired = rpc(&state, &run, &request("control_agent_delegate"));
    assert!(crate::toolbox::failure_text(&retired).is_some());
    assert_ne!(
        crate::toolbox::failure_text(&retired).unwrap(),
        crate::toolbox::failure_text(&response).unwrap()
    );
    assert!(
        !names.contains("control_github_open_pr"),
        "Ordinary tasks have no publication grant"
    );
    let forbidden = rpc(
        &state,
        &run,
        &json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.chat.send"}}}),
    );
    assert!(crate::toolbox::failure_text(&forbidden).is_some());
    state.lock().unwrap().runtime.runs[0].status = "completed".into();
    assert!(
        rpc(
            &state,
            &run,
            &json!({"request":{"jsonrpc":"2.0","id":3,"method":"tools/list"}})
        )
        .get("error")
        .is_some()
    );
}

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and isolated CONTROL_STATE_DIR; stresses the real bridge with wake-driven host pumping"]
fn live_mailbox_survives_sustained_wake_driven_traffic() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        process::Stdio,
        time::{Duration, Instant},
    };
    let root = crate::config::state_directory().unwrap();
    assert!(root.to_string_lossy().starts_with("/private/tmp/control-mailbox-qa."));
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let calls: i64 = std::env::var("CONTROL_QA_CALLS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(2000);
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Mailbox stress probe", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-stress".into()),
        image: Some(image.clone()),
        ..Default::default()
    });
    let run = state.runtime.runs[0].id.clone();
    let workspace = crate::runtime::workspace("attempt-stress").unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let shared = Arc::new(Mutex::new(state));
    let name = format!("control-mailbox-stress-{}", std::process::id());
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = crate::engine::query(&["rm", "-f", &self.0]);
        }
    }
    let _cleanup = Cleanup(name.clone());
    let mut command = crate::engine::scrubbed();
    command.args([
        "run",
        "--rm",
        "--interactive",
        "--pull=never",
        "--name",
        &name,
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--memory=128m",
        "--cpus=1",
        "--pids-limit=32",
        "--user",
        &format!("{}:{}", unsafe { libc::getuid() }, unsafe { libc::getgid() }),
        "--mount",
        &format!("type=bind,src={},dst=/workspace", workspace.display()),
    ]);
    // A locally built bridge can be tested before it is baked into the image.
    if let Ok(bridge) = std::env::var("CONTROL_QA_BRIDGE") {
        command.args([
            "--mount",
            &format!("type=bind,src={bridge},dst=/opt/control-room-mcp,readonly"),
            "--entrypoint=/opt/control-room-mcp",
        ]);
    } else {
        command.arg("--entrypoint=/bin/control-room-mcp");
    }
    command.arg(&image);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let output = child.stdout.take().unwrap();
    let (sender, replies) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let _ = sender.send(line);
        }
    });
    let writer = std::thread::spawn(move || {
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"qa","version":"1"}}})).unwrap();
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        for i in 1..=calls {
            let request = if i % 2 == 0 {
                json!({"jsonrpc":"2.0","id":i,"method":"tools/list"})
            } else {
                json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"control_room_read","arguments":{"room":"general"}}})
            };
            if writeln!(input, "{request}").is_err() {
                return;
            }
        }
    });
    let wake = crate::mailbox_wake::MailboxWake::new(&workspace);
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut observed = 0i64;
    while Instant::now() < deadline && observed <= calls {
        pump(&shared, &run, &workspace, false);
        while let Ok(line) = replies.try_recv() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            assert_eq!(value["id"], json!(observed), "{value}");
            observed += 1;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        wake.wait(Duration::from_millis(250));
    }
    let alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let status = child.wait().unwrap();
    let _ = writer.join();
    reader.join().unwrap();
    let errors = errors.join().unwrap();
    assert!(
        alive || status.success(),
        "bridge exited early after {observed} replies: {status:?} {errors}"
    );
    assert_eq!(observed, calls + 1, "stderr: {errors}");
}
#[test]
fn json_rpc_batches_are_answered_per_element_with_distinct_nonces() {
    let envelope = json!({"nonce":"7-3","request":[
        {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post"}},
        {"jsonrpc":"2.0","method":"notifications/progress"},
        {"jsonrpc":"2.0","id":"b","method":"tools/list"}]});
    let seen = std::cell::RefCell::new(Vec::new());
    let reply = answer(&envelope, |element| {
        seen.borrow_mut().push(element["nonce"].as_str().unwrap().to_owned());
        match element["request"].get("id") {
            Some(id) => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            None => Value::Null,
        }
    });
    assert_eq!(seen.into_inner(), ["7-3-0", "7-3-1", "7-3-2"]);
    let replies = reply.as_array().expect("a batch is answered with an array");
    assert_eq!(
        replies.iter().map(|r| r["id"].clone()).collect::<Vec<_>>(),
        [json!(1), json!("b")],
        "notifications get no reply"
    );
    let single = json!({"nonce":"1","request":{"jsonrpc":"2.0","id":5,"method":"ping"}});
    assert_eq!(answer(&single, |e| e["request"]["id"].clone()), json!(5));
    let notifications = json!({"nonce":"2","request":[{"jsonrpc":"2.0","method":"notifications/initialized"}]});
    assert!(
        answer(&notifications, |_| Value::Null).is_null(),
        "an all-notification batch has no reply"
    );
    assert!(answer(&json!({"nonce":"3","request":[]}), |_| Value::Null)["error"].is_object());
    let malformed = answer(
        &json!({"nonce":"4","request":[42,{}, {"jsonrpc":"2.0","method":"ping","id":8}]}),
        |e| json!({"id":e["request"]["id"],"result":{}}),
    );
    assert_eq!(malformed.as_array().unwrap().len(), 3);
    assert_eq!(malformed[0]["error"]["code"], -32600);
    assert_eq!(malformed[1]["error"]["code"], -32600);
    assert_eq!(malformed[2]["id"], 8);
}

/// Codex 0.164 shows its model only `structuredContent` when a result has
/// one, and Claude Code 2.1 does the same unless a flag opts into content, so
/// host notes must be in both places.
#[test]
fn host_notes_reach_text_and_structured_clients() {
    let mut result = json!({"content":[{"type":"text","text":"{\"posted\":true}"}],"structuredContent":{"data":{"posted":true}},"isError":false});
    attach_notes(&mut result, &["first".to_owned(), "second".to_owned()]);
    assert_eq!(
        result,
        json!({
            "content":[{"type":"text","text":"{\"posted\":true}"},{"type":"text","text":"first"},{"type":"text","text":"second"}],
            "structuredContent":{"data":{"posted":true},"notes":["first","second"]},
            "isError":false,
        })
    );
    attach_notes(&mut result, &["third".to_owned()]);
    assert_eq!(
        result["structuredContent"]["notes"],
        json!(["first", "second", "third"])
    );

    let mut unchanged = result.clone();
    attach_notes(&mut unchanged, &[]);
    assert_eq!(unchanged, result, "no notes adds nothing");

    let mut refusal = json!({"content":[{"type":"text","text":"Run is not active"}],"isError":true});
    attach_notes(&mut refusal, &["note".to_owned()]);
    assert_eq!(
        refusal,
        json!({"content":[{"type":"text","text":"Run is not active"},{"type":"text","text":"note"}],"isError":true}),
        "text-only results stay text-only"
    );
}

#[test]
fn image_results_leave_without_structured_content() {
    let image = json!({"type":"image","mimeType":"image/png","data":"AAAA"});
    let mut capture = json!({"content":[{"type":"text","text":"{\"bytes\":4}"},image],"structuredContent":{"bytes":4,"width":390},"isError":false});
    media_visible(&mut capture);
    assert_eq!(
        capture,
        json!({"content":[{"type":"text","text":"{\"bytes\":4}"},image],"isError":false})
    );

    let mut untexted = json!({"content":[image],"structuredContent":{"bytes":4},"isError":false});
    media_visible(&mut untexted);
    assert_eq!(
        untexted,
        json!({"content":[{"type":"text","text":"{\"bytes\":4}"},image],"isError":false}),
        "the data stays visible as text"
    );

    let text_only = json!({"content":[{"type":"text","text":"{}"}],"structuredContent":{"data":{}},"isError":false});
    let mut kept = text_only.clone();
    media_visible(&mut kept);
    assert_eq!(kept, text_only, "text results keep structured data for programs");
}

/// Every Control tool result a CLI agent receives carries the chat's
/// observation, in text (Claude Code with content) and beside the structured
/// data (Codex, Claude Code without it); unchanged, it is a short marker. The
/// local loop's results carry none: its requests end with it instead.
#[test]
fn tool_results_carry_the_observation_for_text_and_structured_clients() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Play", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        ..Default::default()
    });
    let run = state.runtime.runs[0].id.clone();
    state.watches.push(serde_json::from_value(json!({
        "id":"w-1","name":"game","chat":chat,"service":"svc-game","generation":1,"permanent":true,"command":"run","stopped":false,
        "recent":[],"notifications":{"pending":0,"through":1,"recent":["hp=20"]}
    })).unwrap());
    crate::observe::declare_watch(&mut state, &chat, "w-1");
    let shared = Arc::new(Mutex::new(state));
    let call = |name: &str| json!({"nonce":"1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{}}}});
    let codex = || json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\"posted\":true}"}],"structuredContent":{"data":{"posted":true}},"isError":false}});
    let claude =
        || json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"posted"}],"isError":false}});

    let mut first = codex();
    observed(&shared, &run, &call("control_room_read"), &mut first, true);
    let block = first["result"]["structuredContent"]["observation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        block.starts_with("[Observation: latest output of watch game (w-1)") && block.contains("\nhp=20\n"),
        "{block}"
    );
    assert_eq!(first["result"]["content"][1]["text"], block, "the last text item");
    assert_eq!(first["result"]["structuredContent"]["data"], json!({"posted":true}));

    let mut second = claude();
    observed(&shared, &run, &call("control_service_exec_status"), &mut second, true);
    let marker = second["result"]["content"][1]["text"].as_str().unwrap();
    assert!(marker.starts_with("[Observation unchanged (age "), "{marker}");
    assert!(
        second["result"].get("structuredContent").is_none(),
        "text-only results stay text-only"
    );

    shared.lock().unwrap().watches[0]
        .notifications
        .as_mut()
        .unwrap()
        .recent
        .push("hp=3".into());
    let mut changed = codex();
    observed(&shared, &run, &call("control_room_read"), &mut changed, true);
    assert!(
        changed["result"]["structuredContent"]["observation"]
            .as_str()
            .unwrap()
            .contains("\nhp=3\n")
    );
    // Neither on a handoff, nor on the local loop's results, nor on other methods.
    for (name, attach) in [("control_turn_handoff", true), ("control_room_read", false)] {
        let mut result = codex();
        observed(&shared, &run, &call(name), &mut result, attach);
        assert_eq!(result, codex(), "{name}");
    }
    let mut listed = json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}});
    observed(
        &shared,
        &run,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
        &mut listed,
        true,
    );
    assert_eq!(listed["result"], json!({"tools":[]}));

    // Stored activity never keeps the value.
    let mut state = shared.lock().unwrap();
    let output = format!("posted\n{block}");
    crate::worker::record_activity_for(
        &mut state,
        &run,
        "attempt-a",
        crate::usage::Activity {
            id: "call-1".into(),
            kind: "mcp_tool_call".into(),
            text: "control_room_read".into(),
            output,
            status: "completed".into(),
        },
    );
    assert_eq!(
        state.runtime.runs[0].attempts[0].activity.last().unwrap().output,
        "posted"
    );
}
#[test]
fn an_approved_antibody_reads_every_team_notes_other_leads_and_notifies_the_operator() {
    let mut s = State::default();
    // A team on the node, with its lead and one member.
    let lead = crate::chat::create(&mut s);
    crate::chat::send(&mut s, &lead, "Build the wall", "test").unwrap();
    s.runtime.runs[0].status = "completed".into();
    let member = crate::chat::create(&mut s);
    s.chats.iter_mut().find(|c| c.id == member).unwrap().parent_chat = Some(lead.clone());
    crate::chat::send(&mut s, &member, "Place blocks", "test").unwrap();
    s.runtime.runs.last_mut().unwrap().status = "completed".into();
    s.runtime.runs.last_mut().unwrap().attempts = vec![crate::usage::Attempt {
        started_unix: crate::bots::now_ms() / 1000,
        ..Default::default()
    }];
    // The operator's observer tree: the antibody lens and a judge child.
    let observer = crate::chat::create(&mut s);
    s.chats.iter_mut().find(|c| c.id == observer).unwrap().lenses = Some(vec!["antibody".into()]);
    crate::chat::send(&mut s, &observer, "Watch the node", "test").unwrap();
    s.runtime.runs.last_mut().unwrap().status = "completed".into();
    let chat = crate::chat::create(&mut s);
    let session = s.chats.iter_mut().find(|c| c.id == chat).unwrap();
    (session.parent_chat, session.room) = (Some(observer.clone()), Some("general".into()));
    crate::chat::send(&mut s, &chat, "Tag pathologies", "test").unwrap();
    let run = s.runtime.runs.last_mut().unwrap();
    let id = run.id.clone();
    run.status = "running".into();
    run.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "root".into(),
        depth: 1,
        parent: None,
        bundle: None,
    });
    run.attempts = vec![crate::usage::Attempt {
        review_only: true,
        workspace_id: Some("gen-1".into()),
        ..Default::default()
    }];
    let shared = Arc::new(Mutex::new(s));
    let call = |nonce: &str, name: &str, arguments: Value| {
        rpc(
            &shared,
            &id,
            &json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}}),
        )
    };
    let failure = |value: Value| crate::toolbox::failure_text(&value).unwrap_or_default().to_owned();
    // Before approval it is an ordinary judge: no notes or notices.
    assert!(failure(call("n0", "control_operator_notify", json!({"text":"x"}))).contains("Judge policy"));
    crate::bots::command(&mut shared.lock().unwrap(), &chat, "on").unwrap();
    assert!(shared.lock().unwrap().bots.antibody(&chat));
    // Read-only over the whole node.
    let brief = call("n1", "control_intel_brief", json!({}));
    assert_eq!(brief["result"]["isError"], false, "{brief}");
    let markdown = brief["result"]["structuredContent"]["data"]["markdown"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        markdown.contains(&format!("({lead})")) && !markdown.contains(&format!("({observer})")),
        "{markdown}"
    );
    assert!(
        failure(call("n2", "control_room_post", json!({"message":"Team is looping"})))
            .contains("does not post to rooms")
    );
    for name in [
        "control_agent_spawn",
        "control_service_exec",
        "control_room_claim",
        "control_design_ask",
    ] {
        assert!(failure(call("n3", name, json!({}))).contains("Judge policy"), "{name}");
    }
    // Notes go only to another team's steering chat.
    assert!(
        failure(call(
            "n4",
            "control_agent_message",
            json!({"chat_id":member,"text":"Stop looping"})
        ))
        .contains("another team's steering chat")
    );
    assert!(
        failure(call(
            "n5",
            "control_agent_message",
            json!({"chat_id":observer,"text":"Hello"})
        ))
        .contains("another team's steering chat")
    );
    let note = call(
        "n6",
        "control_agent_message",
        json!({"chat_id":lead,"text":"Member keeps re-running the same failing command"}),
    );
    assert_eq!(note["result"]["isError"], false, "{note}");
    // Operator notices: recorded once, forwarded by channels, never repeated.
    let notice = call(
        "n7",
        "control_operator_notify",
        json!({"text":"Team is looping on one command","team":lead}),
    );
    assert_eq!(notice["result"]["isError"], false, "{notice}");
    assert!(
        failure(call(
            "n8",
            "control_operator_notify",
            json!({"text":"Team is looping on one command"})
        ))
        .contains("never repeated")
    );
    let state = shared.lock().unwrap();
    assert_eq!(state.bots.notices.len(), 1);
    assert!(state.collective.posts.is_empty() && state.services.is_empty());
}
#[test]
fn a_cli_agent_gets_new_conversation_once_on_its_next_tool_result() {
    // The inbox saves its delivery like a callback read.
    std::fs::create_dir_all(crate::config::state_directory().unwrap()).unwrap();
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Carry out the assigned work", "test").unwrap();
    crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    crate::chat::send(&mut state, &chat, "/steer Use the north gate", "test").unwrap();
    let shared = Arc::new(Mutex::new(state));
    let call = |nonce: &str| {
        rpc_at(
            &shared,
            &id,
            &json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_list","arguments":{}}}}),
            None,
        )
    };
    let first = call("1-0");
    let texts = first["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>();
    assert!(
        texts.last().unwrap().starts_with("Inbox (1 new") && texts.last().unwrap().contains("Use the north gate"),
        "{first}"
    );
    assert!(
        !first.to_string().contains("New callbacks"),
        "the inbox replaces the hint"
    );
    assert!(!call("1-1").to_string().contains("north gate"), "shown once");
}
/// Pins the worker server's envelopes: the run is checked before the method,
/// so an inactive run answers even an unknown method with -32602.
#[test]
fn worker_envelopes_are_pinned() {
    let shared = Arc::new(Mutex::new(State::default()));
    let request = |request: Value| json!({"nonce":"1","request":request});
    for (envelope, expected) in [
        (
            request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            Value::Null,
        ),
        (
            request(json!({"jsonrpc":"1.0","id":1,"method":"ping"})),
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":7}})),
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"tools/call requires params.name"}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":3,"method":"prompts/list"})),
            json!({"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Run is not active"}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":4,"method":"initialize"})),
            json!({"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"Run is not active"}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"control_room_list"}})),
            json!({"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"Run is not active"}],"isError":true}}),
        ),
    ] {
        assert_eq!(
            dispatch_raw(&shared, "missing", &envelope, None),
            expected,
            "{envelope}"
        );
    }
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Envelope shapes", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    let run = state.runtime.runs[0].id.clone();
    let shared = Arc::new(Mutex::new(state));
    assert_eq!(
        dispatch_raw(
            &shared,
            &run,
            &request(json!({"jsonrpc":"2.0","id":6,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})),
            None
        ),
        json!({"jsonrpc":"2.0","id":6,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-worker","version":"0.1"}}})
    );
    assert_eq!(
        dispatch_raw(
            &shared,
            &run,
            &request(json!({"jsonrpc":"2.0","id":7,"method":"ping"})),
            None
        ),
        json!({"jsonrpc":"2.0","id":7,"result":{}})
    );
    assert_eq!(
        dispatch_raw(
            &shared,
            &run,
            &request(json!({"jsonrpc":"2.0","id":8,"method":"prompts/list"})),
            None
        ),
        json!({"jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"Unsupported worker method"}})
    );
    let listed = dispatch_raw(
        &shared,
        &run,
        &request(json!({"jsonrpc":"2.0","id":9,"method":"tools/list"})),
        None,
    );
    assert!(
        listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "{listed}"
    );
    let guidance = dispatch_raw(
        &shared,
        &run,
        &request(
            json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"control_guidance","arguments":{}}}),
        ),
        None,
    );
    assert_eq!(guidance["result"]["content"][0]["type"], "text", "{guidance}");
    assert!(guidance["result"].get("isError").is_some());
    assert_eq!(
        answer(&json!({"nonce":"3","request":[]}), |_| Value::Null),
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Empty JSON-RPC batch"}})
    );
    assert_eq!(
        answer(&json!({"nonce":"4","request":[42]}), |_| Value::Null),
        json!([{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid JSON-RPC batch element"}}])
    );
}
