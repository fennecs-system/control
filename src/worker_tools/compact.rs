//! One compact shape for every successful worker tool result. Models read
//! these on every call, so the host omits what carries no information:
//! null, empty strings, arrays and objects, flags that are false by default,
//! and top-level fields that repeat the request's own arguments. Errors are
//! never rewritten, so refusals keep their callable fixes intact.
use serde_json::{Map, Value};

/// Results kept verbatim: tool schemas and guidance text, where an empty
/// object or a `false` is itself the information.
const VERBATIM: [&str; 2] = [super::deferred::SEARCH, "control_guidance"];

/// Flags whose absence means false. Outcome flags an agent acts on
/// (`applied`, `queued`, `active`, `ready`, `armed`, `continuation_enabled`,
/// ...) stay explicit even when false.
const DEFAULT_FALSE: [&str; 13] = [
    "abandoned",
    "fallback",
    "gap",
    "gave_up",
    "includes_logs",
    "listing_incomplete",
    "more",
    "output_truncated",
    "owner_idle",
    "published",
    "source_gap",
    "stopped",
    "truncated",
];

/// Compacts the successful `tools/call` reply `response` to tool `name`
/// called with `arguments`: its structured data and its JSON text item.
pub(super) fn reply(name: &str, arguments: &Value, response: &mut Value) {
    let Some(result) = response.get_mut("result").filter(|result| result["isError"] != true) else {
        return;
    };
    if VERBATIM.contains(&name) {
        return;
    }
    let data = match result.get("structuredContent").and_then(|content| content.get("data")) {
        Some(data) => Some(data.clone()),
        None => result["content"][0]["text"]
            .as_str()
            .filter(|_| result["content"][0]["type"] == "text")
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .filter(Value::is_object),
    };
    let Some(mut data) = data else { return };
    value(&mut data, arguments.as_object());
    if let Some(structured) = result
        .get_mut("structuredContent")
        .and_then(|content| content.get_mut("data"))
    {
        *structured = data.clone();
    }
    if let Some(text) = result
        .get_mut("content")
        .and_then(|content| content.get_mut(0))
        .filter(|item| item["type"] == "text")
        .and_then(|item| item.get_mut("text"))
    {
        *text = Value::String(data.to_string());
    }
}

/// Prunes `data` in place; top-level fields equal to an argument of the same
/// name are dropped as echoes.
pub(crate) fn value(data: &mut Value, arguments: Option<&Map<String, Value>>) {
    prune(data);
    if let (Some(object), Some(arguments)) = (data.as_object_mut(), arguments) {
        object.retain(|key, field| arguments.get(key) != Some(field));
    }
}

fn empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}
fn prune(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for field in fields.values_mut() {
                prune(field);
            }
            fields.retain(|key, field| {
                !empty(field) && !(*field == Value::Bool(false) && DEFAULT_FALSE.contains(&key.as_str()))
            });
        }
        // Array positions can carry meaning, so only nested content is pruned.
        Value::Array(items) => items.iter_mut().for_each(prune),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
