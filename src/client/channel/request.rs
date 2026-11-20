//! Operator requests as structured messages, for services that render more
//! than text. A [`Request`] has a title, a short body, labelled fields, a
//! severity and [`Action`]s. Each action is a reply the operator could have
//! typed (`allow`, `deny`, `allow --loopback`, an option number) or a prompt
//! for free text, so pressing one runs the same decision path as a text reply
//! ([`super::Channel::press`]). No state is added: the routing ledger's notice
//! already records which message stands for which request.
//!
//! A transport renders this where it can: Discord as an embed with buttons
//! and a modal for free text. Telegram inline keyboards or WhatsApp reply
//! buttons map onto the same actions. A transport without rich messages keeps
//! the plain text from [`super::notice_text`] (the default of
//! [`super::Transport::post_request`]), whose typed commands still work.
use super::{Ledger, Notice, Recipient, Settings};
use crate::client::operator::{Kind, Pending};
use serde_json::Value;

/// How urgent a message looks: colour on Discord, an emoji or nothing elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A question waiting for direction.
    Attention,
    /// Approval that grants access.
    Warning,
    /// Approval of something risky, such as a loopback target.
    Danger,
    /// Settled in the operator's favour (allowed or answered).
    Success,
    /// Settled otherwise: denied, or no longer pending.
    Muted,
}

/// How an action looks; services without styles ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Primary,
    Secondary,
    Success,
    Danger,
}

/// What pressing an action does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Decides with [`reply`] for this action's ID.
    Reply,
    /// Asks for free text, then decides with it as the reply.
    Prompt,
    /// Opens a URL; nothing is decided.
    Open(String),
}

/// One action on a request. `id` is a short opaque token from a fixed set
/// (`allow`, `deny`, `loopback`, `answer`, `o1`–`o9`, `open`); a transport
/// carries it back on a press and the core validates it against the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub id: &'static str,
    pub label: String,
    pub style: Style,
    pub act: Act,
}

/// A request rendered as structure. Text is free of control characters except
/// newlines in `body`, and already uses the channel's command spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub title: String,
    pub body: String,
    pub fields: Vec<(&'static str, String)>,
    pub severity: Severity,
    pub actions: Vec<Action>,
    /// The chat's page on the local website.
    pub link: String,
    /// How to decide it by typing, for when actions are unavailable.
    pub hint: String,
}

/// How a request's message ends once decided or no longer pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub text: String,
    pub severity: Severity,
}

/// The most option buttons a question gets.
const OPTIONS: usize = 9;
const OPTION_IDS: [&str; OPTIONS] = ["o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8", "o9"];

/// The reply an action stands for on a request of `kind`, or `None` when the
/// action does not apply to that kind (`answer` needs the prompted text).
pub fn reply(kind: Kind, action: &str) -> Option<&'static str> {
    const NUMBERS: [&str; OPTIONS] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match (kind, action) {
        (Kind::Bot | Kind::Port, "allow") => Some("allow"),
        (Kind::Bot | Kind::Port, "deny") => Some("deny"),
        (Kind::Port, "loopback") => Some("allow --loopback"),
        (Kind::Question, option) => OPTION_IDS.iter().position(|id| *id == option).map(|n| NUMBERS[n]),
        _ => None,
    }
}

/// What the message says once action `id` decided its request.
pub fn decided(action: &str) -> Settled {
    let (text, severity) = match action {
        "allow" => ("Allowed by you".to_owned(), Severity::Success),
        "loopback" => ("Allowed by you, including loopback".to_owned(), Severity::Success),
        "deny" => ("Denied by you".to_owned(), Severity::Muted),
        "answer" => ("Answered by you".to_owned(), Severity::Success),
        option => (format!("Answered by you: option {}", option.trim_start_matches('o')), Severity::Success),
    };
    Settled { text, severity }
}
/// What the message says once its request is no longer pending.
pub fn closed() -> Settled {
    Settled { text: "No longer pending".into(), severity: Severity::Muted }
}

/// Why a press was not carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The message is not one of this recipient's notifications on this node;
    /// the adapter ignores the press so it reveals nothing.
    Unknown,
    /// The request was already decided or is no longer pending.
    Settled,
    /// The action does not apply to this request, or a prompt came without text.
    Invalid,
}

impl Ledger {
    /// The request and reply text that pressing `action` on `message` stands
    /// for. Only `to`'s own notifications match; `text` is the prompted
    /// answer and is accepted only for `answer`.
    pub fn press(&self, to: &Recipient, message: &str, action: &str, text: Option<&str>) -> Result<(Notice, String), Refusal> {
        let notice = self.notices.iter().rev()
            .find(|n| n.person == to.person && n.conversation == to.conversation && n.message == message)
            .ok_or(Refusal::Unknown)?;
        if notice.settled {
            return Err(Refusal::Settled);
        }
        let reply = match (action, text.map(str::trim)) {
            ("answer", Some(text)) if matches!(notice.kind, Kind::Question | Kind::Notice) && !text.is_empty() => text.to_owned(),
            (action, None) => reply(notice.kind, action).ok_or(Refusal::Invalid)?.to_owned(),
            _ => return Err(Refusal::Invalid),
        };
        Ok((notice.clone(), reply))
    }
}

/// Text without control characters, at most `max` characters; newlines kept
/// when `lines`.
fn clean(text: &str, max: usize, lines: bool) -> String {
    let mut out: String = text.chars().filter(|c| !c.is_control() || (lines && *c == '\n')).take(max).collect();
    if text.chars().count() > max {
        out.pop();
        out.push('…');
    }
    out
}

/// The pending item `pending` came from in its `control.requests.list` entry.
fn item<'a>(view: &'a Value, pending: &Pending<'_>) -> &'a Value {
    let (list, key) = match pending.kind {
        Kind::Question => ("pending_questions", "/question/id"),
        Kind::Bot => ("pending_bots", "/request_id"),
        Kind::Port => ("pending_ports", "/port_id"),
        Kind::Notice => ("pending_notices", "/notice_id"),
        Kind::Folder => ("pending_folders", "/request"),
    };
    view[list].as_array().into_iter().flatten()
        .find(|item| item.pointer(key).and_then(Value::as_str) == Some(pending.id))
        .unwrap_or(&Value::Null)
}

/// The structured form of `pending` from chat `title`, read from its
/// `control.requests.list` entry `view`, in `settings`' command spelling.
pub fn request(settings: &Settings, title: &str, link: &str, pending: &Pending<'_>, view: &Value) -> Request {
    let item = item(view, pending);
    let id = clean(pending.id, 100, false);
    let mut fields = vec![("DM", clean(title, 100, false))];
    let open = Action { id: "open", label: "Open DM".into(), style: Style::Secondary, act: Act::Open(link.into()) };
    let decide = |id: &'static str, label: &str, style| Action { id, label: label.into(), style, act: Act::Reply };
    let request = match pending.kind {
        Kind::Question => {
            let input = &item["question"]["input"];
            let question = input["question"].as_str().filter(|q| !q.is_empty());
            let recommended = input["recommended"].as_u64();
            let options = input["options"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let mut actions = Vec::new();
            let mut body = question.map(|q| clean(q, 1500, true)).unwrap_or_else(|| clean(pending.body, 1500, true));
            if question.is_some() && !options.is_empty() {
                body.push('\n');
            }
            for (n, option) in options.iter().filter_map(Value::as_str).enumerate().filter(|_| question.is_some()) {
                let tag = if recommended == Some(n as u64) { " (recommended)" } else { "" };
                body.push_str(&format!("\n**{}.** {}{tag}", n + 1, clean(option, 300, false)));
                if let Some(action) = OPTION_IDS.get(n) {
                    let style = if tag.is_empty() { Style::Secondary } else { Style::Primary };
                    actions.push(decide(action, &format!("{}. {}", n + 1, clean(option, 60, false)), style));
                }
            }
            if let Some(n) = recommended.filter(|n| (*n as usize) < options.len()) {
                fields.push(("Recommended", format!("Option {}", n + 1)));
            }
            actions.push(Action { id: "answer", label: "Answer…".into(), style: Style::Primary, act: Act::Prompt });
            actions.push(open);
            Request {
                title: "Decision requested".into(),
                body,
                fields,
                severity: Severity::Attention,
                actions,
                link: link.into(),
                hint: format!("Or reply with a number or your direction, or /answer {id} YOUR_ANSWER"),
            }
        }
        Kind::Bot => {
            if let Some(chat) = item["chat_id"].as_str() {
                let shown = item["name"].as_str().map_or_else(|| chat.to_owned(), |name| format!("{name} ({chat})"));
                fields.push(("Bot chat", clean(&shown, 140, false)));
            }
            if let Some(minutes) = item["every_minutes"].as_u64() {
                fields.push(("Wakes", format!("every {minutes} min")));
            }
            if let Some(room) = item["room"].as_str().filter(|room| !room.is_empty()) {
                fields.push(("Room", clean(room, 100, false)));
            }
            Request {
                title: "Bot request".into(),
                body: "An agent asks to run a persistent bot.".into(),
                fields,
                severity: Severity::Warning,
                actions: vec![decide("allow", "Approve", Style::Success), decide("deny", "Deny", Style::Danger), open],
                link: link.into(),
                hint: format!("Or reply allow or deny, or /bot allow {id}"),
            }
        }
        Kind::Port => {
            let name = item["name"].as_str().map(|name| clean(name, 100, false));
            let target = match (&item["target"]["host"], &item["target"]["port"]) {
                (Value::String(host), port) if !port.is_null() => Some(format!("{}:{port}", clean(host, 200, false))),
                _ => item["peer_port_id"].as_str().map(|peer| format!("peer ingress {}", clean(peer, 100, false))),
            };
            for (label, value) in [("Name", name), ("Target", target)] {
                if let Some(value) = value {
                    fields.push((label, value));
                }
            }
            if let Some(direction) = item["direction"].as_str() {
                fields.push(("Direction", clean(direction, 40, false)));
            }
            if let Some(port) = item["service_port"].as_u64() {
                fields.push(("Service port", port.to_string()));
            }
            let mut actions = Vec::new();
            let (body, severity, hint) = if pending.loopback {
                actions.push(decide("loopback", "Allow loopback", Style::Danger));
                ("The exact target is a loopback address on this machine. Review it before allowing.", Severity::Danger, format!("Or reply allow --loopback or deny, or /port allow {id} --loopback"))
            } else {
                actions.push(decide("allow", "Allow", Style::Success));
                ("An agent asks to open a network port.", Severity::Warning, format!("Or reply allow or deny, or /port allow {id}"))
            };
            actions.push(decide("deny", "Deny", Style::Secondary));
            actions.push(open);
            Request { title: "Port request".into(), body: body.into(), fields, severity, actions, link: link.into(), hint }
        }
        Kind::Notice => {
            if let Some(team) = item["team"].as_str() {
                fields.push(("Team", clean(team, 100, false)));
            }
            Request {
                title: "Notice".into(),
                body: clean(pending.body, 1500, true),
                fields,
                severity: Severity::Attention,
                actions: vec![Action { id: "answer", label: "Reply…".into(), style: Style::Primary, act: Act::Prompt }, open],
                link: link.into(),
                hint: "No decision is needed; reply to steer the team that sent it.".into(),
            }
        }
        Kind::Folder => {
            for (label, key, max) in [("Folder", "path", 300), ("Name", "name", 100), ("Chat", "chat_id", 100)] {
                if let Some(value) = item[key].as_str().filter(|value| !value.is_empty()) {
                    fields.push((label, clean(value, max, false)));
                }
            }
            Request {
                title: "Folder request".into(),
                body: "A task asks for access to a host folder. Review the exact path in Control before approving.".into(),
                fields,
                severity: Severity::Warning,
                actions: vec![open],
                link: link.into(),
                hint: crate::client::operator::FOLDER_USAGE.into(),
            }
        }
    };
    Request { body: settings.present(&request.body), hint: settings.present(&request.hint), ..request }
}

#[cfg(test)]
mod tests;
