//! One catalogue; lens membership is a filter, never a second tool listing.
use crate::{escape, State};
struct Entry {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    href: &'static str,
    aliases: &'static str,
}
const ENTRIES: &[Entry] = &[
    Entry {
        id: "search.read",
        name: "Search",
        description: "Web · DuckDuckGo / Bing",
        href: "/tools/search",
        aliases: "web search control.web.search",
    },
    Entry {
        id: "rust.run",
        name: "Rust playground",
        description: "Offline · standard library",
        href: "/tools/rust",
        aliases: "code compile execute control.rust.run",
    },
    Entry {
        id: "browser.capture",
        name: "Browser",
        description: "Offline HTML · Firefox",
        href: "/tools/browser",
        aliases: "screenshot preview control.browser.capture",
    },
    Entry {
        id: "context.inspect",
        name: "Context inspector",
        description: "Pure context preview",
        href: "/tool-window/",
        aliases: "wasm lens context",
    },
    Entry {
        id: "knowledge.propose",
        name: "Knowledge",
        description: "Documents and evidence",
        href: "/knowledge",
        aliases: "notes files knowledge.propose",
    },
];
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
    let mut html = format!("<h2>Tools</h2><form class='tool-search' method='get' action='/' role='search'><input type='hidden' name='view' value='tools'><input name='q' value='{}' placeholder='Search tools · lens:engineering' aria-label='Search tools' maxlength='500'><button class='primary'>Search</button></form>", escape(query));
    if let Some(error) = error {
        html.push_str(&format!("<p role='status'>{}</p>", escape(&error)));
        return html;
    }
    html.push_str("<div class='tool-cards'>");
    let mut count = 0;
    for entry in ENTRIES {
        let haystack = format!(
            "{} {} {} {}",
            entry.id, entry.name, entry.description, entry.aliases
        )
        .to_lowercase();
        if !words.iter().all(|word| haystack.contains(word))
            || selected.as_ref().is_some_and(|ids| !ids.contains(entry.id))
        {
            continue;
        }
        count += 1;
        html.push_str(&format!("<article class='record'><h3><a href='{}'>{} ↗</a></h3><p class='muted'>{}</p></article>", entry.href, entry.name, entry.description));
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
                tools: vec!["rust.run".into()],
                ..Default::default()
            });
        let all = render(&state, "");
        assert_eq!(all.matches("href='/tools/browser'").count(), 1);
        assert!(!all.contains("<iframe"));
        assert!(!all.contains("Lens tools"));
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
