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

/// Renamed pages under a DM: `/dm/ID/<old>` (or `/chat/ID/<old>`) moves to
/// `/dm/ID/<new>`. Only the exact page is renamed; deeper routes keep theirs.
const DM_PAGES: [(&str, &str); 1] = [("compaction", "memory")];

/// The DM page for `id`.
pub fn dm(id: &str) -> String {
    format!("/dm/{id}")
}

/// The current path for a legacy `path` (no query), or `None` when `path`
/// does not use an old name. `/chats/history` is a history page, not the DM
/// list: [`history`] names its replacement.
pub fn renamed(path: &str) -> Option<String> {
    if path == "/chats/history" {
        return None;
    }
    let segment = RENAMES.iter().find_map(|(old, new, bare)| {
        let rest = path.strip_prefix(old)?;
        if rest.is_empty() {
            Some((*bare).to_owned())
        } else {
            rest.starts_with('/').then(|| format!("{new}{rest}"))
        }
    });
    dm_page(segment.as_deref().unwrap_or(path)).or(segment)
}
fn dm_page(path: &str) -> Option<String> {
    let (id, page) = path.strip_prefix("/dm/")?.split_once('/')?;
    DM_PAGES.iter().find(|(old, _)| *old == page).map(|(_, new)| format!("/dm/{id}/{new}"))
}

/// The one History page (Bots / History): DMs with their turns.
pub const HISTORY: &str = "/bots/history";

/// Earlier history pages, the filter that shows the same records on
/// [`HISTORY`], and the query keys that still mean the same there. The DM
/// history (`/chats/history`, later Bots / History / Conversations) paged by
/// DM, so its cursors carry over. The run history (`/bots/history/runs`, and
/// `/?view=runs` before it) listed every turn: its run cursors do not name a
/// DM, so it opens the newest DMs with their latest turns listed.
const HISTORIES: [(&str, &str, &[&str]); 2] = [
    ("/chats/history", "", &["older", "newer"]),
    ("/bots/history/runs", "expand=1", &[]),
];

/// The [`HISTORY`] URL equivalent to the legacy history `path` with raw
/// `query`, or `None` when `path` is not one.
pub fn history(path: &str, query: &str) -> Option<String> {
    let (_, filter, kept) = HISTORIES.iter().find(|(old, _, _)| *old == path)?;
    let pairs = query.split('&').filter(|pair| {
        pair.split_once('=').is_some_and(|(key, value)| {
            kept.contains(&key) && !value.is_empty() && value.len() <= 100 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    });
    let query = [*filter].into_iter().filter(|f| !f.is_empty()).chain(pairs).collect::<Vec<_>>().join("&");
    Some(if query.is_empty() { HISTORY.to_owned() } else { format!("{HISTORY}?{query}") })
}

/// `(status, location)` for a request to a legacy path: `301 Moved
/// Permanently` for GET and HEAD, `308 Permanent Redirect` otherwise so the
/// client repeats the same method and body at the new path.
pub fn redirect(method: &str, path: &str, query: &str) -> Option<(&'static str, String)> {
    let location = match history(path, query) {
        Some(location) => location,
        None => {
            let location = renamed(path)?;
            if query.is_empty() { location } else { format!("{location}?{query}") }
        }
    };
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
