//! Local documents and explicit links. Writing does not confer evidence-review status.
use crate::{escape, State};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub body: String,
    pub revision: u64,
    pub saved_at: Option<u64>,
}
pub fn migrate(state: &mut State) -> bool {
    if !state.note.is_empty() && !state.documents.iter().any(|d| d.id == "scratchpad") {
        state.documents.insert(
            0,
            Document {
                id: "scratchpad".into(),
                title: "Scratchpad.md".into(),
                body: state.note.clone(),
                revision: u64::from(state.saved),
                saved_at: None,
            },
        );
        return true;
    }
    false
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn key(title: &str) -> String {
    title
        .trim()
        .to_lowercase()
        .trim_end_matches(".md")
        .to_owned()
}
fn links(body: &str) -> BTreeSet<String> {
    body.split("[[")
        .skip(1)
        .filter_map(|part| part.split_once("]]"))
        .map(|(target, _)| key(target.split(['|', '#']).next().unwrap_or("")))
        .filter(|s| !s.is_empty())
        .collect()
}
fn edges(docs: &[Document]) -> BTreeSet<(usize, usize)> {
    docs.iter()
        .enumerate()
        .flat_map(|(from, doc)| {
            links(&doc.body).into_iter().filter_map(move |target| {
                docs.iter()
                    .position(|d| key(&d.title) == target)
                    .map(|to| (from, to))
            })
        })
        .collect()
}
fn save_with(
    state: &mut State,
    fields: &HashMap<String, String>,
    persist: impl FnOnce(&State) -> io::Result<()>,
) -> Result<String, &'static str> {
    let id = fields.get("id").map(String::as_str).unwrap_or("");
    let title = fields.get("title").map(|s| s.trim()).unwrap_or("");
    let body = fields.get("body").map(String::as_str).unwrap_or("");
    if title.is_empty()
        || title.chars().count()
            + if title.to_lowercase().ends_with(".md") {
                0
            } else {
                3
            }
            > 80
        || title
            .chars()
            .any(|c| c.is_control() || ['/', '\\', '[', ']', '|', '#', '?', '&'].contains(&c))
        || key(title).is_empty()
    {
        return Err("Use a note name of 1–80 characters, without paths or link punctuation.");
    }
    if body.chars().count() > 32000 {
        return Err("Keep a note within 32,000 characters.");
    }
    if state
        .documents
        .iter()
        .any(|d| d.id != id && key(&d.title) == key(title))
    {
        return Err("A note with that name already exists.");
    }
    let count = state.documents.len();
    let previous = if id.is_empty() {
        None
    } else {
        Some(
            state
                .documents
                .iter()
                .position(|d| d.id == id)
                .ok_or("Note not found.")?,
        )
    };
    if let Some(index) = previous
        && fields.get("revision").and_then(|r| r.parse::<u64>().ok())
            != Some(state.documents[index].revision)
        {
            return Err("This note changed in another tab. Your text is below; open the current note before merging your changes.");
        }
    let old = previous.map(|i| (i, state.documents[i].clone()));
    let new_id = if id.is_empty() {
        format!(
            "note-{:x}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            count
        )
    } else {
        id.to_owned()
    };
    let doc = Document {
        id: new_id.clone(),
        title: if title.to_lowercase().ends_with(".md") {
            title.into()
        } else {
            format!("{title}.md")
        },
        body: body.into(),
        revision: old
            .as_ref()
            .map_or(1, |(_, d)| d.revision.saturating_add(1)),
        saved_at: Some(now()),
    };
    if let Some(index) = previous {
        state.documents[index] = doc;
    } else {
        state.documents.push(doc);
    }
    if persist(state).is_err() {
        state.documents.truncate(count);
        if let Some((i, doc)) = old {
            state.documents[i] = doc;
        }
        return Err("Could not save to disk. Your text is below; retry when storage is available.");
    }
    Ok(new_id)
}
fn editor(doc: Option<&Document>, submitted: Option<&HashMap<String, String>>) -> String {
    let field = |name: &str, default: &str| {
        submitted
            .and_then(|f| f.get(name))
            .map_or_else(|| default.to_owned(), Clone::clone)
    };
    let id = field("id", doc.map_or("", |d| d.id.as_str()));
    let title = field("title", doc.map_or("", |d| d.title.as_str()));
    let body = field("body", doc.map_or("", |d| d.body.as_str()));
    let revision = field("revision", &doc.map_or(0, |d| d.revision).to_string());
    let status = doc
        .map(|d| {
            if let Some(stamp) = d.saved_at {
                format!(
                    "Last saved · revision {} · {} {:02}:{:02}:{:02} UTC",
                    d.revision,
                    crate::usage::day_label(stamp),
                    stamp / 3600 % 24,
                    stamp / 60 % 60,
                    stamp % 60
                )
            } else if d.revision > 0 {
                "Previously saved draft · imported intact".into()
            } else {
                "Draft · save to keep your changes".into()
            }
        })
        .unwrap_or_else(|| "New note".into());
    format!("<form class='note-editor' method='post' action='/knowledge/save'><input type='hidden' name='id' value='{}'><input type='hidden' name='revision' value='{}'><div class='note-toolbar'><label class='note-name'>Name<input name='title' required maxlength='80' value='{}' placeholder='Untitled.md'></label><button class='primary'>Save changes ↗</button></div><p class='note-status'>{}</p><label class='note-body-label'>Markdown · connect notes with [[note name]]<textarea class='editor' name='body' maxlength='32000' rows='22' aria-label='Note text'>{}</textarea></label></form>",escape(&id),escape(&revision),escape(&title),escape(&status),escape(&body))
}
fn graph(docs: &[Document]) -> String {
    if docs.is_empty() {
        return "<div class='empty'>Create a note to begin.</div>".into();
    }
    // A deterministic SVG keeps the graph keyboard accessible and works without scripts.
    let height = (docs.len().div_ceil(5) * 120 + 200).max(420);
    let positions: Vec<(f64, f64)> = if docs.len() <= 12 {
        (0..docs.len())
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / docs.len() as f64;
                (
                    400.0 + 250.0 * angle.cos(),
                    height as f64 / 2.0 + 140.0 * angle.sin(),
                )
            })
            .collect()
    } else {
        (0..docs.len())
            .map(|i| {
                (
                    100.0 + (i % 5) as f64 * 150.0,
                    100.0 + (i / 5) as f64 * 120.0,
                )
            })
            .collect()
    };
    let mut html=format!("<p class='muted graph-hint'>Connections come from [[note links]]. Select a node to edit it.</p><svg class='knowledge-graph' viewBox='0 0 800 {height}' role='img' aria-label='Knowledge connections'>");
    for (from, to) in edges(docs) {
        if from != to {
            let (x, y) = positions[from];
            let (xx, yy) = positions[to];
            html.push_str(&format!("<line x1='{x}' y1='{y}' x2='{xx}' y2='{yy}'/>"));
        }
    }
    for (i, doc) in docs.iter().enumerate() {
        let (x, y) = positions[i];
        let label: String = doc.title.chars().take(22).collect();
        html.push_str(&format!("<a href='/knowledge?doc={}' aria-label='Open {}'><title>{}</title><circle cx='{x}' cy='{y}' r='9'/><text x='{x}' y='{}' text-anchor='middle'>{}</text></a>",escape(&doc.id),escape(&doc.title),escape(&doc.title),y+30.0,escape(&label)));
    }
    html.push_str("</svg>");
    html
}
pub fn render(state: &State, selection: &str, saved: Option<u64>) -> String {
    let doc = state
        .documents
        .iter()
        .find(|d| d.id == selection)
        .or_else(|| {
            if ["", "general"].contains(&selection) {
                state.documents.first()
            } else {
                None
            }
        });
    let mut html=String::from("<div class='section-heading knowledge-heading'><h2>Knowledge</h2><a class='primary add-note' href='/knowledge?mode=new' aria-label='Create a note'>＋ <span>New note</span></a></div><div class='tabs knowledge-tabs'><a href='/knowledge'>Documents</a><a href='/knowledge?mode=graph'>Graph</a><a href='/knowledge?mode=review'>Review</a></div><div class='knowledge-workspace'><aside class='note-files' aria-label='Notes'><span class='eyebrow'>DOCUMENTS</span>");
    for note in &state.documents {
        html.push_str(&format!(
            "<a href='/knowledge?doc={}'{}><span>▤</span> {}</a>",
            escape(&note.id),
            if doc.is_some_and(|d| d.id == note.id) {
                " aria-current='page'"
            } else {
                ""
            },
            escape(&note.title)
        ));
    }
    if state.documents.is_empty() {
        html.push_str("<p class='muted'>No notes yet.</p>");
    }
    html.push_str("</aside><section class='knowledge-content'>");
    if selection == "graph" {
        html.push_str(&graph(&state.documents));
    } else if selection == "review" {
        html.push_str(
            &state
                .collective
                .render("knowledge-review", "general")
                .unwrap_or_default(),
        );
    } else if selection == "new" || doc.is_some() || state.documents.is_empty() {
        if let Some(doc) = doc
            && saved == Some(doc.revision) {
                html.push_str(&format!("<div id='saved' class='save-confirmation' role='status'>✓ Saved {} · revision {}</div>",escape(&doc.title),doc.revision));
            }
        html.push_str(&editor(doc, None));
        if let Some(doc) = doc {
            let outgoing = links(&doc.body);
            let incoming: Vec<_> = state
                .documents
                .iter()
                .filter(|d| d.id != doc.id && links(&d.body).contains(&key(&doc.title)))
                .collect();
            if !outgoing.is_empty() || !incoming.is_empty() {
                html.push_str("<div class='note-connections'><h3>Connections</h3>");
                for target in outgoing {
                    if let Some(note) = state.documents.iter().find(|d| key(&d.title) == target) {
                        html.push_str(&format!(
                            "<a href='/knowledge?doc={}'>↗ {}</a>",
                            escape(&note.id),
                            escape(&note.title)
                        ));
                    } else {
                        html.push_str(&format!(
                            "<span class='muted'>Unresolved · {}</span>",
                            escape(&target)
                        ));
                    }
                }
                for note in incoming {
                    html.push_str(&format!(
                        "<a href='/knowledge?doc={}'>↙ Linked from {}</a>",
                        escape(&note.id),
                        escape(&note.title)
                    ));
                }
                html.push_str("</div>");
            }
        }
    } else {
        html.push_str("<div class='empty'>Note not found. Choose one from the list.</div>");
    }
    html.push_str("</section></div>");
    let active = match selection {
        "graph" => "/knowledge?mode=graph",
        "review" => "/knowledge?mode=review",
        _ => "/knowledge",
    };
    html.replace(
        &format!("<a href='{active}'>"),
        &format!("<a href='{active}' aria-current='page'>"),
    )
}
fn respond(stream: &mut impl Write, status: &str, body: &str, extra: &str) {
    let _ = control_http::respond(stream, status, "text/html; charset=utf-8", body.as_bytes(), extra, control_http::Policy::Page);
}
pub fn handle(
    stream: &mut impl Write,
    state: &mut State,
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    lens: &str,
    body: &[u8],
) -> bool {
    if ["/propose", "/review"].contains(&path) && method == "POST" {
        let fields=crate::fields(&String::from_utf8_lossy(body));
        match crate::knowledge_tools::review_action(state,path,&fields,crate::storage::save) {
            Ok(())=>respond(stream,"303 See Other","","Location: /knowledge?mode=review\r\n"),
            Err(message)=>respond(stream,"500 Internal Server Error",&crate::error_page(message,"/knowledge?mode=review"),""),
        }
        return true;
    }
    if path == "/knowledge" && method == "GET" {
        let selection = query
            .get("doc")
            .or_else(|| query.get("mode"))
            .map(String::as_str)
            .unwrap_or("");
        let saved = query.get("saved").and_then(|s| s.parse().ok());
        respond(
            stream,
            "200 OK",
            &crate::render(
                state,
                "knowledge-page",
                lens,
                &render(state, selection, saved),
            ),
            "",
        );
        return true;
    }
    if ["/knowledge/save", "/save"].contains(&path) && method == "POST" {
        let mut fields = crate::fields(&String::from_utf8_lossy(body));
        if path == "/save" {
            migrate(state);
            let Some(doc) = state.documents.iter().find(|d| d.id == "scratchpad") else {
                respond(stream, "400 Bad Request", "No draft to save.", "");
                return true;
            };
            fields.insert("id".into(), doc.id.clone());
            fields.insert("title".into(), doc.title.clone());
            fields.insert(
                "revision".into(),
                if doc.revision <= 1 {
                    doc.revision.to_string()
                } else {
                    "legacy-form".into()
                },
            );
            fields.insert(
                "body".into(),
                fields.get("note").cloned().unwrap_or_default(),
            );
        }
        match save_with(state, &fields, crate::storage::save) {
            Ok(id) => {
                let revision = state
                    .documents
                    .iter()
                    .find(|d| d.id == id)
                    .unwrap()
                    .revision;
                respond(
                    stream,
                    "303 See Other",
                    "",
                    &format!("Location: /knowledge?doc={id}&saved={revision}#saved\r\n"),
                );
            }
            Err(message) => {
                let content=format!("<h2>Save needs attention</h2><p class='save-error' role='alert'>{}</p><a href='/knowledge'>Open current notes ↗</a>{}",escape(message),editor(None,Some(&fields)));
                respond(
                    stream,
                    "409 Conflict",
                    &crate::render(state, "knowledge-page", lens, &content),
                    "",
                );
            }
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests;
