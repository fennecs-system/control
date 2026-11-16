//! Operator messaging channels: Discord now, and Telegram, WhatsApp or Slack as
//! further adapters. An adapter implements [`Transport`] (posting into its
//! service) and pushes each inbound message to [`Channel::route`] and
//! [`Channel::run`]. A push suits both polling services and webhook services.
//! Everything else is written once here: request forwarding, the routing table,
//! reply-to-decision commands, chat navigation and steering, and relaying replies.
//! The core reaches Control only through the operator interface the TUI uses
//! (`client::operator` and `client::chats` over [`Hive`]). Platform IDs are
//! opaque strings, so no service's types appear here.
//!
//! Configuration is per channel in the configuration directory's `channels.json`:
//! `{"discord":{"enabled":true,"kinds":["question","bot","port"],"chats":[]}}`.
//! Forwarding is opt-in: a channel without `"enabled":true` forwards no
//! requests. Commands and steering (`help`, `chats`, `resume`, `new`, `use`)
//! do not depend on it; the adapter decides who may use them by identity.
//! Each channel's routing table is `channels/NAME.json` in the state directory.
use super::chats::{self, Navigation, navigation};
use super::operator::{self, Kind, Pending};
use super::{Hive, updates::Updates};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, time::{Duration, Instant}};

pub mod request;
pub use request::{Refusal, Request, Settled};

/// What a messaging service supports; the core degrades to plain text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// A message can reply to an earlier one (reply-to routing).
    pub replies: bool,
    pub threads: bool,
    /// Structured requests with actions ([`Transport::post_request`]).
    pub buttons: bool,
    pub edits: bool,
    pub attachments: bool,
    /// Only one-to-one conversations are used.
    pub direct_only: bool,
    /// The longest message text, in characters.
    pub max_text: usize,
}

/// One person in one conversation, both as the service names them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Recipient {
    pub person: String,
    pub conversation: String,
    /// The chat this conversation already owns, if the adapter keeps one (a
    /// Discord DM's own chat); `/resume` with it returns there.
    pub home: Option<String>,
}

/// An inbound message from a recipient. A button press arrives as its label
/// in reply to the message that carried it.
#[derive(Clone, Copy, Debug)]
pub struct Inbound<'a> {
    pub replied: Option<&'a str>,
    pub text: &'a str,
}

/// The outcome of posting a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Posted {
    /// Posted, with the service's message ID when it returns one.
    Sent(Option<String>),
    /// A transient failure; it is retried later with the same key.
    Retry,
    /// A permanent refusal; it is not retried.
    Refused,
}

/// Transport and rendering for one messaging service.
pub trait Transport {
    fn capabilities(&self) -> Capabilities;
    /// Post `text` to `to`, as a reply to `reply_to` when given. `key` is the
    /// same for retries of one message, so a service with idempotency keys or
    /// nonces can drop duplicates.
    fn post(&self, to: &Recipient, text: &str, reply_to: Option<&str>, key: &str) -> Posted;
    /// Replace an earlier message's text, where `capabilities().edits`.
    fn edit(&self, _to: &Recipient, _message: &str, _text: &str) -> Posted {
        Posted::Refused
    }
    /// Post an operator request, where `capabilities().buttons`, as a
    /// structured message whose actions come back through
    /// [`Channel::press`]. `text` is the plain rendering, which the default
    /// posts for services without rich messages.
    fn post_request(&self, to: &Recipient, _request: &Request, text: &str, key: &str) -> Posted {
        self.post(to, text, None, key)
    }
    /// Mark a request's message decided or closed (for example by removing
    /// its buttons), where `capabilities().edits`.
    fn settle(&self, _to: &Recipient, _message: &str, _settled: &Settled) -> Posted {
        Posted::Refused
    }
}

/// One channel's settings in `channels.json`. Forwarding is opt-in: absent
/// settings forward no requests; `"enabled":true` alone forwards every kind
/// from every chat. Commands and steering work either way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Whether operator requests are forwarded (and so can be answered here).
    pub enabled: bool,
    pub kinds: Vec<Kind>,
    /// Top-level chat IDs; empty forwards all chats.
    pub chats: Vec<String>,
    /// What starts a command in this service. `!` avoids clashing with
    /// Discord's own slash commands; a service with native `/` keeps it.
    pub prefix: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, kinds: Kind::ALL.to_vec(), chats: Vec::new(), prefix: "!".into() }
    }
}
impl Settings {
    pub fn forwards(&self, chat: &str, kind: Kind) -> bool {
        self.enabled && self.kinds.contains(&kind) && (self.chats.is_empty() || self.chats.iter().any(|c| c == chat))
    }
    /// Valid prefixes are 1–3 visible ASCII punctuation characters.
    pub fn valid_prefix(prefix: &str) -> bool {
        (1..=3).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_punctuation())
    }
    /// Text as typed, with this channel's prefix turned into Control's `/`.
    pub fn command<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        let text = text.trim();
        match text.strip_prefix(self.prefix.as_str()) {
            Some(rest) if self.prefix != "/" && rest.starts_with(|c: char| c.is_ascii_alphabetic()) => format!("/{rest}").into(),
            _ => text.into(),
        }
    }
    /// Control's text with `/command` spelled with this channel's prefix.
    pub fn present(&self, text: &str) -> String {
        if self.prefix == "/" {
            return text.into();
        }
        let mut out = String::with_capacity(text.len());
        let mut previous = None;
        let mut rest = text;
        while let Some(c) = rest.chars().next() {
            let boundary = previous.is_none_or(|p: char| p.is_whitespace() || p == '(' || p == '`');
            if c == '/' && boundary && command_word(rest[1..].split(|n: char| !n.is_ascii_lowercase()).next().unwrap_or("")) {
                out.push_str(&self.prefix);
            } else {
                out.push(c);
            }
            previous = Some(c);
            rest = &rest[c.len_utf8()..];
        }
        out
    }
    /// `help`: every command from the shared tables, in this channel's spelling.
    pub fn help(&self) -> String {
        let server = control_context::commands::DEFINITIONS.iter().filter(|d| d.id != "answer").map(|d| (d.text, d.description));
        let rows = chats::COMMANDS.iter().copied().chain(operator::COMMANDS.iter().copied()).chain(server).chain([("/help", "This list")]);
        let lines = rows.map(|(usage, description)| format!("{usage} — {description}")).collect::<Vec<_>>().join("\n");
        self.present(&format!("Commands (anything else goes to the selected chat):\n{lines}"))
    }
}

/// Whether `word` names a command in the shared tables; `present` respells
/// only those, leaving other `/text` (paths, URLs) alone.
fn command_word(word: &str) -> bool {
    let tables = chats::COMMANDS.iter().chain(operator::COMMANDS).map(|(usage, _)| *usage)
        .chain(control_context::commands::DEFINITIONS.iter().map(|d| d.text)).chain(["/help"]);
    !word.is_empty() && tables.filter_map(|usage| usage.strip_prefix('/')).any(|usage| usage.split(|c: char| !c.is_ascii_lowercase()).next() == Some(word))
}

/// Every channel's settings.
pub fn all_settings() -> std::collections::BTreeMap<String, Settings> {
    let read = || {
        let path = crate::config::directory().ok()?.join("channels.json");
        let bytes = crate::config::read_regular(&path, 65536, false).ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    read().unwrap_or_default()
}
/// The settings of channel `name`; absent means disabled.
pub fn settings(name: &str) -> Settings {
    all_settings().remove(name).unwrap_or_default()
}
/// Saves channel `name`'s settings, keeping the others.
pub fn save_settings(name: &str, settings: Settings) -> Result<(), &'static str> {
    if !Settings::valid_prefix(&settings.prefix) {
        return Err("Use a command prefix of 1–3 punctuation characters, such as !");
    }
    let mut all = all_settings();
    all.insert(name.into(), settings);
    let bytes = serde_json::to_vec_pretty(&all).map_err(|_| "Could not encode channel settings.")?;
    crate::config::write_private("channels.json", &bytes).map_err(|_| "Could not save channel settings.")
}
/// The adapters this build provides; each is configured by name in `channels.json`.
pub const ADAPTERS: &[(&str, &str)] = &[("discord", "Discord · direct messages")];

/// Notifications and relayed replies kept for reply routing; older ones age out.
const LIMIT: usize = 512;
/// Recent chats listed by `/chats`.
pub const RECENT: usize = 15;

/// A request notification: which message stands for which request in which chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notice {
    pub person: String,
    pub conversation: String,
    pub message: String,
    /// The top-level chat the decision is sent to.
    pub chat: String,
    pub kind: Kind,
    pub id: String,
    #[serde(default)]
    pub loopback: bool,
    /// Decided through this message, or seen no longer pending; its actions
    /// are refused (typed replies still reach the server, which rechecks).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settled: bool,
    /// Posted as a structured request with actions, so settling edits it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub actions: bool,
}
/// Every pending request in a `control.requests.list` view, as (chat, kind, ID).
fn open_requests(groups: &Value) -> std::collections::HashSet<(&str, Kind, &str)> {
    groups.as_array().into_iter().flatten()
        .filter_map(|group| Some((group["chat_id"].as_str()?, group)))
        .flat_map(|(chat, group)| operator::pending(group).into_iter().map(move |p| (chat, p.kind, p.id)))
        .collect()
}
impl Notice {
    pub fn pending(&self) -> Pending<'_> {
        Pending { kind: self.kind, id: &self.id, heading: Default::default(), body: "", usage: None, loopback: self.loopback }
    }
}
/// The chat a conversation talks to after `/resume ID`, and how far its
/// replies have been relayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Active {
    pub person: String,
    pub conversation: String,
    pub chat: String,
    /// The next message index whose finished reply is relayed.
    pub delivered: usize,
    /// The chat's `control.chats.list` revision when last read.
    #[serde(default)]
    pub revision: Value,
}
/// A relayed chat reply, so replying to it continues that chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relayed {
    pub person: String,
    pub conversation: String,
    pub message: String,
    pub chat: String,
}

/// One channel's routing table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ledger {
    pub notices: Vec<Notice>,
    pub active: Vec<Active>,
    pub relayed: Vec<Relayed>,
}
fn mine<'a>(to: &'a Recipient) -> impl Fn(&str, &str) -> bool + 'a {
    move |person, conversation| person == to.person && conversation == to.conversation
}
impl Ledger {
    pub fn sent(&self, to: &Recipient, chat: &str, kind: Kind, id: &str) -> bool {
        self.notices.iter().any(|n| mine(to)(&n.person, &n.conversation) && n.chat == chat && n.kind == kind && n.id == id)
    }
    pub fn record(&mut self, notice: Notice) {
        self.notices.push(notice);
        let excess = self.notices.len().saturating_sub(LIMIT);
        self.notices.drain(..excess);
    }
    /// The request a message refers to: a reply to its notification, or a
    /// typed command naming it. Only that recipient's notifications match.
    pub fn target(&self, to: &Recipient, inbound: Inbound<'_>) -> Option<&Notice> {
        let notices = self.notices.iter().rev().filter(|n| mine(to)(&n.person, &n.conversation));
        match inbound.replied {
            Some(message) => notices.into_iter().find(|n| n.message == message),
            None => {
                let (kind, id) = operator::named(inbound.text)?;
                notices.into_iter().find(|n| n.kind == kind && n.id == id)
            }
        }
    }
    pub fn active(&self, to: &Recipient) -> Option<&Active> {
        self.active.iter().find(|a| mine(to)(&a.person, &a.conversation))
    }
    /// Talk to `chat` from now on, relaying replies from message `delivered`;
    /// `None` returns to the conversation's own chat.
    pub fn select(&mut self, to: &Recipient, chat: Option<(String, usize)>) {
        self.active.retain(|a| !mine(to)(&a.person, &a.conversation));
        if let Some((chat, delivered)) = chat {
            self.active.push(Active { person: to.person.clone(), conversation: to.conversation.clone(), chat, delivered, revision: Value::Null });
        }
    }
    pub fn relay(&mut self, relayed: Relayed) {
        self.relayed.push(relayed);
        let excess = self.relayed.len().saturating_sub(LIMIT);
        self.relayed.drain(..excess);
    }
    /// Where a message goes, in order: a decision for a notified request
    /// (only while `forwarding`), navigation, a reply to a relayed chat
    /// message, the selected chat. `None` leaves it to the adapter (a Discord
    /// DM's own chat).
    pub fn route(&self, to: &Recipient, inbound: Inbound<'_>, forwarding: bool) -> Option<Routed> {
        if forwarding && let Some(notice) = self.target(to, inbound) {
            return Some(Routed::Decide(notice.clone()));
        }
        if let Some(navigation) = navigation(inbound.text) {
            return Some(Routed::Navigate(navigation));
        }
        let relayed = inbound.replied.and_then(|message| self.relayed.iter().rev().find(|r| mine(to)(&r.person, &r.conversation) && r.message == message));
        relayed.map(|r| &r.chat).or(self.active(to).map(|a| &a.chat)).map(|chat| Routed::Chat(chat.clone()))
    }
}

/// What an operator's message asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routed {
    /// A decision for a notified request.
    Decide(Notice),
    /// `/chats`, `/resume ID` or `/new`.
    Navigate(Navigation),
    /// A message to a chat through `control.chat.send`.
    Chat(String),
    /// The command list.
    Help,
}

/// What became of an action pressed on a request message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pressed {
    /// Not one of the recipient's request messages from this node: say nothing.
    Ignored,
    /// Decided; the edited message shows the outcome.
    Settled,
    /// Tell the recipient this (an error, or the outcome when the message
    /// could not be edited).
    Reply(String),
}

/// `/chats`: the recent chats in a `control.chats.list` view, with how to
/// select one and how to return to this conversation's own chat.
pub fn chat_list(to: &Recipient, list: &Value) -> String {
    let recent = chats::recent(&chats::entries(list.as_array().map(Vec::as_slice).unwrap_or(&[])), RECENT);
    let back = to.home.as_ref().map(|home| format!(", /resume {home} for this conversation's own chat")).unwrap_or_default();
    format!("Recent chats. Reply /resume ID to talk to one, /new for a new chat{back}.\n\n{}", chats::resume_list(&recent))
}

/// The notification for one pending request.
pub fn notice_text(title: &str, link: &str, pending: &Pending<'_>) -> String {
    let mut text = format!("Control · {}\n{}", title.chars().take(100).collect::<String>(), pending.heading);
    if !pending.body.is_empty() {
        text.push_str(&format!("\n{}", pending.body.chars().take(1200).collect::<String>()));
    }
    if let Some(usage) = &pending.usage {
        text.push_str(&format!("\n{usage}"));
    }
    text.push_str(&format!("\nReply to this message to decide, or open {link}"));
    text
}
/// A finished chat reply, within the service's length.
pub fn reply_text(title: &str, status: &str, link: &str, reply: &str, max: usize) -> String {
    let head = format!("Control · {} · {status}\n{link}\n\n", title.chars().take(100).collect::<String>());
    let reply = if reply.is_empty() { "No report was produced." } else { reply };
    let budget = max.saturating_sub(head.chars().count());
    if reply.chars().count() > budget {
        format!("{head}{}\n… Full reply in chat.", reply.chars().take(budget.saturating_sub(22)).collect::<String>())
    } else {
        format!("{head}{reply}")
    }
}

/// One channel's connection to Control and its routing table.
pub struct Channel {
    name: String,
    hive: Option<(Hive, Updates)>,
    refreshed: Option<Instant>,
    revisions: Value,
    pub ledger: Ledger,
}
impl Channel {
    fn path(name: &str) -> Result<PathBuf, &'static str> {
        crate::config::state(&format!("channels/{name}.json"))
    }
    /// Channel `name` with its saved routing table.
    pub fn open(name: &str) -> Self {
        let ledger = Self::path(name).ok().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { name: name.into(), hive: None, refreshed: None, revisions: Value::Null, ledger }
    }
    fn save(&self) {
        let saved = Self::path(&self.name).and_then(|path| {
            let directory = path.parent().ok_or("Invalid channel path")?;
            std::fs::create_dir_all(directory).map_err(|_| "Could not create channel state.")?;
            let bytes = serde_json::to_vec(&self.ledger).map_err(|_| "Could not encode channel routing.")?;
            crate::config::write_in(directory, &format!("{}.json", self.name), &bytes).map_err(|_| "Could not save channel routing.")
        });
        if let Err(message) = saved {
            eprintln!("{message}");
        }
    }
    /// The operator interface, connected on first use and after a failure.
    fn hive(&mut self) -> Result<&mut Hive, &'static str> {
        if self.hive.is_none() {
            let hive = Hive::connect(&crate::config::server_url()).map_err(|_| "Operator interface unavailable; is control serve running?")?;
            let updates = hive.updates().map_err(|_| "Operator update stream unavailable.")?;
            self.hive = Some((hive, updates));
            self.refreshed = None;
        }
        Ok(&mut self.hive.as_mut().unwrap().0)
    }
    pub fn disconnect(&mut self) {
        self.hive = None;
    }
    fn link(chat: &str) -> String {
        format!("{}/chat/{chat}", crate::config::server_url())
    }

    /// Whether the core handles this message; the adapter consumes it before
    /// calling [`Channel::run`], so a crash cannot repeat it. Only operators
    /// reach here (the adapter checks identity); forwarding only gates
    /// decisions, since without it there is no request to reply to.
    pub fn route(&self, settings: &Settings, to: &Recipient, inbound: Inbound<'_>) -> Option<Routed> {
        let text = settings.command(inbound.text);
        if text == "/help" {
            return Some(Routed::Help);
        }
        self.ledger.route(to, Inbound { replied: inbound.replied, text: &text }, settings.enabled)
    }
    /// Carries out a routed message and returns the reply for the recipient,
    /// in this channel's command spelling.
    pub fn run(&mut self, transport: &impl Transport, settings: &Settings, to: &Recipient, routed: Routed, text: &str) -> String {
        let text = settings.command(text);
        let result = match routed {
            Routed::Help => Ok(settings.help()),
            Routed::Decide(notice) => {
                let result = self.decide(&notice, &text);
                if result.is_ok() {
                    self.settle(transport, &notice, &Settled { text: "Decided by you".into(), severity: request::Severity::Success });
                }
                result
            }
            Routed::Navigate(navigation) => self.navigate(to, navigation),
            Routed::Chat(chat) if text.is_empty() => Ok(format!("Control: send the message as text.\n{}", Self::link(&chat))),
            Routed::Chat(chat) => self.hive().map_err(str::to_owned)
                .and_then(|hive| operator::execute(hive, &chat, &operator::Command::Send(text.as_ref().into())).map_err(|e| e.to_string()))
                .map(|()| format!("Sent.\n{}", Self::link(&chat))),
        };
        self.answer(settings, result)
    }
    /// Saves the routing table and returns `result` for the recipient.
    fn answer(&mut self, settings: &Settings, result: Result<String, String>) -> String {
        self.save();
        settings.present(&result.unwrap_or_else(|error| {
            if error.starts_with("Control is unavailable") {
                self.hive = None;
            }
            format!("Control: {error}")
        }))
    }
    /// Carries out action `action` pressed on request message `message` (with
    /// prompted `text` for `answer`) as the reply it stands for, through the
    /// same decision path as a typed reply, then settles the message. `None`
    /// means the message is not one of `to`'s request notifications from this
    /// node: the adapter ignores the press.
    pub fn press(&mut self, transport: &impl Transport, settings: &Settings, to: &Recipient, message: &str, action: &str, text: Option<&str>) -> Pressed {
        let (notice, reply) = match self.ledger.press(to, message, action, text) {
            Err(Refusal::Unknown) => return Pressed::Ignored,
            Err(Refusal::Settled) => return Pressed::Reply("Control: that request was already decided or is no longer pending.".into()),
            Err(Refusal::Invalid) => return Pressed::Reply("Control: that action does not apply to this request.".into()),
            Ok(pressed) => pressed,
        };
        if !settings.enabled {
            return Pressed::Reply("Control: request forwarding is off, so decide this request on the website or in the TUI.".into());
        }
        let result = self.decide(&notice, &settings.command(&reply));
        let edited = match &result {
            Ok(_) => self.settle(transport, &notice, &request::decided(action)),
            // A refusal for a request that is gone closes its message.
            Err(_) if self.pending_now(&notice) == Some(false) => self.settle(transport, &notice, &request::closed()),
            Err(_) => false,
        };
        let answer = self.answer(settings, result);
        // The edited message already shows a successful decision.
        if edited && !answer.starts_with("Control: ") { Pressed::Settled } else { Pressed::Reply(answer) }
    }
    /// Marks `notice` settled and, if it carries actions, edits its message.
    /// Returns whether the message was edited.
    fn settle(&mut self, transport: &impl Transport, notice: &Notice, settled: &Settled) -> bool {
        let Some(slot) = self.ledger.notices.iter_mut().find(|n| n.person == notice.person && n.conversation == notice.conversation && n.message == notice.message) else { return false };
        slot.settled = true;
        if !slot.actions {
            return false;
        }
        let to = Recipient { person: notice.person.clone(), conversation: notice.conversation.clone(), home: None };
        match transport.settle(&to, &notice.message, settled) {
            Posted::Sent(_) => true,
            Posted::Retry => {
                eprintln!("A channel could not update a decided request's message.");
                false
            }
            Posted::Refused => false,
        }
    }
    /// Whether `notice`'s request is still pending, when that can be read.
    fn pending_now(&mut self, notice: &Notice) -> Option<bool> {
        let groups = self.hive().ok()?.tool("control.requests.list", json!({})).ok()?;
        Some(open_requests(&groups).contains(&(notice.chat.as_str(), notice.kind, notice.id.as_str())))
    }
    /// A reply to `notice`, as the TUI would decide it in that chat; the
    /// server rechecks that it is still pending.
    fn decide(&mut self, notice: &Notice, text: &str) -> Result<String, String> {
        let command = operator::reply(&notice.pending(), text).map_err(str::to_owned)?;
        let hive = self.hive()?;
        operator::execute(hive, &notice.chat, &command).map_err(|e| format!("could not apply that decision: {e}"))?;
        Ok(format!("Decision sent.\n{}", Self::link(&notice.chat)))
    }
    /// `/chats`, `/resume ID` and `/new`, with the TUI's meaning.
    fn navigate(&mut self, to: &Recipient, navigation: Navigation) -> Result<String, String> {
        let home = to.home.as_deref().unwrap_or("");
        if matches!(&navigation, Navigation::Open(id) if id == home) {
            self.ledger.select(to, None);
            return Ok("Back to this conversation's own chat.".into());
        }
        // `/use` applies to the selected chat, else the conversation's own chat.
        let target = self.ledger.active(to).map(|a| a.chat.clone()).or_else(|| to.home.clone());
        let hive = self.hive()?;
        let fail = |e: std::io::Error| e.to_string();
        match navigation {
            // Naming no lens changes nothing; say how to use it.
            Navigation::Use(None) => Ok("Usage: /use LENS… (space or comma separated), or /use none to clear.".into()),
            Navigation::Use(Some(lenses)) => {
                let chat = target.ok_or("Select a chat with /chats first.")?;
                let detail = chats::use_lenses(hive, &chat, &lenses).map_err(fail)?;
                let names = if lenses.is_empty() { "no lenses".to_owned() } else { lenses.join(" + ") };
                Ok(format!("Future messages in {}: {names}.\n{}", detail["title"].as_str().unwrap_or("this chat"), Self::link(&chat)))
            }
            Navigation::Rename(name) => {
                let chat = target.ok_or("Select a chat with /chats first.")?;
                let detail = chats::rename(hive, &chat, &name).map_err(fail)?;
                Ok(format!("Renamed this chat to {}.\n{}", detail["title"].as_str().unwrap_or(&name), Self::link(&chat)))
            }
            Navigation::List => {
                let list = hive.tool("control.chats.list", json!({})).map_err(fail)?;
                Ok(chat_list(to, &list))
            }
            Navigation::Open(id) => {
                let detail = hive.tool("control.chat.read", json!({"id":id})).map_err(fail)?;
                let count = detail["messages"].as_array().map_or(0, Vec::len);
                self.ledger.select(to, Some((id.clone(), count)));
                Ok(format!("Now talking to {}. Messages go to that chat and its replies come here; /chats to switch.\n{}", detail["title"].as_str().unwrap_or("Chat"), Self::link(&id)))
            }
            Navigation::New => {
                let detail = hive.tool("control.chat.create", json!({})).map_err(fail)?;
                let id = detail["id"].as_str().unwrap_or("").to_owned();
                self.ledger.select(to, Some((id.clone(), 0)));
                Ok(format!("New chat started. Messages go to it and its replies come here.\n{}", Self::link(&id)))
            }
        }
    }

    /// Follows the node as the TUI does: on the server's change hints (or
    /// every 10 s while they are disconnected) it reads `control.chats.list`,
    /// then re-reads only what moved: pending requests when a decision revision
    /// changed and forwarding is on, and a selected chat when its revision
    /// changed (relayed whether or not requests are forwarded).
    pub fn follow(&mut self, transport: &impl Transport, settings: &Settings, recipients: &[Recipient]) -> Result<(), &'static str> {
        if recipients.is_empty() || (!settings.enabled && self.ledger.active.is_empty()) {
            self.hive = None;
            return Ok(());
        }
        self.hive()?;
        let stale = self.refreshed.is_none_or(|at| at.elapsed() >= Duration::from_secs(10));
        let first = self.refreshed.is_none();
        let Some((hive, updates)) = self.hive.as_mut() else { return Ok(()) };
        if !(updates.changed() || first || (stale && !updates.connected())) {
            return Ok(());
        }
        let Ok(list) = hive.tool("control.chats.list", json!({})) else {
            self.hive = None;
            return Err("Could not read the chat list; retrying.");
        };
        self.refreshed = Some(Instant::now());
        let mut retry = false;
        let revisions = json!([list[0]["decision_revision"], list[0]["port_revision"], list[0]["bot_revision"]]);
        if !settings.enabled {
            // Turning forwarding on later reads the pending requests afresh.
            self.revisions = Value::Null;
        } else if first || revisions != self.revisions {
            match hive.tool("control.requests.list", json!({})) {
                Ok(groups) => {
                    self.revisions = revisions;
                    retry |= self.notify(transport, settings, recipients, &groups);
                }
                Err(_) => retry = true,
            }
        }
        let entries = chats::entries(list.as_array().map(Vec::as_slice).unwrap_or(&[]));
        for index in 0..self.ledger.active.len() {
            let active = self.ledger.active[index].clone();
            let Some(to) = recipients.iter().find(|to| mine(to)(&active.person, &active.conversation)) else { continue };
            let Some(entry) = entries.iter().find(|entry| entry.id == active.chat) else { continue };
            if *entry.revision == active.revision {
                continue;
            }
            let Some((hive, _)) = self.hive.as_mut() else { break };
            let Ok(detail) = hive.tool("control.chat.read", json!({"id":active.chat})) else { retry = true; continue };
            retry |= self.relay(transport, to, index, entry.title, entry.revision, &detail);
        }
        self.save();
        if retry {
            self.refreshed = None;
        }
        Ok(())
    }
    /// Sends each forwarded pending request in `groups` (`control.requests.list`)
    /// once per recipient. Returns whether a send should be retried.
    pub fn notify(&mut self, transport: &impl Transport, settings: &Settings, recipients: &[Recipient], groups: &Value) -> bool {
        let mut retry = false;
        let actions = transport.capabilities().buttons;
        for group in groups.as_array().into_iter().flatten() {
            let (Some(chat), title) = (group["chat_id"].as_str(), group["title"].as_str().unwrap_or("Chat")) else { continue };
            for pending in operator::pending(group) {
                if pending.id.is_empty() || !settings.forwards(chat, pending.kind) || recipients.iter().all(|to| self.ledger.sent(to, chat, pending.kind, pending.id)) {
                    continue;
                }
                let link = Self::link(chat);
                let text = settings.present(&notice_text(title, &link, &pending));
                let rich = actions.then(|| request::request(settings, title, &link, &pending, group));
                for to in recipients {
                    if self.ledger.sent(to, chat, pending.kind, pending.id) {
                        continue;
                    }
                    let key = format!("{}:request:{}:{}:{chat}:{}:{}", self.name, to.person, to.conversation, pending.kind.as_str(), pending.id);
                    let posted = match &rich {
                        Some(rich) => transport.post_request(to, rich, &text, &key),
                        None => transport.post(to, &text, None, &key),
                    };
                    match posted {
                        Posted::Sent(Some(message)) => {
                            self.ledger.record(Notice { person: to.person.clone(), conversation: to.conversation.clone(), message, chat: chat.into(), kind: pending.kind, id: pending.id.into(), loopback: pending.loopback, settled: false, actions });
                            self.save();
                        }
                        // Without a message ID a reply could not be routed: retry.
                        Posted::Sent(None) | Posted::Retry => retry = true,
                        Posted::Refused => eprintln!("A channel refused an operator notification."),
                    }
                }
            }
        }
        // Requests decided elsewhere (the website, the TUI, another recipient)
        // or expired: their messages stop offering actions.
        let open = open_requests(groups);
        let closed = self.ledger.notices.iter()
            .filter(|n| !n.settled && recipients.iter().any(|to| mine(to)(&n.person, &n.conversation)) && !open.contains(&(n.chat.as_str(), n.kind, n.id.as_str())))
            .cloned().collect::<Vec<_>>();
        for notice in &closed {
            self.settle(transport, notice, &request::closed());
        }
        if !closed.is_empty() {
            self.save();
        }
        retry
    }
    /// Relays the finished replies of selection `index` from a `control.chat.read`
    /// view, in order. Returns whether a send should be retried.
    pub fn relay(&mut self, transport: &impl Transport, to: &Recipient, index: usize, title: &str, revision: &Value, detail: &Value) -> bool {
        let active = self.ledger.active[index].clone();
        let (replies, next) = chats::replies(detail, active.delivered);
        let link = Self::link(&active.chat);
        let max = transport.capabilities().max_text.max(200);
        let mut delivered = next;
        for reply in replies {
            let key = format!("{}:reply:{}:{}:{}:{}", self.name, to.person, to.conversation, active.chat, reply.index);
            match transport.post(to, &reply_text(title, reply.status, &link, reply.text, max), None, &key) {
                Posted::Sent(message) => {
                    if let Some(message) = message {
                        self.ledger.relay(Relayed { person: to.person.clone(), conversation: to.conversation.clone(), message, chat: active.chat.clone() });
                    }
                }
                Posted::Refused => eprintln!("A channel refused a chat reply."),
                Posted::Retry => {
                    delivered = reply.index;
                    break;
                }
            }
        }
        let slot = &mut self.ledger.active[index];
        slot.delivered = delivered;
        if delivered == next {
            slot.revision = revision.clone();
        }
        delivered != next
    }
}

#[cfg(test)]
mod tests;
