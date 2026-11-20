//! Operator decisions as every client presents them. The views are the
//! `pending_questions`, `pending_bots` and `pending_ports` fields that
//! `control.chat.read` (one chat) and `control.requests.list` (all chats)
//! return. A decision is the same command a person would type in that chat;
//! the server rechecks ownership, identity and whether it is still pending.
//! The TUI and the Discord adapter differ only in rendering and transport.
use super::{Hive, PortDecision};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{borrow::Cow, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Question,
    Bot,
    Port,
    /// A notice from an opted-in agent (an antibody or a lead): no decision
    /// is requested; a reply steers the chat it came from.
    Notice,
    /// A folder (`/open`) request: host paths for the task. Approved or
    /// dismissed in Control (web or TUI `/open`), never by a channel reply.
    Folder,
}
impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Question, Kind::Bot, Kind::Port, Kind::Notice, Kind::Folder];
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Question => "question",
            Kind::Bot => "bot",
            Kind::Port => "port",
            Kind::Notice => "notice",
            Kind::Folder => "folder",
        }
    }
}

/// One pending decision, rendered as plain text borrowed from its view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending<'a> {
    pub kind: Kind,
    pub id: &'a str,
    /// Free of control characters, like `usage`.
    pub heading: Cow<'static, str>,
    pub body: &'a str,
    /// How to decide it when `body` does not already say.
    pub usage: Option<String>,
    /// A port whose exact target is loopback must be allowed explicitly.
    pub loopback: bool,
}

/// Server text without control characters, written without an intermediate copy.
struct Clean<T>(T);
impl<T: std::fmt::Display> std::fmt::Display for Clean<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct Filter<'a, 'b>(&'a mut std::fmt::Formatter<'b>);
        impl std::fmt::Write for Filter<'_, '_> {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                text.chars().filter(|c| !c.is_control()).try_for_each(|c| self.0.write_char(c))
            }
        }
        std::fmt::write(&mut Filter(f), format_args!("{}", self.0))
    }
}

/// The pending decisions of a `control.chat.read` view or a
/// `control.requests.list` entry, in display order.
pub fn pending(view: &Value) -> Vec<Pending<'_>> {
    let list = |field: &str| view[field].as_array().map(Vec::as_slice).unwrap_or(&[]);
    fn text(value: &Value) -> &str {
        value.as_str().unwrap_or("")
    }
    let mut items = Vec::new();
    for question in list("pending_questions") {
        items.push(Pending {
            kind: Kind::Question,
            id: text(&question["question"]["id"]),
            heading: Cow::Borrowed("Decision requested"),
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
            heading: Cow::Owned(format!("Port request · {} · {}", Clean(text(&port["name"])), Clean(&port["target"]))),
            body: "",
            usage: Some(format!("/port allow {0}{1} · /port deny {0}", Clean(id), if loopback { " --loopback" } else { "" })),
            id,
            loopback,
        });
    }
    for bot in list("pending_bots") {
        let id = text(&bot["request_id"]);
        items.push(Pending {
            kind: Kind::Bot,
            heading: Cow::Owned(match bot["name"].as_str() {
                Some(name) => format!("Bot request · {} ({})", Clean(name), Clean(text(&bot["chat_id"]))),
                None => format!("Bot request · {}", Clean(text(&bot["chat_id"]))),
            }),
            body: "",
            usage: Some(format!("/bot allow {0} · /bot deny {0}", Clean(id))),
            id,
            loopback: false,
        });
    }
    for notice in list("pending_notices") {
        items.push(Pending {
            kind: Kind::Notice,
            id: text(&notice["notice_id"]),
            heading: Cow::Owned(format!("Notice · {}", Clean(text(&notice["chat_id"])))),
            body: text(&notice["text"]),
            usage: Some("Reply to steer the team that sent it; no decision is needed.".into()),
            loopback: false,
        });
    }
    for folder in list("pending_folders") {
        items.push(Pending {
            kind: Kind::Folder,
            id: text(&folder["request"]),
            heading: Cow::Owned(format!("Folder request · {} ({})", Clean(text(&folder["path"])), Clean(text(&folder["chat_id"])))),
            body: "",
            usage: Some(FOLDER_USAGE.into()),
            loopback: false,
        });
    }
    items
}
/// Folder access is reviewed where the exact path is shown, not by reply.
pub const FOLDER_USAGE: &str = "Review the exact path and approve or dismiss it in Control: /open in that DM (web or TUI).";

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

/// Decision commands, for help text.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/answer ID ANSWER", "Answer a design question"),
    ("/bot allow|deny ID", "Decide a bot request"),
    ("/port allow|deny ID [--loopback]", "Decide a port request"),
];

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
pub fn reply(pending: &Pending<'_>, text: &str) -> Result<Command, &'static str> {
    let (text, id) = (text.trim(), pending.id);
    let words = text.to_lowercase().split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let verdict = match words.first().map(String::as_str) {
        Some("allow" | "approve" | "yes" | "y") => "allow",
        Some("deny" | "reject" | "no" | "n") => "deny",
        _ => "",
    };
    let loopback = pending.kind == Kind::Port && verdict == "allow" && words.len() == 2 && words[1] == "--loopback";
    if pending.kind == Kind::Folder {
        return Err(FOLDER_USAGE);
    }
    let command = if named(text) == Some((pending.kind, id)) {
        text.to_owned()
    } else {
        match pending.kind {
            Kind::Question if !text.is_empty() && !text.starts_with('/') => format!("/answer {id} {text}"),
            Kind::Question => return Err("Reply with an option number or your direction."),
            Kind::Notice if !text.is_empty() && !text.starts_with('/') => text.to_owned(),
            Kind::Notice => return Err("Reply with a message for the team that sent the notice."),
            _ if verdict.is_empty() || (words.len() > 1 && !loopback) => return Err("Reply allow or deny."),
            Kind::Bot => format!("/bot {verdict} {id}"),
            Kind::Port => format!("/port {verdict} {id}{}", if loopback { " --loopback" } else { "" }),
            Kind::Folder => return Err(FOLDER_USAGE),
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


#[cfg(test)]
mod tests;
