//! Operator decisions as every client presents them. The views are the
//! `pending_questions`, `pending_bots` and `pending_ports` fields that
//! `control.chat.read` (one chat) and `control.requests.list` (all chats)
//! return. A decision is the same command a person would type in that chat;
//! the server rechecks ownership, identity and whether it is still pending.
//! The TUI and the Discord adapter differ only in rendering and transport.
use super::{Hive, PortDecision};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Question,
    Bot,
    Port,
}
impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Question, Kind::Bot, Kind::Port];
}

/// One pending decision, rendered as plain text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub kind: Kind,
    pub id: String,
    pub heading: String,
    pub body: String,
    /// How to decide it when `body` does not already say.
    pub usage: Option<String>,
    /// A port whose exact target is loopback must be allowed explicitly.
    pub loopback: bool,
}

/// The pending decisions of a `control.chat.read` view or a
/// `control.requests.list` entry, in display order.
pub fn pending(view: &Value) -> Vec<Pending> {
    let list = |field: &str| view[field].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    let mut items = Vec::new();
    for question in list("pending_questions") {
        items.push(Pending {
            kind: Kind::Question,
            id: text(&question["question"]["id"]),
            heading: "Decision requested".into(),
            body: text(&question["summary"]),
            usage: None,
            loopback: false,
        });
    }
    for port in list("pending_ports") {
        let id = text(&port["port_id"]);
        let loopback = port["loopback_required"] == true;
        items.push(Pending {
            kind: Kind::Port,
            heading: format!("Port request · {} · {}", text(&port["name"]), port["target"]),
            body: String::new(),
            usage: Some(format!("/port allow {id}{} · /port deny {id}", if loopback { " --loopback" } else { "" })),
            id,
            loopback,
        });
    }
    for bot in list("pending_bots") {
        let id = text(&bot["request_id"]);
        items.push(Pending {
            kind: Kind::Bot,
            heading: format!("Bot request · {}", text(&bot["chat_id"])),
            body: String::new(),
            usage: Some(format!("/bot allow {id} · /bot deny {id}")),
            id,
            loopback: false,
        });
    }
    items
}

/// What a client sends to decide a request or talk to a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `control.chat.send`, as typed in the chat (also `/answer` and `/bot allow|deny`).
    Send(String),
    /// The port decision form the web panel posts.
    Port { id: String, decision: PortDecision, loopback: bool },
}

/// `/port allow ID [--loopback]` or `/port deny ID` against what is displayed:
/// `loopback_required` returns `None` when the request is no longer shown.
pub fn port(text: &str, loopback_required: impl Fn(&str) -> Option<bool>) -> Result<Command, &'static str> {
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.first() != Some(&"/port") || !(3..=4).contains(&parts.len()) || !matches!(parts[1], "allow" | "deny") || (parts.len() == 4 && parts[3] != "--loopback") {
        return Err("Use /port allow ID [--loopback] or /port deny ID. Review the exact target before allowing loopback.");
    }
    let required = loopback_required(parts[2]).ok_or("Port request changed. Refresh the conversation before approving it.")?;
    let loopback = parts.get(3) == Some(&"--loopback");
    if parts[1] == "allow" && required && !loopback {
        return Err("This exact loopback target requires /port allow ID --loopback.");
    }
    let decision = if parts[1] == "allow" { PortDecision::Allow } else { PortDecision::Deny };
    Ok(Command::Port { id: parts[2].into(), decision, loopback })
}

/// The request a typed decision command names: `/answer ID ANSWER`,
/// `/bot allow|deny ID` or `/port allow|deny ID [--loopback]`.
pub fn named(text: &str) -> Option<(Kind, &str)> {
    match text.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["/answer", id, _, ..] => Some((Kind::Question, id)),
        ["/bot", "allow" | "deny", id] => Some((Kind::Bot, id)),
        ["/port", "allow" | "deny", id] | ["/port", "allow", id, "--loopback"] => Some((Kind::Port, id)),
        _ => None,
    }
}

/// A reply to `pending`'s notification as the command that decides it: an
/// answer, allow/deny (yes/no), or a typed command naming this request.
pub fn reply(pending: &Pending, text: &str) -> Result<Command, &'static str> {
    let (text, id) = (text.trim(), pending.id.as_str());
    let words = text.to_lowercase().split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let verdict = match words.first().map(String::as_str) {
        Some("allow" | "approve" | "yes" | "y") => "allow",
        Some("deny" | "reject" | "no" | "n") => "deny",
        _ => "",
    };
    let loopback = pending.kind == Kind::Port && verdict == "allow" && words.len() == 2 && words[1] == "--loopback";
    let command = if named(text) == Some((pending.kind, id)) {
        text.to_owned()
    } else {
        match pending.kind {
            Kind::Question if !text.is_empty() && !text.starts_with('/') => format!("/answer {id} {text}"),
            Kind::Question => return Err("Reply with an option number or your direction."),
            _ if verdict.is_empty() || (words.len() > 1 && !loopback) => return Err("Reply allow or deny."),
            Kind::Bot => format!("/bot {verdict} {id}"),
            Kind::Port => format!("/port {verdict} {id}{}", if loopback { " --loopback" } else { "" }),
        }
    };
    match pending.kind {
        Kind::Port => port(&command, |shown| (shown == id).then_some(pending.loopback)),
        _ => Ok(Command::Send(command)),
    }
}

/// Carry out `command` in `chat` through the operator interface.
pub fn execute(hive: &mut Hive, chat: &str, command: &Command) -> io::Result<()> {
    match command {
        Command::Send(message) => hive.tool("control.chat.send", json!({"id":chat,"message":message})).map(drop),
        Command::Port { id, decision, loopback } => hive.decide_port(chat, id, *decision, *loopback),
    }
}

/// Which requests an away-from-keyboard client forwards. Absent settings
/// forward every kind from every chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Forwarding {
    pub enabled: bool,
    pub kinds: Vec<Kind>,
    /// Top-level chat IDs; empty forwards all chats.
    pub chats: Vec<String>,
}
impl Default for Forwarding {
    fn default() -> Self {
        Self { enabled: true, kinds: Kind::ALL.to_vec(), chats: Vec::new() }
    }
}
impl Forwarding {
    pub fn forwards(&self, chat: &str, kind: Kind) -> bool {
        self.enabled && self.kinds.contains(&kind) && (self.chats.is_empty() || self.chats.iter().any(|c| c == chat))
    }
}

#[cfg(test)]
mod tests;
