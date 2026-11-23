//! Deferred tool loading. A worker session lists only its role's core
//! schemas (catalogue metadata: `control_context::Mcp::core`) plus
//! `control_tool_search` and, for CLI adapters, `control_call`. Every other
//! tool the run may call appears in the prompt as a one-line index entry; the
//! search returns full schemas and `control_call` forwards to the same worker
//! dispatch as a direct call, so authority, judge policy, handoff fences,
//! nudges and fixes are unchanged. See the adjacent tests.
use control_context::{Audience, tool_for_mcp};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const SEARCH: &str = "control_tool_search";
pub(crate) const CALL: &str = "control_call";
/// Schemas a query returns, and the names one call may request.
const QUERY_RESULTS: usize = 5;
const NAMED_RESULTS: usize = 16;
/// Byte bound on one search result's schemas.
const RESULT_BYTES: usize = 32 * 1024;

/// Host facts that decide what a run's session may see.
#[derive(Clone, Debug)]
pub(crate) struct Facts {
    pub role: Audience,
    pub improve: bool,
    pub managed: bool,
    pub judge: bool,
    pub advisor: bool,
    /// Backends without their own sandbox shell get `control_workspace_run`.
    pub shell: bool,
    /// Control's own tool loop loads schemas natively instead of `control_call`.
    pub local: bool,
    /// The run's chat may use room tools.
    pub rooms: bool,
    /// The run's standing (`crate::personas::of`): its tool policy.
    pub standing: crate::personas::Effective,
}
impl Facts {
    pub(crate) fn of(state: &crate::State, run: &crate::runtime::Run, managed: bool) -> Self {
        let standing = crate::personas::of(&state.bots, run);
        let judge = standing.read_only();
        let chat = state.chats.iter().find(|c| Some(&c.id) == run.chat_id.as_ref());
        Self {
            role: crate::lens_library::audience(run),
            improve: run.action == crate::identity::Action::Improve,
            managed,
            judge,
            // An approved advisor is a judge whose one extra effect is evidence notes.
            advisor: judge && standing.approved,
            shell: run.target.backend.agent().host_shell(),
            local: run.target.backend == crate::backend::Backend::Local,
            rooms: chat.is_some_and(|chat| chat.actor == run.actor),
            standing,
        }
    }
}

fn name(tool: &Value) -> &str {
    tool["name"].as_str().unwrap_or("")
}
/// Every tool this run may call, with full schemas and worker spellings.
pub(crate) fn offered(facts: &Facts) -> Vec<Value> {
    let mut tools = crate::tool_registry::worker_definitions(facts.improve);
    if facts.shell {
        let mut shell = crate::claude::definition();
        if facts.judge {
            shell["description"] = json!(
                "Inspect source and run verification in a credential-free offline container. /workspace is read-only; use /tmp for scratch outputs. Bounded output and 25-second deadline."
            );
            shell["annotations"]["destructiveHint"] = json!(false);
        }
        tools.push(shell);
    }
    if facts.rooms {
        tools.extend(crate::rooms::worker_definitions(facts.judge));
    }
    for tool in &mut tools {
        if let Some(worker) = control_context::worker_name(name(tool)) {
            tool["name"] = json!(worker);
        }
    }
    tools.retain(|tool| {
        let name = name(tool);
        (facts.managed || !matches!(name, "control_turn_context" | "control_turn_handoff"))
            // Managed sessions read room posts through control_turn_context.
            && !(facts.managed && matches!(name, "control_room_ack" | "control_room_notifications"))
            && (!facts.judge || control_context::judge_may_call(name, facts.advisor))
            // A persona's deny list hides what it may not call.
            && (facts.judge || facts.standing.may_call(name))
    });
    tools
}
/// The session's `tools/list`: role core schemas plus the loaders.
pub(crate) fn listed(facts: &Facts) -> Vec<Value> {
    let mut tools = offered(facts)
        .into_iter()
        .filter(|tool| control_context::loaded(name(tool), facts.role))
        .map(|tool| as_listed(facts, tool))
        .collect::<Vec<_>>();
    tools.extend(definitions(facts.local).into_iter().map(|mut tool| {
        compact(&mut tool["inputSchema"]);
        tool
    }));
    tools
}
/// A core tool as the session's listing shows it.
fn as_listed(facts: &Facts, mut tool: Value) -> Value {
    // A judge's few core tools keep their full, judge-specific text.
    if let Some(brief) = control_context::mcp(name(&tool))
        .map(|m| m.brief)
        .filter(|b| !b.is_empty() && !facts.judge)
    {
        tool["description"] = json!(brief);
    }
    compact(&mut tool["inputSchema"]);
    // Hints only (execution policy is the host's); the search returns them.
    if let Some(tool) = tool.as_object_mut() {
        tool.remove("annotations");
    }
    tool
}
/// A search hit the listing already carries: its name and index phrase.
/// Asked for by name, it adds only the properties the listing shows as
/// "see control_tool_search".
fn known(facts: &Facts, tool: &Value, named: bool) -> Value {
    let about = control_context::mcp(name(tool)).map_or("", |m| m.phrase);
    let mut value = json!({"name":name(tool),"about":about});
    if !named {
        return value;
    }
    let shown = as_listed(facts, tool.clone());
    let omitted = shown["inputSchema"]["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, p)| p["description"] == "see control_tool_search")
        .map(|(key, _)| (key.clone(), tool["inputSchema"]["properties"][key].clone()))
        .collect::<serde_json::Map<_, _>>();
    if !omitted.is_empty() {
        value["properties"] = Value::Object(omitted);
    }
    value
}
/// A top-level property whose compacted schema exceeds this is listed by type.
const PROPERTY_BYTES: usize = 250;
/// Always-loaded schemas keep what a call needs: nested descriptions are
/// dropped (the tool's brief covers them) and a large optional property is
/// reduced to its type with a pointer to the full schema. The host validates
/// every argument against its own rules either way.
fn compact(schema: &mut Value) {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                // Annotations only: the host enforces every bound itself.
                for key in ["description", "pattern"] {
                    if map.get(key).is_some_and(Value::is_string) {
                        map.remove(key);
                    }
                }
                for key in [
                    "maxLength",
                    "minLength",
                    "maxItems",
                    "minItems",
                    "uniqueItems",
                    "additionalProperties",
                ] {
                    if map.get(key).is_some_and(|v| v.is_number() || v.is_boolean()) {
                        map.remove(key);
                    }
                }
                // A type's own range (u32::MAX and above) says nothing.
                if map
                    .get("maximum")
                    .and_then(Value::as_u64)
                    .is_some_and(|max| max >= u64::from(u32::MAX))
                {
                    map.remove("maximum");
                }
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    // The host refuses unknown fields itself.
    if let Some(map) = schema.as_object_mut() {
        map.remove("additionalProperties");
    }
    let required = schema["required"].as_array().cloned().unwrap_or_default();
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, property) in properties.iter_mut() {
        strip(property);
        if property.to_string().len() > PROPERTY_BYTES && !required.iter().any(|r| r == key.as_str()) {
            let kind = property.get("type").cloned().unwrap_or_else(|| json!("object"));
            *property = json!({"type":kind,"description":"see control_tool_search"});
        }
    }
}
/// Tools the run may call that its listing leaves out.
pub(crate) fn deferred(facts: &Facts) -> Vec<Value> {
    offered(facts)
        .into_iter()
        .filter(|tool| !control_context::loaded(name(tool), facts.role))
        .collect()
}
fn definitions(local: bool) -> Vec<Value> {
    let mut tools = vec![
        json!({"name":SEARCH,"description":"Full schemas and descriptions for tools by names, or by query words. Neither lists the deferred tools. Read-only.","inputSchema":{"type":"object","properties":{"names":{"type":"array","items":{"type":"string"},"maxItems":NAMED_RESULTS},"query":{"type":"string","maxLength":200}},"additionalProperties":false}}),
    ];
    if !local {
        tools.push(json!({"name":CALL,"description":"Call a deferred tool by name with its schema's arguments; same rules and result as a direct call.","inputSchema":{"type":"object","properties":{"tool":{"type":"string"},"arguments":{"type":"object"}},"required":["tool"],"additionalProperties":false}}));
    }
    tools
}

/// Full worker spelling; index entries omit the `control_` prefix.
fn spelled(name: &str) -> String {
    let name = name.trim();
    if name.starts_with("control_") || name.contains('.') {
        name.to_owned()
    } else {
        format!("control_{name}")
    }
}
/// The prompt's deferred index: a terse line per tool family.
pub(crate) fn index(facts: &Facts) -> String {
    let tools = deferred(facts);
    if tools.is_empty() {
        return String::new();
    }
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for tool in &tools {
        let name = name(tool);
        let (Some(family), Some(entry)) = (tool_for_mcp(name), control_context::mcp(name)) else {
            continue;
        };
        let line = format!("{} ({})", name.trim_start_matches("control_"), entry.phrase);
        match groups.iter_mut().find(|(title, _)| *title == family.title) {
            Some((_, lines)) => lines.push(line),
            None => groups.push((family.title, vec![line])),
        }
    }
    let how = if facts.local {
        "call by full name; the schema loads on first use"
    } else {
        "schemas: control_tool_search {\"names\":[...]}; call: control_call {\"tool\":\"control_<name>\",\"arguments\":{...}}"
    };
    let mut text = format!("More tools (names omit control_; {how}):\n");
    // One line per family; the names and phrases carry its meaning.
    for (_, lines) in groups {
        text.push_str(&format!("- {}\n", lines.join("; ")));
    }
    text
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    #[serde(default)]
    names: Option<Vec<String>>,
    #[serde(default)]
    query: Option<String>,
}
/// Answer `control_tool_search` from the run's callable tools.
pub(crate) fn search(facts: &Facts, args: &Value) -> std::io::Result<Value> {
    let input: SearchInput =
        serde_json::from_value(if args.is_null() { json!({}) } else { args.clone() }).map_err(|_| {
            std::io::Error::other(
                "Expected names (a list of tool names) or query (words), or neither to list the deferred tools",
            )
        })?;
    let tools = offered(facts);
    let next = if facts.local {
        "These tools are now callable directly by name."
    } else {
        "Call one with control_call {\"tool\":\"<name>\",\"arguments\":{...}}."
    };
    let bounded = |found: Vec<&Value>| {
        let mut size = 0;
        let mut kept = Vec::new();
        for tool in found {
            size += tool.to_string().len();
            if size > RESULT_BYTES && !kept.is_empty() {
                break;
            }
            kept.push(tool.clone());
        }
        kept
    };
    // Tools the session lists already have a schema in the request: name them
    // with their index phrase, plus (when asked by name) only what the
    // listing left out.
    let split = |found: Vec<&Value>, named: bool| {
        let (listed, new): (Vec<&Value>, Vec<&Value>) = found
            .into_iter()
            .partition(|t| control_context::loaded(name(t), facts.role));
        let found_new = !new.is_empty();
        let mut value = json!({"tools":bounded(new),"next":next});
        if !listed.is_empty() {
            value["already_listed"] = json!(listed.iter().map(|t| known(facts, t, named)).collect::<Vec<_>>());
            // Say so plainly: agents otherwise look the same loaded tools up
            // again every turn and get nothing for the round trip.
            let direct = "already_listed tools are in your tool list with their schemas: call them directly by name and do not search for them again.";
            value["next"] = json!(if found_new {
                format!("{next} The {direct}")
            } else {
                format!("Nothing new: the {direct}")
            });
        }
        value
    };
    if let Some(names) = input.names.filter(|n| !n.is_empty()) {
        if names.len() > NAMED_RESULTS {
            return Err(std::io::Error::other(format!(
                "At most {NAMED_RESULTS} names per search; split the request."
            )));
        }
        let wanted = names.iter().map(|n| spelled(n)).collect::<Vec<_>>();
        let found = wanted
            .iter()
            .filter_map(|w| tools.iter().find(|t| name(t) == w))
            .collect::<Vec<_>>();
        let unknown = wanted
            .iter()
            .filter(|w| !tools.iter().any(|t| name(t) == w.as_str()))
            .collect::<Vec<_>>();
        let mut value = split(found, true);
        if !unknown.is_empty() {
            value["unknown"] = json!(unknown);
            value["fix"] = json!(
                "These names are not callable in this session. Call control_tool_search {} to list the deferred tools, or search with {\"query\":\"<task words>\"}."
            );
        }
        return Ok(value);
    }
    let words = input
        .query
        .as_deref()
        .unwrap_or("")
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if words.is_empty() {
        let listing = tools
            .iter()
            .filter(|t| !control_context::loaded(name(t), facts.role))
            .filter_map(|t| {
                control_context::mcp(name(t))
                    .map(|m| json!({"name":m.name,"phrase":m.phrase,"family":tool_for_mcp(m.name).map(|f| f.title)}))
            })
            .collect::<Vec<_>>();
        return Ok(json!({"deferred":listing,"next":"Pass names to read full schemas."}));
    }
    let score = |tool: &Value| -> usize {
        let name = name(tool);
        let entry = control_context::mcp(name);
        let family = tool_for_mcp(name);
        let phrase = entry.map_or("", |m| m.phrase).to_lowercase();
        let about = family
            .map(|f| format!("{} {}", f.title, f.aliases))
            .unwrap_or_default()
            .to_lowercase();
        let description = tool["description"].as_str().unwrap_or("").to_lowercase();
        words
            .iter()
            .map(|w| {
                4 * usize::from(name.contains(w.as_str()))
                    + 3 * usize::from(phrase.contains(w.as_str()))
                    + 2 * usize::from(about.contains(w.as_str()))
                    + usize::from(description.contains(w.as_str()))
            })
            .sum()
    };
    let mut ranked = tools
        .iter()
        .map(|t| (score(t), t))
        .filter(|(s, _)| *s > 0)
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    let found = ranked
        .into_iter()
        .take(QUERY_RESULTS)
        .map(|(_, t)| t)
        .collect::<Vec<_>>();
    let mut value = split(found, false);
    if value["tools"].as_array().is_some_and(Vec::is_empty) && value.get("already_listed").is_none() {
        value["fix"] = json!("No callable tool matched. Call control_tool_search {} to list the deferred tools.");
    }
    Ok(value)
}

/// A `control_call` request rewritten as the direct call it names, so it
/// passes the same dispatch. `Ok(None)` leaves other requests untouched.
pub(crate) fn unwrap(envelope: &Value) -> Result<Option<Value>, String> {
    let request = &envelope["request"];
    if request["method"] != "tools/call" || request["params"]["name"] != CALL || request.get("id").is_none() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        tool: String,
        #[serde(default)]
        arguments: Option<Value>,
    }
    let usage = "Expected {\"tool\":\"<name>\",\"arguments\":{...}}. Next: call control_tool_search {\"names\":[\"<name>\"]} for the tool's schema.";
    let input: Input = serde_json::from_value(request["params"]["arguments"].clone()).map_err(|_| usage.to_owned())?;
    let arguments = input.arguments.unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err(usage.into());
    }
    let tool = spelled(&input.tool);
    if tool == CALL {
        return Err("control_call cannot call itself. Call the tool by name: control_call {\"tool\":\"<name>\",\"arguments\":{...}}.".into());
    }
    if !tool.starts_with("control_") || control_context::mcp(&tool).is_none() {
        return Err(format!(
            "Unknown tool {:?}; nothing ran. Next: call control_tool_search {{\"query\":\"<task words>\"}} to find a callable tool.",
            input.tool.chars().take(64).collect::<String>()
        ));
    }
    let mut rewritten = envelope.clone();
    rewritten["request"]["params"] = json!({"name":tool,"arguments":arguments});
    Ok(Some(rewritten))
}

#[cfg(test)]
mod tests;
