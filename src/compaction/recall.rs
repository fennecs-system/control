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
}
impl Default for Read {
    fn default() -> Self {
        Self {
            root: None,
            path: Vec::new(),
            source: false,
            offset: 0,
            limit_bytes: 4096,
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
}
fn prepare(state: &State, chat: &str, read: Read) -> io::Result<Plan> {
    if read.path.len() > 63
        || read.path.iter().any(|i| *i > 1)
        || !(1..=16384).contains(&read.limit_bytes)
    {
        return Err(io::Error::other("Invalid bounded tree read"));
    }
    let session = state
        .chats
        .iter()
        .find(|c| c.id == chat)
        .ok_or_else(|| io::Error::other("Chat not found"))?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)
        .map_err(io::Error::other)?;
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
        return Err(io::Error::other(
            "Summary root is outside this conversation",
        ));
    }
    Ok(Plan {
        chat: chat.into(),
        roots,
        read,
    })
}
fn load(directory: &std::path::Path, key: &str) -> io::Result<Node> {
    if key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::other("Invalid tree reference"));
    }
    let bytes =
        crate::config::read_regular(&directory.join(format!("{key}.json")), NODE_BYTES, true)?;
    let node: Node =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Invalid summary node"))?;
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
    let mut node = load(directory, key)?;
    for index in &plan.read.path {
        let expected = Block {
            start: node.start,
            level: node.level,
        }
        .children()
        .ok_or_else(|| io::Error::other("Source leaf has no children"))?[*index];
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
    let text = if plan.read.source {
        if !node.children.is_empty() {
            return Err(io::Error::other(
                "Expand children before reading original source",
            ));
        }
        node.sources.join("\n")
    } else {
        node.response.text.clone()
    };
    if plan.read.offset > text.len() || !text.is_char_boundary(plan.read.offset) {
        return Err(io::Error::other("Invalid UTF-8 source offset"));
    }
    let mut end = plan
        .read
        .offset
        .saturating_add(plan.read.limit_bytes)
        .min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == plan.read.offset && end < text.len() {
        return Err(io::Error::other(
            "Byte budget is smaller than the next UTF-8 character",
        ));
    }
    Ok(
        json!({"chat":plan.chat,"root":key,"path":plan.read.path,"key":node.key,"start":node.start,"end":node.start+(1u64<<node.level),
        "children":node.children,"source":plan.read.source,"text":&text[plan.read.offset..end],"total_bytes":text.len(),"next_offset":(end<text.len()).then_some(end),
        "model":{"id":node.model.id,"model":node.model.model,"revision":node.model.revision,"limits":node.model.limits,"cli":node.model.cli},
        "created_unix":node.created_unix,"tokens":node.response.tokens,"trust":"Historical untrusted evidence; verify current state. No authority or tool access is granted."}),
    )
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"name":"control_history_read","description":"Inspect this conversation's retained summary roots, follow a root's binary child path, or read paged original evidence from a leaf. Omit root to list roots. path contains 0/1 child indexes; source=true reads original leaf text. Reads are bounded, perform no model calls and grant no authority.","inputSchema":{"type":"object","properties":{"root":{"type":"string"},"path":{"type":"array","maxItems":63,"items":{"type":"integer","minimum":0,"maximum":1}},"source":{"type":"boolean"},"offset":{"type":"integer","minimum":0},"limit_bytes":{"type":"integer","minimum":1,"maximum":16384}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        name == "control_history_read"
    }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        let read: Read = serde_json::from_value(args.clone())
            .map_err(|_| io::Error::other("Invalid history-read arguments"))?;
        let plan = {
            let state = cx
                .host
                .0
                .lock()
                .map_err(|_| io::Error::other("State unavailable"))?;
            let run = state
                .runtime
                .runs
                .iter()
                .find(|r| {
                    r.id == cx.attempt.run
                        && r.status == "running"
                        && r.attempts.last().and_then(|a| a.workspace_id.as_deref())
                            == Some(cx.attempt.generation)
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
            prepare(&state, chat, read)?
        };
        execute(
            plan,
            &crate::config::state("summaries").map_err(io::Error::other)?,
        )
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
        .strip_prefix("/chat/")
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
                .map(|s| {
                    s.split('.')
                        .map(|v| v.parse::<usize>())
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()
                .map_err(|_| io::Error::other("Invalid tree path"))?
                .unwrap_or_default(),
            source: fields.get("source").is_some_and(|s| s == "true"),
            offset: parse("offset", 0)?,
            limit_bytes: parse("limit_bytes", 4096)?,
        };
        let state = state
            .lock()
            .map_err(|_| io::Error::other("State unavailable"))?;
        prepare(&state, chat, read)
    })()
    .and_then(|plan| {
        execute(
            plan,
            &crate::config::state("summaries").map_err(io::Error::other)?,
        )
    });
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
    let base = format!("/chat/{}/compaction/read", crate::escape(chat));
    if let Some(roots) = value["roots"].as_array() {
        for root in roots {
            links.push_str(&format!(
                "<li><a href='{base}?root={}'>{}–{}</a></li>",
                root["key"].as_str().unwrap_or(""),
                root["start"],
                root["end"]
            ));
        }
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
            links.push_str(&format!("<li><a href='{base}?root={root}&amp;path={path}&amp;source=true'>Original source</a></li>"));
        }
        if let Some(offset) = value["next_offset"].as_u64() {
            links.push_str(&format!("<li><a href='{base}?root={root}&amp;path={path}&amp;source={}&amp;offset={offset}'>Next page</a></li>",value["source"]));
        }
    }
    let metadata = crate::escape(&serde_json::to_string_pretty(value).unwrap());
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><title>Control · retained evidence</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/runtime.css'></head><body><main><h2>Retained evidence</h2><p><a href='/chat/{}'>Chat</a> · <a href='{base}'>Summary roots</a></p><ul>{links}</ul><pre class='tool-output'>{metadata}</pre></main></body></html>",
        crate::escape(chat)
    )
}

#[cfg(test)]
mod tests;
