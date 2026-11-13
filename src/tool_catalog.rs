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
mod tests;
