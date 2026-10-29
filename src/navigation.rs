//! Global navigation stays independent of the selected chat context.
const PRIMARY: &[(&str, &str, &str, &str)] = &[
    ("hub", "/", "▦", "Overview"),
    ("session", "/chats", "⌁", "Chat"),
    ("agents", "/?view=agents", "⌁", "Agents"),
    ("knowledge", "/knowledge", "▤", "Knowledge"),
    ("rooms", "/?view=rooms", "#", "Rooms"),
    ("lenses", "/lenses", "◈", "Lenses"),
    ("tools", "/?view=tools", "⊞", "Tools"),
    ("monitor", "/?view=monitor", "▥", "Usage"),
    ("evolution", "/?view=evolution", "↻", "Improve"),
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
            "session-list" => "session",
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
    let current = if active.is_empty() {
        " aria-current='page'"
    } else {
        ""
    };
    let mut links = format!("<a href='/chats'{current}>+ New chat</a>");
    if let Some(chats) = crate::chat::list(state).as_array() {
        for chat in chats.iter().rev() {
            let id = chat["id"].as_str().unwrap();
            let title = crate::escape(chat["title"].as_str().unwrap_or("Chat"));
            let current = if id == active {
                " aria-current='page'"
            } else {
                ""
            };
            links.push_str(&format!(
                "<a href='/chat/{id}' title='{title}'{current}>{title}</a>"
            ));
        }
    }
    format!("<aside class='section-navigation conversation-navigation'><h2>Conversations</h2><nav aria-label='Conversations'>{links}</nav></aside>")
}
#[cfg(test)]
mod tests {
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
        assert!(landing.contains("href='/chats' aria-current='page'"));
        assert!(chat.contains("chat-workspace"));
        assert!(super::primary("session-list").contains("href='/chats' aria-current='page'"));
    }
}
