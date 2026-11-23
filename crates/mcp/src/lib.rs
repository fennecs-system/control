//! MCP's JSON-RPC envelope: version negotiation, method routing and the
//! reply, error and tool-result shapes every Control transport answers with.
//! No state, I/O, authorization or tools: each transport keeps its framing
//! and authentication, and each audience supplies its own tools.
use serde_json::{json, Value};

/// MCP protocol revisions this node speaks, newest first.
pub const VERSIONS: [&str; 2] = ["2025-11-25", "2025-03-26"];
pub const LATEST: &str = VERSIONS[0];
/// The revision answered to `initialize`: the client's own when supported,
/// otherwise the latest.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    VERSIONS.into_iter().find(|version| Some(*version) == requested).unwrap_or(LATEST)
}
pub fn supported(version: &str) -> bool {
    VERSIONS.contains(&version)
}

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// Parses a request body once; `None` when it is not JSON.
pub fn parse(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method<'a> {
    Initialize,
    Ping,
    ToolsList,
    ToolsCall,
    /// Any other method, or `""` when it is missing or not a string.
    Other(&'a str),
}
/// The method a request names.
pub fn method(request: &Value) -> Method<'_> {
    match request["method"].as_str().unwrap_or("") {
        "initialize" => Method::Initialize,
        "ping" => Method::Ping,
        "tools/list" => Method::ToolsList,
        "tools/call" => Method::ToolsCall,
        other => Method::Other(other),
    }
}
/// A request without an `id` member is a notification and gets no reply.
/// `"id":null` is a request.
pub fn is_notification(request: &Value) -> bool {
    request.get("id").is_none()
}
/// The id a reply echoes; null for a notification or a non-object.
pub fn id(request: &Value) -> &Value {
    request.get("id").unwrap_or(&Value::Null)
}
/// Whether the request declares JSON-RPC 2.0.
pub fn is_v2(request: &Value) -> bool {
    request["jsonrpc"] == "2.0"
}

/// The tool a `tools/call` names. A missing or non-string name is `""`.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub name: &'a str,
    pub arguments: &'a Value,
    /// The whole `params`, for a proxy that forwards the call unchanged.
    pub params: &'a Value,
}
/// `Some` only for `tools/call`.
pub fn call(request: &Value) -> Option<Call<'_>> {
    (method(request) == Method::ToolsCall).then(|| named(request))
}
fn named(request: &Value) -> Call<'_> {
    Call { name: request["params"]["name"].as_str().unwrap_or(""), arguments: &request["params"]["arguments"], params: &request["params"] }
}

/// A successful reply.
pub fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
/// A JSON-RPC error. Reserved for faults no tool result can describe: an
/// invalid request, an unknown method, or a `tools/call` without a tool name.
pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
/// The `initialize` result for a server with tools.
pub fn initialize(version: &str, name: &str, server_version: &str) -> Value {
    json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":name,"version":server_version}})
}
/// The `tools/list` result.
pub fn tools(tools: Vec<Value>) -> Value {
    json!({"tools":tools})
}

/// A tool result holding one text item.
pub fn text(text: &str) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":false})
}
/// A tool result whose data travels both as JSON text and as
/// `structuredContent.data`.
pub fn data(value: &Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":{"data":value},"isError":false})
}
/// A failed tool result. MCP reports tool execution and argument validation
/// failures as results the model can read and correct; Codex never correlates
/// a JSON-RPC error with its pending call, so the call would hang instead.
pub fn failure(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}
/// A reply carrying a failed tool result. Unknown and policy-denied tool
/// names use this too, so the model sees why.
pub fn tool_error(id: &Value, message: &str) -> Value {
    reply(id, failure(message))
}
/// A refusal after the protocol checks: a tool result for `tools/call`,
/// otherwise an invalid-params error.
pub fn refusal(request: &Value, message: &str) -> Value {
    if method(request) == Method::ToolsCall {
        tool_error(id(request), message)
    } else {
        error(id(request), INVALID_PARAMS, message)
    }
}
/// Protocol checks the worker servers make before dispatch.
pub fn request_fault(request: &Value) -> Option<Value> {
    let id = id(request);
    if !is_v2(request) {
        Some(error(id, INVALID_REQUEST, "Invalid JSON-RPC request"))
    } else if method(request) == Method::ToolsCall && !request["params"]["name"].is_string() {
        Some(error(id, INVALID_PARAMS, "tools/call requires params.name"))
    } else {
        None
    }
}

/// A JSON-RPC batch, answered element by element as one array of the
/// non-notification replies; `single` gets each element's index. An empty
/// batch is an error and an all-notification batch has no reply (null).
pub fn batch(requests: &[Value], mut single: impl FnMut(usize, &Value) -> Value) -> Value {
    if requests.is_empty() {
        return error(&Value::Null, INVALID_REQUEST, "Empty JSON-RPC batch");
    }
    let replies: Vec<Value> = requests.iter().enumerate()
        .map(|(index, request)| {
            if !request.is_object() || !is_v2(request) || !request["method"].is_string() {
                error(&Value::Null, INVALID_REQUEST, "Invalid JSON-RPC batch element")
            } else {
                single(index, request)
            }
        })
        .filter(|reply| !reply.is_null())
        .collect();
    if replies.is_empty() { Value::Null } else { Value::Array(replies) }
}

/// How one server names itself and words its two protocol faults. The
/// transports differ here, and each keeps its own wording and codes.
pub struct Server<'a> {
    pub name: &'a str,
    pub version: &'a str,
    /// Optional `instructions` in the `initialize` result.
    pub instructions: Option<&'a str>,
    /// The error for a request that is not JSON-RPC 2.0.
    pub invalid: (i64, &'a str),
    /// The error for a method other than initialize, ping and the tool methods.
    pub unknown: (i64, &'a str),
}
/// One audience's tools: what it lists and how it runs a call. Authorization
/// belongs to the implementation.
pub trait Tools {
    fn list(&mut self) -> Vec<Value>;
    /// A tool result ([`data`], [`text`], [`failure`] or the tool's own).
    fn call(&mut self, call: Call<'_>) -> Value;
}
/// Answers one request: the version check, then `initialize` (negotiated),
/// `ping`, `tools/list` and `tools/call`, else the unknown-method error.
pub fn serve(server: &Server<'_>, request: &Value, tools: &mut impl Tools) -> Value {
    let id = id(request);
    if !is_v2(request) {
        return error(id, server.invalid.0, server.invalid.1);
    }
    let result = match method(request) {
        Method::Initialize => {
            let mut result = initialize(negotiate(request["params"]["protocolVersion"].as_str()), server.name, server.version);
            if let Some(instructions) = server.instructions {
                result["instructions"] = json!(instructions);
            }
            result
        }
        Method::Ping => json!({}),
        Method::ToolsList => self::tools(tools.list()),
        Method::ToolsCall => tools.call(named(request)),
        Method::Other(_) => return error(id, server.unknown.0, server.unknown.1),
    };
    reply(id, result)
}

#[cfg(test)]
mod tests;
