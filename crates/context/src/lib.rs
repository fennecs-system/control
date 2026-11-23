//! Pure lens selection. Resource permissions remain outside this function.
pub mod agent_policy;
pub mod commands;
/// Arguments of the GitHub tools and the `/github` command grammar that produces them.
pub mod github;
mod library;
pub mod limits;
pub mod persona;
pub mod tool_policy;
pub mod turn_context;
pub mod turn_mailbox;
pub mod working_state;
pub use library::{
    Audience, Definition, DocumentInput, Guidance, LensVersion, Library, PackageInput, Resolved, package_reference,
    valid_id,
};
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
    pub mcp_names: &'static [Mcp],
}
/// One protocol name of a catalogued tool and how managed workers see it.
/// The single source for worker tool exposure: which roles load the schema
/// up front, the index phrase for everyone else, and judge permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mcp {
    pub name: &'static str,
    /// Index line for a deferred worker tool: at most eight words.
    pub phrase: &'static str,
    /// Roles (`roles::*` bits) whose sessions list the schema at start.
    pub core: u8,
    pub judge: Judge,
    /// Tight description for an always-loaded schema; `control_tool_search`
    /// adds only the properties the listing compacts away. Empty keeps the
    /// full description.
    pub brief: &'static str,
}
/// Whether a judge (read-only reviewer) may call a worker tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judge {
    Denied,
    Allowed,
    /// Only an approved advisor judge (its one effect: notes to its team).
    Advisor,
}
/// Role bits for `Mcp::core`.
pub mod roles {
    use crate::Audience;
    pub const MEMBER: u8 = 1;
    pub const LEAD: u8 = 2;
    pub const JUDGE: u8 = 4;
    pub const WORKERS: u8 = MEMBER | LEAD;
    pub const ALL: u8 = WORKERS | JUDGE;
    pub fn of(role: Audience) -> u8 {
        match role {
            Audience::Member => MEMBER,
            Audience::Lead => LEAD,
            Audience::Advisor => JUDGE,
        }
    }
}
/// An operator-protocol name with no worker exposure of its own.
const fn op(name: &'static str) -> Mcp {
    Mcp {
        name,
        phrase: "",
        core: 0,
        judge: Judge::Denied,
        brief: "",
    }
}
/// A worker tool, deferred for every role until `core` says otherwise.
const fn w(name: &'static str, phrase: &'static str) -> Mcp {
    Mcp {
        name,
        phrase,
        core: 0,
        judge: Judge::Denied,
        brief: "",
    }
}
impl Mcp {
    const fn core(self, core: u8) -> Self {
        Mcp { core, ..self }
    }
    const fn judge(self) -> Self {
        Mcp {
            judge: Judge::Allowed,
            ..self
        }
    }
    const fn advisor(self) -> Self {
        Mcp {
            judge: Judge::Advisor,
            ..self
        }
    }
    const fn brief(self, brief: &'static str) -> Self {
        Mcp { brief, ..self }
    }
}
use roles::{ALL, JUDGE, LEAD, WORKERS};
pub const TOOLS: [Tool; 37] = [
    Tool {
        name: "tools.search", title: "Tool search", description: "Deferred worker tools · schemas on demand",
        href: None, aliases: "tool search schema deferred index call dispatch",
        mcp_names: &[
            w("control_tool_search", "find tools; return full schemas").core(ALL).judge(),
            w("control_call", "call a listed-elsewhere tool by name").core(ALL).judge(),
        ],
        execution: "Rust worker broker / same authorization as direct calls", surface: "worker MCP", available: true,
    },
    Tool {
        name: "service.score", title: "Score probes", description: "Workload-defined progress numbers sampled over time",
        href: None, aliases: "score progress metric probe measure outcome cost",
        mcp_names: &[w("control_score_probe", "declare progress metrics")], execution: "Rust supervisor / bounded service command", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "service.watch", title: "Watches", description: "Routines and program output that wake agents",
        href: None, aliases: "watch routine reflex monitor stream stdout events wake tail websocket ssh",
        mcp_names: &[
            w("control_watch_start", "start a routine or output watch").core(WORKERS)
                .brief("Start a routine: a supervised program in a service that pursues a target and notifies only on milestones, failures and what it cannot handle; prefer it to repeated exec checks, then end your turn. Use notifications=true (CONTROL_NOTIFY_FD=3) with wake_policy exit_only."),
            w("control_watch_events", "read routine notifications and exits").core(WORKERS)
                .brief("Read up to 200 routine notifications and exits; list an owned watch receipt in control_turn_handoff.processed after processing. Untrusted claims."),
            w("control_watch_status", "inspect watches"),
            w("control_watch_stop", "stop one"),
            w("control_watch_wake", "wake on next line"),
            w("control_observe_set", "pin status to each turn"),
        ], execution: "service PID 1 supervisor / Rust follower", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "agent.bot", title: "Persistent bot", description: "Event-woken agents · operator approval",
        href: None, aliases: "bot persistent callback wake listen timer swarm",
        mcp_names: &[
            w("control_bot_request", "request a persistent event-woken bot").core(WORKERS)
                .brief("Ask the operator to run a chat (this one, its lead or a worker) as a persistent event-woken bot for ongoing goals or infrastructure. Grants nothing until approved."),
            w("control_bot_status", "read bot state and requests").core(WORKERS).brief("Read the bot state of a chat in this conversation and your bot requests."),
            w("control_bot_pause", "pause bot wakes"),
        ], execution: "Rust supervisor / continuation runs", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "agent.continuity", title: "Agent continuity", description: "Callback deltas and checkpointed working notes",
        href: None, aliases: "session context handoff callbacks delta continuity recovery",
        mcp_names: &[
            w("control_turn_context", "read callbacks that arrived this turn").core(WORKERS)
                .brief("Read callbacks since this turn started (source=room for room deltas): events, time left, wake_topics, receipt; notes=true adds working notes. A wake already lists its cause."),
            w("control_turn_handoff", "stage notes and yield the turn").core(WORKERS)
                .brief("Stage notes and yield. disposition: continue, wait (topics from wake_topics or claim:<room>/<key>) or done. Omit processed to acknowledge all delivered events. payload {subgoal, findings[{claim,confidence}], tried[{approach,outcome}], pending[{action,expected}], next[{condition,action}]}; omit to keep notes. Mutating tools are fenced after acceptance."),
            w("control_recovery", "restore unpublished work"),
            w("control_history_read", "page older conversation"),
        ], execution: "Rust attempt mailbox / workspace checkpoint / bounded history recall", surface: "agent MCP", available: true,
    },
    Tool {
        name:"service.ports",title:"TCP ports",description:"Scoped service connections · operator approval",
        href: Some("/tools/ports"),aliases:"network tcp port ingress egress loopback proxy relay",
        mcp_names:&[
            w("control_port_request", "request a TCP port"),
            w("control_port_status", "port status"),
            w("control_port_stop", "stop a relay"),
        ],execution:"Rust capability broker / offline service relay",surface:"chat / local TCP",available:true,
    },
    Tool {
        name: "service.persistent", title: "Persistent service", description: "Private offline tools · leased lifetime",
        href: Some("/tools/services"), aliases: "bot persistent container lifecycle attach stop service",
        mcp_names: &[
            w("control_service_start", "start a persistent offline service").core(WORKERS)
                .brief("Start a chat-owned offline container that survives turns. Keep ALL task data in /workspace (persistent); other paths are lost. Foreground command; confirm with control_service_status."),
            w("control_service_attach", "one service's status, logs"),
            w("control_service_status", "list services and their leases").core(WORKERS).brief("Service status and bounded logs; without service_id, list services. detail node: node capacity."),
            w("control_service_stop", "stop this chat's or a teammate's service").core(WORKERS)
                .brief("Stop this chat's or a teammate's service (status: team_services); /workspace is kept."),
            w("control_service_exec", "run a command in a service").core(WORKERS)
                .brief("Run a bounded command in a chat-owned service; inline result within wait_seconds, else an execution_id. Killed at timeout_seconds; repeated or long work belongs in control_watch_start."),
            w("control_service_exec_status", "await or read a service command").core(WORKERS)
                .brief("Await or read a service command by execution_id (from exec or copy): output, exit code, outcome."),
            w("control_service_copy", "copy workspace files into a service").core(WORKERS)
                .brief("Copy a file or tree from /workspace into a service /workspace (relative paths; secrets and build dirs skipped). If running, await control_service_exec_status before ending your turn."),
        ],
        execution: "credential-free Nix container / Rust supervisor", surface: "chat / worker MCP", available: true,
    },
    Tool {
        name: "github.pull_request", title: "GitHub", description: "Scoped PR inspection · improvement draft publication",
        href: None, aliases: "github git pull request draft publish review",
        mcp_names: &[
            w("control_github_open_pr", "draft PR"),
            w("control_github_read", "read PR, reviews, checks"),
            w("control_github_write", "comment, edit or merge"),
        ], execution: "Rust credential broker / GitHub API", surface: "improve chat / MCP", available: true,
    },
    Tool {
        name:"agent.delegate",title:"Spawn agents",description:"Background tasks · blocking worker or judge",
        href: None,aliases:"persona review judge subagent spawn delegation",
        mcp_names:&[
            w("control_agent_spawn_blocking", "await a child or judge review"),
            w("control_agent_spawn", "start background agents; returns immediately").core(WORKERS)
                .brief("Start background workers, one objective each; returns at once; never poll. Children own goals, inheriting model and genes unless given a persona."),
            w("control_agent_spawn_status", "batch status"),
            w("control_agent_spawn_cancel", "cancel a batch"),
            w("control_agent_result", "read a child result").core(JUDGE).judge(),
            w("control_agent_message", "message a participant chat directly").core(ALL).advisor()
                .brief("Message a peer or descendant in your task tree (IDs from control_room_read); read at its next wake unless urgent=true. Do not also broadcast. services shares resources you hold."),
        ],execution:"local Rust / durable bounded task queue",surface:"chat / worker MCP",available:true,
    },
    // Offered only by the `advisor` lens. Selecting it makes the option exist
    // for that task tree; the operator's bot approval is still the authority.
    Tool {
        name: "team.advisor", title: "Team advisor", description: "Opt-in read-only judge its team's members ask · operator approval",
        href: None, aliases: "advisor judge ask question method lead swarm",
        mcp_names: &[], execution: "Rust judge bot", surface: "control_agent_spawn role=judge · control_bot_request", available: true,
    },
    // Offered only by the `acceptance` lens: its judge
    // hears only its team's completion.
    Tool {
        name: "team.acceptance", title: "Acceptance check", description: "Opt-in read-only judge that compares a finished result with the request · operator approval",
        href: None, aliases: "acceptance check done finished scope quality judge request",
        mcp_names: &[], execution: "Rust lifecycle fact / judge bot", surface: "control_agent_spawn persona=acceptance · control_bot_request", available: true,
    },
    // Offered only by the `monitor` lens: its judge
    // hears its team's host facts.
    Tool {
        name: "team.monitor", title: "Team monitor", description: "Opt-in read-only judge woken by host facts about its team · operator approval",
        href: None, aliases: "monitor observer stall gaps blocked bottleneck facts judge",
        mcp_names: &[], execution: "Rust lifecycle facts / judge bot", surface: "control_agent_spawn persona=monitor · control_bot_request", available: true,
    },
    // Offered only by the `timers` lens: a team's agents may set check-in
    // timers (a bot interval or a wait recheck). Without it they wait for events.
    Tool {
        name: "bot.timer", title: "Check-in timers", description: "Opt-in: agents may wake themselves on a timer (a bot interval or a wait recheck)",
        href: None, aliases: "timer cron schedule recheck interval check-in poll",
        mcp_names: &[], execution: "Rust bot supervisor", surface: "control_bot_request every_minutes · control_turn_handoff recheck_seconds", available: true,
    },
    // Offered only by the `antibody` lens. Selecting it on an operator-owned root
    // chat makes a node-wide antibody option exist; bot approval still decides.
    Tool {
        name: "node.antibody", title: "Antibodies", description: "Opt-in read-only observers that tag pathologies in any team and notify its lead or the operator · operator approval",
        href: None, aliases: "antibody watcher immune counter-intel sentinel observer patrol pathology loop polling churn cost burn",
        mcp_names: &[], execution: "Rust lifecycle and pathology facts / judge bot", surface: "control_agent_spawn role=judge · control_bot_request", available: true,
    },
    Tool {
        name: "intel.brief", title: "Intel brief", description: "Read-only team and node summary: goals, progress, cost, pathologies, open requests",
        href: None, aliases: "intel brief daily report summary progress cost tokens pathology",
        mcp_names: &[
            op("control.intel.brief"),
            w("control_intel_brief", "team or node progress, cost, pathologies").judge()
                .brief("Read-only Markdown summary of your team (an antibody: any team or the node): goals, progress, tokens, pathologies, open operator requests."),
        ],
        execution: "pure Rust over node state", surface: "operator MCP / worker MCP / channels", available: true,
    },
    Tool {
        name: "operator.notify", title: "Operator notice", description: "Rate-limited notice to the operator's channels; no decision requested",
        href: None, aliases: "notify alert operator discord escalate notice",
        mcp_names: &[w("control_operator_notify", "notify the operator (rate-limited)").advisor()],
        execution: "local Rust / channel adapters", surface: "worker MCP / channels", available: true,
    },
    Tool {
        name: "workspace.commands", title: "Workspace commands", description: "Offline jobs · live output",
        href: None, aliases: "shell terminal build test logs cancel environment",
        mcp_names: &[
            w("control_workspace_start", "async workspace command").core(JUDGE).judge(),
            w("control_workspace_status", "await it").core(JUDGE).judge(),
            w("control_workspace_cancel", "cancel it").judge(),
        ],
        execution: "credential-free Nix container", surface: "chat / TUI", available: true,
    },
    Tool {
        name: "preview.serve", title: "Local preview", description: "Nix service · private localhost origin",
        href: None, aliases: "ports proxy service browser preview tunnel",
        mcp_names: &[
            w("control_preview_start", "serve a localhost preview"),
            w("control_preview_stop", "stop it"),
        ], execution: "credential-free Nix container", surface: "chat / browser", available: true,
    },
    Tool {
        name: "project.request",
        title: "Open project",
        description: "Operator-approved folder · isolated worktree",
        href: None,
        aliases: "open directory code workspace trust mount",
        mcp_names: &[op("control.project.request"), w("control_project_request", "request a host folder")],
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
        mcp_names: &[w("control_design_ask", "ask the user; pauses task").core(LEAD)
            .brief("Ask the user a consequential choice (scope, new access, giving up, irreversible); 2-4 options. Pauses the task: stop after asking.")],
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
        mcp_names: &[op("control.artifacts.list"), op("control.artifact.read")],
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
        mcp_names: &[
            op("control.knowledge.search"), op("control.knowledge.read"), op("control.knowledge.propose"),
            w("control_knowledge_search", "search documents").judge(),
            w("control_knowledge_read", "read a document").judge(),
            w("control_knowledge_propose", "propose for review"),
        ],
        execution: "local Rust server",
        surface: "MCP / operator review",
        available: true,
    },
    Tool {
        name: "packages.reuse",
        title: "Tool packages",
        description: "Operator-admitted reusable tools · lens opt-in",
        href: None,
        aliases: "tool package reuse version client library install",
        mcp_names: &[
            op("control.packages.search"), op("control.package.read"), op("control.package.import"), op("control.package.revoke"),
            w("control_packages_search", "search packages").judge(),
            w("control_package_read", "read one").judge(),
        ],
        execution: "local Rust / node Git store / per-attempt copy",
        surface: "lens / worker MCP / operator CLI",
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
        mcp_names: &[op("control.web.search"), w("control_web_search", "web search").judge()],
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
        mcp_names: &[
            op("control.rust.run"), w("control_rust_run", "run a snippet").judge(),
            op("control.rust.bench"), w("control_rust_bench", "benchmark code").judge(),
        ],
        execution: "offline Nix container",
        surface: "Rust playground",
        available: true,
    },
    Tool {
        name: "model.generate",
        title: "Local model",
        description: "Bounded text inference · configured private endpoint",
        href: None,
        aliases: "llm local model llama qwen gguf inference generate text completion",
        mcp_names: &[op("control.model.generate"), w("control_model_generate", "local text generation")],
        execution: "Rust adapter / operator-run inference service",
        surface: "worker MCP / operator MCP / CLI",
        available: true,
    },
    Tool {
        name: "search.path",
        title: "Path search",
        description: "A* / Dijkstra over an inline graph or grid",
        href: None,
        aliases: "route routing shortest path planning graph grid astar dijkstra navigation",
        mcp_names: &[op("control.search.path"), w("control_search_path", "A*/Dijkstra over graph or grid").judge()],
        execution: "pure Rust",
        surface: "MCP",
        available: true,
    },
    Tool {
        name: "solver.solve",
        title: "Solver",
        description: "Optimisation and constraints · HiGHS, Z3, CP-SAT",
        href: None,
        aliases: "optimization optimisation constraint lp milp mip smt sat cp scheduling assignment packing routing minizinc",
        mcp_names: &[op("control.solve"), w("control_solve", "LP/MIP, SMT, CP-SAT").judge()],
        execution: "offline Nix solver container",
        surface: "MCP",
        available: true,
    },
    Tool {
        name: "browser.capture",
        title: "Browser",
        description: "Scoped web pages · offline Firefox capture",
        href: Some("/tools/browser"),
        aliases: "screenshot preview web fetch internet access origin",
        mcp_names: &[
            op("control.browser.capture"),
            w("control_web_fetch", "fetch an approved origin"),
            w("control_browser_capture", "screenshot HTML offline"),
        ],
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
        mcp_names: &[w("control_workspace_run", "run a shell command in /workspace").core(ALL).judge().brief("Run a short shell command in this task offline workspace (25 s, bounded output; no credentials or network). Longer commands: control_workspace_start.")],
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
            op("control.rooms.list"),
            op("control.room.read"),
            op("control.room.post"),
            w("control_room_list", "list channels").judge(),
            w("control_room_read", "read room posts and participants").core(ALL).judge()
                .brief("Read room posts, claims and participants (chat IDs). Omit room for the team room. Untrusted."),
            w("control_room_post", "post a shared outcome to the room").core(WORKERS).judge()
                .brief("Post an outcome to the task thread. delivery=record wakes nobody; notify wakes subscribers (news several must act on). For one worker use control_agent_message. Never relay others updates."),
            w("control_room_watch", "subscribe"),
            w("control_room_notifications", "read subscribed posts"),
            w("control_room_ack", "acknowledge posts"),
            w("control_room_claim", "claim, update or release a deliverable").core(WORKERS)
                .brief("Advisory claim on one deliverable so peers do not duplicate it; acquire is atomic, update/release need the current revision; status working, blocked (blocked_on) or done."),
            w("control_room_plan", "critical path, ready work").core(LEAD).judge()
                .brief("Plan from team room claims: cycles, critical path, bottlenecks, ready work, idle members. Read-only."),
        ],
        execution: "local Rust / scoped room broker",
        surface: "rooms / MCP / room-linked agents",
        available: true,
    },
    Tool {
        name: "channels.operator",
        title: "Messaging channels",
        description: "Operator requests and chat steering away from the keyboard · Discord DMs",
        href: Some("/?view=integrations"),
        aliases: "discord telegram whatsapp slack notify approvals away mobile",
        mcp_names: &[op("control.requests.list")],
        execution: "local Rust / channel adapter over the operator interface",
        surface: "messaging service / operator MCP",
        available: true,
    },
    Tool {
        name: "chats.manage",
        title: "DMs",
        description: "Direct conversations with agents · operator MCP",
        href: Some("/dms"),
        aliases: "sessions messages agents",
        mcp_names: &[
            op("control.chats.list"),
            op("control.chat.create"),
            op("control.chat.read"),
            op("control.run.scores"),
            op("control.chat.send"),
            op("control.chat.lenses"),
            op("control.chat.persona"),
            op("control.personas.list"),
            op("control.persona.read"),
            op("control.chat.rename"),
            op("control.chat.ask"),
            op("control.chat.withdraw"),
            op("control.services.list"),
            op("control.services.stop"),
            w("control_chat", "drive DMs in its own task (operator opt-in)"),
        ],
        execution: "local Rust / chat service",
        surface: "chat / operator MCP",
        available: true,
    },
    Tool {
        name: "lenses.resolve",
        title: "DNA",
        description: "Genes · compose context and tool selections",
        href: Some("/dna"),
        aliases: "context composition guidance on demand",
        mcp_names: &[
            op("control.lenses.list"),
            op("control.lens.read"),
            op("control.lenses.resolve"),
            w("control_guidance", "read captured guidance by topic").core(ALL).judge()
                .brief("Read a guidance topic from your prompt index (or session, rooms); no topic lists them."),
        ],
        execution: "pure Rust / lens library",
        surface: "lens library / MCP",
        available: true,
    },
];
/// Managed-adapter tool names; advertising a name never grants execution authority.
pub fn worker_mcp_names() -> Vec<&'static str> {
    TOOLS
        .iter()
        .filter(|tool| tool.available)
        .flat_map(|tool| tool.mcp_names.iter().map(|m| m.name))
        .filter(|name| name.starts_with("control_"))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
/// UI discovery metadata only; protocol schemas and execution authority remain with their adapters.
pub fn tool_for_mcp(name: &str) -> Option<&'static Tool> {
    TOOLS
        .iter()
        .find(|tool| tool.available && tool.mcp_names.iter().any(|m| m.name == name))
}
/// The catalogue entry of one available protocol name.
pub fn mcp(name: &str) -> Option<&'static Mcp> {
    TOOLS
        .iter()
        .filter(|tool| tool.available)
        .flat_map(|tool| tool.mcp_names)
        .find(|m| m.name == name)
}
/// Worker spelling of a dotted operator name that has an underscore sibling
/// in the same family (`control.web.search` → `control_web_search`).
pub fn worker_name(name: &str) -> Option<&'static str> {
    let family = tool_for_mcp(name)?;
    name.contains('.')
        .then(|| name.replace('.', "_"))
        .and_then(|underscored| family.mcp_names.iter().find(|m| m.name == underscored).map(|m| m.name))
}
/// The dotted operator name a worker spelling stands for, if any.
pub fn dotted_name(name: &str) -> Option<&'static str> {
    tool_for_mcp(name)?
        .mcp_names
        .iter()
        .find(|m| m.name.contains('.') && m.name.replace('.', "_") == name)
        .map(|m| m.name)
}
/// Whether `role`'s sessions list this worker tool's schema at start.
pub fn loaded(name: &str, role: Audience) -> bool {
    mcp(name).is_some_and(|m| m.core & roles::of(role) != 0)
}
/// Host judge policy: read-only inspection, isolated verification and room
/// collaboration; an approved advisor may also send notes to its lead.
pub fn judge_may_call(name: &str, advisor: bool) -> bool {
    mcp(name).is_some_and(|m| match m.judge {
        Judge::Allowed => true,
        Judge::Advisor => advisor,
        Judge::Denied => false,
    })
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
#[path = "lib/tests.rs"]
mod tests;
