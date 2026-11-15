pub mod bridge;
mod permissions;
mod delivery;
// Shared ingress types. Credential loading belongs only to the explicitly activated bridge.
use crate::identity::{Action, DiscordId};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Incoming {
    pub message_id: DiscordId,
    pub user_id: DiscordId,
    pub channel_id: DiscordId,
    pub objective: String,
    #[serde(default)]
    pub action: Action,
    #[serde(default)]
    pub direct: bool,
}
impl Incoming {
    pub fn actor(&self) -> crate::identity::Actor {
        if self.direct {
            crate::identity::Actor::DiscordDm {
                user_id: self.user_id,
                channel_id: self.channel_id,
            }
        } else {
            crate::identity::Actor::Discord {
                user_id: self.user_id,
                channel_id: self.channel_id,
            }
        }
    }
}
pub fn chat_id(user: DiscordId, channel: DiscordId) -> String {
    format!("d-{user}-{channel}")
}
pub fn chat_link(actor: &crate::identity::Actor) -> Option<String> {
    match actor {
        crate::identity::Actor::DiscordDm {
            user_id,
            channel_id,
        } => Some(format!(
            "{}/chat/{}",
            crate::config::server_url(),
            chat_id(*user_id, *channel_id)
        )),
        _ => None,
    }
}
pub fn session_link(actor: &crate::identity::Actor, id: Option<&str>) -> Option<String> {
    let fallback = chat_link(actor)?;
    Some(id.filter(|id| !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .map(|id| format!("{}/chat/{id}", crate::config::server_url()))
        .unwrap_or(fallback))
}
pub fn native_reply(actor: &crate::identity::Actor, content: &str) -> Option<String> {
    let link = chat_link(actor)?;
    if ![
        "hi",
        "hello",
        "hey",
        "hello control",
        "hi control",
        "woof",
        "help",
        "/help",
        "status",
        "/status",
    ]
    .contains(&content.trim().to_lowercase().as_str())
    {
        return None;
    }
    let role = if crate::identity::authorize(actor, Action::Improve).is_ok() {
        "architect"
    } else {
        "user"
    };
    Some(format!("Woof! Control is online. Role: {role}. DM me a task, or use /improve for an architect request.\nContinue on this laptop: {link}"))
}
pub fn direct_command(content: &str) -> Option<(Action, &str)> {
    let text = content.trim();
    if text.starts_with("!control ") {
        return command(text);
    }
    if text.is_empty() || text.chars().count() > 2000 {
        return None;
    }
    chat_command(text)
}
/// Local chat accepts longer input; Discord ingress retains its own bound.
pub fn chat_command(content: &str) -> Option<(Action, &str)> {
    let text = content.trim();
    if text.is_empty() || control_context::limits::message_too_long(text) {
        return None;
    }
    if let Some(task) = text.strip_prefix("!control ").map(str::trim) {
        return (!task.is_empty()).then(|| command_task(task));
    }
    if text == "/improve" {
        return Some((Action::Improve, "on"));
    }
    if let Some(objective) = text
        .strip_prefix("/improve ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some((Action::Improve, objective))
    } else {
        Some((Action::Task, text))
    }
}
/// Why a DM that `direct_command` ignores was not accepted, so the sender is told instead of silence.
pub fn direct_rejection(content: &str, attachments: bool) -> Option<&'static str> {
    let text = content.trim();
    if text.chars().count() > 2000 {
        Some("Control: that message is over 2000 characters, so it was not queued. Shorten it or split it into parts.")
    } else if text.is_empty() && attachments {
        Some("Control: attachments are not supported yet, so nothing was queued. Send the task as text.")
    } else {
        None
    }
}
pub fn command(content: &str) -> Option<(Action, &str)> {
    let task = task(content)?;
    Some(command_task(task))
}
fn command_task(task: &str) -> (Action, &str) {
    if let Some(objective) = task
        .strip_prefix("improve ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        (Action::Improve, objective)
    } else {
        (Action::Task, task)
    }
}
pub fn task(content: &str) -> Option<&str> {
    let objective = content.strip_prefix("!control ")?.trim();
    if objective.is_empty() || objective.chars().count() > 2000 {
        None
    } else {
        Some(objective)
    }
}
#[cfg(test)]
mod tests;
