//! Terminal command discovery. Shared chat syntax still owns server commands;
//! these local entries describe client actions, never authorization or policy.
use control_context::commands;

#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub text: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

const LOCAL: &[Entry] = &[
    Entry { text: "/compact", usage: "/compact", description: "Inspect the compaction model, window and summary tree" },
    Entry { text: "/help", usage: "/help", description: "Show commands and keyboard shortcuts" },
    Entry { text: "/context", usage: "/context", description: "Inspect the next message's lens context" },
    Entry { text: "/chat", usage: "/chat", description: "Return to the conversation" },
    Entry { text: "/chats", usage: "/chats", description: "Choose a conversation" },
    Entry { text: "/resume", usage: "/resume [ID]", description: "Choose or resume a conversation" },
    Entry { text: "/new", usage: "/new", description: "Start a fresh conversation" },
    Entry { text: "/lenses", usage: "/lenses", description: "Browse and select context lenses" },
    Entry { text: "/use", usage: "/use ID ID | /use none", description: "Set lenses for future messages" },
    Entry { text: "/trust", usage: "/trust [REQUEST]", description: "Approve the folder request displayed in this chat" },
    Entry { text: "/port allow", usage: "/port allow ID [--loopback]", description: "Approve the exact displayed port request" },
    Entry { text: "/port deny", usage: "/port deny ID", description: "Deny a port request" },
    Entry { text: "/bot pause", usage: "/bot pause", description: "Pause this bot and retain its pending events" },
    Entry { text: "/bot resume", usage: "/bot resume", description: "Resume this chat's paused bot" },
    Entry { text: "/bot allow", usage: "/bot allow REQUEST_ID", description: "Approve a displayed persistent-bot request" },
    Entry { text: "/bot deny", usage: "/bot deny REQUEST_ID", description: "Deny a persistent-bot request" },
    Entry { text: "/quit", usage: "/quit", description: "Exit the terminal client" },
];

fn entries() -> impl Iterator<Item = Entry> {
    LOCAL.iter().copied().chain(commands::DEFINITIONS.iter().map(|entry| Entry {
        text: entry.text, usage: match entry.id {
            "improve" => "/improve REQUEST",
            "steer" => "/steer DIRECTION",
            "btw" => "/btw QUESTION",
            "spawn" => "/spawn TASK",
            "answer" => "/answer QUESTION_ID CHOICE",
            "open" => "/open [--as NAME] /absolute/path",
            "bot" => "/bot on [every N]",
            "model" => "/model [PROVIDER:MODEL | default]",
            _ => entry.text,
        }, description: entry.description,
    }))
}

pub(super) fn matching(input: &str) -> impl Iterator<Item = Entry> + '_ {
    let eligible = input.starts_with('/') && !input.contains('\n');
    entries().filter(move |entry| eligible && entry.text.starts_with(input))
}

pub(super) fn highlight(input: &str) -> Option<std::ops::Range<usize>> {
    if !input.starts_with('/') { return None; }
    if let Some(entry) = entries().filter(|entry| input.strip_prefix(entry.text).is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))).max_by_key(|entry| entry.text.len()) {
        return Some(0..entry.text.len());
    }
    if let Some(parsed) = commands::parse(input) { return Some(parsed.highlight); }
    matching(input).next().map(|_| 0..input.len())
}

pub(super) fn help() -> String {
    let mut text = String::from("Commands\n\n");
    for entry in entries() {
        text.push_str(entry.usage);
        text.push_str("\n  ");
        text.push_str(entry.description);
        text.push_str("\n\n");
    }
    text.push_str("Keyboard\n\nTab completes the selected command; it does not execute it.\n↑ ↓ choose a command · Esc dismisses suggestions or returns to chat\nEnter sends · Alt+Enter inserts a newline\nPageUp / PageDown scroll · End follows new output\nCtrl+↑ / Ctrl+↓ switch conversations\nCtrl+N starts a chat · Ctrl+Q exits\n\n/improve REQUEST asks agents to develop Control using the server's configured tools and permissions.\n/open /absolute/path requests a project folder; review it before /trust.\n");
    text
}

#[cfg(test)]
mod tests;
