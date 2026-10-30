//! Pure lens selection. Resource permissions remain outside this function.
mod library;
pub use library::{valid_id, Definition, DocumentInput, LensVersion, Library, Resolved};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub execution: &'static str,
    pub surface: &'static str,
    pub available: bool,
    pub title: &'static str,
    pub description: &'static str,
    pub href: &'static str,
    pub aliases: &'static str,
    pub mcp_names: &'static [&'static str],
}
pub const TOOLS: [Tool; 18] = [
    Tool {
        name: "github.pull_request", title: "GitHub pull request", description: "Completed improvement · scoped repository",
        href: "/chats", aliases: "github git pull request draft publish review",
        mcp_names: &["control_github_open_pr"], execution: "Rust credential broker / GitHub API", surface: "improve chat / MCP", available: true,
    },
    Tool {
        name:"agent.delegate",title:"Delegate agent",description:"Isolated workers · independent cross-provider judge",
        href:"/chats",aliases:"persona review judge subagent spawn delegation",
        mcp_names:&["control_agent_delegate", "control_agent_result"],execution:"local Rust / durable bounded task queue",surface:"chat / worker MCP",available:true,
    },
    Tool {
        name: "workspace.commands", title: "Workspace commands", description: "Offline jobs · live output",
        href: "/chats", aliases: "shell terminal build test logs cancel environment",
        mcp_names: &["control_workspace_start", "control_workspace_status", "control_workspace_cancel"],
        execution: "credential-free Nix container", surface: "chat / TUI", available: true,
    },
    Tool {
        name: "preview.serve", title: "Local preview", description: "Nix service · private localhost origin",
        href: "/chats", aliases: "ports proxy service browser preview tunnel",
        mcp_names: &["control_preview_start", "control_preview_stop"], execution: "credential-free Nix container", surface: "chat / browser", available: true,
    },
    Tool {
        name: "project.request",
        title: "Open project",
        description: "Operator-approved folder · isolated worktree",
        href: "/chats",
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
        href: "/chats",
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
        href: "/chats",
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
        href: "/tool-window/",
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
        href: "/knowledge",
        aliases: "notes files",
        mcp_names: &[],
        execution: "local Rust server",
        surface: "review form",
        available: true,
    },
    Tool {
        name: "repo.read",
        title: "Repository read",
        description: "Planned",
        href: "",
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
        href: "",
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
        href: "/tools/search",
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
        href: "/tools/rust",
        aliases: "code compile execute",
        mcp_names: &["control.rust.run", "control_rust_run"],
        execution: "offline Nix container",
        surface: "Rust playground",
        available: true,
    },
    Tool {
        name: "browser.capture",
        title: "Browser",
        description: "Offline HTML · Firefox",
        href: "/tools/browser",
        aliases: "screenshot preview",
        mcp_names: &["control.browser.capture"],
        execution: "offline Nix Firefox",
        surface: "screenshot",
        available: true,
    },
    Tool {
        name: "workspace.run",
        title: "Workspace command",
        description: "Claude agent · offline Nix workspace",
        href: "/chats",
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
        href: "/?view=rooms",
        aliases: "collaboration messages",
        mcp_names: &[
            "control.rooms.list",
            "control.room.read",
            "control.room.post",
            "control_room_read",
            "control_room_post",
        ],
        execution: "local Rust / scoped room broker",
        surface: "rooms / MCP / room-linked agents",
        available: true,
    },
    Tool {
        name: "chats.manage",
        title: "Chats",
        description: "Conversations · operator MCP",
        href: "/chats",
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
        href: "/lenses",
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
                assert!(tool.href.starts_with('/') && !tool.href.starts_with("//"));
            }
            for name in tool.mcp_names {
                assert!(protocol_names.insert(name), "Duplicate MCP name: {name}");
                assert_eq!(tool_for_mcp(name), Some(&tool));
            }
        }
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
