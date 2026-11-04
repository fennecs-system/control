//! Pure lens selection. Resource permissions remain outside this function.
mod library;
pub mod agent_policy;
pub mod commands;
pub mod limits;
pub mod tool_policy;
pub mod turn_context;
pub mod working_state;
pub mod turn_mailbox;
pub use library::{valid_id, Definition, DocumentInput, LensVersion, Library, Resolved};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub execution: &'static str,
    pub surface: &'static str,
    pub available: bool,
    pub title: &'static str,
    pub description: &'static str,
    pub href: Option<&'static str>,
    pub aliases: &'static str,
    pub mcp_names: &'static [&'static str],
}
pub const TOOLS: [Tool; 23] = [
    Tool {
        name: "service.watch", title: "Watches", description: "Program output that wakes bots",
        href: None, aliases: "watch monitor stream stdout events wake tail websocket ssh",
        mcp_names: &["control_watch_start", "control_watch_events", "control_watch_status", "control_watch_stop", "control_watch_wake"], execution: "service PID 1 supervisor / Rust follower", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "agent.bot", title: "Persistent bot", description: "Event-woken agents · operator approval",
        href: None, aliases: "bot persistent callback wake listen timer swarm",
        mcp_names: &["control_bot_request", "control_bot_status", "control_bot_pause"], execution: "Rust supervisor / continuation runs", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "agent.continuity", title: "Agent continuity", description: "Callback deltas and checkpointed working notes",
        href: None, aliases: "session context handoff callbacks delta continuity recovery",
        mcp_names: &["control_turn_context", "control_turn_handoff"], execution: "Rust attempt mailbox / workspace checkpoint", surface: "agent MCP", available: true,
    },
    Tool {
        name:"service.ports",title:"TCP ports",description:"Scoped service connections · operator approval",
        href: Some("/tools/ports"),aliases:"network tcp port ingress egress loopback proxy relay",
        mcp_names:&["control_port_request","control_port_status","control_port_stop"],execution:"Rust capability broker / offline service relay",surface:"chat / local TCP",available:true,
    },
    Tool {
        name: "service.persistent", title: "Persistent service", description: "Private offline tools · leased lifetime",
        href: Some("/tools/services"), aliases: "bot persistent container lifecycle attach stop service",
        mcp_names: &["control_service_start", "control_service_attach", "control_service_status", "control_service_stop", "control_service_exec", "control_service_exec_status"],
        execution: "credential-free Nix container / Rust supervisor", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "github.pull_request", title: "GitHub pull request", description: "Completed improvement · scoped repository",
        href: None, aliases: "github git pull request draft publish review",
        mcp_names: &["control_github_open_pr"], execution: "Rust credential broker / GitHub API", surface: "improve chat / MCP", available: true,
    },
    Tool {
        name:"agent.delegate",title:"Spawn agents",description:"Background tasks · blocking worker or judge",
        href: None,aliases:"persona review judge subagent spawn delegation",
        mcp_names:&["control_agent_spawn_blocking", "control_agent_spawn", "control_agent_spawn_status", "control_agent_spawn_cancel", "control_agent_result", "control_agent_message"],execution:"local Rust / durable bounded task queue",surface:"chat / worker MCP",available:true,
    },
    Tool {
        name: "workspace.commands", title: "Workspace commands", description: "Offline jobs · live output",
        href: None, aliases: "shell terminal build test logs cancel environment",
        mcp_names: &["control_workspace_start", "control_workspace_status", "control_workspace_cancel"],
        execution: "credential-free Nix container", surface: "chat / TUI", available: true,
    },
    Tool {
        name: "preview.serve", title: "Local preview", description: "Nix service · private localhost origin",
        href: None, aliases: "ports proxy service browser preview tunnel",
        mcp_names: &["control_preview_start", "control_preview_stop"], execution: "credential-free Nix container", surface: "chat / browser", available: true,
    },
    Tool {
        name: "project.request",
        title: "Open project",
        description: "Operator-approved folder · isolated worktree",
        href: None,
        aliases: "open directory code workspace trust mount",
        mcp_names: &["control.project.request", "control_project_request"],
        execution: "local Rust / explicit operator approval",
        surface: "chat / TUI / MCP",
        available: true,
    },
    Tool {
        name: "design.ask",
        title: "Design decision",
        description: "Agent question · pauses for your answer",
        href: None,
        aliases: "architecture options recommendation",
        mcp_names: &["control_design_ask"],
        execution: "local Rust / durable task pause",
        surface: "chat / TUI / Discord",
        available: true,
    },
    Tool {
        name: "artifacts.read",
        title: "Artifacts",
        description: "Published files · read only",
        href: None,
        aliases: "files outputs viewer",
        mcp_names: &["control.artifacts.list", "control.artifact.read"],
        execution: "local Rust / published files",
        surface: "chat viewer / MCP",
        available: true,
    },
    Tool {
        name: "context.inspect",
        title: "Context inspector",
        description: "Pure context preview",
        href: Some("/tool-window/"),
        aliases: "wasm lens context",
        mcp_names: &[],
        execution: "pure Rust",
        surface: "embedded window / WASM",
        available: true,
    },
    Tool {
        name: "knowledge.propose",
        title: "Knowledge",
        description: "Documents and evidence",
        href: Some("/knowledge"),
        aliases: "notes files",
        mcp_names: &["control.knowledge.search", "control.knowledge.read", "control.knowledge.propose", "control_knowledge_search", "control_knowledge_read", "control_knowledge_propose"],
        execution: "local Rust server",
        surface: "MCP / operator review",
        available: true,
    },
    Tool {
        name: "repo.read",
        title: "Repository read",
        description: "Planned",
        href: None,
        aliases: "",
        mcp_names: &[],
        execution: "Codex container",
        surface: "artifact viewer",
        available: false,
    },
    Tool {
        name: "repo.patch",
        title: "Repository patch",
        description: "Planned",
        href: None,
        aliases: "",
        mcp_names: &[],
        execution: "Codex container",
        surface: "patch review",
        available: false,
    },
    Tool {
        name: "search.read",
        title: "Search",
        description: "Web · DuckDuckGo / Bing",
        href: Some("/tools/search"),
        aliases: "web search",
        mcp_names: &["control.web.search", "control_web_search"],
        execution: "host search broker",
        surface: "search window",
        available: true,
    },
    Tool {
        name: "rust.run",
        title: "Rust playground",
        description: "Offline · standard library",
        href: Some("/tools/rust"),
        aliases: "code compile execute benchmark performance timing",
        mcp_names: &["control.rust.run", "control_rust_run", "control.rust.bench", "control_rust_bench"],
        execution: "offline Nix container",
        surface: "Rust playground",
        available: true,
    },
    Tool {
        name: "browser.capture",
        title: "Browser",
        description: "Scoped web pages · offline Firefox capture",
        href: Some("/tools/browser"),
        aliases: "screenshot preview web fetch internet access origin",
        mcp_names: &["control.browser.capture", "control_web_fetch", "control_browser_capture"],
        execution: "Rust HTTP capability broker / offline Nix Firefox",
        surface: "screenshot",
        available: true,
    },
    Tool {
        name: "workspace.run",
        title: "Workspace command",
        description: "Claude agent · offline Nix workspace",
        href: None,
        aliases: "shell rust rg fd edit command",
        mcp_names: &["control_workspace_run"],
        execution: "offline Nix container",
        surface: "Claude agent session",
        available: true,
    },
    Tool {
        name: "rooms.collaborate",
        title: "Rooms",
        description: "Shared updates · room-linked agents",
        href: Some("/?view=rooms"),
        aliases: "collaboration messages",
        mcp_names: &[
            "control.rooms.list",
            "control.room.read",
            "control.room.post",
            "control_room_list",
            "control_room_read",
            "control_room_post",
            "control_room_watch",
            "control_room_notifications",
            "control_room_ack",
        ],
        execution: "local Rust / scoped room broker",
        surface: "rooms / MCP / room-linked agents",
        available: true,
    },
    Tool {
        name: "chats.manage",
        title: "Chats",
        description: "Conversations · operator MCP",
        href: Some("/chats"),
        aliases: "sessions messages agents",
        mcp_names: &[
            "control.chats.list",
            "control.chat.create",
            "control.chat.read",
            "control.chat.send",
            "control.chat.lenses",
        ],
        execution: "local Rust / chat service",
        surface: "chat / operator MCP",
        available: true,
    },
    Tool {
        name: "lenses.resolve",
        title: "Lenses",
        description: "Library · compose context and tool selections",
        href: Some("/lenses"),
        aliases: "context composition",
        mcp_names: &[
            "control.lenses.list",
            "control.lens.read",
            "control.lenses.resolve",
        ],
        execution: "pure Rust / lens library",
        surface: "lens library / MCP",
        available: true,
    },
];
/// Managed-adapter tool names; advertising a name never grants execution authority.
pub fn worker_mcp_names() -> Vec<&'static str> {
    TOOLS.iter().filter(|tool|tool.available).flat_map(|tool|tool.mcp_names.iter().copied())
        .filter(|name|name.starts_with("control_")).collect::<std::collections::BTreeSet<_>>().into_iter().collect()
}
/// UI discovery metadata only; protocol schemas and execution authority remain with their adapters.
pub fn tool_for_mcp(name: &str) -> Option<&'static Tool> {
    TOOLS
        .iter()
        .find(|tool| tool.available && tool.mcp_names.contains(&name))
}
pub fn lens<'a>(category: &str, tools: &'a [Tool]) -> Vec<&'a Tool> {
    compose(&[category], tools)
}
pub fn inspect(category: &str, authorized: &[&str]) -> String {
    inspect_composed(&[category], authorized)
}
/// Compose selections by union, retaining registry order and removing shared descriptors.
pub fn compose<'a>(categories: &[&str], tools: &'a [Tool]) -> Vec<&'a Tool> {
    let library = Library::default();
    let roots = categories
        .iter()
        .filter(|id| library.get(id).is_some())
        .map(|id| (*id).into())
        .collect::<Vec<_>>();
    let resolved = library.resolve(&roots, &[]).unwrap_or_default();
    tools
        .iter()
        .filter(|tool| resolved.tools.iter().any(|id| id == tool.name))
        .collect()
}
pub fn inspect_composed(categories: &[&str], authorized: &[&str]) -> String {
    let mut text = format!(
        "Lenses: {}\nPure selection over the supplied registry.\n\n",
        categories.join(" + ")
    );
    for tool in compose(categories, &TOOLS) {
        let status = if !tool.available {
            "not connected"
        } else if authorized.contains(&tool.name) {
            "permitted in this local example"
        } else {
            "not authorized"
        };
        text.push_str(&format!(
            "{}\n  execution: {}\n  presentation: {}\n  status: {}\n\n",
            tool.name, tool.execution, tool.surface, status
        ));
    }
    text.push_str("Selection does not grant authority. No side effects occur here.");
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn available_tools_have_unique_discovery_metadata() {
        let mut ids = std::collections::BTreeSet::new();
        let mut protocol_names = std::collections::BTreeSet::new();
        for tool in TOOLS {
            assert!(ids.insert(tool.name));
            if tool.available {
                assert!(!tool.title.is_empty() && !tool.description.is_empty());
                if let Some(href) = tool.href {
                    assert!(href.starts_with('/') && !href.starts_with("//"));
                }
            }
            for name in tool.mcp_names {
                assert!(protocol_names.insert(name), "Duplicate MCP name: {name}");
                assert_eq!(tool_for_mcp(name), Some(&tool));
            }
        }
        assert!(tool_for_mcp("control_agent_delegate").is_none());
    }
    #[test]
    fn selection_is_deterministic_and_does_not_expand_unknown_lenses() {
        assert_eq!(lens("engineering", &TOOLS), lens("engineering", &TOOLS));
        assert!(lens("missing", &TOOLS).is_empty());
        assert!(!lens("research", &TOOLS)
            .iter()
            .any(|t| t.name == "repo.patch"));
    }
    #[test]
    fn selected_tools_still_need_authority() {
        assert!(inspect("engineering", &[]).contains("not authorized"));
        assert!(inspect("engineering", &["context.inspect"])
            .contains("permitted in this local example"));
        assert!(inspect("engineering", &["repo.patch"]).contains("not connected"));
    }
    #[test]
    fn composition_is_deduplicated_and_order_independent() {
        let combined = compose(&["engineering", "research"], &TOOLS);
        assert_eq!(combined.len(), 5);
        assert_eq!(
            combined,
            compose(&["research", "engineering", "research"], &TOOLS)
        );
        assert!(compose(&[], &TOOLS).is_empty());
        assert!(inspect_composed(&["engineering", "research"], &[]).contains("not authorized"));
    }
}
