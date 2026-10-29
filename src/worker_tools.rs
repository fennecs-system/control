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
    if request["jsonrpc"] != "2.0" {
        return error("Invalid JSON-RPC request");
    }
    let Ok(mut state) = shared.lock() else {
        return error(
            "State unavailable; tools are disabled until Control is checked and restarted",
        );
    };
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
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-worker","version":"0.1"}})
        }
        "ping" => json!({}),
        "tools/list" => {
            let mut tools = crate::toolbox::definitions();
            if claude {
                tools.push(crate::claude::definition());
            }
            for tool in &mut tools {
                tool["name"] = json!(match tool["name"].as_str().unwrap() {
                    "control.web.search" => "control_web_search",
                    "control.rust.run" => "control_rust_run",
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
                let (Some(image), Some(node), Some(workspace)) =
                    (&attempt.image, &attempt.node_id, workspace)
                else {
                    return error("Attempt workspace is unavailable");
                };
                let image = image.clone();
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
                if crate::claude::claim(workspace, nonce).is_err() {
                    return error("Workspace command already admitted or intent could not be saved; inspect the attempt before retrying");
                }
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
                        output: String::new(),
                        status: "running".into(),
                    },
                );
                drop(state);
                let result = crate::claude::shell(&image, &name, &node, workspace, &input.command);
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
                other => other,
            };
            if crate::toolbox::known(name) {
                drop(state);
                // Network requests and compilation must not hold the control-plane state lock.
                crate::toolbox::result(crate::toolbox::call(name, &request["params"]["arguments"]))
            } else if has_room && ["control_room_read", "control_room_post"].contains(&name) {
                let count = state.collective.posts.len();
                let response = rooms::worker_rpc(&mut state, run_id, envelope);
                if state.collective.posts.len() != count && crate::storage::save(&state).is_err() {
                    state.collective.posts.truncate(count);
                    return error("Could not save room post");
                }
                return response;
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
    use super::*;
    fn rpc(state: &Arc<Mutex<State>>, run: &str, value: &Value) -> Value {
        rpc_at(state, run, value, None)
    }
    #[test]
    fn stale_workspace_is_rejected_for_room_posts_before_dispatch() {
        let mut state = State::default();
        state.runtime.enqueue("task".into(), "task", "test");
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        let id = run.id.clone();
        let mut attempt = crate::usage::Attempt::new();
        attempt.workspace_id = Some("current-attempt".into());
        run.attempts.push(attempt);
        let state = Arc::new(Mutex::new(state));
        let stale = std::env::temp_dir().canonicalize().unwrap();
        let request = json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"text":"late effect"}}}});
        let response = rpc_at(&state, &id, &request, Some(&stale));
        assert_eq!(
            response["error"]["message"],
            "Mailbox belongs to an inactive attempt"
        );
        assert!(state.lock().unwrap().collective.posts.is_empty());
    }
    #[test]
    fn workspace_execution_is_backend_scoped_and_rejects_model_supplied_mounts() {
        let mut state = State::default();
        state.runtime.enqueue("Test".into(), "task", "test");
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
        state.runtime.enqueue("Test tools".into(), "task", "test");
        let run = state.runtime.runs[0].id.clone();
        state.runtime.runs[0].status = "running".into();
        let state = Arc::new(Mutex::new(state));
        let list = rpc(
            &state,
            &run,
            &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
        );
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 2);
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
