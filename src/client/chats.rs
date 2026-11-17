//! Chat navigation as every operator client offers it, over `control.chats.list`
//! and `control.chat.read`. `/chats` lists, `/resume ID` switches and `/new`
//! starts a chat, `/genes` lists lenses (shown as genes) and `/use GENE…` sets
//! a chat's lenses (its DNA); anything else is sent to the active chat with
//! `control.chat.send`, where the server interprets chat commands (/steer,
//! /cancel, /answer …) the same way for the web, TUI and Discord.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Navigation {
    List,
    Open(String),
    New,
    /// `/use LENS…` for future messages; `/use none` clears. `None` when it
    /// names no lens: a no-op on every client.
    Use(Option<Vec<String>>),
    /// `/genes` (or `/lenses`) lists the genes (lens definitions) `/use` selects from.
    Genes,
    /// `/rename NAME` names the active chat in every client (`control.chat.rename`).
    /// The server validates the name; an empty one returns its usage.
    Rename(String),
}
/// Client navigation commands, for help text and menus.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/chats", "List recent DMs"),
    ("/resume ID", "Talk to a DM"),
    ("/new", "Start a DM"),
    ("/genes", "List the genes /use can select"),
    ("/use GENE…", "Set this DM's DNA for future messages; /use none clears"),
];
pub fn navigation(text: &str) -> Option<Navigation> {
    let text = text.trim();
    if let Some(arguments) = use_arguments(text) {
        return Some(Navigation::Use(lenses(arguments).map(|names| names.into_iter().map(str::to_owned).collect())));
    }
    if let Some(name) = control_context::commands::tail(text, control_context::commands::Kind::Rename) {
        return Some(Navigation::Rename(name.trim().into()));
    }
    match text {
        "/chats" | "/resume" => Some(Navigation::List),
        "/new" => Some(Navigation::New),
        "/genes" | "/lenses" => Some(Navigation::Genes),
        text => text.strip_prefix("/resume ").map(str::trim).filter(|id| !id.is_empty() && !id.contains(char::is_whitespace)).map(|id| Navigation::Open(id.into())),
    }
}

// The one `/use` grammar, shared by every client's submission and the TUI's
// live highlighting.
/// Lens names are separated by whitespace and/or commas.
pub fn lens_separator(c: char) -> bool {
    c.is_whitespace() || c == ','
}
/// The argument text of a `/use` command (empty for a bare `/use`).
pub fn use_arguments(text: &str) -> Option<&str> {
    let tail = text.strip_prefix("/use")?;
    (tail.is_empty() || tail.starts_with(char::is_whitespace)).then_some(tail)
}
/// Each lens name in `/use` arguments, in order.
pub fn lens_names(arguments: &str) -> impl Iterator<Item = &str> {
    arguments.split(lens_separator).filter(|name| !name.is_empty())
}
/// The lenses `/use` arguments select: `None` when they name none (a no-op),
/// empty for `none` (clear).
pub fn lenses(arguments: &str) -> Option<Vec<&str>> {
    let names = lens_names(arguments).collect::<Vec<_>>();
    match names.as_slice() {
        [] => None,
        ["none"] => Some(Vec::new()),
        _ => Some(names),
    }
}
/// Selected names absent from the library, in input order without repeats.
pub fn unknown<'a>(names: &[&'a str], known: impl Fn(&str) -> bool) -> Vec<&'a str> {
    let mut missing = Vec::new();
    for name in names.iter().copied().filter(|name| !known(name)) {
        if !missing.contains(&name) {
            missing.push(name);
        }
    }
    missing
}
/// Checks lenses against the server's library (`control.lenses.list`) and
/// resolves them, so invalid names change nothing. Returns the library.
pub fn check_lenses(hive: &mut super::Hive, lenses: &[impl AsRef<str>]) -> std::io::Result<Value> {
    let library = hive.tool("control.lenses.list", serde_json::json!({}))?;
    let ids = library.as_array().ok_or_else(|| std::io::Error::other("Invalid lens library response."))?
        .iter().filter_map(|d| d["id"].as_str()).collect::<std::collections::HashSet<_>>();
    let names = lenses.iter().map(AsRef::as_ref).collect::<Vec<_>>();
    let missing = unknown(&names, |id| ids.contains(id));
    if !missing.is_empty() {
        return Err(std::io::Error::other(format!("Unknown {}: {}. Nothing changed; /genes lists them.", if missing.len() == 1 { "gene" } else { "genes" }, missing.join(", "))));
    }
    hive.tool("control.lenses.resolve", serde_json::json!({"lenses":names}))?;
    Ok(library)
}
/// Applies lenses to `chat` as the TUI and web lens picker do: checked first,
/// then set atomically. Returns the chat view.
pub fn use_lenses(hive: &mut super::Hive, chat: &str, lenses: &[String]) -> std::io::Result<Value> {
    check_lenses(hive, lenses)?;
    hive.tool("control.chat.lenses", serde_json::json!({"id":chat,"lenses":lenses}))
}

/// Names `chat` as `/rename` does, for every client. Returns the chat view,
/// whose `title` is the new name.
pub fn rename(hive: &mut super::Hive, chat: &str, name: &str) -> std::io::Result<Value> {
    hive.tool("control.chat.rename", serde_json::json!({"id":chat,"name":name}))
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
/// `/genes`: each gene in a `control.lenses.list` view with its version and
/// the genes it includes, after how to select them.
pub fn gene_list(library: &Value) -> String {
    let mut lines = vec!["Genes. Set a DM's DNA with /use engineering evidence; clear with /use none.".to_owned()];
    for gene in library.as_array().into_iter().flatten() {
        lines.push(format!("{} · {} · v{}", gene["id"].as_str().unwrap_or("?"), gene["name"].as_str().unwrap_or("?"), gene["revision"].as_u64().unwrap_or(0)));
        if let Some(includes) = gene["includes"].as_array().filter(|a| !a.is_empty()) {
            lines.push(format!("  includes {}", includes.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + ")));
        }
    }
    lines.join("\n")
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
