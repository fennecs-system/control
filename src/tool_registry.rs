//! Shared MCP schemas and transport policy. Authorization remains at each broker.
use serde_json::{json, Value};
use std::{
    io,
    sync::{Arc, Mutex, OnceLock},
};

pub use control_context::tool_policy::{Bridge, Route};
pub use crate::application::AttemptRef;
/// Application state as seen by tool families. Each family defines its own
/// operations trait and implements it for `LocalHost` in its module, where it
/// meets `State`; adding a family only lists its tools below.
pub struct LocalHost<'a>(pub &'a Arc<Mutex<crate::State>>);
pub struct AttemptCall<'a> {
    pub attempt: AttemptRef<'a>,
    pub host: &'a LocalHost<'a>,
}
/// Worker tools bound to one attempt. Calls run without the state lock held.
pub trait AttemptTools: Sync {
    fn definitions(&self) -> Vec<Value>;
    fn known(&self, name: &str) -> bool;
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value>;
}
static ATTEMPT_TOOLS: [&dyn AttemptTools; 16] = [
    &crate::github::inspection::Tools,
    &crate::compaction::Tools,
    &crate::recovery::Tools,
    &crate::turn_context::Tools,
    &crate::command_jobs::Tools,
    &crate::services::Tools,
    &crate::ports::Tools,
    &crate::bots::Tools,
    &crate::agent_message::Tools,
    &crate::watches::Tools,
    &crate::observe::Tools,
    &crate::packages::Tools,
    &crate::guidance::Tools,
    &crate::scores::Tools,
    &crate::intel::Tools,
    &crate::mcp_chat::AgentTools,
];
pub fn attempt_tools(name: &str) -> Option<&'static dyn AttemptTools> {
    ATTEMPT_TOOLS.iter().copied().find(|tools| tools.known(name))
}
pub struct Entry {
    pub definition: Value,
    pub route: Route,
    pub checkpoint: bool,
    pub bridge: Bridge,
    pub http: bool,
}
impl Entry {
    pub fn name(&self) -> &str {
        self.definition["name"].as_str().unwrap()
    }
}
fn entry(definition: Value) -> Entry {
    let name = definition["name"]
        .as_str()
        .expect("Tool schema needs a name");
    let policy =
        control_context::tool_policy::lookup(name).expect("Tool schema needs transport policy");
    Entry {
        definition,
        route: policy.route,
        checkpoint: policy.checkpoint,
        bridge: policy.bridge,
        http: policy.http,
    }
}
fn session_definitions() -> Vec<Value> {
    json!({"tools":[
            {"name":"control.chats.list","description":"List local chats.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.chat.create","description":"Create a local operator chat with lenses, optional room/design topic and chat or architect-only improve mode. A channel name creates a fresh task thread; an existing full room address explicitly joins that thread.","inputSchema":{"type":"object","properties":{"mode":{"type":"string","enum":["chat","improve"]},"room":{"type":"string"},"lens":{"type":"string"},"lenses":{"type":"array","items":{"type":"string"}},"design_context":{"type":"string"}}}},
            {"name":"control.chat.lenses","description":"Set lenses from the library for future messages. Existing runs retain their resolved context.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"lenses":{"type":"array","items":{"type":"string"}}},"required":["id","lenses"]}},
            {"name":"control.chat.rename","description":"Rename a chat. Every client shows this name in place of the chat's callsign.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string","minLength":1,"maxLength":crate::callsigns::MAX_NAME}},"required":["id","name"]}},
            {"name":"control.lenses.list","description":"List current declarative lens definitions.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.chat.persona","description":"Select a persona (purpose, DNA, standing, default model, behaviours) for a root DM's future messages, or null to clear. Its genes become the DM's DNA; each message captures the current revision. Read-only personas run only as spawned observers.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"persona":{"type":["string","null"]}},"required":["id","persona"]}},
            {"name":"control.personas.list","description":"List the persona library (current revisions): purpose, DNA, standing, body and behaviours. Personas compose existing pieces and grant nothing.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.persona.read","description":"Read one persona definition (optionally an earlier revision) with its revision history and standing summary.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"revision":{"type":"integer","minimum":1}},"required":["id"]}},
            {"name":"control.lens.read","description":"Read a lens definition and its current revision.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.lenses.resolve","description":"Resolve nested lenses against explicit local document snapshots. Does not grant tool authority.","inputSchema":{"type":"object","properties":{"lenses":{"type":"array","items":{"type":"string"}}},"required":["lenses"]}},
            {"name":"control.rooms.list","description":"List collaboration rooms.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.room.read","description":"Read recent room posts and linked chats.","inputSchema":{"type":"object","properties":{"room":{"type":"string"}},"required":["room"]}},
            {"name":"control.room.post","description":"Post a concise shared update. This does not send a chat prompt or grant authority.","inputSchema":{"type":"object","properties":{"room":{"type":"string"},"message":{"type":"string","minLength":1,"maxLength":2000}},"required":["room","message"]}},
            {"name":"control.chat.read","description":"Read a chat and its run status.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.run.scores","description":"Read the workload score series of a run: the score probes declared anywhere in the chat's team (root chat and descendants), each with its samples (time, trigger, values), plus the team's milestone count. Values are the workload's own claims, not verified by Control.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.intel.brief","description":"Render the Markdown intel brief for the last 24 hours: per team its goal, progress (claims, score probe changes), cost in tokens, observed pathologies and requests waiting on the operator. Omit chat for the whole node. save=true also writes it to intel/brief-DATE.md under the state directory. Read-only over node state.","inputSchema":{"type":"object","properties":{"chat":{"type":"string"},"save":{"type":"boolean","default":false}}}},
            {"name":"control.requests.list","description":"List pending operator decisions across all chats: design questions, bot requests and port approvals, grouped by top-level chat in the same shape as control.chat.read. Decide them in that chat.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.chat.ask","description":"Ask the operator a decision as a design question: in chat id (an idle operator DM) or else the Decisions DM. It is listed and forwarded like an agent's question, approves nothing and resumes no run. Returns the question ID.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"question":{"type":"string","minLength":1,"maxLength":1000},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"string","minLength":1,"maxLength":200}},"recommended":{"type":"integer","minimum":0,"maximum":3}},"required":["question","options"]}},
            {"name":"control.services.list","description":"List the live persistent services a chat owns; team=true includes every chat spawned from it.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"team":{"type":"boolean","default":false}},"required":["id"]}},
            {"name":"control.services.stop","description":"Stop the live persistent services a chat owns (team=true: and every chat spawned from it), or only service_id. The same stop as the services page; ports bound to a stopped service lose authorization.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"team":{"type":"boolean","default":false},"service_id":{"type":"string","maxLength":128}},"required":["id"]}},
            {"name":"control.chat.send","description":"Send as the chat's original actor. Optional mode changes persist; improve requires architect access.","inputSchema":{"type":"object","properties":{"mode":{"type":"string","enum":["chat","improve"]},"id":{"type":"string"},"message":{"type":"string","minLength":1,"maxLength":control_context::limits::CHAT_MESSAGE_CHARS}},"required":["id","message"]}}
        ]})["tools"].as_array().unwrap().clone()
}
pub fn entries() -> &'static [Entry] {
    static CATALOG: OnceLock<Vec<Entry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut entries = Vec::new();
        for definition in session_definitions() {
            let mut registered = entry(definition);
            registered.definition["annotations"]["readOnlyHint"] = json!(!registered.checkpoint);
            entries.push(registered);
        }
        entries.push(entry(crate::projects::definition(false)));
        entries.extend(crate::knowledge_tools::definitions().into_iter().map(entry));
        entries.push(entry(crate::github::definition()));
        entries.push(entry(crate::github::inspection::definition(true)));
        entries.push(entry(crate::github::operations::definition(true)));
        entries.extend(crate::toolbox::definitions().into_iter().map(entry));
        entries.extend(crate::artifacts::definitions().into_iter().map(entry));
        entries.extend(crate::packages::definitions().into_iter().map(entry));
        entries.push(entry(browser_definition()));
        let mut names = std::collections::HashSet::new();
        assert!(
            entries
                .iter()
                .all(|entry| names.insert(entry.name().to_owned())),
            "Duplicate tool registration"
        );
        entries
    })
}
pub fn lookup(name: &str) -> Option<&'static Entry> {
    entries().iter().find(|e| e.name() == name)
}
pub fn checkpoint(name: &str) -> bool {
    lookup(name).is_some_and(|e| e.checkpoint)
}
pub fn http_definitions() -> Vec<Value> {
    entries()
        .iter()
        .filter(|e| e.http)
        .map(|e| e.definition.clone())
        .collect()
}
pub fn local_definitions() -> Vec<Value> {
    entries()
        .iter()
        .filter(|e| matches!(e.bridge, Bridge::Local | Bridge::Forward))
        .map(|e| e.definition.clone())
        .collect()
}
pub fn bridge_read(name: &str) -> bool {
    lookup(name).is_some_and(|e| e.bridge == Bridge::Read)
}
pub fn bridge_forward(name: &str) -> bool {
    lookup(name).is_some_and(|e| matches!(e.bridge, Bridge::Read | Bridge::Forward))
}
pub fn worker_definitions(improve: bool) -> Vec<Value> {
    let mut tools = entries()
        .iter()
        .filter(|e| e.route == Route::Toolbox)
        .map(|e| e.definition.clone())
        .collect::<Vec<_>>();
    tools.push(crate::design::definition());
    tools.extend(crate::knowledge_tools::definitions().into_iter().map(|mut definition| {
        let name = definition["name"].as_str().unwrap();
        definition["name"] = json!(control_context::worker_name(name).expect("Knowledge tools have catalogued worker spellings"));
        definition
    }));
    tools.push(crate::web_access::definition());
    tools.push(crate::web_access::capture_definition());
    tools.push(crate::projects::definition(true));
    tools.extend(crate::previews::definitions());
    if improve {
        tools.push(crate::github::definition());
    }
    tools.push(crate::delegation::definition());
    tools.push(crate::spawning::definition());
    tools.extend(crate::spawning::management_definitions());
    tools.extend(ATTEMPT_TOOLS.iter().flat_map(|tools| tools.definitions())
        .filter(|definition| improve || !matches!(definition["name"].as_str(), Some("control_github_read" | "control_github_write"))));
    tools.push(crate::delegated_results::definition());
    tools
}
fn browser_definition() -> Value {
    json!({"name":"control.browser.capture","description":"Render a self-contained HTML preview inside the configured root using offline Nix Firefox. Returns a PNG artifact. No network or host browser profile. Requires the pinned browser image.","inputSchema":{"type":"object","properties":{"input":{"type":"string"},"width":{"type":"integer","minimum":240,"maximum":2560},"height":{"type":"integer","minimum":240,"maximum":2560}},"required":["input"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
#[cfg(test)]
mod tests;
