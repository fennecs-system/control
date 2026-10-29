use crate::{escape, State};
use control_context::{Definition, DocumentInput, Resolved};
use std::{
    collections::HashMap,
    io::{self, Write},
};

pub fn inputs(state: &State) -> Vec<DocumentInput> {
    state
        .documents
        .iter()
        .map(|d| DocumentInput {
            id: d.id.clone(),
            title: d.title.clone(),
            revision: d.revision,
            body: d.body.clone(),
        })
        .collect()
}
pub fn resolve(state: &State, roots: &[String]) -> Result<Resolved, String> {
    state.lens_library.resolve(roots, &inputs(state))
}
pub fn tool(
    state: &State,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, &'static str> {
    match name {
        "control.lenses.list" => Ok(serde_json::json!(state.lens_library.current())),
        "control.lens.read" => state
            .lens_library
            .get(args["id"].as_str().ok_or("Missing lens ID.")?)
            .map(|d| serde_json::json!(d))
            .ok_or("Lens not found."),
        "control.lenses.resolve" => {
            let roots = args["lenses"]
                .as_array()
                .filter(|a| a.len() <= 128)
                .ok_or("Expected a list of lenses.")?
                .iter()
                .map(|v| v.as_str().map(str::to_owned).ok_or("Invalid lens ID."))
                .collect::<Result<Vec<_>, _>>()?;
            resolve(state, &roots)
                .map(|r| serde_json::json!(r))
                .map_err(|_| "Context cannot be resolved. Inspect the lens library.")
        }
        _ => Err("Unknown lens tool."),
    }
}
pub fn draft(fields: &HashMap<String, String>) -> Definition {
    let value = |key: &str| fields.get(key).cloned().unwrap_or_default();
    Definition {
        id: value("id"),
        name: value("name"),
        revision: value("revision").parse().unwrap_or(0),
        includes: fields
            .keys()
            .filter_map(|k| k.strip_prefix("include_").map(str::to_owned))
            .collect(),
        tools: fields
            .keys()
            .filter_map(|k| k.strip_prefix("tool_").map(str::to_owned))
            .collect(),
        documents: fields
            .keys()
            .filter_map(|k| k.strip_prefix("document_").map(str::to_owned))
            .collect(),
        instructions: value("instructions"),
    }
}
fn save_with(
    state: &mut State,
    mut draft: Definition,
    persist: impl FnOnce(&State) -> io::Result<()>,
) -> Result<Definition, String> {
    if state.lens_library.get(&draft.id).map_or(0, |d| d.revision) != draft.revision {
        return Err("This lens changed in another tab. Your edit is below; open the latest revision before merging.".into());
    }
    draft.revision = draft
        .revision
        .checked_add(1)
        .ok_or("Revision limit reached.")?;
    draft.includes.sort();
    draft.includes.dedup();
    draft.documents.sort();
    draft.documents.dedup();
    draft.tools.sort();
    draft.tools.dedup();
    let mut candidate = state.lens_library.clone();
    candidate.definitions.push(draft.clone());
    candidate.validate(&inputs(state))?;
    let previous = std::mem::replace(&mut state.lens_library, candidate);
    if persist(state).is_err() {
        state.lens_library = previous;
        return Err("Could not save to disk. Your edit is below.".into());
    }
    Ok(draft)
}
pub fn choices(state: &State, selected: &[String], form: &str) -> String {
    let mut html=format!("<input type='hidden' name='composition' value='1' form='{form}'><div class='chat-tags'><details class='lens-selector'><summary>+ Lenses</summary></details><div class='tag-options'>");
    for lens in state.lens_library.current() {
        html.push_str(&format!("<label class='lens-option lens-tag'><input type='checkbox' name='lens_{}' value='{}' form='{form}'{}><span>{}</span></label>",escape(&lens.id),escape(&lens.id),if selected.contains(&lens.id){" checked"}else{""},escape(&lens.name)));
    }
    html.push_str("</div><a class='lens-library-shortcut' href='/lenses' aria-label='Open lens library'>↗</a></div>");
    html
}
fn checkbox(prefix: &str, id: &str, name: &str, checked: bool) -> String {
    format!("<label class='library-choice'><input type='checkbox' name='{prefix}_{}'{}><span>{}</span></label>",escape(id),if checked{" checked"}else{""},escape(name))
}
pub fn render(
    state: &State,
    id: &str,
    submitted: Option<&Definition>,
    notice: Option<&str>,
    saved: bool,
) -> String {
    let current = state.lens_library.current();
    let blank = Definition::default();
    let definition = submitted
        .or_else(|| state.lens_library.get(id))
        .unwrap_or(&blank);
    let mut html=String::from("<div class='section-heading'><h2>Lenses</h2><a class='primary' href='/lenses?new=1'>＋ New lens</a></div><p class='muted'>Build context from knowledge, tools, instructions, and other lenses.</p><div class='knowledge-workspace lens-workspace'><aside class='note-files' aria-label='Lens library'>");
    for lens in current {
        html.push_str(&format!(
            "<a href='/lenses?id={}'{}>{} <small>v{}</small></a>",
            escape(&lens.id),
            if lens.id == definition.id {
                " aria-current='page'"
            } else {
                ""
            },
            escape(&lens.name),
            lens.revision
        ));
    }
    html.push_str("<a class='lens-export' href='/lenses/export'>Export definitions ↓</a></aside><section class='knowledge-content'>");
    if let Some(message) = notice {
        html.push_str(&format!(
            "<p class='{}' role='{}'>{}</p>",
            if saved {
                "save-confirmation"
            } else {
                "save-error"
            },
            if saved { "status" } else { "alert" },
            escape(message)
        ));
    }
    html.push_str(&format!("<form class='lens-definition' method='post' action='/lenses/save'><input type='hidden' name='revision' value='{}'><div class='lens-name-row'><label>Name<input name='name' required maxlength='80' value='{}' placeholder='Project context'></label><label>Identifier<input name='id' required maxlength='64' pattern='[a-z0-9-]+' value='{}' placeholder='project-context'{}></label></div><fieldset><legend>Compose lenses</legend><div class='library-choices'>",definition.revision,escape(&definition.name),escape(&definition.id),if state.lens_library.get(&definition.id).is_some(){" readonly"}else{""}));
    for other in state.lens_library.current() {
        if other.id != definition.id {
            html.push_str(&checkbox(
                "include",
                &other.id,
                &other.name,
                definition.includes.contains(&other.id),
            ));
        }
    }
    html.push_str("</div></fieldset><fieldset><legend>Tools</legend><div class='library-choices'>");
    for tool in control_context::TOOLS {
        html.push_str(&checkbox(
            "tool",
            tool.name,
            &format!(
                "{}{}",
                tool.name,
                if tool.available {
                    ""
                } else {
                    " · not connected"
                }
            ),
            definition.tools.iter().any(|s| s == tool.name),
        ));
    }
    html.push_str("</div></fieldset><fieldset><legend>Knowledge documents</legend><div class='library-choices'>");
    for doc in &state.documents {
        html.push_str(&checkbox(
            "document",
            &doc.id,
            &doc.title,
            definition.documents.contains(&doc.id),
        ));
    }
    if state.documents.is_empty() {
        html.push_str("<p class='muted'>No documents yet. Add them in Knowledge.</p>");
    }
    html.push_str(&format!("</div></fieldset><label>Instructions<textarea name='instructions' rows='4' maxlength='4000' placeholder='Guidance to include in this context…'>{}</textarea></label><button class='primary'>Save lens ↗</button></form>",escape(&definition.instructions)));
    if !definition.id.is_empty() && submitted.is_none() {
        html.push_str("<details class='resolved-context' open><summary>Resolved context</summary>");
        match resolve(state, std::slice::from_ref(&definition.id)) {
            Ok(resolved) => {
                html.push_str("<div class='resolved-tags'>");
                for lens in &resolved.lenses {
                    html.push_str(&format!(
                        "<a href='/lenses?id={}'>{} <small>v{}</small></a>",
                        escape(&lens.id),
                        escape(&lens.name),
                        lens.revision
                    ));
                }
                html.push_str("</div><h3>Tools</h3><ul class='knowledge'>");
                for tool in &resolved.tools {
                    html.push_str(&format!("<li>{}</li>", escape(tool)));
                }
                html.push_str("</ul><h3>Documents</h3>");
                for doc in &resolved.documents {
                    html.push_str(&format!(
                        "<p><a href='/knowledge?doc={}'>{} · revision {}</a></p>",
                        escape(&doc.id),
                        escape(&doc.title),
                        doc.revision
                    ));
                }
                if resolved.documents.is_empty() {
                    html.push_str("<p class='muted'>No documents selected.</p>");
                }
                html.push_str("<p class='muted'>Selections do not grant capabilities. Documents are context, not automatically accepted facts.</p><details><summary>Resolved data</summary><pre class='manifest'>");
                html.push_str(&escape(&serde_json::to_string_pretty(&resolved).unwrap()));
                html.push_str("</pre></details>");
            }
            Err(error) => html.push_str(&format!("<p class='save-error'>{}</p>", escape(&error))),
        }
        html.push_str("</details>");
    }
    html.push_str("</section></div>");
    html
}
fn response(stream: &mut impl Write, status: &str, body: &str, extra: &str, mime: &str) {
    let _=write!(stream,"HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n\r\n{body}",body.len());
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
    if path == "/lenses/export" && method == "GET" {
        response(
            stream,
            "200 OK",
            &serde_json::to_string_pretty(&state.lens_library).unwrap(),
            "Content-Disposition: attachment; filename=control-lenses.json\r\n",
            "application/json",
        );
        return true;
    }
    if path == "/lenses" && method == "GET" {
        let id = if query.contains_key("new") {
            ""
        } else {
            query.get("id").map(String::as_str).unwrap_or("engineering")
        };
        let saved = query
            .get("saved")
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|v| state.lens_library.get(id).is_some_and(|d| d.revision == v));
        let content = render(
            state,
            id,
            None,
            if saved {
                Some("✓ Lens saved. Future messages will resolve this revision.")
            } else {
                None
            },
            saved,
        );
        response(
            stream,
            "200 OK",
            &crate::render(state, "lens-page", lens, &content),
            "",
            "text/html; charset=utf-8",
        );
        return true;
    }
    if path == "/lenses/save" && method == "POST" {
        let draft = draft(&crate::fields(&String::from_utf8_lossy(body)));
        match save_with(state, draft.clone(), crate::storage::save) {
            Ok(d) => response(
                stream,
                "303 See Other",
                "",
                &format!("Location: /lenses?id={}&saved={}\r\n", d.id, d.revision),
                "text/plain",
            ),
            Err(error) => {
                let html = render(state, &draft.id, Some(&draft), Some(&error), false);
                response(
                    stream,
                    "409 Conflict",
                    &crate::render(state, "lens-page", lens, &html),
                    "",
                    "text/html; charset=utf-8",
                );
            }
        }
        return true;
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_validates_and_preserves_history_and_rolls_back_disk_failure() {
        let mut state = State::default();
        let mut lens = state.lens_library.get("engineering").unwrap().clone();
        lens.name = "Engineering revised".into();
        assert!(save_with(&mut state, lens.clone(), |_| Err(io::Error::other("full"))).is_err());
        assert_eq!(state.lens_library.get("engineering").unwrap().revision, 1);
        save_with(&mut state, lens.clone(), |_| Ok(())).unwrap();
        assert_eq!(state.lens_library.get("engineering").unwrap().revision, 2);
        assert!(state
            .lens_library
            .definitions
            .iter()
            .any(|d| d.id == "engineering" && d.revision == 1));
        assert!(save_with(&mut state, lens, |_| Ok(())).is_err());
        let bad = Definition {
            id: "cycle".into(),
            name: "Cycle".into(),
            includes: vec!["cycle".into()],
            ..Definition::default()
        };
        assert!(save_with(&mut state, bad, |_| Ok(())).is_err());
        assert!(state.lens_library.get("cycle").is_none());
    }
}
