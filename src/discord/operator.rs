//! Routing for the operator interface over Discord: which notification message
//! stands for which pending request in which chat. This table is the only
//! Discord-specific state; requests, commands and authorization are the
//! shared ones in `crate::client::operator` and the server.
use crate::client::operator::{Kind, Pending};
use crate::identity::DiscordId;
use serde::{Deserialize, Serialize};

/// Notifications kept for reply routing; older ones age out.
const LIMIT: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Notice {
    pub user: DiscordId,
    pub channel: DiscordId,
    pub message: DiscordId,
    /// The top-level chat the decision is sent to.
    pub chat: String,
    pub kind: Kind,
    pub id: String,
    #[serde(default)]
    pub loopback: bool,
}
impl Notice {
    pub fn pending(&self) -> Pending<'_> {
        Pending { kind: self.kind, id: &self.id, heading: Default::default(), body: "", usage: None, loopback: self.loopback }
    }
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Ledger {
    pub notices: Vec<Notice>,
}
impl Ledger {
    /// The idempotency key of one request for one recipient; also the send nonce.
    pub fn receipt(user: DiscordId, chat: &str, kind: Kind, id: &str) -> String {
        format!("operator:{user}:{chat}:{kind:?}:{id}")
    }
    pub fn sent(&self, user: DiscordId, chat: &str, kind: Kind, id: &str) -> bool {
        self.notices.iter().any(|n| n.user == user && n.chat == chat && n.kind == kind && n.id == id)
    }
    pub fn record(&mut self, notice: Notice) {
        self.notices.push(notice);
        let excess = self.notices.len().saturating_sub(LIMIT);
        self.notices.drain(..excess);
    }
    /// The request a message in `channel` refers to: a reply to its
    /// notification, or a typed command naming it. Only that recipient's
    /// notifications match, so one person cannot address another's.
    pub fn target(&self, user: DiscordId, channel: DiscordId, replied: Option<DiscordId>, text: &str) -> Option<&Notice> {
        let mine = |n: &&Notice| n.user == user && n.channel == channel;
        match replied {
            Some(message) => self.notices.iter().rev().filter(mine).find(|n| n.message == message),
            None => {
                let (kind, id) = crate::client::operator::named(text)?;
                self.notices.iter().rev().filter(mine).find(|n| n.kind == kind && n.id == id)
            }
        }
    }
}

/// The notification for one pending request.
pub(crate) fn text(title: &str, link: &str, pending: &Pending<'_>) -> String {
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

#[cfg(test)]
mod tests;
