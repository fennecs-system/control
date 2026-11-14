//! Credential-free worker tools are authorized against the active run on the host.
use crate::{identity, rooms, State};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn rpc_at(
    shared:&Arc<Mutex<State>>,run_id:&str,envelope:&Value,workspace:Option<&Path>,
)->Value {
    let mut response=dispatch_at(shared,run_id,envelope,workspace);
    if envelope["request"]["method"]=="tools/call"
        && !matches!(envelope["request"]["params"]["name"].as_str(),Some("control_turn_context"|"control_turn_handoff"))
        && let Some(content)=response["result"]["content"].as_array_mut()
        && let Ok(mut state)=shared.lock()
        && crate::turn_context::callback_notice(&mut state,run_id)
    {
        content.push(json!({"type":"text","text":"Use control_turn_context at the next action boundary for new callbacks or remaining attempt time. Keep this session active while useful work remains; hand off when waiting or done, or before an explicitly reported deadline."}));
    }
    response
}
fn dispatch_at(
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
    let claude = run.backend == crate::backend::Backend::Claude;
    let judge = crate::delegation::is_judge(run);
    // Enabling a bot affects the next admission, not the protocol of an
    // already-running provider session whose input was captured earlier.
    let managed=crate::turn_context::managed(&state,run)
        && run.attempts.last().is_some_and(|attempt|attempt.turn_mailbox.is_some());
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({"protocolVersion":control_context::tool_policy::MCP_LATEST,"capabilities":{"tools":{}},"serverInfo":{"name":"control-worker","version":"0.1"}})
        }
        "ping" => json!({}),
        "tools/list" => {
            let mut tools = crate::tool_registry::worker_definitions(run.action == identity::Action::Improve);
            if !managed {
                tools.retain(|tool|!matches!(tool["name"].as_str(),Some("control_turn_context"|"control_turn_handoff")));
            }
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
            let response = rooms::worker_rpc(&mut state, run_id, envelope);
            if let Some(room_tools) = response["result"]["tools"].as_array() {
                tools.extend(room_tools.clone());
            }
            if managed {tools.retain(|tool|!matches!(tool["name"].as_str(),Some("control_room_ack"|"control_room_notifications")));}
            json!({"tools":tools})
        }
        "tools/call" => {
            if run.attempts.last().and_then(|a|a.turn_mailbox.as_ref()).and_then(|m|m.accepted()).is_some()
                && !control_context::tool_policy::after_handoff(request["params"]["name"].as_str().unwrap_or("")) {
                return error("A handoff is staged; only observations and handoff corrections may run before this attempt finishes.");
            }
            if matches!(request["params"]["name"].as_str(),Some("control_room_ack"|"control_room_notifications")) && managed {
                return error("Managed sessions acknowledge delivered callback receipts through control_turn_handoff after processing; use control_turn_context with source=room.");
            }
            if judge && !matches!(request["params"]["name"].as_str(),Some("control_workspace_run"|"control_workspace_start"|"control_workspace_status"|"control_workspace_cancel"|"control_web_search"|"control_rust_run"|"control_rust_bench"|"control_room_list"|"control_room_read"|"control_room_post"|"control_agent_result"|"control_knowledge_read"|"control_knowledge_search")) {
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
            } else if ["control_room_list", "control_room_read", "control_room_post", "control_room_watch", "control_room_notifications", "control_room_ack", "control_room_claim"].contains(&name) {
                return rooms::checkpoint(&mut state, run_id, envelope, crate::storage::save);
            } else {
                return error("Tool is not allowed for this worker");
            }
        }
        _ => return error("Unsupported worker method"),
    };
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn pump(state: &Arc<Mutex<State>>, run: &str, workspace: &Path, managed:bool) {
    pump_at(state, run, workspace, workspace,managed);
}
pub fn pump_at(state: &Arc<Mutex<State>>, run: &str, mailbox: &Path, workspace: &Path, managed:bool) {
    let path = mailbox.join(".control-room-request");
    let Ok(envelope) = rooms::read_request(&path) else {
        return;
    };
    let response = if managed {rpc_at(state, run, &envelope, Some(workspace))}else{dispatch_at(state,run,&envelope,Some(workspace))};
    rooms::reply(mailbox, &response);
}

#[cfg(test)]
mod tests;
