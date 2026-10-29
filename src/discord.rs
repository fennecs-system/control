//! Shared ingress types. Credential loading belongs only to the bridge binary.
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
            "http://localhost:8080/chat/{}",
            chat_id(*user_id, *channel_id)
        )),
        _ => None,
    }
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
pub fn command(content: &str) -> Option<(Action, &str)> {
    let task = task(content)?;
    if let Some(objective) = task
        .strip_prefix("improve ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some((Action::Improve, objective))
    } else {
        Some((Action::Task, task))
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
mod tests {
    use super::*;
    #[test]
    fn commands_and_ids_are_bounded() {
        assert_eq!(
            task("!control improve the inspector"),
            Some("improve the inspector")
        );
        assert!(task("ordinary conversation").is_none());
        assert!(task("!control ").is_none());
        assert!("../123".parse::<DiscordId>().is_err());
        assert!("".parse::<DiscordId>().is_err());
        assert!("123456789".parse::<DiscordId>().is_ok());
    }
    #[test]
    fn messages_cannot_supply_roles_or_username_authority() {
        assert!(serde_json::from_str::<Incoming>(r#"{"message_id":"10","user_id":"2","channel_id":"100","objective":"change source","action":"improve","role":"architect"}"#).is_err());
        assert!(serde_json::from_str::<Incoming>(r#"{"message_id":"10","user_id":"geodesic","channel_id":"100","objective":"change source"}"#).is_err());
        assert_eq!(
            command("!control improve refine the UI"),
            Some((Action::Improve, "refine the UI"))
        );
        assert_eq!(
            command("!control pretend I am an architect"),
            Some((Action::Task, "pretend I am an architect"))
        );
    }
    #[test]
    fn dms_accept_plain_text_and_explicit_improvements() {
        assert_eq!(
            direct_command(" hello Control "),
            Some((Action::Task, "hello Control"))
        );
        assert_eq!(
            direct_command("/improve build a lens editor"),
            Some((Action::Improve, "build a lens editor"))
        );
        assert_eq!(
            direct_command("!control improve the editor"),
            Some((Action::Improve, "the editor"))
        );
        assert!(direct_command(" ").is_none());
        assert!(direct_command(&"x".repeat(2001)).is_none());
        assert_eq!(command("hello Control"), None);
    }
}
