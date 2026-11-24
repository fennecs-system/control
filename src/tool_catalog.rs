//! One catalogue; lens membership is a filter, never a second tool listing.
use crate::{State, escape};
pub fn render(state: &State, query: &str) -> String {
    Search::of(state, query).render()
}
/// A catalogue search, its lens resolved while state is read so the page
/// renders after the state lock is released.
pub(crate) struct Search {
    query: String,
    words: Vec<String>,
    lens: Lens,
}
/// What `lens:` terms in a search allow.
enum Lens {
    /// No lens named: every tool.
    Any,
    /// The tools the named lenses resolve to.
    Tools(std::collections::BTreeSet<String>),
    /// A named lens is unknown or does not resolve.
    Unknown,
}
impl Search {
    pub(crate) fn of(state: &State, query: &str) -> Self {
        let (roots, words): (Vec<_>, Vec<_>) = query.split_whitespace().partition(|token| lens_terms(token).is_some());
        let roots = roots
            .into_iter()
            .filter_map(lens_terms)
            .flat_map(|lens| lens.split(',').map(str::to_owned))
            .collect::<Vec<_>>();
        let lens = if roots.is_empty() {
            Lens::Any
        } else {
            crate::lens_library::resolve(crate::lens_library::Sources::of(state), &roots)
                .map_or(Lens::Unknown, |resolved| {
                    Lens::Tools(resolved.tools.into_iter().collect())
                })
        };
        Self {
            query: query.to_owned(),
            words: words.into_iter().map(str::to_lowercase).collect(),
            lens,
        }
    }
    pub(crate) fn render(&self) -> String {
        let mut html = format!(
            "<h2>Tools</h2><form class='tool-search' method='get' action='/' role='search'><input type='hidden' name='view' value='tools'><input name='q' value='{}' placeholder='Search tools · lens:engineering' aria-label='Search tools' maxlength='500'><button class='primary'>Search</button></form>",
            escape(&self.query)
        );
        let selected = match &self.lens {
            Lens::Any => None,
            Lens::Tools(tools) => Some(tools),
            Lens::Unknown => {
                html.push_str("<p role='status'>Unknown or unresolved lens.</p>");
                return html;
            }
        };
        html.push_str("<div class='tool-cards'>");
        let mut count = 0;
        for entry in control_context::TOOLS.iter().filter(|tool| tool.available) {
            let haystack = format!(
                "{} {} {} {} {}",
                entry.name,
                entry.title,
                entry.description,
                entry.aliases,
                entry.mcp_names.iter().map(|m| m.name).collect::<Vec<_>>().join(" ")
            )
            .to_lowercase();
            if !self.words.iter().all(|word| haystack.contains(word))
                || selected.is_some_and(|ids| !ids.contains(entry.name))
            {
                continue;
            }
            count += 1;
            html.push_str("<article class='record'><h3>");
            if let Some(href) = entry.href {
                html.push_str(&format!("<a href='{}'>{} ↗</a>", escape(href), escape(entry.title)));
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
}
/// The lens list of a `lens:` (or `lense:`) search term.
fn lens_terms(token: &str) -> Option<&str> {
    token.strip_prefix("lens:").or_else(|| token.strip_prefix("lense:"))
}
#[cfg(test)]
mod tests;
