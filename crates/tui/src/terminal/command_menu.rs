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
    Entry {text:"/github",usage:control_context::github::USAGE,description:"Inspect, comment, edit or merge PRs through the configured GitHub tools"},
    Entry {text:"/activity",usage:"/activity [compact|full]",description:"Fold or expand retained tool activity"},
    Entry { text: "/compact", usage: "/compact", description: "Inspect the Memory Tree status, model, window and blocks" },
    Entry { text: "/help", usage: "/help", description: "Show commands and keyboard shortcuts" },
    Entry { text: "/context", usage: "/context", description: "Inspect the next message's DNA context" },
    Entry { text: "/chat", usage: "/chat", description: "Return to the DM" },
    Entry { text: "/chats", usage: "/chats", description: "Choose a DM" },
    Entry { text: "/resume", usage: "/resume [ID]", description: "Choose or resume a DM" },
    Entry { text: "/new", usage: "/new", description: "Start a fresh DM" },
    Entry { text: "/genes", usage: "/genes", description: "Browse and select genes (alias /lenses)" },
    Entry { text: "/use", usage: "/use GENE GENE | /use GENE, GENE | /use none", description: "Set this DM's DNA for future messages" },
    Entry { text: "/personas", usage: "/personas", description: "List personas (purpose, DNA, standing, model)" },
    Entry { text: "/persona", usage: "/persona NAME | /persona none", description: "Give this DM a persona for future messages" },
    Entry { text: "/trust", usage: "/trust [REQUEST]", description: "Approve the folder request displayed in this DM" },
    Entry { text: "/dismiss", usage: "/dismiss [REQUEST]", description: "Reject the displayed folder request; approved folders stay open" },
    Entry { text: "/port allow", usage: "/port allow ID [--loopback]", description: "Approve the exact displayed port request" },
    Entry { text: "/port deny", usage: "/port deny ID", description: "Deny a port request" },
    Entry { text: "/bot pause", usage: "/bot pause", description: "Pause this bot and retain its pending events" },
    Entry { text: "/bot resume", usage: "/bot resume", description: "Resume this DM's paused bot" },
    Entry { text: "/bot allow", usage: "/bot allow REQUEST_ID", description: "Approve a displayed persistent-bot request" },
    Entry { text: "/bot deny", usage: "/bot deny REQUEST_ID", description: "Deny a persistent-bot request" },
    Entry { text: "/quit", usage: "/quit", description: "Exit the terminal client" },
];

fn build() -> impl Iterator<Item = Entry> {
    LOCAL.iter().copied().chain(commands::DEFINITIONS.iter().map(|entry| Entry {
        text: entry.text, usage: match entry.id {
            "improve" => "/improve REQUEST",
            "steer" => "/steer DIRECTION",
            "btw" => "/btw QUESTION",
            "spawn" => "/spawn TASK",
            "answer" => "/answer QUESTION_ID CHOICE",
            "open" => "/open /absolute/path [/another/path …] | /open --as NAME /absolute/path",
            "bot" => "/bot on [every N]",
            "model" => "/model [PROVIDER:MODEL | default]",
            "rename" => "/rename NAME",
            "memory-on" => "/memory on [MODEL]",
            "services-stop" => "/services stop ID | all",
            _ => entry.text,
        }, description: entry.description,
    }))
}

/// Every entry in menu order, plus their positions sorted by text, built once.
/// A keystroke then binary-searches the typed prefix instead of rebuilding and
/// scanning the whole list, so the cost no longer grows with the command count.
struct Index {
    entries: Vec<Entry>,
    sorted: Vec<u8>,
}
fn index() -> &'static Index {
    static INDEX: std::sync::OnceLock<Index> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let entries = build().collect::<Vec<_>>();
        assert!(entries.len() <= 128, "the menu mask holds at most 128 entries");
        let mut sorted = (0..entries.len() as u8).collect::<Vec<_>>();
        sorted.sort_by_key(|&position| entries[usize::from(position)].text);
        Index { entries, sorted }
    })
}
fn entries() -> impl Iterator<Item = Entry> {
    index().entries.iter().copied()
}
/// Positions in `sorted` of the entries whose text starts with `prefix`.
fn prefixed(index: &Index, prefix: &str) -> std::ops::Range<usize> {
    let text = |position: &u8| index.entries[usize::from(*position)].text;
    let start = index.sorted.partition_point(|position| text(position) < prefix);
    start..start + index.sorted[start..].partition_point(|position| text(position).starts_with(prefix))
}

pub(super) fn matching(input: &str) -> impl Iterator<Item = Entry> + '_ {
    let index = index();
    let mut mask = 0u128;
    if input.starts_with('/') && !input.contains('\n') {
        for &position in &index.sorted[prefixed(index, input)] {
            mask |= 1 << position;
        }
    }
    // Set bits in ascending position keep the menu's own order.
    std::iter::from_fn(move || {
        (mask != 0).then(|| {
            let position = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            index.entries[position]
        })
    })
}

pub(super) fn highlight(input: &str) -> Option<std::ops::Range<usize>> {
    if !input.starts_with('/') { return None; }
    // The longest entry equal to the input up to a word boundary.
    let index = index();
    let exact = input.char_indices().filter(|(_, c)| c.is_whitespace()).map(|(at, _)| at).chain([input.len()])
        .rev().find(|&end| index.sorted[prefixed(index, &input[..end])].iter().any(|&p| index.entries[usize::from(p)].text.len() == end));
    if let Some(end) = exact {
        return Some(0..end);
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
    text.push_str("Keyboard\n\nTab completes the selected command; it does not execute it.\n↑ ↓ choose a command · Esc dismisses suggestions or returns to chat\nEnter sends · Alt+Enter inserts a newline\nPageUp / PageDown scroll · End follows new output\nCtrl+↑ / Ctrl+↓ switch conversations\nCtrl+N starts a chat · Ctrl+Q exits\n\n/improve REQUEST asks agents to develop Control using the server's configured tools and permissions.\n/open /absolute/path requests a project folder; review it before /trust, or /dismiss it.\n/open takes several folders: separate paths with spaces or commas, or put one per line (Alt+Enter); quote a path that has a space before a /. ~ is the node user's home.\n/services lists this DM's team services; /services stop ID or all stops them.\n");
    text
}

#[cfg(test)]
mod tests;
