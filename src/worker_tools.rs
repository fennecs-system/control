//! Credential-free worker tools are authorized against the active run on the host.
use crate::{identity, rooms, State};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn rpc(state: &Arc<Mutex<State>>, run_id: &str, envelope: &Value) -> Value {
    let request = &envelope["request"];
    let Some(id) = request.get("id") else {
        return Value::Null;
    };
    let error =
        |message: &str| json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":message}});
    if request["jsonrpc"] != "2.0" {
        return error("Invalid JSON-RPC request");
    }
    let Ok(mut state) = state.lock() else {
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
    if let Err(message) = identity::authorize_codex(&run.actor, run.action) {
        return error(message);
    }
    let has_room = state.chats.iter().any(|c| {
        Some(&c.id) == run.chat_id.as_ref() && c.room.as_deref().is_some_and(rooms::valid)
    });
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-worker","version":"0.1"}})
        }
        "ping" => json!({}),
        "tools/list" => {
            let mut tools = crate::toolbox::definitions();
            for tool in &mut tools {
                tool["name"] = json!(match tool["name"].as_str().unwrap() {
                    "control.web.search" => "control_web_search",
                    _ => "control_rust_run",
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
    let path = workspace.join(".control-room-request");
    let Ok(envelope) = rooms::read_request(&path) else {
        return;
    };
    let response = rpc(state, run, &envelope);
    rooms::reply(workspace, &response);
}

#[cfg(test)]
mod tests {
    use super::*;
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
