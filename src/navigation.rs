//! Global navigation stays independent of the selected chat context.
use control_journal::snapshot::Part;
const PRIMARY: &[(&str, &str, &str, &str)] = &[
    ("session", "/dms", "⌁", "DMs"),
    ("agents", "/?view=agents", "⌁", "Bots"),
    ("knowledge", "/knowledge", "▤", "Knowledge"),
    ("rooms", "/?view=rooms", "#", "Rooms"),
    ("tools", "/?view=tools", "⊞", "Tools"),
    ("lenses", "/dna", "◈", "DNA"),
    ("personas", "/personas", "◇", "Personas"),
    ("monitor", "/?view=monitor", "▥", "Usage"),
    ("manage", "/?view=manage", "⚙", "Manage"),
];
const MANAGE: &[(&str, &str)] = &[
    ("nodes", "Nodes"),
    ("authority", "Permissions"),
    ("providers", "Models &amp; providers"),
    ("discord-access", "Discord access"),
    ("integrations", "Integrations"),
];
pub fn managed(view: &str) -> bool {
    MANAGE.iter().any(|(id, _)| *id == view)
}
pub fn primary(view: &str) -> String {
    let current = if managed(view) {
        "manage"
    } else {
        match view {
            "runs" | "history" => "agents",
            "session-list" | "artifact-page" => "session",
            "editor" | "knowledge-page" => "knowledge",
            "lens-page" => "lenses",
            "persona-page" => "personas",
            "browser" | "tool-page" => "tools",
            other => other,
        }
    };
    PRIMARY
        .iter()
        .map(|(id, href, icon, label)| {
            let selected = if *id == current {
                if *id == "manage" {
                    " aria-current='location'"
                } else {
                    " aria-current='page'"
                }
            } else {
                ""
            };
            format!("<a href='{href}'{selected}><span aria-hidden='true'>{icon}</span><span>{label}</span></a>")
        })
        .collect()
}
pub fn secondary(view: &str) -> String {
    if !managed(view) {
        return String::new();
    }
    let links: String = MANAGE
        .iter()
        .map(|(id, label)| {
            let selected = if *id == view { " aria-current='page'" } else { "" };
            format!("<a href='/?view={id}'{selected}>{label}</a>")
        })
        .collect();
    format!("<aside class='section-navigation'><h2>Manage</h2><nav aria-label='Manage'>{links}</nav></aside>")
}
pub fn heading(view: &str) -> String {
    if let Some((_, label)) = MANAGE.iter().find(|(id, _)| *id == view) {
        return format!("Manage<span class='slash' aria-hidden='true'>/</span>{label}");
    }
    String::new()
}
/// Tabs between the live Bots page and its History sub-page.
pub fn bots_tabs(history: bool) -> String {
    let current = |selected| if selected { " aria-current='page'" } else { "" };
    format!(
        "<nav class='tabs' aria-label='Bots'><a href='/?view=agents'{}>Live</a><a href='/bots/history'{}>History</a></nav>",
        current(!history),
        current(history)
    )
}
/// The one toolbar row above the Bots and History pages: the tabs, then the
/// page's search form, then `controls` (the tree's zoom slot, or empty).
pub fn bots_toolbar(history: bool, search: &str, controls: &str) -> String {
    format!(
        "<div class='bots-toolbar'>{}{search}{controls}</div>",
        bots_tabs(history)
    )
}
/// The DM picker: the newest ten threads and the active one's thread. All DMs
/// are on Bots / History (`crate::history_view`). Built from the chat list
/// under the state lock; rendering reads only this.
pub struct Picker {
    /// Each row's id, then its title.
    texts: crate::texts::Texts,
    rows: Vec<Row>,
    /// No DM is open: "+ New DM" is the current page.
    new: bool,
    /// More threads than the picker shows: link to History.
    more: bool,
}
struct Row {
    depth: usize,
    /// The open DM or one of its ancestors; read only for rows with children.
    open: bool,
    current: bool,
}
impl Picker {
    pub fn of(chats: &[Part<crate::chat::Session>], active: &str) -> Self {
        use std::collections::{HashMap, HashSet};
        let entries = crate::conversations::ordered(chats);
        let callsigns = crate::callsigns::all(chats);
        let roots = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.depth == 0)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let range = crate::pagination::window(roots.len(), None, None);
        let start = roots.get(range.start).copied().unwrap_or(entries.len());
        let end = roots.get(range.end).copied().unwrap_or(entries.len());
        let mut selected = entries[start..end].iter().collect::<Vec<_>>();
        let parents: HashMap<_, _> = entries
            .iter()
            .map(|entry| (entry.session.id.as_str(), entry.parent))
            .collect();
        let mut expanded = HashSet::from([active]);
        let mut ancestor = parents.get(active).copied().flatten();
        while let Some(id) = ancestor {
            if !expanded.insert(id) {
                break;
            }
            ancestor = parents.get(id).copied().flatten();
        }
        if let Some(index) = entries.iter().position(|entry| entry.session.id == active) {
            let root = (0..=index).rev().find(|index| entries[*index].depth == 0).unwrap();
            if root < start || root >= end {
                // Keep the active thread reachable without letting it reorder the
                // newest history or exceeding the ten-root picker budget.
                if let Some(last) = selected.iter().rposition(|entry| entry.depth == 0) {
                    selected.truncate(last);
                }
                let root_end = roots.iter().copied().find(|next| *next > root).unwrap_or(entries.len());
                selected.extend(entries[root..root_end].iter());
            }
        }
        let mut texts = crate::texts::Texts::with_capacity(2 * selected.len(), 96 * selected.len());
        let rows = selected
            .iter()
            .enumerate()
            .map(|(position, entry)| {
                let id = entry.session.id.as_str();
                texts.push(id);
                texts.push(&crate::callsigns::label(entry.session, callsigns[entry.position]));
                Row {
                    depth: entry.depth,
                    open: selected.get(position + 1).is_some_and(|next| next.depth > entry.depth)
                        && expanded.contains(id),
                    current: id == active,
                }
            })
            .collect();
        Self {
            texts,
            rows,
            new: active.is_empty(),
            more: roots.len() > crate::pagination::SIZE,
        }
    }
    pub fn render(&self) -> String {
        let current = if self.new { " aria-current='page'" } else { "" };
        let mut links = format!("<a href='/dms?new=1'{current}>+ New DM</a>");
        let mut branches = Vec::new();
        for (position, row) in self.rows.iter().enumerate() {
            let (id, title) = (self.texts.get(2 * position), self.texts.get(2 * position + 1));
            while branches.last().is_some_and(|depth| *depth >= row.depth) {
                links.push_str("</div></details>");
                branches.pop();
            }
            // Keep very deep threads readable without narrowing their row backgrounds.
            let depth = row.depth.min(8);
            let title = crate::escape(title);
            let current = if row.current { " aria-current='page'" } else { "" };
            let link = format!(
                "<a class='conversation-link' href='/dm/{}' title='{title}' data-conversation-depth='{depth}'{current}><span class='conversation-title'>{title}</span></a>",
                id
            );
            if self.rows.get(position + 1).is_some_and(|next| next.depth > row.depth) {
                links.push_str(&format!("<details class='conversation-branch' data-chat-branch='{}'{}><summary data-conversation-depth='{depth}'>{link}</summary><div class='conversation-children'>",id,if row.open {" open"}else{""}));
                branches.push(row.depth);
            } else {
                links.push_str(&link);
            }
        }
        for _ in branches {
            links.push_str("</div></details>");
        }
        let controls = if self.more {
            "<a class='conversation-history-link' href='/bots/history'>All DMs ↗</a>"
        } else {
            ""
        };
        format!(
            "<aside class='section-navigation conversation-navigation'><h2>DMs</h2><nav aria-label='DMs'>{links}</nav>{controls}</aside>"
        )
    }
    /// The picker behind the DMs toggle at the top of a chat page.
    pub fn button(&self) -> String {
        format!(
            "<details class='conversation-toggle'><summary aria-label='DMs' title='DMs'>☰ <span>DMs</span></summary>{}</details>",
            self.render()
        )
    }
}
#[cfg(test)]
mod tests;
