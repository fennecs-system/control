mod access;
mod accounts;
mod agent_host;
mod agent_message;
mod application;
mod approvals;
mod artifacts;
mod auto_approval;
mod backend;
mod bots;
#[path = "../tools/browser/main.rs"]
mod browser;
mod browser_ui;
mod build_environment;
mod callsigns;
mod channel_settings;
mod chat;
mod collective;
mod command_jobs;
mod conversation;
mod conversations;
mod delegated_results;
mod delegation;
mod design;
mod history;
mod history_view;
mod host_shell;
mod ids;
mod observe;
mod pagination;
mod turn_context;
#[path = "../tools/service-runner/frame.rs"]
#[allow(dead_code)]
mod watch_frame;
mod watches;
use control_agent::child;
use control_config as config;
use control_discord::bridge as discord_bridge;
use control_identity as identity;
use control_ingress as discord;
use control_url_names as url_names;
mod attempt_control;
mod compaction;
mod doctor;
mod engine;
mod federation;
mod github;
mod graph_search;
mod graveyard;
mod guidance;
mod http_admission;
mod intel;
mod knowledge;
mod knowledge_tools;
mod lens_library;
mod lens_ui;
mod lifecycle;
mod live_updates;
mod local_agent;
mod logins;
mod mcp_chat;
mod mcp_http;
mod mcp_stdio;
mod model_calls;
mod navigation;
mod packages;
#[cfg(test)]
#[path = "main/tests/performance.rs"]
mod performance;
mod permissions_page;
mod personas;
mod ports;
mod previews;
mod process_tree;
mod projects;
mod providers;
mod recovery;
mod rooms;
mod runtime;
mod rust_bench;
mod scheduler;
mod scores;
mod services;
mod session_http;
mod shutdown;
mod solver;
mod spawning;
mod storage;
mod store;
use store::Store;
#[path = "main/posts.rs"]
mod posts;
#[cfg(test)]
mod test_support;
mod texts;
mod tool_catalog;
mod tool_registry;
mod toolbox;
mod tools_http;
mod usage;
mod web_access;
mod web_assets;
mod worker;
mod worker_profile;
mod worker_tools;
use control_journal::snapshot::Part;
use std::{
    collections::HashMap,
    io::{Read as _, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
};

/// Each field is a `Part` (`runtime` holds its runs as parts): a save encodes
/// and diffs only the fields, room posts, agent notes and runs borrowed
/// mutably since the last one.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct State {
    ports: Part<Vec<ports::Port>>,
    web_grants: Part<Vec<web_access::Grant>>,
    services: Part<Vec<services::Service>>,
    github_publications: Part<Vec<github::Publication>>,
    previews: Part<Vec<previews::Preview>>,
    chats: Part<Vec<chat::Session>>,
    discord_seen: Part<Vec<String>>,
    /// Split at every save, not kept: archiving a run changes its encoding
    /// through a shared borrow (`storage::archive`). Its runs are parts.
    #[serde(serialize_with = "control_journal::snapshot::split_each_save")]
    runtime: runtime::Runtime,
    collective: Part<collective::Collective>,
    bots: Part<bots::Bots>,
    watches: Part<Vec<watches::Watch>>,
    #[serde(skip_serializing_if = "is_empty")]
    scores: Part<Vec<scores::Probe>>,
    observations: Part<observe::Observations>,
    agent_messages: Part<Vec<application::messages::Sent>>,
    documents: Part<Vec<knowledge::Document>>,
    lens_library: Part<control_context::Library>,
    personas: Part<control_context::persona::Library>,
    packages: Part<Vec<packages::Manifest>>,
    /// Requests the node's policy approved, newest last and bounded.
    #[serde(skip_serializing_if = "is_empty")]
    auto_decisions: Part<Vec<auto_approval::Record>>,
    messages: Part<Vec<String>>,
    agents: Part<Vec<Part<String>>>,
    note: Part<String>,
    saved: bool,
}
fn is_empty<T>(items: &Part<Vec<T>>) -> bool {
    items.is_empty()
}
impl State {
    fn initial() -> Self {
        let mut state = Self {
            note: include_str!("../DESIGN.md").to_owned().into(),
            ..Self::default()
        };
        knowledge::migrate(&mut state);
        state
    }
}
/// Minimal error page; the browser's Back button keeps the submitted form draft.
pub(crate) fn error_page(message: &str, back: &str) -> String {
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><title>Control / Not saved</title><link rel='stylesheet' href='/style.css'></head><body><main><h1>Not saved</h1><p role='alert'>{}</p><p>Use the browser's Back button to recover your draft, or <a href='{}'>return</a>.</p></main></body></html>",
        escape(message),
        escape(back)
    )
}
/// HTML-escapes `&`, `<`, `>`, `"` and `'` in one pass.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    escape_into(&mut out, s);
    out
}
/// [`escape`], appended to `out` without a temporary string.
fn escape_into(out: &mut String, s: &str) {
    // The escaped bytes are ASCII, so every split is on a character boundary.
    let start = s.bytes().enumerate().fold(0, |start, (at, byte)| {
        let entity = match byte {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\'' => "&#39;",
            _ => return start,
        };
        out.push_str(&s[start..at]);
        out.push_str(entity);
        at + 1
    });
    out.push_str(&s[start..]);
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
    render_with_content(state, view, lens, channel, None)
}
fn render_with_content(state: &State, view: &str, lens: &str, channel: &str, supplied: Option<String>) -> String {
    let view = if view == "manage" { "nodes" } else { view };
    let lens = lens_ui::normalize(lens);
    let supplied = supplied.or_else(|| {
        (view == "authority").then(|| {
            format!(
                "{}{}",
                permissions_page::render(state),
                state.collective.render(view, channel).unwrap_or_default()
            )
        })
    });
    let content = supplied.or_else(|| if view == "monitor" { Some(usage::render(state)) } else if view == "agents" { Some(bots::page(state, &graveyard::Query::default())) } else { state.runtime.render(view,lens) }).or_else(|| state.collective.render(view, channel)).unwrap_or_else(|| match view {
        "providers" => providers::render(&state.runtime),
        "tools" => tool_catalog::render(state, if channel == "general" { "" } else { channel }),
        "session" => chat::render(state,channel).unwrap_or_else(escape),
        "session-list" | "history" | "artifact-page" | "browser" | "tool-page" => channel.to_owned(),
        "editor" | "knowledge" => knowledge::render(&state.documents,&state.collective,if view=="editor"{"scratchpad"}else{""},None),
        "knowledge-page" => channel.to_owned(),
        "rooms" => rooms::render(state,channel),
        "lenses" => lens_library::render(lens_library::Sources::of(state),if lens=="none"{"engineering"}else{lens},None,None,false),
        "lens-page" => channel.to_owned(),
        "persona-page" => channel.to_owned(),
        "personas" => personas::render(state, "lead", None, None),
        "agents" => format!("<div class='section-heading'><div><span class='eyebrow'>EXECUTION / SIMULATED</span><h2>Agent sessions</h2></div></div><p class='muted'>Spawn a local demonstration session. Remote execution is not connected.</p>{}<form method='post' action='/spawn'><label>Session objective<input required name='objective' placeholder='Map the knowledge graph…' maxlength='300'></label><label>Provider<select name='provider'><option>Anthropic</option><option>OpenAI</option><option>Self-hosted</option></select></label><button class='primary'>＋ Spawn demo agent</button></form>", state.agents.iter().enumerate().map(|(i,a)| format!("<div class='session'><span class='dot'></span><div><strong>agent / {:03}</strong><p>{}</p></div><span class='tag'>simulated</span></div>", i+1, escape(a))).collect::<String>()),
        "nodes" => NODES.into(),
        // Unknown views (including the retired Overview) show the Usage page.
        _ => usage::render(state),
    });
    shell(view, &content)
}
/// The page frame around content an internal route already rendered. It reads
/// no state, so such routes need not hold or fake one.
pub(crate) fn shell(view: &str, content: &str) -> String {
    let chat_navigation = matches!(view, "session" | "session-list");
    include_str!("../assets/index.html")
        .replace("</head>", "<link rel='stylesheet' href='/collective.css'><link rel='stylesheet' href='/runtime.css'><link rel='stylesheet' href='/workspace.css'><link rel='stylesheet' href='/navigation.css'></head>")
        .replace("{{page_discussion}}", &chat::discussion_link(view))
        .replace("{{navigation}}", &navigation::primary(view))
        .replace("{{section_navigation}}", &navigation::secondary(view))
        .replace("{{layout_class}}", if chat_navigation { "chat-workspace" } else if view == "rooms" { "rooms-workspace" } else if navigation::managed(view) { "with-section-nav" } else { "" })
        .replace("{{page_title}}", &navigation::heading(view))
        .replace("{{content}}", content)
}
/// Views whose `channel` is trusted, pre-rendered HTML from internal routes only.
const INTERNAL_VIEWS: [&str; 8] = [
    "session-list",
    "history",
    "artifact-page",
    "browser",
    "tool-page",
    "knowledge-page",
    "lens-page",
    "persona-page",
];
/// `GET /?view=…`: query values are untrusted, so internal HTML-carrying views fall back to Usage.
fn public_render(state: &State, query: &HashMap<String, String>, lens: &str) -> String {
    let view = query
        .get("view")
        .map(String::as_str)
        .filter(|v| !INTERNAL_VIEWS.contains(v))
        .unwrap_or("monitor");
    let channel = query
        .get(if view == "tools" { "q" } else { "channel" })
        .map(String::as_str)
        .unwrap_or("general");
    if view == "agents" {
        return shell(view, &bots::page(state, &graveyard::Query::from(query)));
    }
    render(state, view, lens, channel)
}
/// `GET /?view=agents`, the Bots page, served by [`BOTS_PAGE`].
struct BotsPage {
    query: graveyard::Query,
    /// A lens the request chose, which the response remembers in a cookie.
    lens: Option<String>,
}
impl BotsPage {
    fn of(method: &str, path: &str, query: &HashMap<String, String>) -> Option<Self> {
        (method == "GET" && path == "/" && query.get("view").is_some_and(|view| view == "agents")).then(|| Self {
            query: graveyard::Query::from(query),
            lens: query.get("lens").cloned(),
        })
    }
}
const BOTS_PAGE: Read<BotsPage, (bots::Page, String)> = Read {
    view: |state, BotsPage { query, lens }| {
        let cookie = lens.map_or_else(String::new, |lens| {
            lens_ui::cookie(lens_ui::selected(&state.lens_library, "", Some(&lens)))
        });
        (bots::Page::of(state, &query), cookie)
    },
    render: |(page, cookie)| Reply {
        status: "200 OK",
        mime: "text/html; charset=utf-8",
        body: web_assets::with_client(&shell("agents", &page.render())),
        extra: cookie,
        policy: control_http::Policy::Client,
    },
};
/// `GET /` other than the Bots page: an operator page, or a redirect from an
/// older address. Served by [`OPERATOR_PAGES`].
struct OperatorPage {
    query: HashMap<String, String>,
    /// The lens the request names (`lens_ui::requested`).
    lens: String,
    /// The UTC day that ends the Usage page's activity grid.
    today: u64,
    /// The time the Models page's cooling accounts count from.
    now: accounts::UnixSeconds,
    /// A page, or the Permissions form, that reads files and no state,
    /// rendered before locking.
    unlocked: Option<String>,
}
impl OperatorPage {
    fn of(method: &str, path: &str, query: &HashMap<String, String>, header: &str) -> Option<Self> {
        (method == "GET" && path == "/").then(|| Self {
            query: query.clone(),
            lens: lens_ui::requested(header, query.get("lens").map(String::as_str)).to_owned(),
            today: usage::today(),
            now: providers::accounts::now(),
            unlocked: match query.get("view").map(String::as_str) {
                Some("authority") => Some(permissions_page::form()),
                Some("discord-access") => Some(runtime::discord_access()),
                Some("integrations") => Some(runtime::integrations()),
                _ => None,
            },
        })
    }
}
enum Operator {
    /// `/?view=evolution` predates `/dms?improve=1`.
    Improve,
    /// An older address (`legacy_session_location`).
    Moved { location: String, cookie: String },
    Page {
        view: String,
        content: Box<Content>,
        cookie: String,
    },
}
/// An operator page's content, copied out of state.
enum Content {
    Usage {
        page: usage::Page,
        today: u64,
    },
    /// The Permissions form (rendered before locking), the rest of the
    /// Permissions page and the access requests.
    Authority(String, permissions_page::Page, collective::Page),
    Collective(collective::Page),
    /// Its logins are checked after the state lock.
    Providers(providers::Page),
    Tools(tool_catalog::Search),
    Knowledge(knowledge::Page),
    Rooms(rooms::Page),
    /// Content that reads no state.
    Fixed(String),
}
impl Content {
    fn render(self) -> String {
        match self {
            Self::Usage { page, today } => page.render(today),
            Self::Authority(form, permissions, requests) => {
                format!("{form}{}{}", permissions.render(), requests.render())
            }
            Self::Collective(page) => page.render(),
            Self::Tools(search) => search.render(),
            Self::Rooms(page) => page.render(),
            Self::Providers(page) => page.sign_ins().render(),
            Self::Knowledge(page) => page.render(),
            Self::Fixed(html) => html,
        }
    }
}
const NODES: &str = "<span class='eyebrow'>NODES · PLANNED</span><h2>Nodes</h2><p class='muted'>Remote nodes are not connected.</p><div class='node-grid'><article><span class='node-icon'>⌘</span><h3>control.local</h3><p>This laptop</p><span class='tag'>local</span></article><article><span class='node-icon'>◇</span><h3>forge.tailnet</h3><p>Development · planned</p><span class='tag'>not connected</span></article><article><span class='node-icon'>◈</span><h3>archive.tailnet</h3><p>Knowledge · planned</p><span class='tag'>not connected</span></article></div>";
const OPERATOR_PAGES: Read<OperatorPage, Operator> = Read {
    view: |state,
           OperatorPage {
               query,
               lens,
               today,
               now,
               unlocked,
           }| {
        if query.get("view").is_some_and(|view| view == "evolution") {
            return Operator::Improve;
        }
        let cookie = if query.contains_key("lens") {
            lens_ui::cookie(lens_ui::known(&state.lens_library, &lens))
        } else {
            String::new()
        };
        if let Some(location) = legacy_session_location(state, &query) {
            return Operator::Moved { location, cookie };
        }
        // Query values are untrusted, so internal HTML-carrying views fall back to Usage.
        let view = match query.get("view").map(String::as_str) {
            Some("manage") => "nodes",
            Some(view) if !INTERNAL_VIEWS.contains(&view) => view,
            _ => "monitor",
        };
        let channel = query
            .get(if view == "tools" { "q" } else { "channel" })
            .map(String::as_str)
            .unwrap_or("general");
        let content = match view {
            "authority" => Content::Authority(
                unlocked.unwrap_or_default(),
                permissions_page::Page::of(state),
                state.collective.access(),
            ),
            "knowledge-review" => Content::Collective(state.collective.review()),
            "providers" => Content::Providers(providers::Page::of(&state.runtime, now)),
            "tools" => Content::Tools(tool_catalog::Search::of(
                state,
                if channel == "general" { "" } else { channel },
            )),
            "editor" | "knowledge" => Content::Knowledge(knowledge::Page::of(
                &state.documents,
                &state.collective,
                if view == "editor" { "scratchpad" } else { "" },
                None,
            )),
            "rooms" => Content::Rooms(rooms::Page::of(state, channel)),
            "nodes" => Content::Fixed(NODES.to_owned()),
            "discord-access" | "integrations" => Content::Fixed(unlocked.unwrap_or_default()),
            _ => Content::Usage {
                page: usage::Page::of(state),
                today,
            },
        };
        Operator::Page {
            view: view.to_owned(),
            content: Box::new(content),
            cookie,
        }
    },
    render: |operator| match operator {
        Operator::Improve => Reply {
            status: "303 See Other",
            mime: "text/plain",
            body: String::new(),
            extra: "Location: /dms?improve=1\r\n".to_owned(),
            policy: control_http::Policy::Embedded,
        },
        Operator::Moved { location, cookie } => Reply {
            status: "303 See Other",
            mime: "text/plain",
            body: String::new(),
            extra: format!("Location: {location}\r\n{cookie}"),
            policy: control_http::Policy::Page,
        },
        Operator::Page { view, content, cookie } => {
            let page = shell(&view, &content.render());
            // The Models page's client refreshes its live server statistics.
            let (body, policy) = if view == "providers" {
                (web_assets::with_client(&page), control_http::Policy::Client)
            } else {
                (page, control_http::Policy::Page)
            };
            Reply {
                status: "200 OK",
                mime: "text/html; charset=utf-8",
                body,
                extra: cookie,
                policy,
            }
        }
    },
};
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
    let Some(view) = query.get("view") else {
        return Some("/dms?new=1".to_owned());
    };
    match view.as_str() {
        "hub" | "overview" => Some("/?view=monitor".to_owned()),
        "runs" => url_names::history("/bots/history/runs", ""),
        "lenses" => Some("/dna".to_owned()),
        "personas" => Some("/personas".to_owned()),
        "session" => Some(
            query
                .get("channel")
                .filter(|id| chat::valid_id(id) && chat::viewable(&state.chats, id).is_ok())
                .map_or_else(|| "/dms".to_owned(), |id| url_names::dm(id)),
        ),
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
        || path
            .strip_prefix("/dm/")
            .and_then(|p| p.strip_suffix("/messages"))
            .is_some_and(chat::valid_id)
    {
        control_context::limits::CHAT_REQUEST_BYTES
    } else {
        32768
    }
}
enum Body {
    Length(usize),
    Invalid,
    Chunked,
}
fn body_length(header: &str) -> Body {
    let values = |name: &str| {
        header
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
            .collect::<Vec<_>>()
    };
    if !values("transfer-encoding").is_empty() {
        return Body::Chunked;
    }
    match values("content-length").as_slice() {
        [] => Body::Length(0),
        [first, rest @ ..] if rest.iter().all(|v| v == first) && first.bytes().all(|b| b.is_ascii_digit()) => {
            first.parse().map_or(Body::Invalid, Body::Length)
        }
        _ => Body::Invalid,
    }
}
/// Read until `done` or the overall request deadline; per-read timeouts alone allow trickled bytes.
fn read_until(
    stream: &mut TcpStream,
    data: &mut Vec<u8>,
    deadline: std::time::Instant,
    limit: usize,
    done: impl Fn(&[u8]) -> bool,
) -> bool {
    let mut buf = [0; 4096];
    while !done(data) {
        if shutdown::requested() {
            return false;
        }
        let Some(left) = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
        else {
            return false;
        };
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
fn handle(mut stream: TcpStream, state: Store, access: Arc<access::Access>) {
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(3)));
    if shutdown::requested() {
        let _ = control_http::respond(
            &mut stream,
            "503 Service Unavailable",
            "text/plain",
            b"Control is stopping.",
            "",
            control_http::Policy::Page,
        );
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut data = Vec::new();
    if !read_until(&mut stream, &mut data, deadline, MAX_HEADER, |d| {
        d.windows(4).any(|w| w == b"\r\n\r\n")
    }) {
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
        let _ = control_http::respond(
            &mut stream,
            "403 Forbidden",
            "text/plain",
            b"Forbidden",
            "",
            control_http::Policy::Page,
        );
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
            let _ = control_http::respond(
                &mut stream,
                "411 Length Required",
                "text/plain",
                b"Send a fixed Content-Length.\r\n",
                "",
                control_http::Policy::Page,
            );
            return;
        }
        Body::Invalid => {
            let _ = control_http::respond(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                b"Invalid Content-Length.\r\n",
                "",
                control_http::Policy::Page,
            );
            return;
        }
    };
    let limit = body_limit(target);
    if length > limit {
        let _ = control_http::respond(
            &mut stream,
            "413 Content Too Large",
            "text/plain",
            b"Request too large",
            "",
            control_http::Policy::Page,
        );
        return;
    }
    if !read_until(&mut stream, &mut data, deadline, usize::MAX, |d| {
        d.len() >= header_end + length
    }) {
        return;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if let Some((status, location)) = url_names::redirect(method, path, query) {
        let _ = control_http::respond(
            &mut stream,
            status,
            "text/plain",
            b"",
            &format!("Location: {location}\r\n"),
            control_http::Policy::Page,
        );
        return;
    }
    // Routes below read state their own way: replies wait for released changes.
    let mut gated = store::Gated::new(&mut stream, &state);
    if compaction::http(
        &mut gated,
        &state,
        method,
        path,
        query,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    if providers::live_http(&mut gated, &state, method, path) {
        return;
    }
    if ports::http(&mut gated, &state, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if bots::http(&mut gated, &state, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if channel_settings::http(&mut gated, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if permissions_page::http(&mut gated, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if services::http(&mut gated, &state, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if web_access::http(&mut gated, &state, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if mcp_http::handle(
        &mut gated,
        &state,
        method,
        path,
        &header,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    if github::http(&mut gated, &state, method, path, &data[header_end..header_end + length]) {
        return;
    }
    if projects::http(
        &mut gated,
        &state,
        method,
        path,
        query,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    if tools_http::handle(&mut stream, method, target, &data[header_end..header_end + length]) {
        return;
    }
    if browser_ui::handle(&mut stream, method, target, &data[header_end..header_end + length]) {
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
    if let Some(page) = session_http::ChatPage::of(method, path, &header) {
        read_response(&mut stream, &state, &session_http::CHAT_PAGES, page);
        return;
    }
    if let Some(page) = session_http::FilePage::of(method, path, &query) {
        read_response(&mut stream, &state, &session_http::FILE_PAGES, page);
        return;
    }
    if let Some(page) = BotsPage::of(method, path, &query) {
        read_response(&mut stream, &state, &BOTS_PAGE, page);
        return;
    }
    if method == "GET" && path == "/dms" {
        let lens = lens_ui::requested(&header, query.get("lens").map(String::as_str)).to_owned();
        let dms = session_http::Dms::of(&query, &header, lens);
        read_response(&mut stream, &state, &session_http::DMS, dms);
        return;
    }
    if method == "GET" && path == history_view::PATH {
        read_response(
            &mut stream,
            &state,
            &history_view::HISTORY,
            history_view::Filter::from(&query),
        );
        return;
    }
    if method == "GET" && path == "/bots/history/turns" {
        read_response(
            &mut stream,
            &state,
            &history_view::DM_TURNS,
            history_view::Filter::from(&query),
        );
        return;
    }
    if method == "GET" && path == "/bots/graveyard" {
        let page = graveyard::Query::from(&query);
        read_response(&mut stream, &state, &graveyard::NEXT_PAGE, page);
        return;
    }
    if let Some(page) = OperatorPage::of(method, path, &query, &header) {
        read_response(&mut stream, &state, &OPERATOR_PAGES, page);
        return;
    }
    if method == "GET" && path == "/knowledge" {
        read_response(&mut stream, &state, &knowledge::PAGE, knowledge::Request::from(&query));
        return;
    }
    if lens_library::http(
        &mut stream,
        &state,
        method,
        path,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    if let Some(post) = posts::Post::of(method, path) {
        posts::respond(&mut stream, &state, post, &data[header_end..header_end + length]);
        return;
    }
    if session_http::messages(
        &mut stream,
        &state,
        method,
        path,
        &header,
        query.get("lens").map(String::as_str),
        &data[header_end..header_end + length],
    ) {
        return;
    }
    state_response(&mut stream, &state, |mut stream, state| {
        let lens = lens_ui::selected(&state.lens_library, &header, query.get("lens").map(String::as_str));
        if method == "POST" && path == "/lens" {
            let f = fields(&String::from_utf8_lossy(&data[header_end..header_end + length]));
            let selected = lens_ui::selected(&state.lens_library, "", f.get("lens").map(String::as_str));
            let location = lens_ui::return_to(f.get("next").map(String::as_str).unwrap_or("/"));
            let _ = control_http::respond(
                stream,
                "303 See Other",
                "text/plain",
                b"",
                &format!("Location: {location}\r\n{}", lens_ui::cookie(selected)),
                control_http::Policy::Page,
            );
            return;
        }
        if method == "POST" && path == "/providers" {
            let f = fields(&String::from_utf8_lossy(&data[header_end..header_end + length]));
            // Clearing a cooldown changes node state, saved like any run change.
            let result = if f.get("action").is_some_and(|a| a == "account-clear") {
                providers::clear_account(&mut state.runtime, &f)
                    .and_then(|()| storage::save(state).map_err(|_| "Could not save the cleared account"))
            } else {
                providers::save_with(&mut state.runtime, &f, providers::save_settings)
            };
            let (status, extra, body) = match result {
                Ok(()) => ("303 See Other", "Location: /?view=providers#saved\r\n", String::new()),
                Err(message) => ("400 Bad Request", "", message.to_owned()),
            };
            let _ = control_http::respond(
                stream,
                status,
                "text/plain; charset=utf-8",
                body.as_bytes(),
                extra,
                control_http::Policy::Page,
            );
            return;
        }
        if personas::handle(
            &mut stream,
            state,
            method,
            path,
            &query,
            &data[header_end..header_end + length],
        ) {
            return;
        }
        if lens_library::handle(&mut stream, state, method, path, &query) {
            return;
        }
        if knowledge::handle(
            &mut stream,
            state,
            method,
            path,
            &query,
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
            let f = fields(&String::from_utf8_lossy(&data[header_end..header_end + length]));
            let cancellation = (path == "/cancel-run").then(|| {
                runtime::RunStatusCheckpoint::capture(&state.runtime, |run| {
                    f.get("id").is_some_and(|id| run.id() == id)
                })
            });
            let cancel_chat = if path == "/cancel-run" {
                state
                    .runtime
                    .runs
                    .iter()
                    .find(|run| f.get("id").is_some_and(|id| run.id() == id))
                    .and_then(|run| run.chat_id.as_ref())
                    .and_then(|id| state.chats.iter().position(|chat| &chat.id == id))
                    .map(|index| (index, state.chats[index].revision))
            } else {
                None
            };
            let location = match path {
                // POST /chat and /improve are handled by session_http.
                "/cancel-run" => state.runtime.action(path, &f),
                "/propose" | "/review" | "/request" | "/decide" => state.collective.action(path, &f),
                "/room-claim-release" => {
                    rooms::claims::operator_action(state, &f);
                    "/?view=rooms"
                }
                _ => "/",
            };
            let thread_location = (path == "/room-claim-release")
                .then(|| f.get("channel"))
                .flatten()
                .filter(|room| rooms::valid(room))
                .map(|room| format!("/?view=rooms&channel={room}"));
            let location = thread_location.as_deref().unwrap_or(location);
            if let Some((index, _)) = cancel_chat {
                state.chats[index].revision = state.chats[index].revision.saturating_add(1);
            }
            let persisted = if let Some(checkpoint) = cancellation {
                checkpoint.save(state, storage::save)
            } else {
                storage::save(state)
            };
            if persisted.is_err() {
                if let Some((index, revision)) = cancel_chat {
                    state.chats[index].revision = revision;
                }
                let body = error_page("Could not persist the local change.", location);
                let _ = control_http::respond(
                    stream,
                    "500 Internal Server Error",
                    "text/html; charset=utf-8",
                    body.as_bytes(),
                    "",
                    control_http::Policy::Page,
                );
                return;
            }
            (
                "303 See Other",
                "text/plain",
                String::new(),
                format!("Location: {location}\r\n"),
            )
        } else if let Some(location) = (path == "/").then(|| legacy_session_location(state, &query)).flatten() {
            (
                "303 See Other",
                "text/plain",
                String::new(),
                format!("Location: {location}\r\n"),
            )
        } else if path == "/" {
            let page;
            (page, policy) = public_page(state, &query, lens);
            ("200 OK", "text/html; charset=utf-8", page, String::new())
        } else {
            ("404 Not Found", "text/plain", "Not found".into(), String::new())
        };
        let preference = if method == "GET" && query.contains_key("lens") {
            lens_ui::cookie(lens)
        } else {
            String::new()
        };
        let _ = control_http::respond(
            stream,
            status,
            mime,
            body.as_bytes(),
            &format!("{extra}{preference}"),
            policy,
        );
    });
}

// Commit while locked, then release state before touching the socket. Slow
// readers must never prevent other clients or the supervisor from progressing.
// Routes that change state run here; routes that only read it use `Read`.
fn state_response(stream: &mut impl Write, state: &Store, respond: impl FnOnce(&mut Vec<u8>, &mut State)) {
    let mut response = Vec::new();
    match state.write_seen(|state| respond(&mut response, state)) {
        // The reply may show a change another request let others see before
        // it was durable: it goes out once that change is.
        Ok(((), seen)) => seen.wait(),
        Err(_) => unavailable(&mut response),
    }
    let _ = stream.write_all(&response);
}
fn unavailable(response: &mut Vec<u8>) {
    let body = "State unavailable. Restart Control after checking.";
    let _ = control_http::respond(
        response,
        "503 Service Unavailable",
        "text/plain",
        body.as_bytes(),
        "",
        control_http::Policy::Page,
    );
}
/// A route that only reads state, in two steps: `view` copies what the page
/// shows out of one critical section, and `render` builds the response after
/// the lock is released. `render` gets nothing but that copy: the copy is
/// `'static`, so it cannot borrow `State`, and a `fn` captures nothing that
/// could reach the lock. `view` gets `&State`, so it cannot change state.
pub(crate) struct Read<Q, V: 'static> {
    pub view: fn(&State, Q) -> V,
    pub render: fn(V) -> Reply,
}
/// A response that a read route built without the state lock.
pub(crate) struct Reply {
    pub status: &'static str,
    pub mime: &'static str,
    pub body: String,
    pub extra: String,
    pub policy: control_http::Policy,
}
impl Reply {
    fn write(&self, output: &mut impl Write) -> std::io::Result<()> {
        control_http::respond(
            output,
            self.status,
            self.mime,
            self.body.as_bytes(),
            &self.extra,
            self.policy,
        )
    }
}
impl<Q, V: 'static> Read<Q, V> {
    /// The response for a caller that already holds the state.
    pub(crate) fn reply(&self, state: &State, request: Q) -> Reply {
        (self.render)((self.view)(state, request))
    }
}
// Not inlined, so profiles can tell its wait for the state lock from rendering.
#[inline(never)]
pub(crate) fn read_response<Q, V: 'static>(stream: &mut impl Write, state: &Store, route: &Read<Q, V>, request: Q) {
    // The guard is dropped as soon as the view is copied.
    let view = state.read_seen(|state| (route.view)(state, request));
    let mut response = Vec::new();
    match view {
        Ok((view, seen)) => {
            let _ = (route.render)(view).write(&mut response);
            // Rendering first overlaps the wait for a change the view showed.
            seen.wait();
        }
        Err(_) => unavailable(&mut response),
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
    let _ = control_http::respond(
        stream,
        "200 OK",
        "text/css; charset=utf-8",
        body.as_bytes(),
        "",
        control_http::Policy::Asset,
    );
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
    if args.get(1).is_some_and(|s| s == "accounts") {
        return providers::accounts::command(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "models") {
        return model_calls::command(&args[2..]);
    }
    // `control codex|claude|grok|antigravity login|check [--account NAME]`.
    let backend_command = [
        (
            backend::Backend::Codex,
            control_codex::command as fn(control_agent::Command<'_>) -> std::io::Result<()>,
        ),
        (backend::Backend::Claude, control_claude::command),
        (backend::Backend::Antigravity, control_antigravity::command),
        (backend::Backend::Grok, control_grok::command),
    ]
    .into_iter()
    .find(|(backend, _)| args.get(1).is_some_and(|s| s == backend.id()));
    if let Some((backend, command)) = backend_command {
        return logins::command(backend, &args[2..], command);
    }
    if args.get(1).is_some_and(|s| s == "--version") {
        if args.get(2).is_some_and(|s| s == "--json") {
            println!(
                "{}",
                serde_json::to_string(&control::build_info::current()).map_err(std::io::Error::other)?
            );
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
        // A client of the running server, so it lives with the other client entry points.
        let (tool, arguments) = packages::cli_request(&args[2..])?;
        let result =
            control_client::Hive::connect(&format!("http://{}", config::server_addr()))?.tool(tool, arguments)?;
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--help" || s == "help") {
        println!(
            "control [serve | tui | mcp | discord | doctor | config | open | browser | federation | package]\nserve: local web/MCP server on CONTROL_ADDR (default 127.0.0.1:8080)\ntui: Rust terminal client\nmcp [--root DIRECTORY]: local tools for an external harness\nmcp --chat: drive chats as the local operator from an external harness\ndiscord --enable: listen to this node's configured DMs\ndoctor: offline prerequisites; no model calls\nconfig init | path: initialize or locate personal configuration\nopen: sign in to the local website\nbrowser capture FILE [WIDTH HEIGHT]: offline Nix Firefox capture\nfederation help: signed offline lens exchange\npackage import | revoke | list | show: admit and inspect reusable tool packages\n--check-worker: one authenticated sandbox test (uses model usage)\n--version [--json]: version, platform and optional build identification"
        );
        println!(
            "codex login --account NAME | check: container device login for a named Codex account, or the offline isolation check and each account's sign-in\nclaude login | check [--account NAME]: container login or offline tool isolation check\ngrok login | check [--account NAME]: the same for Grok Build\nantigravity login | check [--account NAME]: the same for the Antigravity CLI\naccounts list | add BACKEND NAME [--kind K] [--cap N] [--label TEXT] [--disabled] | remove BACKEND NAME | default BACKEND NAME | clear BACKEND NAME: a backend's named accounts (accounts.json)\nconfig init [--provider codex|claude|grok|antigravity] [--source-root PATH]: initialize private configuration without replacing saved choices\nconfig path: show the node configuration directory\n--bind-discord LABEL ID: bind a configured principal locally"
        );
        println!("open --link: print a one-use local browser sign-in link");
        println!("github configure: bind improvement PR publication to a repository and private credential reference");
        return Ok(());
    }
    let addr = config::server_addr();
    let socket: std::net::SocketAddr = addr.parse().map_err(|_| {
        std::io::Error::other(format!(
            "CONTROL_ADDR must be a loopback IP:port, such as 127.0.0.1:8080; got {addr:?}"
        ))
    })?;
    if !socket.ip().is_loopback() {
        return Err(std::io::Error::other(
            "Control currently requires a loopback bind address.",
        ));
    }
    if args.get(1).is_some_and(|s| s == "tui") {
        #[cfg(feature = "tui")]
        return control_tui::terminal::run();
        #[cfg(not(feature = "tui"))]
        return Err(std::io::Error::other(
            "This server build excludes the terminal UI; use a Control build with the tui feature.",
        ));
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
                println!("Private node configuration ready in {}", directory.display());
            }
            _ => return Err(std::io::Error::other("Use control config init or control config path")),
        }
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|s| s != "serve" && s != "open" && !s.starts_with("--"))
    {
        return Err(std::io::Error::other("Unknown command. Use control --help."));
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
        return Err(std::io::Error::other("Unknown command. Use control --help."));
    }
    let listener = TcpListener::bind(&addr).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            std::io::Error::other(format!(
                "{addr} is already in use. Stop the other server or set CONTROL_ADDR, e.g. CONTROL_ADDR=127.0.0.1:8081."
            ))
        } else {
            error
        }
    })?;
    storage::initialize()?;
    let _state_lock = storage::lock()?;
    let access = Arc::new(access::Access::load()?);
    let mut loaded = storage::load()?;
    if let Err(error) = model_calls::recover() {
        loaded.runtime.notice = format!("Text-call cleanup needs inspection: {error}");
    }
    // No import runs yet; a staged copy left by the last process is unreferenced.
    if let Err(error) = projects::discard_interrupted_imports() {
        eprintln!("Could not remove interrupted folder imports: {error}");
    }
    let state = Store::new(loaded);
    shutdown::install()?;
    // Blocking accept removes the 50 ms admission poll. Cancellation wakes it
    // through the same loopback listener, without doing socket I/O in a signal handler.
    let wake_address = listener.local_addr()?;
    let accept_cancel = shutdown::token();
    let accept_wake = std::thread::spawn(move || {
        accept_cancel.wait(None);
        let _ = TcpStream::connect_timeout(&wake_address, std::time::Duration::from_secs(1));
    });
    let preview_state = state.clone();
    let preview_restore = std::thread::spawn(move || previews::restore(preview_state));
    let workers = worker::start(state.clone());
    let services = services::supervise(state.clone());
    let ports = ports::supervise(state.clone());
    let scores = scores::supervise(state.clone());
    println!(
        "control · {} · run control open in another terminal to sign in",
        config::server_url()
    );
    let mut listener_error = None;
    let mut requests = http_admission::Requests::new();
    while !shutdown::requested() {
        match listener.accept() {
            Ok((stream, _)) => {
                if shutdown::requested() {
                    break;
                }
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
    let _ = preview_restore.join();
    // Already-admitted HTTP tool calls have their own bounded waits. Let those
    // handlers finish too before the final state save and process exit.
    let requests_result = requests.join();
    // Approved folder imports run after their HTTP response; each stops at its
    // next copied entry and records the interruption beside its request.
    projects::join_imports();
    state
        .read(|state| -> std::io::Result<()> {
            // A stopped node leaves a self-contained `state.json` (empty patch log), so
            // older builds and offline readers need nothing else.
            storage::compact(state)?;
            stopped?;
            if service_failed {
                return Err(std::io::Error::other(
                    "Service supervisor stopped unexpectedly; inspect retained service identities.",
                ));
            }
            if port_failed {
                return Err(std::io::Error::other(
                    "Port supervisor stopped unexpectedly; inspect retained port approvals.",
                ));
            }
            requests_result?;
            match listener_error {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
        .map_err(|_| {
            std::io::Error::other("State unavailable during shutdown; retained attempts require inspection on restart.")
        })?
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

#[cfg(test)]
#[path = "main/tests/page_bytes.rs"]
mod page_bytes;
#[cfg(test)]
#[path = "main/tests/write_release.rs"]
mod write_release;
