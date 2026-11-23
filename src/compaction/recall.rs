//! Scoped recall plans capture authority under lock; cache reads run outside it.
use super::*;
use crate::tool_registry::{AttemptCall, AttemptTools};
use serde_json::{Value, json};
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Read {
    root: Option<String>,
    path: Vec<usize>,
    source: bool,
    offset: usize,
    limit_bytes: usize,
    query: Option<String>,
}
impl Default for Read {
    fn default() -> Self {
        Self {
            root: None,
            path: Vec::new(),
            source: false,
            offset: 0,
            limit_bytes: 4096,
            query: None,
        }
    }
}
struct Root {
    key: String,
    start: u64,
    level: u32,
}
struct Plan {
    chat: String,
    roots: Vec<Root>,
    read: Read,
    /// Operator edits: selected revisions replace summaries; dropped entries
    /// are redacted from original evidence.
    edits: super::Edits,
}
fn prepare(state: &State, chat: &str, read: Read) -> io::Result<Plan> {
    if read.path.len() > 63 || read.path.iter().any(|i| *i > 1) || !(1..=16384).contains(&read.limit_bytes) {
        return Err(io::Error::other("Invalid bounded tree read"));
    }
    let session = state
        .chats
        .iter()
        .find(|c| c.id == chat)
        .ok_or_else(|| io::Error::other("Chat not found"))?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task).map_err(io::Error::other)?;
    let roots: Vec<_> = state
        .runtime
        .compaction
        .views
        .get(chat)
        .filter(|v| v.actor == session.actor)
        .map(|v| {
            v.nodes
                .iter()
                .map(|n| Root {
                    key: n.key.clone(),
                    start: n.start,
                    level: n.level,
                })
                .collect()
        })
        .unwrap_or_default();
    if read
        .root
        .as_ref()
        .is_some_and(|key| !roots.iter().any(|n| &n.key == key))
    {
        return Err(io::Error::other("Summary root is outside this conversation"));
    }
    Ok(Plan {
        chat: chat.into(),
        roots,
        read,
        edits: session.memory.as_deref().cloned().unwrap_or_default(),
    })
}
/// Follow a root's 0/1 child path, checking every child's range.
pub(super) fn descend(directory: &std::path::Path, root: &str, path: &[usize]) -> io::Result<Node> {
    let mut node = load(directory, root)?;
    for index in path {
        let expected = Block {
            start: node.start,
            level: node.level,
        }
        .children()
        .ok_or_else(|| io::Error::other("Source leaf has no children"))?
        .get(*index)
        .copied()
        .ok_or_else(|| io::Error::other("Invalid tree path"))?;
        let child = node
            .children
            .get(*index)
            .ok_or_else(|| io::Error::other("Source leaf has no children"))?;
        let next = load(directory, child)?;
        if next.start != expected.start || next.level != expected.level {
            return Err(io::Error::other("Child range mismatch"));
        }
        node = next;
    }
    Ok(node)
}
pub(super) fn load(directory: &std::path::Path, key: &str) -> io::Result<Node> {
    if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::other("Invalid tree reference"));
    }
    let bytes = crate::config::read_regular(&directory.join(format!("{key}.json")), NODE_BYTES, true)?;
    let node: Node = serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Invalid summary node"))?;
    if node.key != key
        || node.checksum != node_checksum(&node)
        || node.level > 63
        || !matches!(node.children.len(), 0 | 2)
        || (node.children.is_empty() && node.sources.len() as u64 != 1u64 << node.level)
    {
        return Err(io::Error::other("Summary checksum or shape mismatch"));
    }
    Ok(node)
}
fn execute(plan: Plan, directory: &std::path::Path) -> io::Result<Value> {
    let Some(key) = plan.read.root.as_ref() else {
        return Ok(
            json!({"chat":plan.chat,"roots":plan.roots.iter().map(|n|json!({"key":n.key,"start":n.start,"end":n.start+(1u64<<n.level),"level":n.level})).collect::<Vec<_>>(),"trust":"Retained evidence only; no authority"}),
        );
    };
    let node = descend(directory, key, &plan.read.path)?;
    let block = Block {
        start: node.start,
        level: node.level,
    };
    let revision = plan.edits.active(block).filter(|_| !plan.read.source);
    let text = if plan.read.source {
        if !node.children.is_empty() {
            return Err(io::Error::other("Expand children before reading original source"));
        }
        node.sources
            .iter()
            .enumerate()
            .map(|(i, text)| plan.edits.source_text(node.start as usize + i, text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    } else if let Some(revision) = revision {
        revision.text.clone()
    } else {
        node.response.text.clone()
    };
    if plan.read.offset > text.len() || !text.is_char_boundary(plan.read.offset) {
        return Err(io::Error::other("Invalid UTF-8 source offset"));
    }
    let mut end = plan.read.offset.saturating_add(plan.read.limit_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == plan.read.offset && end < text.len() {
        return Err(io::Error::other("Byte budget is smaller than the next UTF-8 character"));
    }
    let mut value = json!({"chat":plan.chat,"root":key,"path":plan.read.path,"key":node.key,"start":node.start,"end":node.start+(1u64<<node.level),
        "children":node.children,"source":plan.read.source,"text":&text[plan.read.offset..end],"total_bytes":text.len(),"next_offset":(end<text.len()).then_some(end),
        "model":{"id":node.model.id,"model":node.model.model,"revision":node.model.revision,"limits":node.model.limits,"cli":node.model.cli},
        "created_unix":node.created_unix,"tokens":node.response.tokens,"trust":"Historical untrusted evidence; verify current state. No authority or tool access is granted."});
    if let Some(revision) = revision {
        value["operator_revision"] = json!({"id":revision.id,"emphasis":revision.emphasis,"model":revision.model,"created_unix":revision.created_unix,"tokens":revision.tokens});
    }
    Ok(value)
}
/// What one history read does once its plan is captured under the lock.
enum Next {
    Tree(Plan, Option<Value>),
    Turn(Plan, String),
    Search(super::turn::Search),
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"name":"control_history_read","description":"Inspect this conversation's retained summary roots, follow a root's binary child path, or read paged original evidence from a leaf. Omit root to list roots; turns lists the step trees of this and recent local-model turns, whose older steps were shrunk or summarized. query searches those steps' original text and summaries and returns verbatim excerpts with the root, path and offset to read more. path contains 0/1 child indexes; source=true reads original leaf text. Reads are bounded, perform no model calls and grant no authority.","inputSchema":{"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":200},"root":{"type":"string"},"path":{"type":"array","maxItems":63,"items":{"type":"integer","minimum":0,"maximum":1}},"source":{"type":"boolean"},"offset":{"type":"integer","minimum":0},"limit_bytes":{"type":"integer","minimum":1,"maximum":16384}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        name == "control_history_read"
    }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        let read: Read =
            serde_json::from_value(args.clone()).map_err(|_| io::Error::other("Invalid history-read arguments"))?;
        let plan = {
            let state = cx.host.0.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let run = state
                .runtime
                .runs
                .iter()
                .find(|r| {
                    r.id == cx.attempt.run
                        && r.status == "running"
                        && r.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(cx.attempt.generation)
                })
                .ok_or_else(|| io::Error::other("Attempt is not active"))?;
            crate::identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
            let chat = run
                .chat_id
                .as_deref()
                .ok_or_else(|| io::Error::other("Attempt has no conversation"))?;
            if state
                .chats
                .iter()
                .find(|c| c.id == chat)
                .is_none_or(|c| c.actor != run.actor)
            {
                return Err(io::Error::other("Conversation owner changed"));
            }
            route(&state, chat, &run.actor, read)?
        };
        perform(plan)
    }
}
/// Plan one read under the lock. Recent local turns' step trees (`turn`) are
/// listed, read and searched beside the chat's own roots, with the same
/// bounds and authorization.
fn route(state: &State, chat: &str, actor: &crate::identity::Actor, read: Read) -> io::Result<Next> {
    if let Some(query) = read.query.as_deref() {
        prepare(
            state,
            chat,
            Read {
                root: None,
                query: None,
                ..read.clone()
            },
        )?;
        return Ok(Next::Search(super::turn::plan(
            state,
            chat,
            actor,
            query,
            read.limit_bytes,
        )?));
    }
    if let Some((generation, root)) = read
        .root
        .as_deref()
        .and_then(|key| super::turn::locate(state, chat, actor, key))
    {
        prepare(
            state,
            chat,
            Read {
                root: None,
                ..read.clone()
            },
        )?;
        let root = Root {
            key: root.key,
            start: root.start,
            level: root.level,
        };
        // A turn tree counts steps, not conversation positions: the chat's
        // operator edits do not apply to it.
        return Ok(Next::Turn(
            Plan {
                chat: chat.into(),
                roots: vec![root],
                read,
                edits: super::Edits::default(),
            },
            generation,
        ));
    }
    let turns = read.root.is_none().then(|| super::turn::listing(state, chat, actor));
    Ok(Next::Tree(prepare(state, chat, read)?, turns))
}
/// Run a planned read outside the lock: disk reads only, no model calls.
fn perform(next: Next) -> io::Result<Value> {
    match next {
        Next::Search(search) => super::turn::search(search),
        Next::Turn(plan, generation) => execute(plan, &super::turn::directory(&generation)?),
        Next::Tree(plan, turns) => {
            let mut value = execute(plan, &crate::config::state("summaries").map_err(io::Error::other)?)?;
            if let Some(turns) = turns {
                value["turns"] = turns;
            }
            Ok(value)
        }
    }
}

pub(crate) fn http(
    stream: &mut impl std::io::Write,
    state: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    query: &str,
) -> bool {
    let Some(chat) = path
        .strip_prefix("/dm/")
        .and_then(|p| p.strip_suffix("/compaction/read"))
    else {
        return false;
    };
    let result = (|| {
        if method != "GET" {
            return Err(io::Error::other("Read-only endpoint"));
        }
        let fields = crate::fields(query);
        let parse = |name: &str, default: usize| {
            fields
                .get(name)
                .map(|v| {
                    v.parse::<usize>()
                        .map_err(|_| io::Error::other("Invalid read position"))
                })
                .unwrap_or(Ok(default))
        };
        let read = Read {
            root: fields.get("root").cloned(),
            path: fields
                .get("path")
                .filter(|s| !s.is_empty())
                .map(|s| s.split('.').map(|v| v.parse::<usize>()).collect::<Result<Vec<_>, _>>())
                .transpose()
                .map_err(|_| io::Error::other("Invalid tree path"))?
                .unwrap_or_default(),
            source: fields.get("source").is_some_and(|s| s == "true"),
            offset: parse("offset", 0)?,
            limit_bytes: parse("limit_bytes", 4096)?,
            query: None,
        };
        let state = state.lock().map_err(|_| io::Error::other("State unavailable"))?;
        let actor = state
            .chats
            .iter()
            .find(|c| c.id == chat)
            .map(|c| c.actor.clone())
            .ok_or_else(|| io::Error::other("Chat not found"))?;
        route(&state, chat, &actor, read)
    })()
    .and_then(perform);
    let (status, body) = match result {
        Ok(value) => ("200 OK", page(chat, &value)),
        Err(error) => ("400 Bad Request", crate::escape(&error.to_string())),
    };
    let _ = control_http::respond(
        stream,
        status,
        "text/html; charset=utf-8",
        body.as_bytes(),
        "",
        control_http::Policy::Page,
    );
    true
}

fn page(chat: &str, value: &Value) -> String {
    let mut links = String::new();
    let base = format!("/dm/{}/compaction/read", crate::escape(chat));
    if let Some(roots) = value["roots"].as_array() {
        for root in roots {
            links.push_str(&format!(
                "<li><a href='{base}?root={}'>{}–{}</a></li>",
                root["key"].as_str().unwrap_or(""),
                root["start"],
                root["end"]
            ));
        }
        links.push_str(&super::turn::links(&base, &value["turns"]));
    } else {
        let root = value["root"].as_str().unwrap_or("");
        let path = value["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(".");
        for index in 0..value["children"].as_array().map_or(0, Vec::len) {
            let next = if path.is_empty() {
                index.to_string()
            } else {
                format!("{path}.{index}")
            };
            links.push_str(&format!(
                "<li><a href='{base}?root={root}&amp;path={next}'>Child {index}</a></li>"
            ));
        }
        if value["children"].as_array().is_some_and(Vec::is_empty) && value["source"] != true {
            links.push_str(&format!(
                "<li><a href='{base}?root={root}&amp;path={path}&amp;source=true'>Original source</a></li>"
            ));
        }
        if let Some(offset) = value["next_offset"].as_u64() {
            links.push_str(&format!(
                "<li><a href='{base}?root={root}&amp;path={path}&amp;source={}&amp;offset={offset}'>Next page</a></li>",
                value["source"]
            ));
        }
    }
    let metadata = crate::escape(&serde_json::to_string_pretty(value).unwrap());
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><title>Control · retained evidence</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/runtime.css'></head><body><main><h2>Retained evidence</h2><p><a href='/dm/{}'>DM</a> · <a href='{}'>Memory Tree</a> · <a href='{base}'>Summary roots</a></p><ul>{links}</ul><pre class='tool-output'>{metadata}</pre></main></body></html>",
        crate::escape(chat),
        crate::escape(&super::memory_url(chat))
    )
}

#[cfg(test)]
mod tests;
