//! Credential-free worker tools are authorized against the active run on the host.
use crate::{identity, rooms, State};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

mod compact;
pub(crate) mod deferred;
pub(crate) mod fixes;
pub(crate) mod nudges;

/// `control_call` becomes the direct call it names before any policy runs,
/// so both spellings share one authorization and dispatch path.
fn direct(envelope: &Value) -> Result<std::borrow::Cow<'_, Value>, Value> {
    match deferred::unwrap(envelope) {
        Ok(Some(rewritten)) => Ok(std::borrow::Cow::Owned(rewritten)),
        Ok(None) => Ok(std::borrow::Cow::Borrowed(envelope)),
        Err(message) => Err(crate::toolbox::tool_error(&envelope["request"]["id"], &message)),
    }
}
fn rpc_at(
    shared:&Arc<Mutex<State>>,run_id:&str,envelope:&Value,workspace:Option<&Path>,
)->Value {
    let envelope = match direct(envelope) { Ok(envelope) => envelope, Err(refused) => return refused };
    let envelope = envelope.as_ref();
    let mut response=dispatch_at(shared,run_id,envelope,workspace);
    let name=envelope["request"]["params"]["name"].as_str().unwrap_or("");
    // A refused call changes no state, so it neither counts nor carries notes.
    let refused=response["result"]["isError"]==true;
    // Every refusal from these families names a callable fix.
    if refused && envelope["request"]["method"]=="tools/call" && fixes::covered(name)
        && let Some(text)=response.get_mut("result").and_then(|result|result.get_mut("content"))
            .and_then(|content|content.get_mut(0)).and_then(|item|item.get_mut("text"))
        && let Some(message)=text.as_str()
    {
        *text=json!(fixes::actionable(name,message));
    }
    if envelope["request"]["method"]!="tools/call" {
        return response;
    }
    // get_mut, not IndexMut: indexing would insert "result":null into an
    // error reply, which clients reject as an invalid JSON-RPC response.
    let Some(result)=response.get_mut("result").filter(|result|result["content"].is_array()) else { return response };
    media_visible(result);
    if name!="control_turn_handoff" && let Ok(mut state)=shared.lock() {
        let mut notes=Vec::new();
        if name!="control_turn_context" && crate::turn_context::callback_notice(&mut state,run_id) {
            notes.push("New callbacks: read them with control_turn_context at the next action boundary.".to_owned());
        }
        if !refused {
            notes.extend(nudges::notes(&mut state,run_id,name,nudges::now()));
        }
        attach_notes(result,&notes);
    }
    response
}
/// Adds host `notes` to the tool `result` so every MCP client shows them to
/// its model. Clients that find `structuredContent` show only that JSON in
/// place of the text items (Codex 0.164 always; Claude Code 2.1 unless a
/// flag opts into content), so each note goes both into a text item and into
/// a `notes` array beside the structured data.
fn attach_notes(result: &mut Value, notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    if let Some(content) = result.get_mut("content").and_then(Value::as_array_mut) {
        content.extend(notes.iter().map(|note| json!({"type":"text","text":note})));
    }
    if let Some(structured) = result.get_mut("structuredContent").and_then(Value::as_object_mut) {
        let listed = structured.entry("notes").or_insert_with(|| json!([]));
        if let Some(listed) = listed.as_array_mut() {
            listed.extend(notes.iter().cloned().map(Value::String));
        }
    }
}
/// A result carrying an image or other non-text item leaves without
/// `structuredContent`. Codex 0.164 replaces the whole content with
/// `structuredContent` when it is present, so the image would never reach the
/// model; Claude Code keeps non-text items either way. The data stays in a
/// text item: the result's own (compacted) JSON text, or the structured JSON
/// when the result has no text item.
fn media_visible(result: &mut Value) {
    let Some(fields) = result.as_object_mut() else { return };
    let Some(content) = fields.get("content").and_then(Value::as_array) else { return };
    if !content.iter().any(|item| item["type"] != "text") || !fields.contains_key("structuredContent") {
        return;
    }
    let texted = content.iter().any(|item| item["type"] == "text");
    let structured = fields.remove("structuredContent").unwrap_or_default();
    if !texted && let Some(content) = fields.get_mut("content").and_then(Value::as_array_mut) {
        content.insert(0, json!({"type":"text","text":structured.to_string()}));
    }
}
/// Dispatches one request; a successful tool result leaves in the shared
/// compact shape ([`compact`]), whichever tool family produced it.
fn dispatch_at(shared: &Arc<Mutex<State>>, run_id: &str, envelope: &Value, workspace: Option<&Path>) -> Value {
    let mut response = dispatch_raw(shared, run_id, envelope, workspace);
    if let Ok(envelope) = direct(envelope) && envelope["request"]["method"] == "tools/call" {
        let params = &envelope["request"]["params"];
        compact::reply(params["name"].as_str().unwrap_or(""), &params["arguments"], &mut response);
    }
    response
}
fn dispatch_raw(
    shared: &Arc<Mutex<State>>,
    run_id: &str,
    envelope: &Value,
    workspace: Option<&Path>,
) -> Value {
    let envelope = match direct(envelope) { Ok(envelope) => envelope, Err(refused) => return refused };
    let envelope = envelope.as_ref();
    let request = &envelope["request"];
    let Some(id) = request.get("id") else {
        return Value::Null;
    };
    if let Some(fault) = crate::toolbox::request_fault(request) {
        return fault;
    }
    // Every tools/call failure past the protocol checks is a tool result.
    let call = request["method"] == "tools/call";
    let error = |message: &str| if call { crate::toolbox::tool_error(id, message) }
        else { crate::toolbox::protocol_error(id, -32602, message) };
    if crate::shutdown::requested() { return error("Control is stopping; no new tools may run."); }
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
    // Enabling a bot affects the next admission, not the protocol of an
    // already-running provider session whose input was captured earlier.
    let managed=crate::turn_context::managed(&state,run)
        && run.attempts.last().is_some_and(|attempt|attempt.turn_mailbox.is_some());
    let facts = deferred::Facts::of(&state, run, managed);
    // Backends without their own sandbox shell get Control's offline workspace tool.
    let (claude, judge, advisor) = (facts.shell, facts.judge, facts.advisor);
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({"protocolVersion":control_context::tool_policy::MCP_LATEST,"capabilities":{"tools":{}},"serverInfo":{"name":"control-worker","version":"0.1"}})
        }
        "ping" => json!({}),
        // The role's core schemas; the rest are in the prompt's deferred index.
        "tools/list" => json!({"tools":deferred::listed(&facts)}),
        "tools/call" => {
            if run.attempts.last().and_then(|a|a.turn_mailbox.as_ref()).and_then(|m|m.accepted()).is_some()
                && !control_context::tool_policy::after_handoff(request["params"]["name"].as_str().unwrap_or("")) {
                return error("A handoff is staged; only observations and handoff corrections may run before this attempt finishes. No further call is needed; to change it, call control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} again.");
            }
            if matches!(request["params"]["name"].as_str(),Some("control_room_ack"|"control_room_notifications")) && managed {
                return error("Managed sessions read room posts with control_turn_context {\"source\":\"room\"}; control_turn_handoff acknowledges them when you yield.");
            }
            // Pure read of the run's captured context, for every role including judges.
            if request["params"]["name"]=="control_guidance" {
                return json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(crate::guidance::answer(run,&request["params"]["arguments"]))});
            }
            // Schemas of the tools this session may call; a pure read for every role.
            if request["params"]["name"]==deferred::SEARCH {
                return json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(deferred::search(&facts,&request["params"]["arguments"]))});
            }
            if judge && !control_context::judge_may_call(request["params"]["name"].as_str().unwrap_or(""), advisor) {
                return error("Judge policy permits read-only source inspection, isolated verification and room collaboration only. Next: call control_tool_search {} to list the tools you may call.");
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
            // Worker spellings of toolbox tools resolve to their registered names.
            let name = request["params"]["name"].as_str().unwrap_or("");
            let name = control_context::dotted_name(name)
                .filter(|dotted| crate::tool_registry::lookup(dotted).is_some_and(|e| e.route == crate::tool_registry::Route::Toolbox))
                .unwrap_or(name);
            if name == crate::model_calls::tool::NAME {
                drop(state);
                // Inference runs on the host, outside the state lock; cancelling the
                // attempt closes the endpoint connection so the server stops generating.
                crate::toolbox::result(crate::model_calls::tool::call(&request["params"]["arguments"], &|| {
                    crate::lifecycle::cancelled(Some((shared, run_id)))
                }))
            } else if crate::toolbox::known(name) {
                drop(state);
                // Network requests and compilation must not hold the control-plane state lock.
                crate::toolbox::result(crate::toolbox::call(name, &request["params"]["arguments"]))
            } else if name == "control_web_fetch" {
                drop(state);
                crate::toolbox::result(crate::web_access::worker(shared, run_id, &request["params"]["arguments"]))
            } else if name == "control_browser_capture" {
                drop(state);
                crate::web_access::capture_tool(&request["params"]["arguments"]).unwrap_or_else(|error|crate::toolbox::result(Err(error)))
            } else if ["control_room_list", "control_room_read", "control_room_post", "control_room_watch", "control_room_notifications", "control_room_ack", "control_room_claim", "control_room_plan"].contains(&name) {
                return rooms::checkpoint(&mut state, run_id, envelope, crate::storage::save);
            } else {
                return error("Tool is not allowed for this worker. Next: call control_tool_search {} to list the tools this session may call.");
            }
        }
        _ => return crate::toolbox::protocol_error(id, -32601, "Unsupported worker method"),
    };
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
/// The prompt's deferred tool index for `run`: the tools its session may
/// call beyond the listed core, one terse line per family.
pub(crate) fn prompt_index(state: &State, run: &crate::runtime::Run) -> String {
    deferred::index(&deferred::Facts::of(state, run, crate::turn_context::managed(state, run)))
}
/// The in-process equivalent of one mailbox request, for Control's own tool
/// loop: identical fencing, policy, nudges and idempotency nonces.
pub(crate) fn local_rpc(state: &Arc<Mutex<State>>, run: &str, envelope: &Value, workspace: &Path, managed: bool) -> Value {
    if managed { rpc_at(state, run, envelope, Some(workspace)) } else { dispatch_at(state, run, envelope, Some(workspace)) }
}
pub fn pump(state: &Arc<Mutex<State>>, run: &str, workspace: &Path, managed:bool) {
    pump_at(state, run, workspace, workspace,managed);
}
pub fn pump_at(state: &Arc<Mutex<State>>, run: &str, mailbox: &Path, workspace: &Path, managed:bool) {
    let Ok(envelope) = rooms::take_request(mailbox) else {
        return;
    };
    let response = answer(&envelope, |envelope| if managed {rpc_at(state, run, envelope, Some(workspace))}else{dispatch_at(state,run,envelope,Some(workspace))});
    // Versioned only at this transport boundary: old pinned bridges still receive
    // raw JSON-RPC, while new bridges correlate every reply by delivery nonce.
    let response = if envelope["version"] == 1 {
        json!({"nonce":envelope["nonce"],"response":response})
    } else { response };
    rooms::reply(mailbox, &response);
}
/// A JSON-RPC batch is answered element by element, as one array of the
/// non-notification replies. Each element gets its own nonce, so idempotent
/// tools such as room posts do not collapse two calls into one.
fn answer(envelope: &Value, single: impl Fn(&Value) -> Value) -> Value {
    let Some(batch) = envelope["request"].as_array() else { return single(envelope) };
    if batch.is_empty() {
        return json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Empty JSON-RPC batch"}});
    }
    let nonce = envelope["nonce"].as_str().unwrap_or_default();
    let replies: Vec<Value> = batch.iter().enumerate()
        .map(|(index, request)| {
            if !request.is_object() || request["jsonrpc"] != "2.0" || !request["method"].is_string() {
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid JSON-RPC batch element"}})
            } else { single(&json!({"nonce":format!("{nonce}-{index}"),"request":request})) }
        })
        .filter(|reply| !reply.is_null())
        .collect();
    if replies.is_empty() { Value::Null } else { Value::Array(replies) }
}

#[cfg(test)]
mod tests;
