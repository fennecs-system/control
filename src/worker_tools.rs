//! Credential-free worker tools are authorized against the active run on the host.
use crate::{State, rooms};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

mod authority;
mod compact;
pub(crate) mod deferred;
pub(crate) mod fixes;
mod kinds;
pub(crate) mod nudges;
mod table;

/// `control_call` becomes the direct call it names before any policy runs,
/// so both spellings share one authorization and dispatch path.
fn direct(envelope: &Value) -> Result<std::borrow::Cow<'_, Value>, Value> {
    match deferred::unwrap(envelope) {
        Ok(Some(rewritten)) => Ok(std::borrow::Cow::Owned(rewritten)),
        Ok(None) => Ok(std::borrow::Cow::Borrowed(envelope)),
        Err(message) => Err(control_mcp::tool_error(&envelope["request"]["id"], &message)),
    }
}
fn rpc_at(shared: &Arc<Mutex<State>>, run_id: &str, envelope: &Value, workspace: Option<&Path>) -> Value {
    rpc_for(shared, run_id, envelope, workspace, false)
}
/// `local`: Control's own loop ends each model request with the inbox, so
/// its tool results carry neither the inbox nor a conversation hint.
fn rpc_for(shared: &Arc<Mutex<State>>, run_id: &str, envelope: &Value, workspace: Option<&Path>, local: bool) -> Value {
    let envelope = match direct(envelope) {
        Ok(envelope) => envelope,
        Err(refused) => return refused,
    };
    let envelope = envelope.as_ref();
    let mut response = dispatch_at(shared, run_id, envelope, workspace);
    let name = envelope["request"]["params"]["name"].as_str().unwrap_or("");
    // A refused call changes no state, so it neither counts nor carries notes.
    let refused = response["result"]["isError"] == true;
    // Every refusal from these families names a callable fix.
    if refused
        && envelope["request"]["method"] == "tools/call"
        && fixes::covered(name)
        && let Some(text) = response
            .get_mut("result")
            .and_then(|result| result.get_mut("content"))
            .and_then(|content| content.get_mut(0))
            .and_then(|item| item.get_mut("text"))
        && let Some(message) = text.as_str()
    {
        *text = json!(fixes::actionable(name, message));
    }
    if envelope["request"]["method"] != "tools/call" {
        return response;
    }
    // get_mut, not IndexMut: indexing would insert "result":null into an
    // error reply, which clients reject as an invalid JSON-RPC response.
    let Some(result) = response.get_mut("result").filter(|result| result["content"].is_array()) else {
        return response;
    };
    media_visible(result);
    if name != "control_turn_handoff"
        && let Ok(mut state) = shared.lock()
    {
        let mut notes = Vec::new();
        if name != "control_turn_context" {
            // A CLI agent's next request ends with this result: new
            // conversation callbacks ride on it, delivered once.
            let inbox = if local {
                None
            } else {
                crate::turn_context::inbox(&mut state, run_id, crate::storage::save)
            };
            if crate::turn_context::callback_notice(&mut state, run_id, !local) && inbox.is_none() {
                notes
                    .push("New callbacks: read them with control_turn_context at the next action boundary.".to_owned());
            }
            notes.extend(inbox);
        }
        if !refused {
            notes.extend(nudges::notes(&mut state, run_id, name, crate::config::unix_seconds()));
        }
        // A service command that could not reach a host one of this chat's
        // egress requests names: wait for the decision, or use the relay.
        if matches!(name, "control_service_exec" | "control_service_exec_status")
            && let Some(hint) = crate::ports::egress_hint(
                &state.runtime.runs,
                &state.ports,
                run_id,
                &envelope["request"]["params"]["arguments"].to_string(),
                &result.to_string(),
            )
        {
            notes.push(hint);
        }
        attach_notes(result, &notes);
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
    let Some(content) = fields.get("content").and_then(Value::as_array) else {
        return;
    };
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
    if let Ok(envelope) = direct(envelope)
        && envelope["request"]["method"] == "tools/call"
    {
        let params = &envelope["request"]["params"];
        compact::reply(
            params["name"].as_str().unwrap_or(""),
            &params["arguments"],
            &mut response,
        );
    }
    response
}
fn dispatch_raw(shared: &Arc<Mutex<State>>, run_id: &str, envelope: &Value, workspace: Option<&Path>) -> Value {
    let envelope = match direct(envelope) {
        Ok(envelope) => envelope,
        Err(refused) => return refused,
    };
    let envelope = envelope.as_ref();
    let request = &envelope["request"];
    if control_mcp::is_notification(request) {
        return Value::Null;
    }
    let id = control_mcp::id(request);
    if let Some(fault) = control_mcp::request_fault(request) {
        return fault;
    }
    // Every tools/call failure past the protocol checks is a tool result.
    let error = |message: &str| control_mcp::refusal(request, message);
    if crate::shutdown::requested() {
        return error("Control is stopping; no new tools may run.");
    }
    let Ok(state) = shared.lock() else {
        return error("State unavailable; tools are disabled until Control is checked and restarted");
    };
    if crate::shutdown::requested() {
        return error("Control is stopping; no new tools may run.");
    }
    let caller = match authority::admit(&state, run_id, workspace) {
        Ok(caller) => caller,
        Err(message) => return error(message),
    };
    let result = match control_mcp::method(request) {
        control_mcp::Method::Initialize => control_mcp::initialize(control_mcp::LATEST, "control-worker", "0.1"),
        control_mcp::Method::Ping => json!({}),
        // The role's core schemas; the rest are in the prompt's deferred index.
        control_mcp::Method::ToolsList => control_mcp::tools(deferred::listed(caller.facts())),
        control_mcp::Method::ToolsCall => {
            let call = kinds::Call::of(envelope, workspace);
            let entry = table::lookup(call.name);
            match fence(&state, &caller, &call, entry) {
                Ok(()) => entry
                    .handler
                    .answer(state, shared, &caller, &call, crate::storage::save),
                Err(refusal) => control_mcp::failure(refusal),
            }
        }
        control_mcp::Method::Other(_) => {
            return control_mcp::error(id, control_mcp::METHOD_NOT_FOUND, "Unsupported worker method");
        }
    };
    control_mcp::reply(id, result)
}
/// The run-level checks before a tool's handler, in order: a staged handoff,
/// a managed session's room cursor, then the entry's fence (standing, the
/// delegation pause, this attempt's design question).
fn fence(
    state: &State,
    caller: &authority::Caller<'_>,
    call: &kinds::Call<'_>,
    entry: &table::Entry,
) -> Result<(), &'static str> {
    use table::{Fence, Session};
    let run = caller.run(state);
    let attempt = run.attempts.last();
    if attempt
        .and_then(|a| a.turn_mailbox.as_ref())
        .and_then(|m| m.accepted())
        .is_some()
        && !control_context::tool_policy::after_handoff(call.name)
    {
        return Err(
            "A handoff is staged; only observations and handoff corrections may run before this attempt finishes. No further call is needed; to change it, call control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} again.",
        );
    }
    if entry.session == Session::Unmanaged && caller.facts().managed {
        return Err(
            "Managed sessions read room posts with control_turn_context {\"source\":\"room\"}; control_turn_handoff acknowledges them when you yield.",
        );
    }
    // Standing (captured persona, else the built-in of its position) decides.
    let standing = || {
        if caller.facts().standing.may_call(call.name) {
            Ok(())
        } else if caller.facts().judge {
            Err(
                "Judge policy permits read-only source inspection, isolated verification and room collaboration only. Next: call control_tool_search {} to list the tools you may call.",
            )
        } else {
            Err(
                "This persona's standing denies that tool. Next: call control_tool_search {} to list the tools you may call.",
            )
        }
    };
    let delegation = || {
        if crate::delegation::pausing(run) {
            Err("This attempt is pausing for delegated work; no more tools may run.")
        } else {
            Ok(())
        }
    };
    // Only a question asked by this attempt pauses it.
    let design =
        || {
            if run.question.as_ref().is_some_and(|q| {
                q.pending() && attempt.is_some_and(|a| a.workspace_id.as_deref() == Some(&q.generation))
            }) {
                Err("This attempt is pausing for a design answer; no more tools may run.")
            } else {
                Ok(())
            }
        };
    match entry.fence {
        Fence::Open => Ok(()),
        Fence::Standing => standing(),
        Fence::Delegation => standing().and_then(|()| delegation()),
        Fence::Design => standing().and_then(|()| delegation()).and_then(|()| design()),
    }
}
/// The prompt's deferred tool index for `run`: the tools its session may
/// call beyond the listed core, one terse line per family.
pub(crate) fn prompt_index(state: &State, run: &crate::runtime::Run) -> String {
    deferred::index(&deferred::Facts::of(
        state,
        run,
        crate::turn_context::managed(state, run),
    ))
}
/// The in-process equivalent of one mailbox request, for Control's own tool
/// loop: identical fencing, policy, nudges and idempotency nonces.
pub(crate) fn local_rpc(
    state: &Arc<Mutex<State>>,
    run: &str,
    envelope: &Value,
    workspace: &Path,
    managed: bool,
) -> Value {
    let mut response = if managed {
        rpc_for(state, run, envelope, Some(workspace), true)
    } else {
        dispatch_at(state, run, envelope, Some(workspace))
    };
    // The loop itself ends each request with the observation, so results do not repeat it.
    observed(state, run, envelope, &mut response, false);
    response
}
/// The chat's fresh observation (when `attach`) and the once-per-attempt
/// polling nudge on a tool result. Both come after the result's own notes, so
/// the observation is always the last text item.
fn observed(shared: &Arc<Mutex<State>>, run_id: &str, envelope: &Value, response: &mut Value, attach: bool) {
    let Ok(envelope) = direct(envelope) else { return };
    let request = &envelope["request"];
    if request["method"] != "tools/call" {
        return;
    }
    let name = request["params"]["name"].as_str().unwrap_or("");
    let Some(result) = response.get_mut("result").filter(|result| result["content"].is_array()) else {
        return;
    };
    if result["isError"] != true
        && let Some(note) = crate::observe::polling_note(shared, run_id, name, &request["params"]["arguments"])
    {
        attach_notes(result, &[note]);
    }
    // A handoff ends the turn, and a declaration shows its own preview.
    if attach
        && !matches!(name, "control_turn_handoff" | "control_observe_set")
        && let Some(block) = crate::observe::for_result(shared, run_id)
    {
        attach_observation(result, block);
    }
}
/// Adds the observation as the result's last text item and, as with notes,
/// beside the structured data for clients that show only that.
fn attach_observation(result: &mut Value, block: String) {
    if let Some(structured) = result.get_mut("structuredContent").and_then(Value::as_object_mut) {
        structured.insert("observation".into(), Value::String(block.clone()));
    }
    if let Some(content) = result.get_mut("content").and_then(Value::as_array_mut) {
        content.push(json!({"type":"text","text":block}));
    }
}
pub fn pump_at(state: &Arc<Mutex<State>>, run: &str, mailbox: &Path, workspace: &Path, managed: bool) {
    let Ok(envelope) = rooms::take_request(mailbox) else {
        return;
    };
    let response = answer(&envelope, |envelope| {
        let mut response = if managed {
            rpc_at(state, run, envelope, Some(workspace))
        } else {
            dispatch_at(state, run, envelope, Some(workspace))
        };
        observed(state, run, envelope, &mut response, true);
        response
    });
    // Versioned only at this transport boundary: old pinned bridges still receive
    // raw JSON-RPC, while new bridges correlate every reply by delivery nonce.
    let response = if envelope["version"] == 1 {
        json!({"nonce":envelope["nonce"],"response":response})
    } else {
        response
    };
    rooms::reply(mailbox, &response);
}
/// A JSON-RPC batch is answered element by element, as one array of the
/// non-notification replies. Each element gets its own nonce, so idempotent
/// tools such as room posts do not collapse two calls into one.
fn answer(envelope: &Value, single: impl Fn(&Value) -> Value) -> Value {
    let Some(batch) = envelope["request"].as_array() else {
        return single(envelope);
    };
    let nonce = envelope["nonce"].as_str().unwrap_or_default();
    control_mcp::batch(batch, |index, request| {
        single(&json!({"nonce":format!("{nonce}-{index}"),"request":request}))
    })
}

#[cfg(test)]
mod tests;
