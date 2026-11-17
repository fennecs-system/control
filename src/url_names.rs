//! User-facing URL names. DMs live at `/dm/ID` (listed at `/dms`) and the
//! gene library at `/dna`, matching the names the interface shows. Earlier
//! releases used `/chat/ID`, `/chats` and `/lenses`; links already sent
//! (bookmarks, Discord cards, stored replies) keep working because the server
//! answers each old path with a permanent redirect to its new name that keeps
//! the path tail and query: 301 for reads, 308 for anything else so a form's
//! method and body survive. Internal identifiers, state fields and tool names
//! (`control.chat.*`, `control.lenses.*`) keep their names. See
//! `url_names/tests.rs` for the full mapping.

/// Old first path segment and its current name. `/chat` alone was the POST
/// alias for creating a DM, so it maps to the list that now accepts that POST.
const RENAMES: [(&str, &str, &str); 3] = [("/chats", "/dms", "/dms"), ("/chat", "/dm", "/dms"), ("/lenses", "/dna", "/dna")];

/// The DM page for `id`.
pub fn dm(id: &str) -> String {
    format!("/dm/{id}")
}

/// The current path for a legacy `path` (no query), or `None` when `path`
/// does not use an old name. `/chats/history` keeps its own redirect to the
/// Bots history page.
pub fn renamed(path: &str) -> Option<String> {
    if path == "/chats/history" {
        return None;
    }
    RENAMES.iter().find_map(|(old, new, bare)| {
        let rest = path.strip_prefix(old)?;
        if rest.is_empty() {
            Some((*bare).to_owned())
        } else {
            rest.starts_with('/').then(|| format!("{new}{rest}"))
        }
    })
}

/// `(status, location)` for a request to a legacy path: `301 Moved
/// Permanently` for GET and HEAD, `308 Permanent Redirect` otherwise so the
/// client repeats the same method and body at the new path.
pub fn redirect(method: &str, path: &str, query: &str) -> Option<(&'static str, String)> {
    let location = renamed(path)?;
    let location = if query.is_empty() { location } else { format!("{location}?{query}") };
    let status = if matches!(method, "GET" | "HEAD") { "301 Moved Permanently" } else { "308 Permanent Redirect" };
    Some((status, location))
}

/// The `ID[/...]` route after a DM path in `text` (`/dm/ID...`, or the legacy
/// `/chat/ID...` that replies stored before the rename still contain).
pub fn dm_route(text: &str) -> Option<&str> {
    text.strip_prefix("/dm/").or_else(|| text.strip_prefix("/chat/"))
}

#[cfg(test)]
mod tests;
