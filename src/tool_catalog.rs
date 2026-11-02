//! One catalogue; lens membership is a filter, never a second tool listing.
use crate::{State, escape};
pub fn render(state: &State, query: &str) -> String {
    let mut roots = Vec::new();
    let mut words = Vec::new();
    let mut error = None;
    for token in query.split_whitespace() {
        if let Some(lens) = token
            .strip_prefix("lens:")
            .or_else(|| token.strip_prefix("lense:"))
        {
            roots.extend(lens.split(',').map(str::to_owned));
        } else {
            words.push(token.to_lowercase());
        }
    }
    let selected = if roots.is_empty() {
        None
    } else {
        match crate::lens_library::resolve(state, &roots) {
            Ok(resolved) => Some(
                resolved
                    .tools
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>(),
            ),
            Err(_) => {
                error = Some("Unknown or unresolved lens.".to_owned());
                None
            }
        }
    };
    let mut html = format!(
        "<h2>Tools</h2><form class='tool-search' method='get' action='/' role='search'><input type='hidden' name='view' value='tools'><input name='q' value='{}' placeholder='Search tools · lens:engineering' aria-label='Search tools' maxlength='500'><button class='primary'>Search</button></form>",
        escape(query)
    );
    if let Some(error) = error {
        html.push_str(&format!("<p role='status'>{}</p>", escape(&error)));
        return html;
    }
    html.push_str("<div class='tool-cards'>");
    let mut count = 0;
    for entry in control_context::TOOLS.iter().filter(|tool| tool.available) {
        let haystack = format!(
            "{} {} {} {} {}",
            entry.name,
            entry.title,
            entry.description,
            entry.aliases,
            entry.mcp_names.join(" ")
        )
        .to_lowercase();
        if !words.iter().all(|word| haystack.contains(word))
            || selected
                .as_ref()
                .is_some_and(|ids| !ids.contains(entry.name))
        {
            continue;
        }
        count += 1;
        html.push_str("<article class='record'><h3>");
        if let Some(href) = entry.href {
            html.push_str(&format!(
                "<a href='{}'>{} ↗</a>",
                escape(href),
                escape(entry.title)
            ));
        } else {
            html.push_str(&escape(entry.title));
        }
        html.push_str(&format!(
            "</h3><p class='muted'>{}</p></article>",
            escape(entry.description)
        ));
    }
    html.push_str("</div>");
    if count == 0 {
        html.push_str("<p class='muted' role='status'>No tools found.</p>");
    }
    html
}
#[cfg(test)]
mod tests {
    use super::*;
    fn heading(entry: &control_context::Tool) -> String {
        match entry.href {
            Some(href) => format!(
                "<h3><a href='{}'>{} ↗</a></h3>",
                escape(href),
                escape(entry.title)
            ),
            None => format!("<h3>{}</h3>", escape(entry.title)),
        }
    }
    #[test]
    fn every_operator_mcp_tool_is_discoverable_in_the_catalogue() {
        let mut state = State::default();
        let listed = crate::session_http::rpc(
            &mut state,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        let tools = listed["result"]["tools"].as_array().unwrap();
        for definition in tools
            .iter()
            .chain(crate::mcp_stdio::local_definitions().iter())
        {
            let name = definition["name"].as_str().unwrap();
            let entry = control_context::tool_for_mcp(name)
                .unwrap_or_else(|| panic!("Missing catalogue registration: {name}"));
            let html = render(&state, name);
            assert!(
                html.contains(&heading(entry)),
                "MCP name not searchable: {name}"
            );
        }
        let html = render(&state, "");
        for entry in control_context::TOOLS.iter().filter(|tool| tool.available) {
            assert_eq!(html.matches(&heading(entry)).count(), 1);
        }
        assert!(!html.contains("Repository patch") && !html.contains("Repository read"));
    }
    #[test]
    fn catalogue_search_resolves_live_composed_lenses_without_duplicate_lists() {
        let mut state = State::default();
        state
            .lens_library
            .definitions
            .push(control_context::Definition {
                id: "test-tools".into(),
                name: "Test".into(),
                revision: 1,
                includes: vec!["source-search".into()],
                tools: vec!["rust.run".into(), "design.ask".into()],
                ..Default::default()
            });
        let all = render(&state, "");
        assert_eq!(all.matches("href='/tools/browser'").count(), 1);
        assert!(!all.contains("<iframe"));
        assert!(!all.contains("Lens tools"));
        assert_eq!(all.matches("href='/chats'").count(), 1);
        assert!(all.contains("<h3><a href='/tools/services'>Persistent service ↗</a></h3>"));
        assert!(all.contains("<h3>Design decision</h3>"));
        assert!(render(&state, "control_design_ask").contains("<h3>Design decision</h3>"));
        assert!(
            render(&state, "lens:test-tools control_design_ask")
                .contains("<h3>Design decision</h3>")
        );
        let result = render(&state, "lens:test-tools rust");
        assert!(result.contains("href='/tools/rust'"));
        assert!(!result.contains("href='/tools/search'"));
        assert!(render(&state, "lens:research").contains("href='/tools/search'"));
        assert!(!render(&state, "lens:research").contains("href='/tools/rust'"));
        assert!(render(&state, "lense:unknown").contains("Unknown or unresolved lens"));
        let composed = render(&state, "lens:engineering lens:research");
        assert!(composed.contains("href='/tools/search'"));
        assert!(composed.contains("href='/knowledge'"));
        assert!(render(&state, "<script>").contains("&lt;script&gt;"));
        if let Ok(path) = std::env::var("CONTROL_CATALOG_PREVIEW") {
            let mut page = crate::render(&state, "tools", "none", "");
            for (name, css) in [
                ("style.css", include_str!("../assets/style.css")),
                ("workspace.css", include_str!("../assets/workspace.css")),
                ("navigation.css", include_str!("../assets/navigation.css")),
                ("runtime.css", include_str!("../assets/runtime.css")),
                ("collective.css", include_str!("../assets/collective.css")),
            ] {
                page = page
                    .replace(
                        &format!("<link rel='stylesheet' href='/{name}'>"),
                        &format!("<style>{css}</style>"),
                    )
                    .replace(
                        &format!("<link rel=\"stylesheet\" href=\"/{name}\">"),
                        &format!("<style>{css}</style>"),
                    );
            }
            std::fs::write(path, page).unwrap();
        }
    }
}
