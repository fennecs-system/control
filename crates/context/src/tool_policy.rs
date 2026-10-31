//! Pure MCP transport metadata, shared by the server and standalone TUI.
//! This classifies calls; execution and actor authorization stay in the brokers.
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
mod tests {
    use super::*;
    #[test]
    fn metadata_is_unique_and_does_not_infer_authority_from_readonly_hints() {
        let mut names = std::collections::HashSet::new();
        for policy in POLICIES {
            assert!(names.insert(policy.name));
            assert!(!(policy.checkpoint && policy.bridge == Bridge::Read));
        }
        assert!(checkpoint("control_github_open_pr"));
        assert!(!checkpoint("control.rust.run")); // Executes locally, not a state transaction.
        assert_eq!(lookup("control.rust.run").unwrap().bridge, Bridge::Local);
        assert!(lookup("unknown").is_none());
    }
}
