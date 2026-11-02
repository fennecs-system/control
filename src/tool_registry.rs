//! Shared MCP schemas and transport policy. Authorization remains at each broker.
use serde_json::{json, Value};
use std::{
    io,
    sync::{Arc, Mutex, OnceLock},
};

pub use control_context::tool_policy::{Bridge, Route};
/// The calling attempt and request identity. Carries no access to state.
pub struct AttemptRef<'a> {
    pub run: &'a str,
    pub generation: &'a str,
    pub nonce: &'a str,
}
/// Typed operations Control offers to tool sets. Authorization, persistence
/// and fences stay inside the host.
pub trait Host {
    fn jobs(&self) -> &dyn crate::command_jobs::JobOps;
    fn services(&self) -> &dyn crate::services::ServiceOps;
    fn ports(&self) -> &dyn crate::ports::PortOps;
}
pub struct AttemptCall<'a> {
    pub attempt: AttemptRef<'a>,
    pub host: &'a dyn Host,
}
/// Adapts application state to `Host`; the only place tools meet `State`.
pub struct LocalHost<'a>(pub &'a Arc<Mutex<crate::State>>);
impl Host for LocalHost<'_> {
    fn jobs(&self) -> &dyn crate::command_jobs::JobOps {
        self
    }
    fn services(&self) -> &dyn crate::services::ServiceOps {
        self
    }
    fn ports(&self) -> &dyn crate::ports::PortOps {
        self
    }
}
impl crate::command_jobs::JobOps for LocalHost<'_> {
    fn job(&self, attempt: &AttemptRef<'_>, call: crate::command_jobs::JobCall) -> io::Result<Value> {
        crate::command_jobs::execute(self.0, attempt, call)
    }
}
impl crate::services::ServiceOps for LocalHost<'_> {
    fn service(&self, attempt: &AttemptRef<'_>, call: crate::services::ServiceCall) -> io::Result<Value> {
        crate::services::execute(self.0, attempt, call)
    }
}
impl crate::ports::PortOps for LocalHost<'_> {
    fn port(&self, attempt: &AttemptRef<'_>, call: crate::ports::PortCall) -> io::Result<Value> {
        crate::ports::execute(self.0, attempt, call)
    }
}
/// Worker tools bound to one attempt. Calls run without the state lock held.
pub trait AttemptTools: Sync {
    fn definitions(&self) -> Vec<Value>;
    fn known(&self, name: &str) -> bool;
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value>;
}
static ATTEMPT_TOOLS: [&dyn AttemptTools; 3] = [
    &crate::command_jobs::Tools,
    &crate::services::Tools,
    &crate::ports::Tools,
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
            {"name":"control.chat.create","description":"Create a local operator chat with lenses, optional room/design topic and chat or architect-only improve mode.","inputSchema":{"type":"object","properties":{"mode":{"type":"string","enum":["chat","improve"]},"room":{"type":"string","enum":["general","engineering","research"]},"lens":{"type":"string"},"lenses":{"type":"array","items":{"type":"string"}},"design_context":{"type":"string"}}}},
            {"name":"control.chat.lenses","description":"Set lenses from the library for future messages. Existing runs retain their resolved context.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"lenses":{"type":"array","items":{"type":"string"}}},"required":["id","lenses"]}},
            {"name":"control.lenses.list","description":"List current declarative lens definitions.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.lens.read","description":"Read a lens definition and its current revision.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.lenses.resolve","description":"Resolve nested lenses against explicit local document snapshots. Does not grant tool authority.","inputSchema":{"type":"object","properties":{"lenses":{"type":"array","items":{"type":"string"}}},"required":["lenses"]}},
            {"name":"control.rooms.list","description":"List collaboration rooms.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.room.read","description":"Read recent room posts and linked chats.","inputSchema":{"type":"object","properties":{"room":{"type":"string"}},"required":["room"]}},
            {"name":"control.room.post","description":"Post a concise shared update. This does not send a chat prompt or grant authority.","inputSchema":{"type":"object","properties":{"room":{"type":"string"},"message":{"type":"string","minLength":1,"maxLength":2000}},"required":["room","message"]}},
            {"name":"control.chat.read","description":"Read a chat and its run status.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.chat.send","description":"Send as the chat's original actor. Optional mode changes persist; improve requires architect access.","inputSchema":{"type":"object","properties":{"mode":{"type":"string","enum":["chat","improve"]},"id":{"type":"string"},"message":{"type":"string","minLength":1,"maxLength":2000}},"required":["id","message"]}}
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
        entries.extend(crate::toolbox::definitions().into_iter().map(entry));
        entries.extend(crate::artifacts::definitions().into_iter().map(entry));
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
        definition["name"] = json!(definition["name"].as_str().unwrap().replace('.', "_"));
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
    tools.extend(ATTEMPT_TOOLS.iter().flat_map(|tools| tools.definitions()));
    tools.push(crate::delegated_results::definition());
    tools
}
fn browser_definition() -> Value {
    json!({"name":"control.browser.capture","description":"Render a self-contained HTML preview inside the configured root using offline Nix Firefox. Returns a PNG artifact. No network or host browser profile. Requires the pinned browser image.","inputSchema":{"type":"object","properties":{"input":{"type":"string"},"width":{"type":"integer","minimum":240,"maximum":2560},"height":{"type":"integer","minimum":240,"maximum":2560}},"required":["input"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemas_and_transport_policy_share_one_registration() {
        let mut names = std::collections::HashSet::new();
        for entry in entries() {
            assert!(names.insert(entry.name()));
            assert!(entry.definition["inputSchema"].is_object());
            if entry.checkpoint {
                assert_eq!(entry.definition["annotations"]["readOnlyHint"], false);
            }
            if entry.bridge == Bridge::Read {
                assert!(!entry.checkpoint);
                assert_ne!(entry.definition["annotations"]["readOnlyHint"], false);
            }
            assert!(
                control_context::TOOLS
                    .iter()
                    .any(|t| t.mcp_names.contains(&entry.name())),
                "Missing UI registration: {}",
                entry.name()
            );
        }
        assert_eq!(names.len(), control_context::tool_policy::POLICIES.len());
        for policy in control_context::tool_policy::POLICIES {
            assert!(names.contains(policy.name));
        }
        for name in [
            "control.chat.create",
            "control.chat.send",
            "control.room.post",
            "control.chat.lenses",
            "control.project.request",
            "control_github_open_pr",
        ] {
            assert!(checkpoint(name));
        }
        for name in ["control.chat.send", "unknown", "control.browser.capture"] {
            assert!(!bridge_forward(name));
        }
        assert!(bridge_forward("control.project.request"));
        assert!(!http_definitions()
            .iter()
            .any(|d| d["name"] == "control.browser.capture"));
    }
    #[test]
    fn every_worker_schema_is_registered_for_ui_and_provider_discovery() {
        let names=control_context::worker_mcp_names();
        for definition in worker_definitions(true) {
            let name=definition["name"].as_str().unwrap();
            assert!(control_context::tool_for_mcp(name).is_some(),"Missing UI registration: {name}");
            let worker_name=name.replace('.',"_");
            assert!(names.contains(&worker_name.as_str()),"Missing provider discovery: {name}");
        }
    }
    #[test]
    fn attempt_tools_route_each_name_to_exactly_one_group() {
        let workers = worker_definitions(false);
        let mut seen = std::collections::HashSet::new();
        for &tools in &ATTEMPT_TOOLS {
            let definitions = tools.definitions();
            assert!(!definitions.is_empty());
            for definition in definitions {
                let name = definition["name"].as_str().unwrap();
                assert_eq!(ATTEMPT_TOOLS.iter().filter(|other| other.known(name)).count(), 1, "{name}");
                assert!(std::ptr::addr_eq(attempt_tools(name).unwrap(), tools), "{name}");
                assert!(workers.iter().any(|worker| worker["name"] == name), "{name}");
                assert!(seen.insert(name.to_owned()), "{name}");
            }
        }
        for name in ["control_agent_result", "control_workspace_run", "control.chat.send", "unknown", ""] {
            assert!(attempt_tools(name).is_none(), "{name}");
        }
    }
}
