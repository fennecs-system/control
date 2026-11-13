//! Global navigation stays independent of the selected chat context.
const PRIMARY: &[(&str, &str, &str, &str)] = &[
    ("hub", "/", "▦", "Overview"),
    ("session", "/chats", "⌁", "Chat"),
    ("agents", "/?view=agents", "⌁", "Bots"),
    ("history", "/chats/history", "⌁", "History"),
    ("knowledge", "/knowledge", "▤", "Knowledge"),
    ("rooms", "/?view=rooms", "#", "Rooms"),
    ("lenses", "/lenses", "◈", "Lenses"),
    ("tools", "/?view=tools", "⊞", "Tools"),
    ("monitor", "/?view=monitor", "▥", "Usage"),
    ("manage", "/?view=manage", "⚙", "Manage"),
];
const MANAGE: &[(&str, &str)] = &[
    ("nodes", "Nodes"),
    ("authority", "Access requests"),
    ("personas", "Personas"),
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
            "runs" => "history",
            "session-list" | "artifact-page" => "session",
            "editor" | "knowledge-page" => "knowledge",
            "lens-page" => "lenses",
            "browser" | "tool-page" => "tools",
            "hub" => "hub",
            other => other,
        }
    };
    PRIMARY.iter().map(|(id, href, icon, label)| {
        let selected = if *id == current {
            if *id == "manage" {" aria-current='location'"} else {" aria-current='page'"}
        } else {""};
        format!("<a href='{href}'{selected}><span aria-hidden='true'>{icon}</span><span>{label}</span></a>")
    }).collect()
}
pub fn secondary(view: &str) -> String {
    if !managed(view) {
        return String::new();
    }
    let links: String = MANAGE
        .iter()
        .map(|(id, label)| {
            let selected = if *id == view {
                " aria-current='page'"
            } else {
                ""
            };
            format!("<a href='/?view={id}'{selected}>{label}</a>")
        })
        .collect();
    format!("<aside class='section-navigation'><h2>Manage</h2><nav aria-label='Manage'>{links}</nav></aside>")
}
pub fn conversation_button(state: &crate::State, active: &str) -> String {
    format!("<details class='conversation-toggle'><summary aria-label='Conversations' title='Conversations'>☰ <span>Conversations</span></summary>{}</details>",conversations(state,active))
}
pub fn heading(view: &str) -> String {
    if let Some((_, label)) = MANAGE.iter().find(|(id, _)| *id == view) {
        return format!("Manage<span class='slash' aria-hidden='true'>/</span>{label}");
    }
    String::new()
}
pub fn conversations(state: &crate::State, active: &str) -> String {
    conversation_window(state,active,None,None,true)
}
pub fn history(state:&crate::State,older:Option<&str>,newer:Option<&str>)->String {
    format!("{}<section class='conversation-history'>{}</section>",history_tabs(false),conversation_window(state,"",older,newer,false))
}
pub fn history_tabs(runs: bool) -> String {
    let current = |selected| if selected {" aria-current='page'"} else {""};
    format!("<div class='section-heading'><h2>History</h2><a href='/chats?new=1'>+ New chat</a></div><nav class='tabs' aria-label='History'><a href='/chats/history'{}>Conversations</a><a href='/?view=runs'{}>Runs</a></nav>",current(!runs),current(runs))
}
fn conversation_window(state:&crate::State,active:&str,older:Option<&str>,newer:Option<&str>,picker:bool)->String {
    use std::collections::{HashMap,HashSet};
    let entries=crate::conversations::ordered(state);
    let roots=entries.iter().enumerate().filter(|(_,entry)|entry.depth==0).map(|(index,_)|index).collect::<Vec<_>>();
    let find=|id:Option<&str>|id.and_then(|id|roots.iter().position(|index|entries[*index].session.id==id));
    let range=crate::pagination::window(roots.len(),find(older),find(newer));
    let start=roots.get(range.start).copied().unwrap_or(entries.len());
    let end=roots.get(range.end).copied().unwrap_or(entries.len());
    let mut selected=entries[start..end].iter().collect::<Vec<_>>();
    let parents:HashMap<_,_>=entries.iter().map(|entry|(entry.session.id.as_str(),entry.parent)).collect();
    let mut expanded=HashSet::from([active]);let mut ancestor=parents.get(active).copied().flatten();
    while let Some(id)=ancestor {
        if !expanded.insert(id) {break;}
        ancestor=parents.get(id).copied().flatten();
    }
    if picker && let Some(index)=entries.iter().position(|entry|entry.session.id==active) {
        let root=(0..=index).rev().find(|index|entries[*index].depth==0).unwrap();
        if root<start || root>=end {
            // Keep the active thread reachable without letting it reorder the
            // newest history or exceeding the ten-root picker budget.
            if let Some(last)=selected.iter().rposition(|entry|entry.depth==0) { selected.truncate(last); }
            let root_end=roots.iter().copied().find(|next|*next>root).unwrap_or(entries.len());
            selected.extend(entries[root..root_end].iter());
        }
    }
    let current=if active.is_empty() {" aria-current='page'"}else{""};
    let mut links=if picker {format!("<a href='/chats?new=1'{current}>+ New chat</a>")} else {String::new()};
    // One borrowed projection for the page, rather than scanning run history per chat.
    let mut latest=HashMap::new();
    if !picker {
        let selected_ids=selected.iter().map(|entry|entry.session.id.as_str()).collect::<HashSet<_>>();
        for run in state.runtime.runs.iter().rev() {
            if let Some(chat)=run.chat_id.as_deref().filter(|chat|selected_ids.contains(chat)) {
                latest.entry(chat).or_insert(run);
                if latest.len()==selected_ids.len() {break;}
            }
        }
    }
    let mut branches=Vec::new();
    for (position,entry) in selected.iter().enumerate() {
        while branches.last().is_some_and(|depth| *depth>=entry.depth) {links.push_str("</div></details>");branches.pop();}
        let chat=entry.session;
        // Keep very deep threads readable without narrowing their row backgrounds.
        let depth=entry.depth.min(8);
        let title=crate::escape(&chat.title);
        let current=if chat.id==active {" aria-current='page'"}else{""};
        let metadata=latest.get(chat.id.as_str()).map(|run| {
            let model=run.model.as_deref().map(|model|format!(" · {}",crate::escape(model))).unwrap_or_default();
            format!("<span class='conversation-meta'>{} · {}{model}</span>",crate::escape(&run.status),crate::escape(run.backend.label()))
        }).unwrap_or_default();
        let link=format!("<a class='conversation-link' href='/chat/{}' title='{title}' data-conversation-depth='{depth}'{current}><span class='conversation-title'>{title}</span>{metadata}</a>",chat.id);
        if selected.get(position+1).is_some_and(|next|next.depth>entry.depth) {
            links.push_str(&format!("<details class='conversation-branch' data-chat-branch='{}'{}><summary data-conversation-depth='{depth}'>{link}</summary><div class='conversation-children'>",chat.id,if expanded.contains(chat.id.as_str()) {" open"}else{""}));
            branches.push(entry.depth);
        }else{links.push_str(&link);}
    }
    for _ in branches {links.push_str("</div></details>");}
    if !picker && selected.is_empty() {links.push_str("<p class='muted'>No conversations yet.</p>");}
    let controls=if picker {
        if roots.len()>crate::pagination::SIZE {"<a class='conversation-history-link' href='/chats/history'>All conversations ↗</a>".into()}else{String::new()}
    }else{
        crate::pagination::controls("/chats/history",(range.start>0).then(||entries[roots[range.start]].session.id.as_str()),(range.end<roots.len()).then(||entries[roots[range.end-1]].session.id.as_str()))
    };
    let heading=if picker {"<h2>Conversations</h2>"}else{""};
    format!("<aside class='section-navigation conversation-navigation'>{heading}<nav aria-label='Conversations'>{links}</nav>{controls}</aside>")
}
#[cfg(test)]
mod tests;
