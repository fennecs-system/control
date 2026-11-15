//! Pure MCP transport metadata, shared by the server and standalone TUI.
//! This classifies calls; execution and actor authorization stay in the brokers.
/// MCP protocol revisions this node speaks, newest first.
pub const MCP_VERSIONS: [&str; 2] = ["2025-11-25", "2025-03-26"];
pub const MCP_LATEST: &str = MCP_VERSIONS[0];
/// The revision answered to `initialize`: the client's own when supported,
/// otherwise the latest.
pub fn negotiate(requested: Option<&str>) -> &'static str {
    MCP_VERSIONS.into_iter().find(|version| Some(*version) == requested).unwrap_or(MCP_LATEST)
}
pub fn supported(version: &str) -> bool {
    MCP_VERSIONS.contains(&version)
}
/// Host policy, never a caller-supplied MCP annotation. Unknown tools fail
/// closed. Isolated execution still starts work, so it is fenced after yield.
pub fn after_handoff(name:&str)->bool {
    matches!(name,
        "control_turn_context"|"control_turn_handoff"|"control_history_read"|
        "control_service_status"|"control_service_exec_status"|
        "control_port_status"|"control_bot_status"|
        "control_workspace_status"|"control_watch_status"|"control_watch_events"|
        "control_room_list"|"control_room_read"|"control_room_notifications"|
        "control_knowledge_read"|"control_knowledge_search"|
        "control_agent_result"|"control_agent_spawn_status"|"control_github_read")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Chat,
    Lens,
    Knowledge,
    Room,
    Artifacts,
    Project,
    Github,
    Toolbox,
    Browser,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bridge {
    Hidden,
    Read,
    Local,
    Forward,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub name: &'static str,
    pub route: Route,
    pub checkpoint: bool,
    pub bridge: Bridge,
    pub http: bool,
}
macro_rules! policies {
    ($($name:literal => ($route:ident, $checkpoint:literal, $bridge:ident, $http:literal)),* $(,)?) => {
        pub const POLICIES: &[Policy] = &[$(Policy {name:$name, route:Route::$route, checkpoint:$checkpoint, bridge:Bridge::$bridge, http:$http}),*];
    };
}
policies! {
    "control.chats.list" => (Chat,false,Read,true),
    "control.chat.create" => (Chat,true,Hidden,true),
    "control.chat.lenses" => (Chat,true,Hidden,true),
    "control.chat.read" => (Chat,false,Read,true),
    "control.chat.send" => (Chat,true,Hidden,true),
    "control.lenses.list" => (Lens,false,Read,true),
    "control.lens.read" => (Lens,false,Read,true),
    "control.lenses.resolve" => (Lens,false,Read,true),
    "control.knowledge.search" => (Knowledge,false,Read,true),
    "control.knowledge.read" => (Knowledge,false,Read,true),
    "control.knowledge.propose" => (Knowledge,true,Forward,true),
    "control.rooms.list" => (Room,false,Read,true),
    "control.room.read" => (Room,false,Read,true),
    "control.room.post" => (Room,true,Hidden,true),
    "control.project.request" => (Project,true,Forward,true),
    "control_github_open_pr" => (Github,true,Forward,true),
    "control_github_read" => (Github,false,Read,true),
    "control_github_write" => (Github,true,Forward,true),
    "control.web.search" => (Toolbox,false,Local,true),
    "control.rust.run" => (Toolbox,false,Local,true),
    "control.rust.bench" => (Toolbox,false,Local,true),
    "control.artifacts.list" => (Artifacts,false,Read,true),
    "control.artifact.read" => (Artifacts,false,Read,true),
    "control.browser.capture" => (Browser,false,Local,false),
}
pub fn lookup(name: &str) -> Option<&'static Policy> {
    POLICIES.iter().find(|p| p.name == name)
}
pub fn checkpoint(name: &str) -> bool {
    lookup(name).is_some_and(|p| p.checkpoint)
}

#[cfg(test)]
mod tests;
