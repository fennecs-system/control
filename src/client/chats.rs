//! Chat navigation as every operator client offers it, over `control.chats.list`
//! and `control.chat.read`. `/chats` lists, `/resume ID` switches and `/new`
//! starts a chat; anything else is sent to the active chat with
//! `control.chat.send`, where the server interprets chat commands (/steer,
//! /cancel, /answer …) the same way for the web, TUI and Discord.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Navigation {
    List,
    Open(String),
    New,
}
pub fn navigation(text: &str) -> Option<Navigation> {
    match text.trim() {
        "/chats" | "/resume" => Some(Navigation::List),
        "/new" => Some(Navigation::New),
        text => text.strip_prefix("/resume ").map(str::trim).filter(|id| !id.is_empty() && !id.contains(char::is_whitespace)).map(|id| Navigation::Open(id.into())),
    }
}

/// One `control.chats.list` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub depth: usize,
    pub activity: u64,
    pub revision: &'a Value,
}
/// Rows of `control.chats.list`, in its order.
pub fn entries(rows: &[Value]) -> Vec<Entry<'_>> {
    rows.iter().map(|chat| Entry {
        id: chat["id"].as_str().unwrap_or(""),
        title: chat["title"].as_str().unwrap_or("Chat"),
        depth: chat["depth"].as_u64().unwrap_or(0).min(8) as usize,
        activity: chat["activity_order"].as_u64().unwrap_or(0),
        revision: &chat["revision"],
    }).collect()
}
/// The most recently active chats first.
pub fn recent<'a>(entries: &[Entry<'a>], limit: usize) -> Vec<Entry<'a>> {
    let mut recent = entries.to_vec();
    recent.sort_by_key(|entry| std::cmp::Reverse(entry.activity));
    recent.truncate(limit);
    recent
}
/// Each chat with the command that opens it.
pub fn resume_list(entries: &[Entry<'_>]) -> String {
    entries.iter().map(|entry| format!("{}\n  /resume {}", entry.title, entry.id)).collect::<Vec<_>>().join("\n\n")
}

/// Statuses whose message is still in progress.
const UNFINISHED: [&str; 4] = ["queued", "running", "cancelling", "waiting"];
/// A finished reply from a `control.chat.read` view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply<'a> {
    pub index: usize,
    pub status: &'a str,
    pub text: &'a str,
}
/// Finished replies from message `from` on, stopping at the first unfinished
/// one, and the index to continue from. Waiting questions are shown as
/// operator requests instead; other statuses without a result are skipped.
pub fn replies(detail: &Value, from: usize) -> (Vec<Reply<'_>>, usize) {
    let messages = detail["messages"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut replies = Vec::new();
    let mut next = from.min(messages.len());
    for (index, message) in messages.iter().enumerate().skip(next) {
        let status = message["status"].as_str().unwrap_or("completed");
        if UNFINISHED.contains(&status) {
            break;
        }
        if crate::outbox::DELIVERABLE.contains(&status) {
            replies.push(Reply { index, status, text: message["reply"].as_str().unwrap_or("") });
        }
        next = index + 1;
    }
    (replies, next)
}

#[cfg(test)]
mod tests;
