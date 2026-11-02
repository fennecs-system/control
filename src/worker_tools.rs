//! Credential-free worker tools are authorized against the active run on the host.
use crate::{identity, rooms, State};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn rpc_at(
    shared: &Arc<Mutex<State>>,
    run_id: &str,
    envelope: &Value,
    workspace: Option<&Path>,
) -> Value {
    let request = &envelope["request"];
    let Some(id) = request.get("id") else {
        return Value::Null;
    };
    let error =
        |message: &str| json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":message}});
    if crate::shutdown::requested() { return error("Control is stopping; no new tools may run."); }
    if request["jsonrpc"] != "2.0" {
        return error("Invalid JSON-RPC request");
    }
    let Ok(mut state) = shared.lock() else {
        return error(
            "State unavailable; tools are disabled until Control is checked and restarted",
        );
    };
    if crate::shutdown::requested() { return error("Control is stopping; no new tools may run."); }
    let Some(run) = state
        .runtime
        .runs
        .iter()
        .find(|r| r.id == run_id && r.status == "running")
    else {
        return error("Run is not active");
    };
    if let Err(message) = identity::authorize(&run.actor, run.action) {
        return error(message);
    }
    // Every request is fenced before dispatch, including room writes and search.
    // A late mailbox from a previous attempt cannot acquire the retry's authority.
    if let Some(workspace) = workspace {
        let expected = run
            .attempts
            .last()
            .and_then(|a| a.workspace_id.as_deref())
            .and_then(|id| crate::runtime::workspace(id).ok())
            .and_then(|p| p.canonicalize().ok());
        if expected.as_deref() != Some(workspace) {
            return error("Mailbox belongs to an inactive attempt");
        }
    }
    let has_room = state.chats.iter().any(|c| {
        Some(&c.id) == run.chat_id.as_ref() && c.room.as_deref().is_some_and(rooms::valid)
    });
    let claude = run.backend == crate::backend::Backend::Claude;
    let judge = crate::delegation::is_judge(run);
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-worker","version":"0.1"}})
        }
        "ping" => json!({}),
        "tools/list" => {
            let mut tools = crate::tool_registry::worker_definitions(run.action == identity::Action::Improve);
            if judge {
                tools.retain(|tool|matches!(tool["name"].as_str(),Some("control.web.search"|"control.rust.run"|"control.rust.bench"|"control_agent_result"|"control_workspace_start"|"control_workspace_status"|"control_workspace_cancel"|"control_knowledge_read"|"control_knowledge_search")));
            }
            if claude {
                tools.push(crate::claude::definition());
                if judge { let tool=tools.last_mut().unwrap();
                    tool["description"]=json!("Inspect source and run verification in a credential-free offline container. /workspace is read-only; use /tmp for scratch outputs. Bounded output and 25-second deadline.");
                    tool["annotations"]["destructiveHint"]=json!(false);
                }
            }
            for tool in &mut tools {
                tool["name"] = json!(match tool["name"].as_str().unwrap() {
                    "control.web.search" => "control_web_search",
                    "control.rust.run" => "control_rust_run",
                    "control.rust.bench" => "control_rust_bench",
                    other => other,
                });
            }
            if has_room {
                let response = rooms::worker_rpc(&mut state, run_id, envelope);
                if let Some(room_tools) = response["result"]["tools"].as_array() {
                    tools.extend(room_tools.clone());
                }
            }
            json!({"tools":tools})
        }
        "tools/call" => {
            if judge && !matches!(request["params"]["name"].as_str(),Some("control_workspace_run"|"control_workspace_start"|"control_workspace_status"|"control_workspace_cancel"|"control_web_search"|"control_rust_run"|"control_rust_bench"|"control_room_read"|"control_room_post"|"control_agent_result"|"control_knowledge_read"|"control_knowledge_search")) {
                return error("Judge policy permits read-only source inspection, isolated verification and room collaboration only.");
            }
            let blocking_spawn=request["params"]["name"]=="control_agent_spawn_blocking";
            if crate::delegation::pausing(run) && !blocking_spawn {
                return error("This attempt is pausing for delegated work; no more tools may run.");
            }
            if matches!(request["params"]["name"].as_str(),Some("control_agent_spawn_status"|"control_agent_spawn_cancel")) {
                if run.question.as_ref().is_some_and(|q|q.answer.is_none()) {return error("This attempt is pausing for a design answer.");}
                return match crate::spawning::manage(&mut state,run_id,request["params"]["name"].as_str().unwrap(),&request["params"]["arguments"],crate::storage::save) {
                    Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(Ok(value))}),
                    Err(message)=>error(message),
                };
            }
            if request["params"]["name"]=="control_agent_spawn" {
                let Some(generation)=run.attempts.last().and_then(|a|a.workspace_id.clone()) else {return error("Missing attempt identity");};
                let Some(nonce)=envelope["nonce"].as_str() else {return error("Missing request identity");};
                let input=match serde_json::from_value(request["params"]["arguments"].clone()) {Ok(input)=>input,Err(_)=>return error("Expected a bounded list of agent objectives")};
                return match crate::spawning::request(&mut state,run_id,&generation,nonce,input,crate::storage::save) {
                    Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(Ok(value))}),
                    Err(message)=>error(message),
                };
            }
            if blocking_spawn {
                let Some(nonce)=envelope["nonce"].as_str() else {return error("Missing request identity");};
                let Ok(input)=serde_json::from_value(request["params"]["arguments"].clone()) else{return error("Expected a delegation objective and role");};
                return match crate::delegation::request(&mut state,run_id,nonce,input) {
                    Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}}),
                    Err(message)=>error(message),
                };
            }
            if request["params"]["name"]!="control_design_ask" && run.question.as_ref().is_some_and(|q|q.answer.is_none()&&run.attempts.last().is_some_and(|a|a.workspace_id.as_deref()==Some(&q.generation))) {return error("This attempt is pausing for a design answer; no more tools may run.");}
            if crate::knowledge_tools::known(request["params"]["name"].as_str().unwrap_or("")) {
                return match crate::knowledge_tools::worker(&mut state,run_id,request["params"]["name"].as_str().unwrap(),&request["params"]["arguments"],envelope["nonce"].as_str().unwrap_or(""),crate::storage::save) {
                    Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"structuredContent":{"data":value},"isError":false}}),
                    Err(message)=>error(message),
                };
            }
            if let Some(tools) = crate::tool_registry::attempt_tools(request["params"]["name"].as_str().unwrap_or("")) {
                let generation=run.attempts.last().and_then(|a|a.workspace_id.clone()).unwrap_or_default();
                let nonce=envelope["nonce"].as_str().unwrap_or("").to_owned();
                drop(state);
                let host=crate::tool_registry::LocalHost(shared);
                let cx=crate::tool_registry::AttemptCall{attempt:crate::tool_registry::AttemptRef{run:run_id,generation:&generation,nonce:&nonce},host:&host};
                return json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(tools.call(&cx,request["params"]["name"].as_str().unwrap(),&request["params"]["arguments"]))});
            }
            if request["params"]["name"]=="control_github_open_pr" {
                if run.action != identity::Action::Improve {return error("GitHub publication requires an improvement run");}
                let chat=run.chat_id.clone().unwrap_or_default();
                let input=serde_json::from_value::<crate::github::Input>(request["params"]["arguments"].clone());
                drop(state);
                return match input.map_err(|_|std::io::Error::other("Expected run_id, title and optional body")).and_then(|input|crate::github::open(shared,&chat,input)) {
                    Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}}),
                    Err(message)=>error(&message.to_string()),
                };
            }
            if request["params"]["name"]=="control_preview_start" || request["params"]["name"]=="control_preview_stop" {
                let stopping=request["params"]["name"]=="control_preview_stop";
                let chat=run.chat_id.clone().unwrap_or_default();
                let preview_id=run.attempts.last().and_then(|a|a.workspace_id.as_deref()).map(|id|format!("control-{id}-preview")).unwrap_or_default();
                let input=serde_json::from_value::<crate::previews::Input>(request["params"]["arguments"].clone());
                drop(state);
                let result=if stopping {crate::previews::stop(shared,&chat,&preview_id).map(|_|json!({"stopped":true}))}
                    else {input.map_err(|_|std::io::Error::other("Expected a preview command and port")).and_then(|input|crate::previews::start(shared,run_id,input))};
                return match result {Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}}),Err(message)=>error(&message.to_string())};
            }
            if request["params"]["name"]=="control_project_request" {
                let chat=run.chat_id.clone().unwrap_or_default();
                let Some(index)=state.chats.iter().position(|c|c.id==chat) else {return error("Chat not found");};
                let previous=state.chats[index].clone();
                let result=crate::projects::request_named(&mut state,&chat,request["params"]["arguments"]["path"].as_str().unwrap_or(""),request["params"]["arguments"]["name"].as_str());
                return match result {
                    Ok(value) if crate::storage::save(&state).is_ok()=>json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}}),
                    Ok(_)=>{state.chats[index]=previous;error("Could not save project request")},
                    Err(message)=>error(message),
                };
            }
            if request["params"]["name"] == "control_design_ask" {
                let Some(nonce) = envelope["nonce"].as_str() else { return error("Missing request identity"); };
                let input = match crate::design::parse_input(request["params"]["arguments"].clone()) {
                    Ok(input) => input,
                    Err(message) => return error(message),
                };
                return match crate::design::ask(&mut state, run_id, nonce, input) {
                    Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}}),
                    Err(message) => error(message),
                };
            }
            if request["params"]["name"] == "control_agent_result" {
                let plan=match crate::delegated_results::prepare(&state,run_id,request["params"]["arguments"].clone()) {
                    Ok(plan)=>plan,Err(message)=>return error(message),
                };
                drop(state);
                return match crate::delegated_results::execute(plan) {
                    Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"isError":false}}),
                    Err(message)=>error(message),
                };
            }
            if request["params"]["name"] == "control_workspace_run" {
                if !claude {
                    return error("Workspace tool is not enabled for this backend");
                }
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    command: String,
                }
                let Ok(input) =
                    serde_json::from_value::<Input>(request["params"]["arguments"].clone())
                else {
                    return error("Expected a command");
                };
                let Some(attempt) = run.attempts.last() else {
                    return error("No active attempt");
                };
                if attempt.command_jobs.iter().any(|job|["preparing","running","stopping"].contains(&job.status.as_str())) {
                    return error("An asynchronous workspace command owns this attempt; await or cancel it before running another command");
                }
                let (Some(image), Some(node), Some(workspace)) =
                    (&attempt.image, &attempt.node_id, workspace)
                else {
                    return error("Attempt workspace is unavailable");
                };
                let image = image.clone();
                let review_only = attempt.review_only;
                let node = node.clone();
                let generation = attempt.workspace_id.clone().unwrap_or_default();
                let name = format!(
                    "control-{}-tool",
                    attempt.workspace_id.as_deref().unwrap_or(run_id)
                );
                let Some(expected) = attempt
                    .workspace_id
                    .as_deref()
                    .and_then(|id| crate::runtime::workspace(id).ok())
                else {
                    return error("No captured workspace identity");
                };
                if expected.canonicalize().ok().as_deref() != Some(workspace) {
                    return error("Workspace does not match the active attempt");
                }
                let Some(nonce) = envelope["nonce"].as_str().filter(|s| {
                    !s.is_empty()
                        && s.len() <= 128
                        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }) else {
                    return error("Invalid tool request nonce");
                };
                // Persist intent outside both model-visible mounts before the command.
                // A retry after an ambiguous acknowledgement never repeats the effect.
                // The fsync runs without the state lock; this mailbox is served serially,
                // and the run is re-fenced below before anything executes.
                drop(state);
                if crate::claude::claim(workspace, nonce).is_err() {
                    return error("Workspace command already admitted or intent could not be saved; inspect the attempt before retrying");
                }
                let Ok(mut state) = shared.lock() else {
                    return error("State unavailable; tools are disabled until Control is checked and restarted");
                };
                // Records only while the same attempt is still running (cancellation fences here).
                if !crate::worker::record_activity_for(
                    &mut state,
                    run_id,
                    &generation,
                    crate::usage::Activity {
                        id: envelope["nonce"]
                            .as_str()
                            .unwrap_or("workspace-command")
                            .into(),
                        kind: "command_execution".into(),
                        text: input.command.chars().take(2000).collect(),
                        output: String::new(),
                        status: "running".into(),
                    },
                ) {
                    return error("Run is not active");
                }
                drop(state);
                let result = crate::claude::shell_scoped(&image, &name, &node, workspace, &input.command, review_only);
                if !review_only {crate::worker::observe_workspace_diff(shared,run_id,&generation,workspace,"observed");}
                if let Ok(mut state) = shared.lock() {
                    crate::worker::record_activity_for(
                        &mut state,
                        run_id,
                        &generation,
                        crate::usage::Activity {
                            id: envelope["nonce"]
                                .as_str()
                                .unwrap_or("workspace-command")
                                .into(),
                            kind: "command_execution".into(),
                            text: input.command.chars().take(2000).collect(),
                            output: result
                                .as_ref()
                                .ok()
                                .map(|v| v.to_string())
                                .unwrap_or_default()
                                .chars()
                                .take(8000)
                                .collect(),
                            status: if result.as_ref().is_ok_and(|v| v["exit_code"] == 0) {
                                "completed"
                            } else {
                                "failed"
                            }
                            .into(),
                        },
                    );
                }
                return json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(result)});
            }
            let name = match request["params"]["name"].as_str().unwrap_or("") {
                "control_web_search" => "control.web.search",
                "control_rust_run" => "control.rust.run",
                "control_rust_bench" => "control.rust.bench",
                other => other,
            };
            if crate::toolbox::known(name) {
                drop(state);
                // Network requests and compilation must not hold the control-plane state lock.
                crate::toolbox::result(crate::toolbox::call(name, &request["params"]["arguments"]))
            } else if name == "control_web_fetch" {
                drop(state);
                crate::toolbox::result(crate::web_access::worker(shared, run_id, &request["params"]["arguments"]))
            } else if name == "control_browser_capture" {
                drop(state);
                crate::web_access::capture_tool(&request["params"]["arguments"]).unwrap_or_else(|error|crate::toolbox::result(Err(error)))
            } else if has_room && ["control_room_read", "control_room_post", "control_room_watch", "control_room_notifications", "control_room_ack"].contains(&name) {
                return rooms::checkpoint(&mut state, run_id, envelope, crate::storage::save);
            } else {
                return error("Tool is not allowed for this worker");
            }
        }
        _ => return error("Unsupported worker method"),
    };
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn pump(state: &Arc<Mutex<State>>, run: &str, workspace: &Path) {
    pump_at(state, run, workspace, workspace);
}
pub fn pump_at(state: &Arc<Mutex<State>>, run: &str, mailbox: &Path, workspace: &Path) {
    let path = mailbox.join(".control-room-request");
    let Ok(envelope) = rooms::read_request(&path) else {
        return;
    };
    let response = rpc_at(state, run, &envelope, Some(workspace));
    rooms::reply(mailbox, &response);
}

#[cfg(test)]
mod tests {
    use crate::runtime::RunKind;
    use super::*;
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
        let id=state.runtime.runs[0].id.clone();
        let envelope=design_transport_envelope();
        let input=crate::design::parse_input(envelope["request"]["params"]["arguments"].clone()).unwrap();
        let admitted=crate::design::tests::ask_without_io(&mut state,&id,envelope["nonce"].as_str().unwrap(),input).unwrap();
        let shared=Arc::new(Mutex::new(state));
        let before=serde_json::to_vec(&*shared.lock().unwrap()).unwrap();
        for (field,value,expected) in [
            ("question",json!(null),"question must be a string"),
            ("question",json!(" "),"question must contain"),
            ("options",json!("A or B"),"options must be an array"),
            ("options",json!(["Only one"]),"options must contain"),
            ("options",json!(["Same"," Same "]),"options must be distinct"),
            ("options",json!(["A","x".repeat(201)]),"Each option must contain"),
            ("recommended",json!(3),"recommended must be a zero-based index"),
            ("recommended",json!(-1),"recommended must be a nonnegative"),
            ("recommended",json!("first"),"recommended must be a nonnegative"),
        ] {
            let mut malformed=envelope.clone();malformed["request"]["params"]["arguments"][field]=value;
            let response=rpc_at(&shared,&id,&malformed,None);
            let message=response["error"]["message"].as_str().unwrap();
            assert!(message.starts_with(expected),"{message}");assert!(!message.contains("identity"));
            assert_eq!(serde_json::to_vec(&*shared.lock().unwrap()).unwrap(),before);
        }
        for _ in 0..2 {
            let replay=rpc_at(&shared,&id,&envelope,None);
            assert_eq!(serde_json::from_str::<Value>(replay["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),admitted);
        }
        let mut unknown=envelope.clone();unknown["request"]["params"]["arguments"]["extra"]=json!("do-not-echo-this-value");
        let response=rpc_at(&shared,&id,&unknown,None);
        let message=response["error"]["message"].as_str().unwrap();
        assert!(message.contains("an object containing only"));assert!(!message.contains("do-not-echo-this-value"));
        for nonce in ["", "unsupported_identity", &"a".repeat(129)] {
            let mut bad_identity=envelope.clone();bad_identity["nonce"]=json!(nonce);
            assert_eq!(rpc_at(&shared,&id,&bad_identity,None)["error"]["message"],"Invalid design request identity; retry the tool call.");
        }
        let mut different=envelope.clone();different["request"]["params"]["arguments"]["question"]=json!("A different valid question?");
        assert_eq!(rpc_at(&shared,&id,&different,None)["error"]["message"],"This attempt already has a design question.");
        let write=json!({"nonce":"1793455807009221658-6","request":{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"control_workspace_start","arguments":{"command":"true"}}}});
        assert_eq!(rpc_at(&shared,&id,&write,None)["error"]["message"],"This attempt is pausing for a design answer; no more tools may run.");
        assert_eq!(serde_json::to_vec(&*shared.lock().unwrap()).unwrap(),before);
    }
    #[test]
    fn completed_child_summary_is_available_through_the_worker_broker() {
        let shared=Arc::new(Mutex::new(crate::delegated_results::tests::fixture()));
        let response=rpc_at(&shared,"resumed",&json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_agent_result","arguments":{"child":"child","section":"summary","limit":10}}}}),None);
        let text=response["result"]["content"][0]["text"].as_str().unwrap();
        let value:Value=serde_json::from_str(text).unwrap();
        assert_eq!(value["summary"]["text"],"All reques");
        assert_eq!(value["summary"]["next_offset"],10);
        assert_eq!(shared.lock().unwrap().runtime.runs.len(),3);
    }
    #[test]
    fn judge_tools_are_enforced_even_when_unadvertised_names_are_forged() {
        let mut s=State::default();let chat=crate::chat::create(&mut s);
        crate::chat::send(&mut s,&chat,"Review this","test").unwrap();
        let run=&mut s.runtime.runs[0];let id=run.id.clone();run.status="running".into();run.backend=crate::backend::Backend::Claude;
        run.persona=Some(crate::delegation::Persona{role:crate::delegation::Role::Judge,root:"root".into(),depth:1,parent:Some("parent".into())});
        let shared=Arc::new(Mutex::new(s));
        let listed=rpc_at(&shared,&id,&json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),None);
        let names=listed["result"]["tools"].as_array().unwrap().iter().map(|t|t["name"].as_str().unwrap()).collect::<Vec<_>>();
        assert!(names.contains(&"control_workspace_run"));assert!(!names.contains(&"control_agent_spawn_blocking"));assert!(!names.contains(&"control_agent_delegate"));assert!(names.contains(&"control_agent_result"));
        assert!(names.contains(&"control_knowledge_read"));assert!(names.contains(&"control_knowledge_search"));
        for name in ["control_agent_spawn_blocking","control_agent_delegate","control_agent_spawn","control_agent_message","control_project_request","control_design_ask","control_preview_start","control_preview_stop","control_github_pr","control_service_start","control_service_attach","control_service_status","control_service_stop","control_service_exec","control_service_exec_status","control_knowledge_propose"] {
            assert!(!names.contains(&name));
            let denied=rpc_at(&shared,&id,&json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{}}}}),None);
            assert!(denied["error"]["message"].as_str().unwrap().contains("Judge policy"));
        }
        assert_eq!(shared.lock().unwrap().runtime.runs.len(),1);
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
                let response = rpc_at(&shared, &id, &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}), None);
                for definition in response["result"]["tools"].as_array().unwrap() {
                    let name = definition["name"].as_str().unwrap();
                    let entry = control_context::tool_for_mcp(name).unwrap_or_else(|| panic!("Missing worker tool in UI: {name}"));
                    assert!(crate::tool_catalog::render(&shared.lock().unwrap(), name).contains(entry.title));
                }
            }
        }
    }
    #[test]
    fn delegated_pause_fences_command_writers_and_github_before_dispatch() {
        let mut state=State::default();let chat=crate::chat::create(&mut state);
        crate::chat::send(&mut state,&chat,"/improve Check a fixture","test").unwrap();
        let run=&mut state.runtime.runs[0];run.status="running".into();
        run.attempts.push(crate::usage::Attempt{workspace_id:Some("pause-generation".into()),..Default::default()});
        run.delegation=Some(crate::delegation::Delegation{input:crate::delegation::Input{objective:"Inspect".into(),role:crate::delegation::Role::Worker},nonce:"delegate-fixture".into(),generation:"pause-generation".into(),backend:run.backend,model:"fixture".into(),child:None,continuation:None});
        let id=run.id.clone();let shared=Arc::new(Mutex::new(state));
        for (name,args) in [("control_workspace_start",json!({"command":"true"})),("control_github_open_pr",json!({"run_id":"r-other","title":"Must not publish"}))] {
            let response=rpc(&shared,&id,&json!({"nonce":"request-fixture","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}}));
            assert!(response["error"]["message"].as_str().unwrap().contains("pausing for delegated"));
        }
        let host=crate::tool_registry::LocalHost(&shared);
        let cx=crate::tool_registry::AttemptCall{attempt:crate::tool_registry::AttemptRef{run:&id,generation:"pause-generation",nonce:"request-fixture"},host:&host};
        let response=crate::tool_registry::attempt_tools("control_workspace_start").unwrap().call(&cx,"control_workspace_start",&json!({"command":"true"}));
        assert!(response.unwrap_err().to_string().contains("pausing for delegated"));
        let state=shared.lock().unwrap();assert!(state.runtime.runs[0].attempts[0].command_jobs.is_empty());assert!(state.github_publications.is_empty());
    }
    #[test]
    fn design_pause_blocks_further_tool_effects_until_user_answers() {
        let (mut state,_) = crate::design::tests::paused();
        state.runtime.runs[0].status="running".into();
        let id = state.runtime.runs[0].id.clone();
        let state = Arc::new(Mutex::new(state));
        assert!(crate::design::requested(Some((&state,&id))));
        let response = rpc(&state,&id,&json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_rust_run","arguments":{"code":"fn main() {}"}}}}));
        assert!(response["error"]["message"].as_str().unwrap().contains("pausing"));
        state.lock().unwrap().runtime.runs[0].attempts[0].workspace_id=Some("retry-generation".into());
        assert!(!crate::design::requested(Some((&state,&id))));
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
        let request=json!({"nonce":"late-proposal","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_knowledge_propose","arguments":{"claim":"Must not persist","evidence":"stale mailbox","scope":"fixture","validity":"always"}}}});
        let response=rpc_at(&state,&id,&request,Some(&stale));
        assert_eq!(response["error"]["message"],"Mailbox belongs to an inactive attempt");
        assert!(state.lock().unwrap().collective.proposals.is_empty());
        for (name, args) in [("control_room_post",json!({"message":"late effect"})),("control_room_watch",json!({"enabled":true,"wake":true})),("control_room_ack",json!({"through":0}))] {
            let request = json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}});
            let response = rpc_at(&state, &id, &request, Some(&stale));
            assert_eq!(response["error"]["message"], "Mailbox belongs to an inactive attempt");
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
        assert!(tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control_workspace_run"));
        let mut forged = request.clone();
        forged["request"]["params"]["arguments"]["mount"] =
            json!("/Users/operator/.config/control");
        assert!(rpc(&state, &id, &forged).get("error").is_some());
        assert!(rpc(&state, &id, &request).get("error").is_some()); // no captured attempt
    }
    #[test]
    fn workers_get_concrete_tools_without_operator_authority_or_room_access() {
        let mut state = State::default();
        state.runtime.enqueue("Test tools".into(), RunKind::Task, "test");
        let run = state.runtime.runs[0].id.clone();
        state.runtime.runs[0].status = "running".into();
        let state = Arc::new(Mutex::new(state));
        let list = rpc(
            &state,
            &run,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
        );
        let names:std::collections::BTreeSet<_>=list["result"]["tools"].as_array().unwrap().iter().filter_map(|t|t["name"].as_str()).collect();
        for name in ["control_agent_spawn_blocking","control_agent_result","control_workspace_start","control_project_request"] {assert!(names.contains(name));}
        assert!(!names.contains("control_agent_delegate"));
        let request=|name|json!({"nonce":"1-1","request":{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":{"unexpected":true}}}});
        let response=rpc(&state,&run,&request("control_agent_spawn_blocking"));
        assert_eq!(response["error"]["message"],"Expected a delegation objective and role");
        let retired=rpc(&state,&run,&request("control_agent_delegate"));
        assert!(retired.get("error").is_some());
        assert_ne!(retired["error"]["message"],response["error"]["message"]);
        assert!(!names.contains("control_github_open_pr"),"Ordinary tasks have no publication grant");
        let forbidden = rpc(
            &state,
            &run,
            &json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.chat.send"}}}),
        );
        assert!(forbidden.get("error").is_some());
        state.lock().unwrap().runtime.runs[0].status = "completed".into();
        assert!(rpc(
            &state,
            &run,
            &json!({"request":{"jsonrpc":"2.0","id":3,"method":"tools/list"}})
        )
        .get("error")
        .is_some());
    }
}
