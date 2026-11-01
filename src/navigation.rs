//! Global navigation stays independent of the selected chat context.
const PRIMARY: &[(&str, &str, &str, &str)] = &[
    ("hub", "/", "▦", "Overview"),
    ("session", "/chats", "⌁", "Chat"),
    ("agents", "/?view=agents", "⌁", "Agents"),
    ("runs", "/?view=runs", "⌁", "Runs"),
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
    "Control".into()
}
pub fn conversations(state: &crate::State, active: &str) -> String {
    conversation_window(state,active,None,None,true)
}
pub fn history(state:&crate::State,older:Option<&str>,newer:Option<&str>)->String {
    format!("<section class='conversation-history'>{}</section>",conversation_window(state,"",older,newer,false))
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
    let mut expanded=HashSet::new();let mut ancestor=parents.get(active).copied().flatten();
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
    let mut links=format!("<a href='/chats?new=1'{current}>+ New chat</a>");
    let mut branches=Vec::new();
    for (position,entry) in selected.iter().enumerate() {
        while branches.last().is_some_and(|depth| *depth>=entry.depth) {links.push_str("</div></details>");branches.pop();}
        let chat=entry.session;
        let title=crate::escape(&chat.title);
        let current=if chat.id==active {" aria-current='page'"}else{""};
        let link=format!("<a href='/chat/{}' title='{title}'{current}>{title}</a>",chat.id);
        if selected.get(position+1).is_some_and(|next|next.depth>entry.depth) {
            links.push_str(&format!("<details class='conversation-branch' data-chat-branch='{}'{}><summary>{link}</summary><div class='conversation-children'>",chat.id,if expanded.contains(chat.id.as_str()) {" open"}else{""}));
            branches.push(entry.depth);
        }else{links.push_str(&link);}
    }
    for _ in branches {links.push_str("</div></details>");}
    let controls=if picker {
        if roots.len()>crate::pagination::SIZE {"<a class='conversation-history-link' href='/chats/history'>All conversations ↗</a>".into()}else{String::new()}
    }else{
        crate::pagination::controls("/chats/history",(range.start>0).then(||entries[roots[range.start]].session.id.as_str()),(range.end<roots.len()).then(||entries[roots[range.end-1]].session.id.as_str()))
    };
    format!("<aside class='section-navigation conversation-navigation'><h2>Conversations</h2><nav aria-label='Conversations'>{links}</nav>{controls}</aside>")
}
#[cfg(test)]
mod tests {
    #[test]
    fn conversation_pages_keep_threads_whole_and_cursors_stable() {
        let mut state=crate::State::default();let mut ids=Vec::new();
        for _ in 0..25 {ids.push(crate::chat::create(&mut state));}
        let child=crate::chat::create(&mut state);state.chats.last_mut().unwrap().parent_chat=Some(ids[0].clone());
        let roots=crate::conversations::ordered(&state).iter().filter(|entry|entry.depth==0).map(|entry|entry.session.id.clone()).collect::<Vec<_>>();
        let picker=super::conversations(&state,&child);
        assert_eq!(picker.matches("href='/chat/").count(),11,"Ten parent threads plus their nested child");
        assert!(picker.contains("/chats/history"));assert!(picker.contains(&format!("data-chat-branch='{}' open",ids[0])));
        let page=super::history(&state,Some(&roots[9]),None);
        assert_eq!(page.matches("href='/chat/").count(),10);
        for id in &roots[10..20] {assert!(page.contains(&format!("href='/chat/{id}'")));}
        assert!(!page.contains(&format!("href='/chat/{child}'")));
        crate::chat::create(&mut state);
        assert_eq!(super::history(&state,Some(&roots[9]),None),page,"New chats must not shift an older cursor boundary");
        let last=super::history(&state,Some(&roots[19]),None);
        assert_eq!(last.matches("href='/chat/").count(),6,"The oldest root retains its nested child on the final page");
        assert!(!last.contains("Older →"));assert!(last.contains("← Newer"));
        assert_eq!(super::history(&state,None,Some(&roots[20])),page);
        let mut response=Vec::new();
        assert!(crate::session_http::handle(&mut response,&mut state,"GET",&format!("/chats/history?older={}",roots[9]),"none","",&[]));
        let response=String::from_utf8(response).unwrap();assert!(response.starts_with("HTTP/1.1 200 OK"));assert!(response.contains(&page));
    }
    #[test]
    fn run_pages_render_only_ten_records_and_keep_cursor_boundaries() {
        let mut state=crate::State::default();
        for index in 0..25 {
            state.runtime.enqueue(format!("task-{index}"),"chat","test");state.runtime.runs.last_mut().unwrap().status="completed".into();
        }
        let newest=state.runtime.runs_page(None,None);assert_eq!(newest.matches("<article class='record'>").count(),10);
        assert!(newest.contains("<h3>task-24</h3>"));assert!(!newest.contains("<h3>task-14</h3>"));assert!(!newest.contains("← Newer"));
        let cursor=state.runtime.runs[15].id.clone();let middle=state.runtime.runs_page(Some(&cursor),None);
        assert_eq!(middle.matches("<article class='record'>").count(),10);assert!(middle.contains("<h3>task-14</h3>"));assert!(middle.contains("<h3>task-5</h3>"));
        state.runtime.enqueue("new task".into(),"chat","test");assert_eq!(state.runtime.runs_page(Some(&cursor),None),middle);
        let html=crate::public_render(&state,&crate::fields(&format!("view=runs&older={cursor}")),"none");
        assert!(html.contains(&middle));assert!(html.contains("href='/?view=runs' aria-current='page'"));
        let last_cursor=state.runtime.runs[5].id.clone();let last=state.runtime.runs_page(Some(&last_cursor),None);
        assert_eq!(last.matches("<article class='record'>").count(),5);assert!(!last.contains("Older →"));
    }
    #[test]
    fn manage_routes_preserve_section_navigation_and_global_context() {
        let state = crate::State::default();
        for (view, label) in super::MANAGE {
            let html = crate::render(&state, view, "research", "");
            assert!(html.contains("href='/?view=manage' aria-current='location'"));
            assert!(html.contains(&format!(
                "href='/?view={view}' aria-current='page'>{label}</a>"
            )));
            assert!(html.contains("class=\"body-grid with-section-nav\""));
            assert_eq!(html.matches("aria-label='Manage'").count(), 1);
            assert!(!html.contains("nav-more"));
        }
        let entry = crate::render(&state, "manage", "none", "");
        assert!(entry.contains("href='/?view=nodes' aria-current='page'"));
        for view in ["hub", "knowledge-page", "lens-page", "rooms", "monitor"] {
            let html = crate::render(&state, view, "engineering", "");
            assert!(!html.contains("class='section-navigation'"));
            assert!(!html.contains("nav-more"));
        }
        assert!(super::primary("knowledge-page").contains("href='/knowledge' aria-current='page'"));
        let chat = crate::render(&state, "session-list", "none", "");
        let landing = crate::chat::landing(&state, "none", None);
        assert!(landing.contains("<details class='conversation-toggle'>"));
        assert!(!landing.contains("conversation-toggle' open"));
        assert!(landing.contains("href='/chats?new=1' aria-current='page'"));
        assert!(chat.contains("chat-workspace"));
        assert!(super::primary("session-list").contains("href='/chats' aria-current='page'"));
    }
}
