//! Discord rendering of structured operator requests: an embed coloured by
//! severity with buttons for its actions, a modal for free-text answers, and
//! the interactions those produce. Pure functions over JSON, so they are
//! tested without Discord; the bridge and gateway do the I/O.
//!
//! A button's custom ID is `ctl:NODE:ACTION`. `NODE` is a short digest of
//! this node's identity, so a node only acknowledges presses on its own
//! messages; `ACTION` is one of the core's fixed action IDs. Neither carries a
//! request ID: the core finds the request from the pressed message in its
//! routing table and rechecks it ([`control_client::channel::Channel::press`]).
use control_client::channel::request::{Act, Request, Settled, Severity, Style};
use control_identity::DiscordId;
use serde_json::{Value, json};

/// This node's custom-ID tag: eight hex digits of its identity's digest.
pub(super) fn tag() -> String {
    let node = control_config::node_id().unwrap_or_default();
    ring::digest::digest(&ring::digest::SHA256, format!("control-discord:{node}").as_bytes()).as_ref()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn custom_id(tag: &str, action: &str) -> String {
    format!("ctl:{tag}:{action}")
}

pub(super) fn color(severity: Severity) -> u32 {
    match severity {
        Severity::Attention => 0x5865F2,
        Severity::Warning => 0xF0B232,
        Severity::Danger => 0xDA373C,
        Severity::Success => 0x23A55A,
        Severity::Muted => 0x80848E,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.into();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// The message body for `request`: an embed and up to five rows of buttons.
/// Without `links`, link buttons are left out and the link goes in the text.
pub(super) fn message(request: &Request, tag: &str, links: bool) -> Value {
    let chat = request.fields.first().map(|(_, chat)| chat.as_str()).unwrap_or("");
    let mut description = truncate(&request.body, 3500);
    if !links {
        description.push_str(&format!("\n\n{}", truncate(&request.link, 400)));
    }
    let fields = request
        .fields
        .iter()
        .take(25)
        .map(|(name, value)| json!({"name":name,"value":truncate(value, 1024),"inline":value.chars().count() <= 40}))
        .collect::<Vec<_>>();
    let embed = json!({
        "title": truncate(&request.title, 256),
        "description": description,
        "color": color(request.severity),
        "fields": fields,
        "footer": {"text": truncate(&request.hint, 2048)},
    });
    // Options first, then decisions and links, each group in rows of five.
    let button = |action: &control_client::channel::request::Action| {
        let label = truncate(&action.label, 80);
        match &action.act {
            Act::Open(url) => links.then(|| json!({"type":2,"style":5,"label":label,"url":url})),
            Act::Reply | Act::Prompt => {
                let style = match action.style {
                    Style::Primary => 1,
                    Style::Secondary => 2,
                    Style::Success => 3,
                    Style::Danger => 4,
                };
                Some(json!({"type":2,"style":style,"label":label,"custom_id":custom_id(tag, action.id)}))
            }
        }
    };
    let option = |action: &&control_client::channel::request::Action| action.id.starts_with('o') && action.id != "open";
    let (options, rest): (Vec<_>, Vec<_>) = request.actions.iter().partition(option);
    let mut rows = Vec::new();
    for group in [options, rest] {
        let buttons = group.into_iter().filter_map(button).collect::<Vec<_>>();
        rows.extend(buttons.chunks(5).map(|row| json!({"type":1,"components":row})));
    }
    rows.truncate(5);
    json!({
        "content": truncate(&format!("Control · {} · {chat}", request.title), 200),
        "embeds": [embed],
        "components": rows,
        "allowed_mentions": {"parse": []},
    })
}

/// The edit that settles a posted request `message` (as Discord returns it):
/// its colour and footer show the outcome and only link buttons remain.
/// `None` when the message has no embed (a plain-text fallback).
pub(super) fn settled(message: &Value, settled: &Settled) -> Option<Value> {
    let embed = message["embeds"].get(0)?;
    let mut kept = json!({"color": color(settled.severity), "footer": {"text": truncate(&settled.text, 2048)}});
    for key in ["title", "description", "fields", "url"] {
        if !embed[key].is_null() {
            kept[key] = embed[key].clone();
        }
    }
    let links = message["components"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row["components"].as_array().into_iter().flatten())
        .filter(|button| button["type"] == 2 && button["style"] == 5)
        .map(|button| json!({"type":2,"style":5,"label":button["label"],"url":button["url"]}))
        .collect::<Vec<_>>();
    let rows = if links.is_empty() {
        json!([])
    } else {
        json!([{"type":1,"components":links}])
    };
    Some(json!({"embeds":[kept],"components":rows}))
}

/// A button press or modal answer on one of this node's request messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Press {
    /// The interaction's ID and token, for its response.
    pub id: String,
    pub token: String,
    pub user: DiscordId,
    pub channel: DiscordId,
    /// The pressed message.
    pub message: String,
    pub action: String,
    /// The text submitted in the answer modal.
    pub text: Option<String>,
}
impl Press {
    /// Whether this press opens the answer modal instead of deciding.
    pub fn prompts(&self) -> bool {
        self.action == "answer" && self.text.is_none()
    }
    /// The immediate interaction response: the answer modal, or an
    /// acknowledgement that the message will be updated.
    pub fn response(&self, tag: &str) -> Value {
        if !self.prompts() {
            return json!({"type":6});
        }
        json!({"type":9,"data":{"custom_id":custom_id(tag, "answer"),"title":"Answer the question","components":[
            {"type":1,"components":[{"type":4,"custom_id":"text","label":"Your answer or an option number","style":2,"min_length":1,"max_length":1500,"required":true}]}]}})
    }
}

fn snowflake(value: &Value) -> Option<DiscordId> {
    value.as_str()?.parse().ok()
}

/// The text a modal submitted for its `text` input, in either component layout.
fn submitted(data: &Value) -> Option<String> {
    let inputs = data["components"].as_array()?.iter().flat_map(|row| {
        row["components"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(row.get("component"))
    });
    for input in inputs {
        if input["custom_id"] == "text" {
            return input["value"]
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| text.chars().take(1500).collect());
        }
    }
    None
}

/// An `INTERACTION_CREATE` payload as a press on this node's message, or
/// `None` when it is not one: another node's or application's custom ID, a
/// server interaction (only DMs are served), or a malformed payload. Who
/// pressed and whether the message is theirs is checked by the core.
pub(super) fn press(interaction: &Value, tag: &str) -> Option<Press> {
    if !interaction["guild_id"].is_null() {
        return None;
    }
    let data = &interaction["data"];
    let action = data["custom_id"]
        .as_str()?
        .strip_prefix("ctl:")?
        .strip_prefix(tag)?
        .strip_prefix(':')?;
    if action.is_empty() || action.len() > 12 || !action.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
        return None;
    }
    let text = match interaction["type"].as_u64()? {
        3 if data["component_type"] == 2 => None,
        5 if action == "answer" => Some(submitted(data)?),
        _ => return None,
    };
    let (id, token) = (interaction["id"].as_str()?, interaction["token"].as_str()?);
    if id.is_empty() || token.is_empty() || token.len() > 1000 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    snowflake(&interaction["id"])?;
    let message = interaction["message"]["id"]
        .as_str()
        .filter(|m| m.parse::<DiscordId>().is_ok())?;
    Some(Press {
        id: id.into(),
        token: token.into(),
        user: snowflake(&interaction["user"]["id"])?,
        channel: snowflake(&interaction["channel_id"]).or_else(|| snowflake(&interaction["channel"]["id"]))?,
        message: message.into(),
        action: action.into(),
        text,
    })
}

#[cfg(test)]
mod tests;
