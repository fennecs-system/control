mod access;
mod artifacts;
mod backend;
mod agent_message;
mod application;
mod approvals;
mod auto_approval;
mod bots;
#[path = "../tools/service-runner/frame.rs"]
#[allow(dead_code)]
mod watch_frame;
mod watches;
mod observe;
#[path = "../tools/browser/main.rs"]
mod browser;
mod browser_ui;
mod callsigns;
mod channel_settings;
mod chat;
mod child;
mod claude;
mod codex;
mod grok;
mod command_jobs;
mod build_environment;
mod collective;
mod conversation;
mod turn_context;
mod conversations;
mod pagination;
mod design;
mod delegation;
mod delegated_results;
mod history;
mod history_view;
use control_config as config;
use control_identity as identity;
use control_client::url_names;
use control_discord as discord;
use control_discord::bridge as discord_bridge;
mod doctor;
mod engine;
mod federation;
mod github;
mod graph_search;
mod graveyard;
mod knowledge;
mod knowledge_tools;
mod guidance;
mod lens_library;
mod personas;
mod lens_ui;
mod lifecycle;
mod attempt_control;
mod recovery;
mod mcp_chat;
mod mcp_http;
mod mcp_stdio;
mod navigation;
#[cfg(test)]
#[path = "main/tests/performance.rs"]
mod performance;
mod providers;
mod model_calls;
mod local_agent;
mod compaction;
mod packages;
mod permissions_page;
mod projects;
mod ports;
mod previews;
mod process_tree;
mod rooms;
mod runtime;
mod rust_bench;
mod solver;
mod scheduler;
mod scores;
mod intel;
mod services;
mod spawning;
mod session_http;
mod shutdown;
mod live_updates;
mod http_admission;
mod mailbox_wake;
mod storage;
mod tool_catalog;
mod toolbox;
mod web_access;
mod tools_http;
mod tool_registry;
mod usage;
mod web_assets;
mod worker;
mod worker_tools;
mod worker_profile;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct State {
    ports: Vec<ports::Port>,
    web_grants: Vec<web_access::Grant>,
    services: Vec<services::Service>,
    github_publications: Vec<github::Publication>,
    previews: Vec<previews::Preview>,
    chats: Vec<chat::Session>,
    discord_seen: Vec<String>,
    runtime: runtime::Runtime,
    collective: collective::Collective,
    bots: bots::Bots,
    watches: Vec<watches::Watch>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scores: Vec<scores::Probe>,
    observations: observe::Observations,
    agent_messages: Vec<application::messages::Sent>,
    documents: Vec<knowledge::Document>,
    lens_library: control_context::Library,
    personas: control_context::persona::Library,
    packages: Vec<packages::Manifest>,
    /// Requests the node's policy approved, newest last and bounded.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    auto_decisions: Vec<auto_approval::Record>,
    messages: Vec<String>,
    agents: Vec<String>,
    note: String,
    saved: bool,
}
impl State {
    fn initial() -> Self {
        let mut state = Self {
            note: include_str!("../DESIGN.md").to_owned(),
            ..Self::default()
        };
        knowledge::migrate(&mut state);
        state
    }
}
/// Minimal error page; the browser's Back button keeps the submitted form draft.
pub(crate) fn error_page(message: &str, back: &str) -> String {
    format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><title>Control / Not saved</title><link rel='stylesheet' href='/style.css'></head><body><main><h1>Not saved</h1><p role='alert'>{}</p><p>Use the browser's Back button to recover your draft, or <a href='{}'>return</a>.</p></main></body></html>", escape(message), escape(back))
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Some(n) = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    out.push(n);
                    i += 2;
                } else {
                    out.push(bytes[i]);
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn fields(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|p| p.split_once('=').map(|(k, v)| (decode(k), decode(v))))
        .collect()
}
fn render(state: &State, view: &str, lens: &str, channel: &str) -> String {
    render_with_content(state,view,lens,channel,None)
}
fn render_with_content(state: &State, view: &str, lens: &str, channel: &str, supplied:Option<String>) -> String {
    let view = if view == "manage" { "nodes" } else { view };
    let lens = lens_ui::normalize(lens);
    let chat_navigation = matches!(view, "session" | "session-list");
    let mut page = include_str!("../assets/index.html")
        .replace("</head>", "<link rel='stylesheet' href='/collective.css'><link rel='stylesheet' href='/runtime.css'><link rel='stylesheet' href='/workspace.css'><link rel='stylesheet' href='/navigation.css'></head>")
        .replace("{{page_discussion}}", &chat::discussion_link(view))
        .replace("{{navigation}}", &navigation::primary(view))
        .replace("{{section_navigation}}", &navigation::secondary(view))
        .replace("{{layout_class}}", if chat_navigation { "chat-workspace" } else if view == "rooms" { "rooms-workspace" } else if navigation::managed(view) { "with-section-nav" } else { "" })
        .replace("{{page_title}}", &navigation::heading(view));
    let supplied = supplied.or_else(|| (view == "authority").then(|| format!("{}{}", permissions_page::render(state), state.collective.render(view, channel).unwrap_or_default())));
    let content = supplied.or_else(|| if view == "monitor" { Some(usage::render(state)) } else if view == "agents" { Some(bots::page(state, &graveyard::Query::default())) } else { state.runtime.render(view,lens) }).or_else(|| state.collective.render(view, channel)).unwrap_or_else(|| match view {
        "providers" => providers::render(&state.runtime),
        "tools" => tool_catalog::render(state, if channel == "general" { "" } else { channel }),
        "session" => chat::render(state,channel).unwrap_or_else(escape),
        "session-list" | "history" | "artifact-page" | "browser" | "tool-page" => channel.to_owned(),
        "editor" | "knowledge" => knowledge::render(state,if view=="editor"{"scratchpad"}else{""},None),
        "knowledge-page" => channel.to_owned(),
        "rooms" => rooms::render(state,channel),
        "lenses" => lens_library::render(state,if lens=="none"{"engineering"}else{lens},None,None,false),
        "lens-page" => channel.to_owned(),
        "persona-page" => channel.to_owned(),
        "personas" => personas::render(state, "lead", None, None),
        "agents" => format!("<div class='section-heading'><div><span class='eyebrow'>EXECUTION / SIMULATED</span><h2>Agent sessions</h2></div></div><p class='muted'>Spawn a local demonstration session. Remote execution is not connected.</p>{}<form method='post' action='/spawn'><label>Session objective<input required name='objective' placeholder='Map the knowledge graph…' maxlength='300'></label><label>Provider<select name='provider'><option>Anthropic</option><option>OpenAI</option><option>Self-hosted</option></select></label><button class='primary'>＋ Spawn demo agent</button></form>", state.agents.iter().enumerate().map(|(i,a)| format!("<div class='session'><span class='dot'></span><div><strong>agent / {:03}</strong><p>{}</p></div><span class='tag'>simulated</span></div>", i+1, escape(a))).collect::<String>()),
        "nodes" => "<span class='eyebrow'>NODES · PLANNED</span><h2>Nodes</h2><p class='muted'>Remote nodes are not connected.</p><div class='node-grid'><article><span class='node-icon'>⌘</span><h3>control.local</h3><p>This laptop</p><span class='tag'>local</span></article><article><span class='node-icon'>◇</span><h3>forge.tailnet</h3><p>Development · planned</p><span class='tag'>not connected</span></article><article><span class='node-icon'>◈</span><h3>archive.tailnet</h3><p>Knowledge · planned</p><span class='tag'>not connected</span></article></div>".into(),
        // Unknown views (including the retired Overview) show the Usage page.
        _ => usage::render(state),
    });
    page = page.replace("{{content}}", &content);
    page
}
/// Views whose `channel` is trusted, pre-rendered HTML from internal routes only.
const INTERNAL_VIEWS: [&str; 8] = ["session-list", "history", "artifact-page", "browser", "tool-page", "knowledge-page", "lens-page", "persona-page"];
/// `GET /?view=…`: query values are untrusted, so internal HTML-carrying views fall back to Usage.
fn public_render(state: &State, query: &HashMap<String, String>, lens: &str) -> String {
    let view = query.get("view").map(String::as_str).filter(|v| !INTERNAL_VIEWS.contains(v)).unwrap_or("monitor");
    let channel = query.get(if view == "tools" { "q" } else { "channel" }).map(String::as_str).unwrap_or("general");
    if view=="agents" {
        return render_with_content(state,view,lens,channel,Some(bots::page(state,&graveyard::Query::from(query))));
    }
    render(state, view, lens, channel)
}
/// `GET /` with its response policy. The Bots page's process tree is a
/// pan/zoom canvas run by the Rust/WASM client, which also refreshes the
/// Models page's live server statistics; other views run no script.
fn public_page(state: &State, query: &HashMap<String, String>, lens: &str) -> (String, control_http::Policy) {
    let page = public_render(state, query, lens);
    if matches!(query.get("view").map(String::as_str), Some("agents" | "providers")) {
        (web_assets::with_client(&page), control_http::Policy::Client)
    } else {
        (page, control_http::Policy::Page)
    }
}
/// `/?view=session` predates `/dm/{id}`; send it to the page that renders chat state.
/// `/?view=runs` predates Bots / History and opens it with every DM's turns.
/// `/?view=lenses` predates the DNA page at `/dna`.
/// `/` lands on a new DM; the retired Overview (`/?view=hub`) moved to Usage.
fn legacy_session_location(state: &State, query: &HashMap<String, String>) -> Option<String> {
    let Some(view) = query.get("view") else { return Some("/dms?new=1".to_owned()) };
    match view.as_str() {
        "hub" | "overview" => Some("/?view=monitor".to_owned()),
        "runs" => url_names::history("/bots/history/runs", ""),
        "lenses" => Some("/dna".to_owned()),
        "personas" => Some("/personas".to_owned()),
        "session" => Some(query.get("channel")
            .filter(|id| chat::valid_id(id) && chat::detail(state, id).is_ok())
            .map_or_else(|| "/dms".to_owned(), |id| url_names::dm(id))),
        _ => None,
    }
}
const MAX_HEADER: usize = 65536;
/// Limits follow the current path, so a legacy path redirected with its body
/// (`/chat/ID/messages` → `/dm/ID/messages`) is admitted like its new name.
fn body_limit(target: &str) -> usize {
    let path = target.split('?').next().unwrap_or(target);
    let renamed = url_names::renamed(path);
    let path = renamed.as_deref().unwrap_or(path);
    if ["/knowledge/save", "/save", "/dna/save"].contains(&path) {
        512 * 1024
    } else if ["/dms", "/mcp"].contains(&path)
        || path.strip_prefix("/dm/").and_then(|p| p.strip_suffix("/messages"))
            .is_some_and(chat::valid_id)
    {
        control_context::limits::CHAT_REQUEST_BYTES
    } else {
        32768
    }
}
enum Body { Length(usize), Invalid, Chunked }
fn body_length(header: &str) -> Body {
    let values = |name: &str| header.lines().skip(1).filter_map(|l| l.split_once(':')).filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()).collect::<Vec<_>>();
    if !values("transfer-encoding").is_empty() {
        return Body::Chunked;
    }
    match values("content-length").as_slice() {
        [] => Body::Length(0),
        [first, rest @ ..] if rest.iter().all(|v| v == first) && first.bytes().all(|b| b.is_ascii_digit()) => first.parse().map_or(Body::Invalid, Body::Length),
        _ => Body::Invalid,
    }
}
/// Read until `done` or the overall request deadline; per-read timeouts alone allow trickled bytes.
fn read_until(stream: &mut TcpStream, data: &mut Vec<u8>, deadline: std::time::Instant, limit: usize, done: impl Fn(&[u8]) -> bool) -> bool {
    let mut buf = [0; 4096];
    while !done(data) {
        if shutdown::requested() { return false; }
        let Some(left) = deadline.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero()) else { return false };
        let _ = stream.set_read_timeout(Some(left.min(std::time::Duration::from_secs(3))));
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
        if data.len() > limit {
            return false;
        }
    }
    true
}
fn handle(mut stream: TcpStream, state: Arc<Mutex<State>>, access: Arc<access::Access>) {
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(3)));
    if shutdown::requested() {
        let _ = control_http::respond(&mut stream, "503 Service Unavailable", "text/plain", b"Control is stopping.", "", control_http::Policy::Page);
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut data = Vec::new();
    if !read_until(&mut stream, &mut data, deadline, MAX_HEADER, |d| d.windows(4).any(|w| w == b"\r\n\r\n")) {
        return;
    }
    let header_end = data.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let header = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let mut request = header.lines().next().unwrap_or("").split_whitespace();
    let method = request.next().unwrap_or("");
    let target = request.next().unwrap_or("/");
    // This service is local-only until an authenticated remote access layer exists.
    let host = header
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.trim())
        .unwrap_or("");
    let local_host = ["127.0.0.1", "localhost", "[::1]"].iter().any(|name| {
        host == *name
            || host
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with(':') && rest[1..].parse::<u16>().is_ok())
    });
    let origin = header
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("origin"))
        .map(|(_, v)| v.trim());
    if !local_host || origin.is_some_and(|v| v != format!("http://{host}")) {
        let _ = control_http::respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden", "", control_http::Policy::Page);
        return;
    }
    if access.gate(&mut stream, method, target, &header) {
        return;
    }
    if method == "GET" && target == "/events" {
        let _ = live_updates::serve(&mut stream);
        return;
    }
    let length = match body_length(&header) {
        Body::Length(length) => length,
        Body::Chunked => {
            let _ = control_http::respond(&mut stream, "411 Length Required", "text/plain", b"Send a fixed Content-Length.\r\n", "", control_http::Policy::Page);
            return;
        }
        Body::Invalid => {
            let _ = control_http::respond(&mut stream, "400 Bad Request", "text/plain", b"Invalid Content-Length.\r\n", "", control_http::Policy::Page);
            return;
        }
    };
    let limit = body_limit(target);
    if length > limit {
        let _ = control_http::respond(&mut stream, "413 Content Too Large", "text/plain", b"Request too large", "", control_http::Policy::Page);
        return;
    }
    if !read_until(&mut stream, &mut data, deadline, usize::MAX, |d| d.len() >= header_end + length) {
        return;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if let Some((status, location)) = url_names::redirect(method, path, query) {
        let _ = control_http::respond(&mut stream, status, "text/plain", b"", &format!("Location: {location}\r\n"), control_http::Policy::Page);
        return;
    }
    if compaction::http(&mut stream,&state,method,path,query,&data[header_end..header_end + length]){return;}
    if providers::live_http(&mut stream, &state, method, path) { return; }
    if ports::http(&mut stream, &state, method, path, &data[header_end..header_end + length]) { return; }
    if bots::http(&mut stream, &state, method, path, &data[header_end..header_end + length]) { return; }
    if channel_settings::http(&mut stream, method, path, &data[header_end..header_end + length]) { return; }
    if permissions_page::http(&mut stream, method, path, &data[header_end..header_end + length]) { return; }
    if services::http(&mut stream, &state, method, path, &data[header_end..header_end + length]) { return; }
    if web_access::http(&mut stream, &state, method, path, &data[header_end..header_end + length]) { return; }
    if mcp_http::handle(&mut stream,&state,method,path,&header,&data[header_end..header_end+length]) {return;}
    if github::http(&mut stream,&state,method,path,&data[header_end..header_end+length]) {return;}
    if projects::http(&mut stream,&state,method,path,query,&data[header_end..header_end+length]) {return;}
    if tools_http::handle(
        &mut stream,
        method,
        target,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    if browser_ui::handle(
        &mut stream,
        method,
        target,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    if method == "GET" && path.starts_with("/tool-window/") {
        web_assets::serve(&mut stream, path);
        return;
    }
    let query = fields(query);
    if method == "GET" && serve_stylesheet(&mut stream, path) {
        return;
    }
    state_response(&mut stream, &state, |mut stream, state| {
        let lens = lens_ui::selected(&state.lens_library, &header, query.get("lens").map(String::as_str));
        if method == "POST" && path == "/lens" {
            let f = fields(&String::from_utf8_lossy(&data[header_end..header_end + length]));
            let selected = lens_ui::selected(&state.lens_library, "", f.get("lens").map(String::as_str));
            let location = lens_ui::return_to(f.get("next").map(String::as_str).unwrap_or("/"));
            let _ = control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: {location}\r\n{}",lens_ui::cookie(selected)), control_http::Policy::Page);
            return;
        }
        if method == "POST" && path == "/providers" {
            let f = fields(&String::from_utf8_lossy(
                &data[header_end..header_end + length],
            ));
            let result = providers::save_with(state, &f, providers::save_settings);
            let (status, extra, body) = match result {
                Ok(()) => (
                    "303 See Other",
                    "Location: /?view=providers#saved\r\n",
                    String::new(),
                ),
                Err(message) => ("400 Bad Request", "", message.to_owned()),
            };
            let _ = control_http::respond(stream, status, "text/plain; charset=utf-8", body.as_bytes(), extra, control_http::Policy::Page);
            return;
        }
        if personas::handle(&mut stream, state, method, path, &query, lens, &data[header_end..header_end + length]) {
            return;
        }
        if lens_library::handle(
            &mut stream,
            state,
            method,
            path,
            &query,
            lens,
            &data[header_end..header_end + length],
        ) {
            return;
        }
        if knowledge::handle(
            &mut stream,
            state,
            method,
            path,
            &query,
            lens,
            &data[header_end..header_end + length],
        ) {
            return;
        }
        if session_http::handle(
            &mut stream,
            state,
            method,
            target,
            lens,
            &header,
            &data[header_end..header_end + length],
        ) {
            return;
        }
        let mut policy = control_http::Policy::Page;
        let (status, mime, body, extra) = if method == "POST" {
            let f = fields(&String::from_utf8_lossy(
                &data[header_end..header_end + length],
            ));
            let cancellation = (path == "/cancel-run").then(|| runtime::RunStatusCheckpoint::capture(&state.runtime, |run| Some(&run.id) == f.get("id")));
            let cancel_chat = if path == "/cancel-run" {
                state.runtime.runs.iter().find(|run| Some(&run.id) == f.get("id"))
                    .and_then(|run| run.chat_id.as_ref())
                    .and_then(|id| state.chats.iter().position(|chat| &chat.id == id))
                    .map(|index| (index, state.chats[index].revision))
            } else { None };
            let location = match path {
                // POST /chat and /improve are handled by session_http.
                "/task" | "/retry-run" | "/cancel-run" => {
                    state.runtime.action(path, &f)
                }
                "/room" | "/propose" | "/review" | "/request" | "/decide" => {
                    state.collective.action(path, &f)
                }
                "/room-claim-release" => {
                    rooms::claims::operator_action(state, &f);
                    "/?view=rooms"
                }
                "/spawn" => {
                    if let Some(task) = f.get("objective").filter(|s| !s.trim().is_empty()) {
                        state.agents.push(format!(
                            "{} · {}",
                            task.chars().take(300).collect::<String>(),
                            f.get("provider").unwrap_or(&"Self-hosted".into())
                        ));
                    }
                    "/?view=agents"
                }
                _ => "/",
            };
            let thread_location = matches!(path, "/room" | "/room-claim-release").then(|| f.get("channel"))
                .flatten().filter(|room| rooms::valid(room))
                .map(|room| format!("/?view=rooms&channel={room}"));
            let location = thread_location.as_deref().unwrap_or(location);
            if let Some((index, _)) = cancel_chat {
                state.chats[index].revision = state.chats[index].revision.saturating_add(1);
            }
            let persisted = if let Some(checkpoint) = cancellation { checkpoint.save(state, storage::save) } else { storage::save(state) };
            if persisted.is_err() {
                if let Some((index, revision)) = cancel_chat {
                    state.chats[index].revision = revision;
                }
                let body = error_page("Could not persist the local change.", location);
                let _ = control_http::respond(stream, "500 Internal Server Error", "text/html; charset=utf-8", body.as_bytes(), "", control_http::Policy::Page);
                return;
            }
            (
                "303 See Other",
                "text/plain",
                String::new(),
                format!("Location: {location}\r\n"),
            )
        } else if let Some(location) = (path == "/").then(|| legacy_session_location(state, &query)).flatten() {
            ("303 See Other", "text/plain", String::new(), format!("Location: {location}\r\n"))
        } else if path == "/" {
            let page;
            (page, policy) = public_page(state, &query, lens);
            ("200 OK", "text/html; charset=utf-8", page, String::new())
        } else {
            (
                "404 Not Found",
                "text/plain",
                "Not found".into(),
                String::new(),
            )
        };
        let preference = if method == "GET" && query.contains_key("lens") {
            lens_ui::cookie(lens)
        } else {
            String::new()
        };
        let _ = control_http::respond(stream, status, mime, body.as_bytes(), &format!("{extra}{preference}"), policy);
    });
}

// Render and commit while locked, then release state before touching the socket.
// Slow readers must never prevent other clients or the supervisor from progressing.
fn state_response(
    stream: &mut impl Write,
    state: &Mutex<State>,
    respond: impl FnOnce(&mut Vec<u8>, &mut State),
) {
    let mut response = Vec::new();
    match state.lock() {
        Ok(mut state) => respond(&mut response, &mut state),
        Err(_) => {
            let body = "State unavailable. Restart Control after checking.";
            let _ = control_http::respond(&mut response, "503 Service Unavailable", "text/plain", body.as_bytes(), "", control_http::Policy::Page);
        }
    }
    let _ = stream.write_all(&response);
}

fn serve_stylesheet(stream: &mut impl Write, path: &str) -> bool {
    let body = match path {
        "/navigation.css" => include_str!("../assets/navigation.css"),
        "/workspace.css" => include_str!("../assets/workspace.css"),
        "/runtime.css" => include_str!("../assets/runtime.css"),
        "/collective.css" => include_str!("../assets/collective.css"),
        "/style.css" => include_str!("../assets/style.css"),
        _ => return false,
    };
    let _ = control_http::respond(stream, "200 OK", "text/css; charset=utf-8", body.as_bytes(), "", control_http::Policy::Asset);
    true
}

fn main() -> std::process::ExitCode {
    // Print the message, not io::Error's Debug form ("Custom { kind: Other, … }").
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
fn run() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "models") { return model_calls::command(&args[2..]); }
    if args.get(1).is_some_and(|s| s == "claude") {
        return claude::command(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "grok") {
        return grok::command(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "--version") {
        if args.get(2).is_some_and(|s| s == "--json") {
            println!("{}", serde_json::to_string(&control::build_info::current()).map_err(std::io::Error::other)?);
            return Ok(());
        }
        println!(
            "control {} · {}-{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "doctor") {
        return doctor::run();
    }
    if args.get(1).is_some_and(|s| s == "discord") {
        discord_bridge::command(&args[1..]);
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "federation") {
        return federation::run(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "mcp") {
        return mcp_stdio::run(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "browser") {
        return mcp_stdio::browser_cli(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "github") {
        return github::cli(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "package") {
        return packages::cli(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "--help" || s == "help") {
        println!("control [serve | tui | mcp | discord | doctor | config | open | browser | federation | package]\nserve: local web/MCP server on CONTROL_ADDR (default 127.0.0.1:8080)\ntui: Rust terminal client\nmcp [--root DIRECTORY]: local tools for an external harness\nmcp --chat: drive chats as the local operator from an external harness\ndiscord --enable: listen to this node's configured DMs\ndoctor: offline prerequisites; no model calls\nconfig init | path: initialize or locate personal configuration\nopen: sign in to the local website\nbrowser capture FILE [WIDTH HEIGHT]: offline Nix Firefox capture\nfederation help: signed offline lens exchange\npackage import | revoke | list | show: admit and inspect reusable tool packages\n--check-worker: one authenticated sandbox test (uses model usage)\n--version [--json]: version, platform and optional build identification");
        println!("claude login | check: container login or offline tool isolation check\ngrok login | check: the same for Grok Build\nconfig init [--provider codex|claude|grok] [--source-root PATH]: initialize private configuration without replacing saved choices\nconfig path: show the node configuration directory\n--bind-discord LABEL ID: bind a configured principal locally");
        println!("open --link: print a one-use local browser sign-in link");
        println!("github configure: bind improvement PR publication to a repository and private credential reference");
        return Ok(());
    }
    let addr = config::server_addr();
    let socket: std::net::SocketAddr = addr.parse().map_err(|_| std::io::Error::other(format!("CONTROL_ADDR must be a loopback IP:port, such as 127.0.0.1:8080; got {addr:?}")))?;
    if !socket.ip().is_loopback() {
        return Err(std::io::Error::other(
            "Control currently requires a loopback bind address.",
        ));
    }
    if args.get(1).is_some_and(|s| s == "tui") {
        #[cfg(feature = "tui")]
        return control_tui::terminal::run();
        #[cfg(not(feature = "tui"))]
        return Err(std::io::Error::other("This server build excludes the terminal UI; use a Control build with the tui feature."));
    }
    if args.get(1).is_some_and(|s| s == "config") {
        let directory = config::directory().map_err(std::io::Error::other)?;
        match args.get(2).map(String::as_str) {
            Some("path") => println!("{}", directory.display()),
            Some("init") => {
                let (provider, source_root) = providers::initialization_options(&args[3..])?;
                config::initialize(include_bytes!("../assets/discord-access.example.json"))?;
                storage::initialize()?;
                if let Some(source_root) = source_root {
                    config::initialize_source_root(&source_root)?;
                }
                if let Some(provider) = provider {
                    providers::initialize(provider)?;
                }
                println!(
                    "Private node configuration ready in {}",
                    directory.display()
                );
            }
            _ => {
                return Err(std::io::Error::other(
                    "Use control config init or control config path",
                ))
            }
        }
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|s| s != "serve" && s != "open" && !s.starts_with("--"))
    {
        return Err(std::io::Error::other(
            "Unknown command. Use control --help.",
        ));
    }
    if args.get(1).is_some_and(|s| s == "--check-worker") {
        storage::initialize()?;
        return worker::check();
    }
    if args.get(1).is_some_and(|a| a == "--discord-channel") {
        return Err(std::io::Error::other(
            "Server-channel access is disabled. Control is DM-only.",
        ));
    }
    if args.get(1).is_some_and(|s| s == "open") {
        return access::command(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "--bind-discord") {
        let result = (|| -> Result<(), &'static str> {
            let mut policy = identity::Policy::load()?;
            policy.bind(
                args.get(2).ok_or("Missing principal label")?,
                args.get(3).ok_or("Missing Discord ID")?.parse()?,
            )?;
            policy.save()
        })();
        return result.map_err(std::io::Error::other);
    }
    if args.get(1).is_some_and(|s| s != "serve") {
        return Err(std::io::Error::other(
            "Unknown command. Use control --help.",
        ));
    }
    let listener = TcpListener::bind(&addr).map_err(|error| if error.kind() == std::io::ErrorKind::AddrInUse {
        std::io::Error::other(format!("{addr} is already in use. Stop the other server or set CONTROL_ADDR, e.g. CONTROL_ADDR=127.0.0.1:8081."))
    } else { error })?;
    storage::initialize()?;
    let _state_lock = storage::lock()?;
    let access = Arc::new(access::Access::load()?);
    let mut loaded=storage::load()?;
    if let Err(error)=model_calls::recover() {loaded.runtime.notice=format!("Text-call cleanup needs inspection: {error}");}
    // No import runs yet; a staged copy left by the last process is unreferenced.
    if let Err(error)=projects::discard_interrupted_imports() {eprintln!("Could not remove interrupted folder imports: {error}");}
    let state = Arc::new(Mutex::new(loaded));
    shutdown::install()?;
    // Blocking accept removes the 50 ms admission poll. Cancellation wakes it
    // through the same loopback listener, without doing socket I/O in a signal handler.
    let wake_address = listener.local_addr()?;
    let accept_cancel = shutdown::token();
    let accept_wake = std::thread::spawn(move || {
        accept_cancel.wait(None);
        let _ = TcpStream::connect_timeout(&wake_address, std::time::Duration::from_secs(1));
    });
    let preview_state=state.clone();
    let preview_restore=std::thread::spawn(move||previews::restore(preview_state));
    let workers = worker::start(state.clone());
    let services = services::supervise(state.clone());
    let ports = ports::supervise(state.clone());
    let scores = scores::supervise(state.clone());
    println!("control · {} · run ./target/release/control open in another terminal to sign in", config::server_url());
    let mut listener_error = None;
    let mut requests = http_admission::Requests::new();
    while !shutdown::requested() {
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown::requested() { break; }
                let state = state.clone();
                let access = access.clone();
                if let Err(error) = requests.dispatch(stream, move |stream| handle(stream, state, access)) {
                    listener_error = Some(error);
                    shutdown::request();
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                listener_error = Some(error);
                shutdown::request();
            }
        }
    }
    drop(listener);
    let _ = accept_wake.join();
    eprintln!("Control is stopping; waiting for owned workers and saving state.");
    let stopped = workers.join();
    let service_failed = services.join().is_err();
    let port_failed = ports.join().is_err();
    let _ = scores.join();
    let _=preview_restore.join();
    // Already-admitted HTTP tool calls have their own bounded waits. Let those
    // handlers finish too before the final state save and process exit.
    let requests_result = requests.join();
    // Approved folder imports run after their HTTP response; each stops at its
    // next copied entry and records the interruption beside its request.
    projects::join_imports();
    let state = state.lock().map_err(|_| std::io::Error::other("State unavailable during shutdown; retained attempts require inspection on restart."))?;
    // A stopped node leaves a self-contained `state.json` (empty patch log), so
    // older builds and offline readers need nothing else.
    storage::compact(&state)?;
    stopped?;
    if service_failed { return Err(std::io::Error::other("Service supervisor stopped unexpectedly; inspect retained service identities.")); }
    if port_failed { return Err(std::io::Error::other("Port supervisor stopped unexpectedly; inspect retained port approvals.")); }
    requests_result?;
    match listener_error { Some(error) => Err(error), None => Ok(()) }
}

#[cfg(test)]
#[path = "main/tests/responsiveness_tests.rs"]
mod responsiveness_tests;

#[cfg(test)]
#[path = "main/tests/url_names_tests.rs"]
mod url_names_tests;

#[cfg(test)]
#[path = "main/tests/ui_previews.rs"]
mod ui_previews;
