//! The operator's `/mcp` endpoint: HTTP framing around `control_mcp`. The body
//! is parsed once and routed by tool name. GitHub, package and toolbox calls
//! run without the state lock, so network, Git and compilation work never
//! blocks other requests; everything else runs under it.
use crate::State;
use control_http::Policy;
use serde_json::Value;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

/// The `Mcp-Protocol-Version` header, when the client sent one.
fn protocol_version(header: &str) -> Option<&str> {
    header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("mcp-protocol-version"))
        .map(|(_, value)| value.trim())
}
pub(crate) fn handle(
    stream: &mut impl Write,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    header: &str,
    body: &[u8],
) -> bool {
    if path != "/mcp" {
        return false;
    }
    let request = control_mcp::parse(body);
    let version = protocol_version(header);
    if method == "POST"
        && let Some(request) = &request
        && let Some((reply, policy)) = unlocked(shared, version, request)
    {
        let _ = match reply {
            Ok(reply) => control_http::respond(stream, "200 OK", "application/json", reply.to_string().as_bytes(), "", policy),
            Err(message) => control_http::respond(stream, "400 Bad Request", "text/plain", message.as_bytes(), "", policy),
        };
        return true;
    }
    crate::state_response(stream, shared, |stream, state| locked(stream, state, method, version, request.as_ref()));
    true
}

type Reply = Result<Value, &'static str>;
/// Tools served before the state lock, in the order their separate `/mcp`
/// intercepts used to run. `None` leaves the request to the operator MCP.
/// Each keeps its own checks: GitHub answers unacknowledged requests itself,
/// packages leave them to the operator MCP, and the toolbox refuses them.
fn unlocked(shared: &Arc<Mutex<State>>, version: Option<&str>, request: &Value) -> Option<(Reply, Policy)> {
    let call = control_mcp::call(request)?;
    let id = control_mcp::id(request);
    let acknowledged = control_mcp::is_v2(request) && !control_mcp::is_notification(request);
    Some(match call.name {
        "control_github_write" => (Ok(crate::github::operations::mcp(shared, request)), Policy::Restricted),
        "control_github_read" => (Ok(crate::github::inspection::mcp(shared, request)), Policy::Restricted),
        "control_github_open_pr" => (Ok(crate::github::mcp_open_pr(shared, request)), Policy::Restricted),
        name if acknowledged && crate::packages::known(name) => {
            let result = crate::packages::operator(shared, name, call.arguments);
            (Ok(control_mcp::reply(id, crate::toolbox::result(result))), Policy::Embedded)
        }
        name if crate::toolbox::known(name) => {
            if !acknowledged || version.is_some_and(|version| !control_mcp::supported(version)) {
                (Err("Invalid MCP request."), Policy::Embedded)
            } else {
                let result = crate::toolbox::call(name, call.arguments);
                (Ok(control_mcp::reply(id, crate::toolbox::result(result))), Policy::Embedded)
            }
        }
        _ => return None,
    })
}

/// The operator MCP under the state lock, with its own HTTP checks.
fn locked(stream: &mut Vec<u8>, state: &mut State, method: &str, version: Option<&str>, request: Option<&Value>) {
    let mut respond = |status: &str, mime: &str, body: &str, extra: &str| {
        let _ = control_http::respond(stream, status, mime, body.as_bytes(), extra, Policy::Embedded);
    };
    if method != "POST" {
        return respond("405 Method Not Allowed", "text/plain", "Use MCP POST requests.", "Allow: POST\r\n");
    }
    if version.is_some_and(|version| !control_mcp::supported(version)) {
        return respond("400 Bad Request", "text/plain", "Unsupported MCP protocol version.", "");
    }
    let Some(request) = request else {
        let error = control_mcp::error(&Value::Null, control_mcp::PARSE_ERROR, "Invalid JSON.");
        return respond("400 Bad Request", "application/json", &error.to_string(), "");
    };
    if control_mcp::is_notification(request) {
        // A notification must never execute a mutating tool and silently lose its acknowledgement.
        return if request["method"].as_str().is_some_and(|method| method.starts_with("notifications/")) {
            respond("202 Accepted", "application/json", "", "")
        } else {
            respond("400 Bad Request", "text/plain", "A request ID is required.", "")
        };
    }
    respond("200 OK", "application/json", &crate::session_http::rpc(state, request).to_string(), "");
}

#[cfg(test)]
mod tests;
