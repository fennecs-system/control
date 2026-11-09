//! Browser defaults are preferences; each chat captures its own context.
pub fn normalize(value: &str) -> &str {
    if control_context::valid_id(value) { value } else { "none" }
}
pub fn title(value: &str) -> &str {
    match normalize(value) {
        "engineering" => "Engineering",
        "research" => "Research",
        "none" => "No lens",
        other => other,
    }
}
pub fn selected<'a>(library: &control_context::Library, header: &'a str, query: Option<&'a str>) -> &'a str {
    let candidate = query.map(normalize).unwrap_or_else(|| {
        header
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.eq_ignore_ascii_case("cookie"))
            .flat_map(|(_, value)| value.split(';'))
            .filter_map(|part| part.trim().split_once('='))
            .find(|(name, _)| *name == "control_lens")
            .map(|(_, value)| normalize(value))
            .unwrap_or("none")
    });
    if library.get(candidate).is_some() { candidate } else { "none" }
}
pub fn cookie(value: &str) -> String {
    format!(
        "Set-Cookie: control_lens={}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Strict\r\n",
        normalize(value)
    )
}
pub fn return_to(value: &str) -> &str {
    if value.chars().any(char::is_control) {
        return "/";
    }
    if value == "/"
        || value == "/chats"
        || value == "/knowledge"
        || value.starts_with("/knowledge?")
        || value.starts_with("/?")
        || value
            .strip_prefix("/chat/")
            .is_some_and(crate::chat::valid_id)
    {
        value
    } else {
        "/"
    }
}
pub fn overview(state: &crate::State, lens: &str) -> String {
    format!("<h2>Overview</h2><div class='metrics'><div><b>{}</b><span>CHATS</span></div><div><b>{}</b><span>RUNS</span></div><div><b>{}</b><span>LENSES</span></div></div><div class='section-heading compact'><h3>Lens library</h3><a href='/lenses'>Open library ↗</a></div>{}",state.chats.len(),state.runtime.runs.len(),state.lens_library.current().len(),library(state,lens))
}
pub fn library(state: &crate::State, lens: &str) -> String {
    let mut html = String::from("<div class='lens-grid'>");
    for definition in state
        .lens_library
        .current()
        .into_iter()
        .filter(|d| !d.includes.is_empty() || !d.instructions.trim().is_empty())
    {
        let resolved = crate::lens_library::resolve(state, std::slice::from_ref(&definition.id));
        html.push_str(&format!("<a class='lens-card {}' href='/lenses?id={}'><span class='card-symbol'>◈</span><h3>{}</h3><p>{} component lenses · v{}</p><footer>{} <span>↗</span></footer></a>",if definition.id==lens{"selected"}else{""},crate::escape(&definition.id),crate::escape(&definition.name),definition.includes.len(),definition.revision,resolved.map(|r|format!("{} tools · {} documents",r.tools.len(),r.documents.len())).unwrap_or_else(|_|"Needs attention".into())));
    }
    html.push_str("</div>");
    html
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_defaults_and_instruction_only_lenses_reach_the_chat_composer() {
        let mut state = crate::State::default();
        state.lens_library.definitions.push(control_context::Definition {
            id:"field-work".into(),name:"Field <work>".into(),revision:1,instructions:"Local guidance".into(),..Default::default()
        });
        assert_eq!(selected(&state.lens_library,"Cookie: control_lens=field-work",None),"field-work");
        assert_eq!(selected(&state.lens_library,"",Some("autonomy")),"autonomy");
        assert_eq!(selected(&state.lens_library,"",Some("bad\r\nInjected")),"none");
        assert!(cookie("bad\r\nInjected").starts_with("Set-Cookie: control_lens=none;"));
        let cards = library(&state,"field-work");
        assert!(cards.contains("Field &lt;work&gt;") && cards.contains("id=autonomy"));
        let page = crate::chat::landing(&state,"field-work",None);
        assert!(page.contains("value='field-work' form='chat-list-new' checked"));
        assert_eq!(crate::chat::form_lenses(&std::collections::HashMap::new(),"field-work"),serde_json::json!(["field-work"]));
    }
    #[test]
    fn defaults_survive_navigation_and_never_become_redirect_authority() {
        let header = "Cookie: control_operator=unrelated; control_lens=research\r\n";
        let library = control_context::Library::default();
        assert_eq!(selected(&library, header, None), "research");
        assert_eq!(selected(&library, header, Some("engineering")), "engineering");
        assert_eq!(selected(&library, "Cookie: control_lens=bad\r\n", None), "none");
        for target in ["//evil.test", "https://evil.test", "/\r\nSet-Cookie: bad=1"] {
            assert_eq!(return_to(target), "/");
        }
        assert_eq!(return_to("/?view=agents"), "/?view=agents");
    }
    #[test]
    fn navigation_is_global_while_the_new_chat_context_stays_selected() {
        let mut state = crate::State::default();
        state
            .runtime
            .enqueue_for(
                "One task".into(),
                crate::runtime::RunKind::Task,
                "test",
                crate::identity::Actor::LocalOperator,
                crate::identity::Action::Task,
            )
            .unwrap();
        for view in ["agents", "monitor", "rooms", "editor", "hub"] {
            let html = crate::render(&state, view, "research", "general");
            assert!(!html.contains("class=\"right-panel\""));
            assert!(!html.contains("id=\"new-chat\""));
            assert!(html.contains("/chats?discuss="));
            assert!(!html.contains("lens-label"));
            assert!(!html.contains("<h2>Engineering"));
        }
        assert!(overview(&state, "research").contains("<h2>Overview</h2>"));
        assert!(overview(&state, "engineering").contains("<b>1</b><span>RUNS</span>"));
    }
}
