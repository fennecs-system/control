
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
    assert!(root
        .to_string_lossy()
        .starts_with("/private/tmp/control-mailbox-qa."));
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(
        &mut state,
        &chat,
        "Isolated mailbox transport probe",
        "test",
    )
    .unwrap();
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
        &format!("{}:{}", unsafe { libc::getuid() }, unsafe {
            libc::getgid()
        }),
        "--mount",
        &format!("type=bind,src={},dst=/workspace", workspace.display()),
        "--entrypoint=/bin/control-room-mcp",
        &image,
    ]);
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
        requests.push(json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"general","message":format!("isolated-probe-{i}"),"delivery":"record"}}}));
        requests.push(
            json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":i}}),
        );
    }
    let expected = requests
        .iter()
        .filter_map(|r| r.get("id").cloned())
        .collect::<Vec<_>>();
    for request in requests {
        writeln!(input, "{request}").unwrap();
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
    while Instant::now() < deadline {
        pump(&shared, &run, &workspace, false);
        while let Ok(line) = replies.try_recv() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            assert!(value.get("error").is_none(), "{value}");
            observed.push(value["id"].clone());
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
        assert!(value.get("error").is_none(), "{value}");
        observed.push(value["id"].clone());
    }
    assert!(
        status.success(),
        "MCP bridge did not complete; replies: {observed:?}"
    );
    assert_eq!(observed, expected);
    assert_eq!(shared.lock().unwrap().collective.posts.len(), 20);
}
#[test]
fn ordinary_agents_receive_the_shared_continuity_mcp_without_bot_authority() {
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
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
        for name in [
            "control_turn_context",
            "control_turn_handoff",
            "control_room_read",
        ] {
            assert!(
                tools.iter().any(|t| t["name"] == name),
                "{backend:?}: {name}"
            );
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
            denied["error"]["message"]
                .as_str()
                .unwrap()
                .contains("handoff is staged"),
            "{name}"
        );
    }
    assert_eq!(
        serde_json::to_value(&*shared.lock().unwrap()).unwrap(),
        before
    );
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
    let input =
        crate::design::parse_input(envelope["request"]["params"]["arguments"].clone()).unwrap();
    let admitted = crate::design::tests::ask_without_io(
        &mut state,
        &id,
        envelope["nonce"].as_str().unwrap(),
        input,
    )
    .unwrap();
    let shared = Arc::new(Mutex::new(state));
    let before = serde_json::to_vec(&*shared.lock().unwrap()).unwrap();
    for (field, value, expected) in [
        ("question", json!(null), "question must be a string"),
        ("question", json!(" "), "question must contain"),
        ("options", json!("A or B"), "options must be an array"),
        ("options", json!(["Only one"]), "options must contain"),
        (
            "options",
            json!(["Same", " Same "]),
            "options must be distinct",
        ),
        (
            "options",
            json!(["A", "x".repeat(201)]),
            "Each option must contain",
        ),
        (
            "recommended",
            json!(3),
            "recommended must be a zero-based index",
        ),
        (
            "recommended",
            json!(-1),
            "recommended must be a nonnegative",
        ),
        (
            "recommended",
            json!("first"),
            "recommended must be a nonnegative",
        ),
    ] {
        let mut malformed = envelope.clone();
        malformed["request"]["params"]["arguments"][field] = value;
        let response = rpc_at(&shared, &id, &malformed, None);
        let message = response["error"]["message"].as_str().unwrap();
        assert!(message.starts_with(expected), "{message}");
        assert!(!message.contains("identity"));
        assert_eq!(
            serde_json::to_vec(&*shared.lock().unwrap()).unwrap(),
            before
        );
    }
    for _ in 0..2 {
        let replay = rpc_at(&shared, &id, &envelope, None);
        assert_eq!(
            serde_json::from_str::<Value>(replay["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap(),
            admitted
        );
    }
    let mut unknown = envelope.clone();
    unknown["request"]["params"]["arguments"]["extra"] = json!("do-not-echo-this-value");
    let response = rpc_at(&shared, &id, &unknown, None);
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("an object containing only"));
    assert!(!message.contains("do-not-echo-this-value"));
    for nonce in ["", "unsupported_identity", &"a".repeat(129)] {
        let mut bad_identity = envelope.clone();
        bad_identity["nonce"] = json!(nonce);
        assert_eq!(
            rpc_at(&shared, &id, &bad_identity, None)["error"]["message"],
            "Invalid design request identity; retry the tool call."
        );
    }
    let mut different = envelope.clone();
    different["request"]["params"]["arguments"]["question"] = json!("A different valid question?");
    assert_eq!(
        rpc_at(&shared, &id, &different, None)["error"]["message"],
        "This attempt already has a design question."
    );
    let write = json!({"nonce":"1793455807009221658-6","request":{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"control_workspace_start","arguments":{"command":"true"}}}});
    assert_eq!(
        rpc_at(&shared, &id, &write, None)["error"]["message"],
        "This attempt is pausing for a design answer; no more tools may run."
    );
    assert_eq!(
        serde_json::to_vec(&*shared.lock().unwrap()).unwrap(),
        before
    );
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
    });
    let shared = Arc::new(Mutex::new(s));
    let listed = rpc_at(
        &shared,
        &id,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
        None,
    );
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect::<Vec<_>>();
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
        "control_knowledge_propose",
    ] {
        assert!(!names.contains(&name));
        let denied = rpc_at(
            &shared,
            &id,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{}}}}),
            None,
        );
        assert!(
            denied["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Judge policy")
        );
    }
    assert_eq!(shared.lock().unwrap().runtime.runs.len(), 1);
}
#[test]
fn every_advertised_worker_tool_has_ui_registration() {
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
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
                let entry = control_context::tool_for_mcp(name)
                    .unwrap_or_else(|| panic!("Missing worker tool in UI: {name}"));
                assert!(
                    crate::tool_catalog::render(&shared.lock().unwrap(), name)
                        .contains(entry.title)
                );
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
            model:None,
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
            response["error"]["message"]
                .as_str()
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
    assert!(
        response
            .unwrap_err()
            .to_string()
            .contains("pausing for delegated")
    );
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
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pausing")
    );
    state.lock().unwrap().runtime.runs[0].attempts[0].workspace_id =
        Some("retry-generation".into());
    assert!(!crate::design::requested(Some((&state, &id))));
}
fn rpc(state: &Arc<Mutex<State>>, run: &str, value: &Value) -> Value {
    rpc_at(state, run, value, None)
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
        response["error"]["message"],
        "Mailbox belongs to an inactive attempt"
    );
    assert!(state.lock().unwrap().collective.proposals.is_empty());
    for (name, args) in [
        ("control_room_post", json!({"message":"late effect"})),
        ("control_room_watch", json!({"enabled":true,"wake":true})),
        ("control_room_ack", json!({"through":0})),
    ] {
        let request = json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}});
        let response = rpc_at(&state, &id, &request, Some(&stale));
        assert_eq!(
            response["error"]["message"],
            "Mailbox belongs to an inactive attempt"
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
    assert!(rpc(&state, &id, &request).get("error").is_some());
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
    assert!(rpc(&state, &id, &forged).get("error").is_some());
    assert!(rpc(&state, &id, &request).get("error").is_some()); // no captured attempt
}
#[test]
fn workers_get_concrete_tools_without_operator_authority_or_room_access() {
    let mut state = State::default();
    state
        .runtime
        .enqueue("Test tools".into(), RunKind::Task, "test");
    let run = state.runtime.runs[0].id.clone();
    state.runtime.runs[0].status = "running".into();
    let state = Arc::new(Mutex::new(state));
    let list = rpc(
        &state,
        &run,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
    );
    let names: std::collections::BTreeSet<_> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
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
        response["error"]["message"],
        "Expected a delegation objective and role"
    );
    let retired = rpc(&state, &run, &request("control_agent_delegate"));
    assert!(retired.get("error").is_some());
    assert_ne!(retired["error"]["message"], response["error"]["message"]);
    assert!(
        !names.contains("control_github_open_pr"),
        "Ordinary tasks have no publication grant"
    );
    let forbidden = rpc(
        &state,
        &run,
        &json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.chat.send"}}}),
    );
    assert!(forbidden.get("error").is_some());
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
    let calls: i64 = std::env::var("CONTROL_QA_CALLS").ok().and_then(|n| n.parse().ok()).unwrap_or(2000);
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
        "run", "--rm", "--interactive", "--pull=never", "--name", &name, "--network=none",
        "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--memory=128m",
        "--cpus=1", "--pids-limit=32", "--user",
        &format!("{}:{}", unsafe { libc::getuid() }, unsafe { libc::getgid() }),
        "--mount", &format!("type=bind,src={},dst=/workspace", workspace.display()),
    ]);
    // A locally built bridge can be tested before it is baked into the image.
    if let Ok(bridge) = std::env::var("CONTROL_QA_BRIDGE") {
        command.args(["--mount", &format!("type=bind,src={bridge},dst=/opt/control-room-mcp,readonly"), "--entrypoint=/opt/control-room-mcp"]);
    } else {
        command.arg("--entrypoint=/bin/control-room-mcp");
    }
    command.arg(&image);
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
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
        writeln!(input, "{}", json!({"jsonrpc":"2.0","method":"notifications/initialized"})).unwrap();
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
    assert!(alive || status.success(), "bridge exited early after {observed} replies: {status:?} {errors}");
    assert_eq!(observed, calls + 1, "stderr: {errors}");
}
