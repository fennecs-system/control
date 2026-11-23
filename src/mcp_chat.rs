//! `control mcp --chat`: the operator's chat loop as MCP tools for an external
//! harness. Every call goes to the running server's operator MCP endpoint over
//! loopback (the same `Hive` connection the TUI uses), so the server's own
//! chat, lens, cancel and service functions do the work under its state lock.
//! This module only checks inputs, gates operator decisions on the node's
//! opt-ins (`control::permissions`) and formats bounded plain text. The same
//! tools, scoped to an agent's own task, back the opt-in `control_chat`
//! worker tool (`AgentTools`). See docs/CHAT-MCP.md.
use control::permissions::Approvals;
use control_client::{Hive, PortDecision};
use serde_json::{Value, json};
use std::io;

mod agent;
pub use agent::Tools as AgentTools;

/// The operator endpoint's tools as this bridge calls them. `Hive` is the
/// production transport; tests serve the same calls in process.
pub(crate) trait Remote {
    fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value>;
    /// Folder or port decisions, which the server takes through the same
    /// confirmed forms as the web panels rather than as chat commands.
    fn decide(
        &mut self,
        _chat: &str,
        _kind: &str,
        _request: &str,
        _allow: bool,
        _loopback: bool,
    ) -> io::Result<String> {
        Err(io::Error::other("Decisions are not available here."))
    }
}
impl Remote for Hive {
    fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value> {
        Hive::tool(self, name, arguments)
    }
    fn decide(&mut self, chat: &str, kind: &str, request: &str, allow: bool, loopback: bool) -> io::Result<String> {
        match (kind, allow) {
            ("folder", true) => self
                .approve_project(chat, request)?
                .result
                .map(|()| "Folder opened.".to_owned())
                .map_err(io::Error::other),
            ("folder", false) => self.dismiss_project(chat, request),
            ("port", _) => self
                .decide_port(
                    chat,
                    request,
                    if allow { PortDecision::Allow } else { PortDecision::Deny },
                    loopback,
                )
                .map(|()| if allow { "Port allowed." } else { "Port denied." }.to_owned()),
            ("knowledge", _) => self.review_knowledge(request, allow).map(|()| {
                if allow {
                    "Proposal accepted."
                } else {
                    "Proposal rejected."
                }
                .to_owned()
            }),
            _ => Err(io::Error::other("Unknown decision.")),
        }
    }
}

/// Upper bound on one tool result's text.
const OUTPUT: usize = 48 * 1024;
const LIST: usize = 30;
const LIST_MAX: usize = 200;
const TEAM_MAX: usize = 200;

struct Tool {
    name: &'static str,
    /// Changes chat state. A restricted caller (for example a Control agent
    /// limited to reading its own task) would list only `write == false`.
    write: bool,
    description: &'static str,
    schema: fn() -> Value,
}
const ID: &str = "Chat ID (c-…), as chats_list shows it.";
const TOOLS: &[Tool] = &[
    Tool {
        name: "chats_list",
        write: false,
        description: "List chats (DMs), most recently active first, with run status (running, waiting, queued, cancelling, completed, failed, cancelled or idle), mode, parent and whether a folder request is pending.",
        schema: || json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":LIST_MAX,"default":LIST},"roots_only":{"type":"boolean","default":false,"description":"Only top-level DMs, not spawned agents."}}}),
    },
    Tool {
        name: "chat_read",
        write: false,
        description: "Read a chat's recent messages, replies and agent activity as plain text, newest last. Page back with before=<first index shown>. Long fields are clipped.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"before":{"type":"integer","minimum":0,"description":"Show messages before this index (default: the end)."},"limit":{"type":"integer","minimum":1,"maximum":20,"default":5},"activity":{"type":"integer","minimum":0,"maximum":40,"default":5,"description":"Latest activity events shown per message."},"max_chars":{"type":"integer","minimum":200,"maximum":8000,"default":1500,"description":"Clip each text field to this many characters."}},"required":["id"]}),
    },
    Tool {
        name: "chat_team",
        write: false,
        description: "List a task's team: the chat and every agent chat spawned from it, with status, plus the live persistent services they own.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID}},"required":["id"]}),
    },
    Tool {
        name: "chat_requests",
        write: false,
        description: "List what a task is waiting on the operator for: design questions, bot requests, port approvals, notices, folder (/open) requests and knowledge proposals across its team. Read only: the operator decides them in Control.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID}},"required":["id"]}),
    },
    Tool {
        name: "chat_create",
        write: true,
        description: "Create a DM as the local operator, optionally with a title, model (as /model accepts, e.g. codex:MODEL, codex+ACCOUNT:MODEL or claude:opus), genes (lens IDs; its DNA) and persona. Returns its ID. Send the prompt with chat_send.",
        schema: || json!({"type":"object","properties":{"title":{"type":"string","minLength":1,"maxLength":32},"model":{"type":"string","maxLength":128},"genes":{"type":"array","items":{"type":"string"},"maxItems":32},"persona":{"type":"string","maxLength":128}}}),
    },
    Tool {
        name: "chat_send",
        write: true,
        description: "Send a prompt to a chat as the local operator, starting or queueing a turn. Plain text only: slash commands are refused (use chat_model, chat_genes, chat_cancel), as are improve-mode chats and chats waiting on a design question.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"message":{"type":"string","minLength":1,"maxLength":control_context::limits::CHAT_MESSAGE_CHARS}},"required":["id","message"]}),
    },
    Tool {
        name: "chat_model",
        write: true,
        description: "Select the model for a chat's future turns (as /model does: BACKEND:MODEL, BACKEND+ACCOUNT:MODEL, or BACKEND+*:MODEL for any enabled account); active turns keep theirs. Omit model to list the configured choices and one example per form with this node's backends and accounts.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"model":{"type":"string","maxLength":128}},"required":["id"]}),
    },
    Tool {
        name: "chat_genes",
        write: true,
        description: "Set a chat's genes (lens IDs, its DNA) for future messages; an empty list clears them. Existing runs keep theirs.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"genes":{"type":"array","items":{"type":"string"},"maxItems":32}},"required":["id","genes"]}),
    },
    Tool {
        name: "chat_cancel",
        write: true,
        description: "Cancel a chat's current or queued turn (or run_id). all=true cancels every run and stops every bot in the chat and every chat it spawned (/cancel all); services keep running (chat_stop_services).",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"all":{"type":"boolean","default":false},"run_id":{"type":"string","maxLength":128}},"required":["id"]}),
    },
    Tool {
        name: "chat_stop_services",
        write: true,
        description: "Stop the live persistent services a chat owns; team=true includes every chat it spawned; service_id stops only that one. Ports bound to a stopped service lose authorization.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"team":{"type":"boolean","default":false},"service_id":{"type":"string","maxLength":128}},"required":["id"]}),
    },
    Tool {
        name: "chat_open",
        write: true,
        description: "Request host folders for a chat (as /open, or an agent's folder request). Each request waits for the operator, who is pinged on Discord when forwarding is on. A chat holds one pending request at a time, so several paths need folder approvals enabled in Manage → Permissions; then each is requested and approved in turn.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":ID},"paths":{"type":"array","items":{"type":"string","maxLength":4096},"minItems":1,"maxItems":16,"description":"Absolute folder paths on the Control node."}},"required":["id","paths"]}),
    },
    Tool {
        name: "control_design_ask",
        write: true,
        description: "The decision tool Control's agents use, for this caller: ask the operator a consequential choice with 2–4 options and optionally a recommended index. The question is listed by chat_requests, shown in the web UI and TUI and forwarded to Discord; the operator answers it there. Asking approves nothing and pauses nothing. With id it is asked in that DM, which must be an idle operator DM; otherwise in the operator's Decisions DM. The answer is appended to that DM: read it with chat_read.",
        schema: || {
            let mut schema = crate::design::definition()["inputSchema"].clone();
            schema["properties"]["id"] = json!({"type":"string","description":"Optional idle operator DM to ask in (default: the Decisions DM)."});
            schema
        },
    },
    Tool {
        name: "control_design_withdraw",
        write: true,
        description: "Withdraw a pending question this caller asked with control_design_ask, for example when it became moot or was settled elsewhere, with an optional short reason. It leaves chat_requests, the web UI, TUI and Discord and stays in that DM's history as withdrawn. Questions an agent asked can be withdrawn here only when the operator enabled question decisions in Manage → Permissions; the run waiting on one ends as if cancelled.",
        schema: || json!({"type":"object","properties":{"question_id":{"type":"string","maxLength":128,"description":"The ID control_design_ask returned."},"reason":{"type":"string","minLength":1,"maxLength":200}},"required":["question_id"]}),
    },
    Tool {
        name: "chat_decide",
        write: true,
        description: "Decide a pending request that chat_requests listed, for the kinds the operator enabled in Manage → Permissions (listed only when one is). kind folder or port: decision allow|deny (loopback=true for a loopback port target); bot: allow|deny; knowledge: allow (accept) or deny (reject); question: answer text.",
        schema: || json!({"type":"object","properties":{"id":{"type":"string","description":"The chat chat_requests names for this request."},"kind":{"type":"string","enum":Approvals::KINDS},"request":{"type":"string","maxLength":128},"decision":{"type":"string","enum":["allow","deny"]},"loopback":{"type":"boolean","default":false},"answer":{"type":"string","maxLength":2000}},"required":["id","kind","request"]}),
    },
];

/// Tool schemas, or only the read tools when `read_only`. `chat_decide` is
/// listed only when the operator enabled at least one kind of decision.
pub(crate) fn definitions(read_only: bool, approvals: Approvals) -> Vec<Value> {
    TOOLS.iter().filter(|tool| !(read_only && tool.write) && (tool.name != "chat_decide" || approvals.any())).map(|tool| json!({
        "name":tool.name,"description":tool.description,"inputSchema":(tool.schema)(),
        "annotations":{"readOnlyHint":!tool.write,"destructiveHint":matches!(tool.name,"chat_cancel"|"chat_stop_services"),"openWorldHint":false}
    })).collect()
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}… [{} more chars]", &text[..end], text[end..].chars().count()),
        None => text.to_owned(),
    }
}
fn number(args: &Value, key: &str, default: usize, min: usize, max: usize) -> io::Result<usize> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .map(|n| n as usize)
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| io::Error::other(format!("{key} must be an integer from {min} to {max}."))),
    }
}
fn flag(args: &Value, key: &str) -> io::Result<bool> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(io::Error::other(format!("{key} must be true or false."))),
    }
}
/// A string argument the server will see as one command token.
fn token<'a>(args: &'a Value, key: &str) -> io::Result<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.is_empty()
                && value.len() <= 128
                && value.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/@".contains(c)) =>
        {
            Ok(Some(value))
        }
        _ => Err(io::Error::other(format!(
            "{key} must be a single word of letters, digits and -_.:/@."
        ))),
    }
}
/// A model selection, parsed by the one selection grammar before it is sent
/// as `/model SELECTION`: whatever the grammar accepts, and nothing that could
/// add a second word to the command.
fn selection(args: &Value) -> io::Result<Option<&str>> {
    match args.get("model") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= 128 => crate::providers::selection::parse(value)
            .map(|_| Some(value.trim()))
            .map_err(|error| io::Error::other(format!("model: {error}"))),
        _ => Err(io::Error::other("model must be a selection of at most 128 characters.")),
    }
}
fn chat_id(args: &Value) -> io::Result<&str> {
    args["id"]
        .as_str()
        .filter(|id| {
            !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .ok_or_else(|| io::Error::other("Specify a chat id, as chats_list shows it."))
}
fn genes(args: &Value) -> io::Result<Option<Vec<String>>> {
    match args.get("genes") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) if values.len() <= 32 => values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| io::Error::other("genes must be lens IDs."))
            })
            .collect::<io::Result<Vec<_>>>()
            .map(Some),
        _ => Err(io::Error::other("genes must be a list of at most 32 lens IDs.")),
    }
}
fn rows(remote: &mut impl Remote) -> io::Result<Vec<Value>> {
    Ok(remote
        .tool("control.chats.list", json!({}))?
        .as_array()
        .cloned()
        .unwrap_or_default())
}
fn row<'a>(rows: &'a [Value], id: &str) -> io::Result<&'a Value> {
    rows.iter()
        .find(|row| row["id"] == id)
        .ok_or_else(|| io::Error::other("Chat not found."))
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_owned()
}
fn line(row: &Value) -> String {
    let mut notes = vec![format!("{} msgs", row["messages"].as_u64().unwrap_or(0))];
    if row["mode"] == "improve" {
        notes.push("improve mode".into());
    }
    if let Some(parent) = row["parent_chat"].as_str() {
        notes.push(format!("child of {parent}"));
    }
    if row["project_pending"] == true {
        notes.push("folder request pending".into());
    }
    if let Some(held) = row["held"].as_str() {
        notes.push(held.to_owned());
    }
    format!(
        "{} [{}] {} ({})",
        text(row, "id"),
        row["status"].as_str().unwrap_or("?"),
        clip(&text(row, "title"), 80),
        notes.join(", ")
    )
}
/// The latest operator-visible reply in a chat view, after a command.
fn last_reply(detail: &Value) -> String {
    detail["messages"]
        .as_array()
        .and_then(|m| m.last())
        .map(|m| text(m, "reply"))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "Done.".into())
}
/// The chat and its spawned descendants, depth first, from `control.chats.list` rows.
fn team<'a>(rows: &'a [Value], id: &str) -> Vec<(usize, &'a Value)> {
    let mut out = Vec::new();
    let mut stack = rows
        .iter()
        .filter(|row| row["id"] == id)
        .map(|row| (0, row))
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    while let Some((depth, row)) = stack.pop() {
        if out.len() >= TEAM_MAX || !seen.insert(row["id"].as_str().unwrap_or("")) {
            continue;
        }
        out.push((depth, row));
        stack.extend(
            rows.iter()
                .rev()
                .filter(|child| child["parent_chat"] == row["id"])
                .map(|child| (depth + 1, child)),
        );
    }
    out
}

fn chats_list(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let limit = number(args, "limit", LIST, 1, LIST_MAX)?;
    let roots_only = flag(args, "roots_only")?;
    let mut rows = rows(remote)?;
    rows.retain(|row| !roots_only || row["parent_chat"].is_null());
    rows.sort_by_key(|row| std::cmp::Reverse(row["activity_order"].as_u64().unwrap_or(0)));
    let total = rows.len();
    let mut out = rows.iter().take(limit).map(line).collect::<Vec<_>>();
    if out.is_empty() {
        out.push("No chats yet. Create one with chat_create.".into());
    }
    if total > limit {
        out.push(format!("… {} more; raise limit to see them.", total - limit));
    }
    Ok(out.join("\n"))
}

fn chat_read(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    let limit = number(args, "limit", 5, 1, 20)?;
    let activity = number(args, "activity", 5, 0, 40)?;
    let max = number(args, "max_chars", 1500, 200, 8000)?;
    let detail = remote.tool("control.chat.read", json!({"id":id}))?;
    let messages = detail["messages"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let before = number(args, "before", messages.len(), 0, usize::MAX)?.min(messages.len());
    let start = before.saturating_sub(limit);
    let genes = detail["lenses"]
        .as_array()
        .map(|g| g.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let mut out = vec![format!(
        "Chat {id} · {} · mode {} · genes {}{}{}",
        clip(&text(&detail, "title"), 80),
        detail["mode"].as_str().unwrap_or("chat"),
        if genes.is_empty() { "none" } else { &genes },
        detail["parent_chat"]
            .as_str()
            .map(|p| format!(" · child of {p}"))
            .unwrap_or_default(),
        detail["persona"]
            .as_str()
            .map(|p| format!(" · persona {p}"))
            .unwrap_or_default()
    )];
    if let Some(project) = detail["project"].as_object() {
        let state = if project.get("pending") == Some(&json!(true)) {
            "pending operator approval"
        } else if project.get("grant").is_some_and(Value::is_string) {
            "open"
        } else {
            "not open"
        };
        out.push(format!(
            "Folder {}: {state}",
            project.get("path").and_then(Value::as_str).unwrap_or("?")
        ));
    }
    let pending = [
        "pending_questions",
        "pending_ports",
        "pending_bots",
        "pending_knowledge",
    ]
    .iter()
    .map(|k| {
        (
            k.trim_start_matches("pending_"),
            detail[*k].as_array().map_or(0, Vec::len),
        )
    })
    .filter(|(_, n)| *n > 0)
    .map(|(k, n)| format!("{n} {k}"))
    .collect::<Vec<_>>();
    if !pending.is_empty() {
        out.push(format!(
            "Waiting on the operator: {} (chat_requests lists them).",
            pending.join(", ")
        ));
    }
    out.push(format!(
        "Messages {start}–{} of {}{}",
        before,
        messages.len(),
        if start > 0 {
            format!("; older: before={start}")
        } else {
            String::new()
        }
    ));
    for (index, message) in messages.iter().enumerate().take(before).skip(start) {
        out.push(String::new());
        let run = message["run_id"]
            .as_str()
            .map(|r| format!(" run {r}"))
            .unwrap_or_default();
        let kind = message["kind"].as_str().map(|k| format!(" ({k})")).unwrap_or_default();
        out.push(format!(
            "#{index} [{}]{run}{kind}",
            message["status"].as_str().unwrap_or("?")
        ));
        if let Some(held) = message["held"].as_str() {
            out.push(held.to_owned());
        }
        out.push(format!("> {}", clip(&text(message, "text"), max)));
        let reply = text(message, "reply");
        if !reply.is_empty() {
            out.push(format!("< {}", clip(&reply, max)));
        }
        if activity > 0 {
            let events = message["attempts"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(|a| a["activity"].as_array())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            for event in &events[events.len().saturating_sub(activity)..] {
                out.push(format!(
                    "  - {}/{}: {}",
                    event["kind"].as_str().unwrap_or("?"),
                    event["status"].as_str().unwrap_or("?"),
                    clip(&text(event, "text"), 200)
                ));
                let output = text(event, "output");
                if !output.is_empty() {
                    out.push(format!("    → {}", clip(&output, 200)));
                }
            }
        }
    }
    let agents = detail["agent_activity"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if !agents.is_empty() {
        out.push(String::new());
        out.push("Spawned agents, latest activity:".into());
        for agent in agents.iter().take(16) {
            out.push(format!(
                "  {}: {}",
                text(agent, "chat"),
                clip(&text(&agent["activity"], "text"), 200)
            ));
        }
    }
    Ok(out.join("\n"))
}

fn chat_team(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    let rows = rows(remote)?;
    row(&rows, id)?;
    let mut out = team(&rows, id)
        .into_iter()
        .map(|(depth, row)| format!("{}{}", "  ".repeat(depth.min(8)), line(row)))
        .collect::<Vec<_>>();
    if out.len() >= TEAM_MAX {
        out.push(format!("… team listing stops at {TEAM_MAX} chats."));
    }
    let services = remote.tool("control.services.list", json!({"id":id,"team":true}))?;
    let services = services["services"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    out.push(String::new());
    if services.is_empty() {
        out.push("No live services.".into());
    }
    for service in services {
        out.push(format!(
            "service {} [{}] in {}: {}",
            text(service, "service_id"),
            text(service, "phase"),
            text(service, "chat"),
            clip(&text(service, "command"), 120)
        ));
    }
    Ok(out.join("\n"))
}

fn chat_requests(remote: &mut impl Remote, args: &Value, approvals: Approvals) -> io::Result<String> {
    let id = chat_id(args)?;
    let rows = rows(remote)?;
    row(&rows, id)?;
    // Requests are grouped by top-level chat (the top of the visible rows).
    let mut root = id.to_owned();
    for _ in 0..64 {
        match row(&rows, &root)?["parent_chat"]
            .as_str()
            .filter(|parent| row(&rows, parent).is_ok())
        {
            Some(parent) => root = parent.to_owned(),
            None => break,
        }
    }
    let groups = remote.tool("control.requests.list", json!({}))?;
    let mut out = Vec::new();
    // The same reading of a request group as the TUI and Discord.
    for group in groups
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| g["chat_id"] == root.as_str())
    {
        for pending in control_client::operator::pending(group) {
            let kind = pending.kind.as_str();
            let chat = group["pending_folders"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|f| pending.kind == control_client::operator::Kind::Folder && f["request"] == pending.id)
                .and_then(|f| f["chat_id"].as_str())
                .unwrap_or(&root);
            let body = if pending.body.is_empty() {
                String::new()
            } else {
                format!(": {}", clip(pending.body, 400))
            };
            out.push(format!(
                "{kind} {} (chat {chat}) · {}{body}",
                pending.id,
                clip(&pending.heading, 300)
            ));
        }
    }
    if out.is_empty() {
        return Ok(format!("Nothing in task {root} is waiting on the operator."));
    }
    let enabled = Approvals::KINDS
        .iter()
        .filter(|kind| approvals.allows(kind))
        .copied()
        .collect::<Vec<_>>();
    out.push(if enabled.is_empty() {
        format!("The operator decides these in Control (web /dm/{root}, TUI or Discord); this bridge cannot approve or deny.")
    } else {
        format!("chat_decide may decide {} requests (Manage → Permissions); the operator decides the rest in Control.", enabled.join(", "))
    });
    Ok(out.join("\n"))
}

fn chat_open(remote: &mut impl Remote, args: &Value, approvals: Approvals) -> io::Result<String> {
    let id = chat_id(args)?;
    let paths = args["paths"]
        .as_array()
        .filter(|p| (1..=16).contains(&p.len()))
        .ok_or_else(|| io::Error::other("paths must list 1 to 16 absolute folder paths."))?
        .iter()
        .map(|p| {
            p.as_str()
                .filter(|p| p.starts_with('/'))
                .ok_or_else(|| io::Error::other("paths must be absolute."))
        })
        .collect::<io::Result<Vec<_>>>()?;
    if paths.len() > 1 && !approvals.folder {
        return Err(io::Error::other(
            "A chat holds one pending folder request at a time. Request one path, or enable folder approvals in Manage → Permissions to open several.",
        ));
    }
    let mut out = Vec::new();
    for path in paths {
        // The request an agent makes (`control.project.request`); it grants nothing.
        remote.tool("control.project.request", json!({"id":id,"path":path}))?;
        if !approvals.folder {
            out.push(format!("Requested {path}. The operator reviews it in Control (pinged on Discord when forwarding is on); chat_requests shows it."));
            break;
        }
        let detail = remote.tool("control.chat.read", json!({"id":id}))?;
        let request = detail["project"]["request"]
            .as_str()
            .ok_or_else(|| io::Error::other("Folder request was not recorded."))?
            .to_owned();
        out.push(format!(
            "{path}: {}",
            remote.decide(id, "folder", &request, true, false)?
        ));
    }
    Ok(out.join("\n"))
}

fn chat_decide(remote: &mut impl Remote, args: &Value, approvals: Approvals) -> io::Result<String> {
    let id = chat_id(args)?;
    let kind = args["kind"].as_str().unwrap_or("");
    if !approvals.allows(kind) {
        return Err(io::Error::other(
            "The operator has not let this bridge decide that kind of request (Manage → Permissions).",
        ));
    }
    let request =
        token(args, "request")?.ok_or_else(|| io::Error::other("Specify the request ID chat_requests shows."))?;
    let allow = match args["decision"].as_str() {
        Some("allow") => Some(true),
        Some("deny") => Some(false),
        None if kind == "question" => None,
        _ => return Err(io::Error::other("decision must be allow or deny.")),
    };
    match (kind, allow) {
        ("question", _) => {
            let answer = args["answer"]
                .as_str()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .ok_or_else(|| io::Error::other("Specify the answer: an option number or your direction."))?;
            command(remote, id, format!("/answer {request} {answer}"))
        }
        ("bot", Some(allow)) => command(
            remote,
            id,
            format!("/bot {} {request}", if allow { "allow" } else { "deny" }),
        ),
        (_, Some(allow)) => remote.decide(id, kind, request, allow, flag(args, "loopback")?),
        _ => Err(io::Error::other("decision must be allow or deny.")),
    }
}

fn design_ask(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let mut arguments = json!({"question":args["question"],"options":args["options"]});
    if !args["recommended"].is_null() {
        arguments["recommended"] = args["recommended"].clone();
    }
    if !args["id"].is_null() {
        arguments["id"] = json!(chat_id(args)?);
    }
    let asked = remote.tool("control.chat.ask", arguments)?;
    Ok(format!(
        "Asked question {} in chat {}. The operator answers it in Control (web, TUI) or by Discord reply; asking approves nothing. Its answer is appended to that chat: read it with chat_read.",
        text(&asked, "question_id"),
        text(&asked, "chat_id")
    ))
}

fn design_withdraw(remote: &mut impl Remote, args: &Value, approvals: Approvals) -> io::Result<String> {
    let question = token(args, "question_id")?
        .ok_or_else(|| io::Error::other("Specify the question_id control_design_ask returned."))?;
    let mut arguments = json!({"question_id":question});
    match args.get("reason") {
        None | Some(Value::Null) => {}
        Some(Value::String(reason)) => arguments["reason"] = json!(reason),
        _ => return Err(io::Error::other("reason must be text.")),
    }
    // Another party's question is the operator's to withdraw, as answering it is.
    if approvals.allows("question") {
        arguments["as"] = json!("operator");
    }
    remote.tool("control.chat.withdraw", arguments)?;
    Ok(format!(
        "Withdrew question {question}. It no longer waits on the operator and stays in its DM's history (chat_read)."
    ))
}

fn chat_create(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let title = match args.get("title") {
        None | Some(Value::Null) => None,
        // Refuse a name /rename would refuse before the chat exists, not after.
        Some(Value::String(title)) if !title.trim().is_empty() => {
            Some(crate::callsigns::validate(title).map_err(io::Error::other)?)
        }
        _ => return Err(io::Error::other("title must be non-empty text.")),
    };
    let model = selection(args)?;
    let persona = token(args, "persona")?;
    let mut create = json!({"mode":"chat"});
    if let Some(genes) = genes(args)? {
        create["lenses"] = json!(genes);
    }
    let detail = remote.tool("control.chat.create", create)?;
    let id = detail["id"]
        .as_str()
        .ok_or_else(|| io::Error::other("Invalid chat response."))?
        .to_owned();
    // The chat exists now; report any later step's failure with its ID.
    let mut notes = Vec::new();
    let steps = [
        title.map(|title| ("Rename", "control.chat.rename", json!({"id":id,"name":title}))),
        persona.map(|persona| ("Persona", "control.chat.persona", json!({"id":id,"persona":persona}))),
        model.map(|model| {
            (
                "Model",
                "control.chat.send",
                json!({"id":id,"message":format!("/model {model}")}),
            )
        }),
    ];
    for (what, tool, arguments) in steps.into_iter().flatten() {
        match remote.tool(tool, arguments) {
            Ok(detail) if what == "Model" => notes.push(last_reply(&detail)),
            Ok(_) => {}
            Err(error) => notes.push(format!("{what} failed: {error}")),
        }
    }
    Ok(format!(
        "Created chat {id}.{}{}",
        if notes.is_empty() { "" } else { " " },
        notes.join(" ")
    ))
}

fn chat_send(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    let message = args["message"]
        .as_str()
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| io::Error::other("Specify a message."))?;
    if message.trim_start().starts_with('/') {
        return Err(io::Error::other(
            "Slash commands are not sent through this bridge. Use chat_model, chat_genes or chat_cancel; folder, bot, answer and improve commands stay with the operator.",
        ));
    }
    let rows = rows(remote)?;
    if row(&rows, id)?["mode"] != "chat" {
        return Err(io::Error::other(
            "This chat is in improve mode; the operator sends to it in Control.",
        ));
    }
    // Plain text answers a waiting design question; that decision stays with the operator.
    let groups = remote.tool("control.requests.list", json!({}))?;
    if groups
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["pending_questions"].as_array().into_iter().flatten())
        .any(|q| q["chat_id"] == id)
    {
        return Err(io::Error::other(
            "This chat is waiting on a design question; the operator answers it in Control (chat_requests lists it).",
        ));
    }
    let detail = remote.tool("control.chat.send", json!({"id":id,"message":message}))?;
    let count = detail["messages"].as_array().map_or(0, Vec::len);
    let status = detail["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["status"].as_str())
        .unwrap_or("queued");
    Ok(format!(
        "Sent to {id} as message #{} [{status}]. Follow it with chat_read.",
        count.saturating_sub(1)
    ))
}

fn command(remote: &mut impl Remote, id: &str, text: String) -> io::Result<String> {
    Ok(last_reply(
        &remote.tool("control.chat.send", json!({"id":id,"message":text}))?,
    ))
}
fn chat_model(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    command(
        remote,
        id,
        format!("/model {}", selection(args)?.unwrap_or(""))
            .trim_end()
            .to_owned(),
    )
}
fn chat_genes(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    let genes = genes(args)?.ok_or_else(|| io::Error::other("Specify genes, or an empty list to clear them."))?;
    let detail = remote.tool("control.chat.lenses", json!({"id":id,"lenses":genes}))?;
    let set = detail["lenses"]
        .as_array()
        .map(|g| g.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    Ok(format!(
        "Genes for future messages in {id}: {}.",
        if set.is_empty() { "none" } else { &set }
    ))
}
fn chat_cancel(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    let run = token(args, "run_id")?;
    let text = match (flag(args, "all")?, run) {
        (true, Some(_)) => return Err(io::Error::other("Use all or run_id, not both.")),
        (true, None) => "/cancel all".to_owned(),
        (false, Some(run)) => format!("/cancel {run}"),
        (false, None) => "/cancel".to_owned(),
    };
    command(remote, id, text)
}
fn chat_stop_services(remote: &mut impl Remote, args: &Value) -> io::Result<String> {
    let id = chat_id(args)?;
    let mut arguments = json!({"id":id,"team":flag(args, "team")?});
    if let Some(service) = token(args, "service_id")? {
        arguments["service_id"] = json!(service);
    }
    let stopped = remote.tool("control.services.stop", arguments)?;
    let stopped = stopped["stopped"]
        .as_array()
        .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(if stopped.is_empty() {
        "No live services to stop.".into()
    } else {
        format!("Stopping {} service(s): {}.", stopped.len(), stopped.join(", "))
    })
}

/// One tool call: text content, never a credential (the operator token stays
/// inside `Hive`, and the server's chat views carry none).
pub(crate) fn call(remote: &mut impl Remote, name: &str, args: &Value, approvals: Approvals) -> io::Result<String> {
    let args = if args.is_null() { &json!({}) } else { args };
    if !args.is_object() {
        return Err(io::Error::other("Arguments must be an object."));
    }
    let text = match name {
        "chats_list" => chats_list(remote, args),
        "chat_read" => chat_read(remote, args),
        "chat_team" => chat_team(remote, args),
        "chat_requests" => chat_requests(remote, args, approvals),
        "chat_create" => chat_create(remote, args),
        "chat_send" => chat_send(remote, args),
        "chat_model" => chat_model(remote, args),
        "chat_genes" => chat_genes(remote, args),
        "chat_cancel" => chat_cancel(remote, args),
        "chat_stop_services" => chat_stop_services(remote, args),
        "chat_open" => chat_open(remote, args, approvals),
        "control_design_ask" => design_ask(remote, args),
        "control_design_withdraw" => design_withdraw(remote, args, approvals),
        "chat_decide" => chat_decide(remote, args, approvals),
        _ => Err(io::Error::other("Unknown tool. This bridge serves chat tools only.")),
    }?;
    Ok(clip(&text, OUTPUT))
}

const CHAT: control_mcp::Server = control_mcp::Server {
    name: "control-chat",
    version: env!("CARGO_PKG_VERSION"),
    instructions: Some(
        "Drive Control chats as the local operator: chats_list, chat_create, chat_send, chat_read. chat_open raises folder requests and control_design_ask asks the operator a decision (its answer appears in that chat; control_design_withdraw withdraws it); decisions stay with the operator unless enabled in Manage → Permissions (then chat_decide).",
    ),
    invalid: (control_mcp::INVALID_REQUEST, "Invalid JSON-RPC request"),
    unknown: (control_mcp::METHOD_NOT_FOUND, "Method not found"),
};
/// The chat tools over one operator connection, opened on first use.
struct Chats<'a, R, C> {
    connect: &'a mut C,
    connection: &'a mut Option<R>,
}
impl<R: Remote, C: FnMut() -> io::Result<R>> control_mcp::Tools for Chats<'_, R, C> {
    fn list(&mut self) -> Vec<Value> {
        definitions(false, control::permissions::read().mcp_approvals)
    }
    fn call(&mut self, call: control_mcp::Call<'_>) -> Value {
        let result = match self.connection.take().map_or_else(&mut *self.connect, Ok) {
            Ok(mut remote) => {
                let result = self::call(
                    &mut remote,
                    call.name,
                    call.arguments,
                    control::permissions::read().mcp_approvals,
                );
                // Keep the session unless the transport failed; reconnect next call.
                if !result
                    .as_ref()
                    .is_err_and(|e| e.to_string().starts_with("Control is unavailable"))
                {
                    *self.connection = Some(remote);
                }
                result
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(text) => control_mcp::text(&text),
            Err(error) => control_mcp::failure(&error.to_string()),
        }
    }
}
fn dispatch<R: Remote>(
    connect: &mut impl FnMut() -> io::Result<R>,
    connection: &mut Option<R>,
    request: &Value,
) -> Value {
    control_mcp::serve(&CHAT, request, &mut Chats { connect, connection })
}

/// Serves the chat tools on stdio until EOF. The connection authenticates with
/// the local operator credential, so only a process the operator starts on
/// this host (with read access to that private file) can act as the operator.
pub fn run() -> io::Result<()> {
    let mut connect = || Hive::connect(&format!("http://{}", crate::config::server_addr()));
    let mut connection = None;
    crate::mcp_stdio::serve(|request| dispatch(&mut connect, &mut connection, request))
}

#[cfg(test)]
mod tests;
