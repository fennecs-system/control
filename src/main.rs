mod access;
#[path = "../tools/browser/main.rs"]
mod browser;
mod chat;
mod collective;
mod discord;
mod federation;
mod harness;
mod identity;
mod knowledge;
mod lens_library;
mod lens_ui;
mod mcp_stdio;
mod navigation;
mod providers;
mod rooms;
mod runtime;
mod session_http;
mod storage;
mod usage;
mod web_assets;
mod worker;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct State {
    chats: Vec<chat::Session>,
    discord_seen: Vec<String>,
    runtime: runtime::Runtime,
    collective: collective::Collective,
    documents: Vec<knowledge::Document>,
    lens_library: control_context::Library,
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
    let view = if view == "manage" { "nodes" } else { view };
    let lens = lens_ui::normalize(lens);
    let chat_navigation = matches!(view, "session" | "session-list");
    let title = lens_ui::title(lens);
    let mut page = include_str!("../assets/index.html")
        .replace("</head>", "<link rel='stylesheet' href='/collective.css'><link rel='stylesheet' href='/runtime.css'><link rel='stylesheet' href='/workspace.css'><link rel='stylesheet' href='/navigation.css'></head>")
        .replace("{{title}}", title)
        .replace("{{page_discussion}}", &chat::discussion_link(view))
        .replace("{{navigation}}", &navigation::primary(view))
        .replace("{{section_navigation}}", &navigation::secondary(view))
        .replace("{{layout_class}}", if chat_navigation { "chat-workspace" } else if navigation::managed(view) { "with-section-nav" } else { "" })
        .replace("{{page_title}}", &navigation::heading(view))
        .replace(
            "{{lens}}",
            lens,
        );
    let content = (if view == "monitor" { Some(usage::render(&state.runtime)) } else { state.runtime.render(view,lens) }).or_else(|| state.collective.render(view, channel)).unwrap_or_else(|| match view {
        "providers" => providers::render(&state.runtime),
        "session" => chat::render(state,channel).unwrap_or_else(escape),
        "session-list" => channel.to_owned(),
        "editor" | "knowledge" => knowledge::render(state,if view=="editor"{"scratchpad"}else{""},None),
        "knowledge-page" => channel.to_owned(),
        "rooms" => rooms::render(state,channel),
        "lenses" => lens_library::render(state,if lens=="none"{"engineering"}else{lens},None,None,false),
        "lens-page" => channel.to_owned(),
        "agents" => format!("<div class='section-heading'><div><span class='eyebrow'>EXECUTION / SIMULATED</span><h2>Agent sessions</h2></div></div><p class='muted'>Spawn a local demonstration session. Remote execution is not connected.</p>{}<form method='post' action='/spawn'><label>Session objective<input required name='objective' placeholder='Map the knowledge graph…' maxlength='300'></label><label>Provider<select name='provider'><option>Anthropic</option><option>OpenAI</option><option>Self-hosted</option></select></label><button class='primary'>＋ Spawn demo agent</button></form>", state.agents.iter().enumerate().map(|(i,a)| format!("<div class='session'><span class='dot'></span><div><strong>agent / {:03}</strong><p>{}</p></div><span class='tag'>simulated</span></div>", i+1, escape(a))).collect::<String>()),
        "nodes" => "<span class='eyebrow'>NODES · PLANNED</span><h2>Nodes</h2><p class='muted'>Remote nodes are not connected.</p><div class='node-grid'><article><span class='node-icon'>⌘</span><h3>control.local</h3><p>This laptop</p><span class='tag'>local</span></article><article><span class='node-icon'>◇</span><h3>forge.tailnet</h3><p>Development · planned</p><span class='tag'>not connected</span></article><article><span class='node-icon'>◈</span><h3>archive.tailnet</h3><p>Knowledge · planned</p><span class='tag'>not connected</span></article></div>".into(),
        "monitor" => "<span class='eyebrow'>PROVIDERS</span><h2>Providers</h2><p class='muted'>Additional providers are not connected.</p><div class='provider'><b>Anthropic</b><span>API adapter</span><span class='tag'>not configured</span></div><div class='provider'><b>OpenAI</b><span>API adapter</span><span class='tag'>not configured</span></div><div class='provider'><b>Self-hosted</b><span>Local endpoint</span><span class='tag'>not configured</span></div>".into(),
        _ => lens_ui::overview(state,lens),
    });
    page = page.replace("{{content}}", &content);
    let category = lens;
    let tools = control_context::lens(category, &control_context::TOOLS)
        .iter()
        .map(|tool| {
            format!(
                "<li>{} <span>{}</span></li>",
                tool.name,
                if tool.available {
                    "local"
                } else {
                    "not connected"
                }
            )
        })
        .collect::<String>();
    page.replace("{{tools}}", &tools)
        .replace("DEMO AGENTS", "RUNS")
        .replace("3 sources · 2 tools", "3 sources · 3 tools")
}
fn handle(mut stream: TcpStream, state: Arc<Mutex<State>>, access: Arc<access::Access>) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let mut data = Vec::new();
    let mut buf = [0; 4096];
    let header_end;
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
        if data.len() > 65536 {
            return;
        }
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = i + 4;
            break;
        }
    }
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
        let _ = stream.write_all(
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\nConnection: close\r\n\r\nForbidden",
        );
        return;
    }
    if access.gate(&mut stream, method, target, &header) {
        return;
    }
    let length = header
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let limit = if target
        .split('?')
        .next()
        .is_some_and(|p| ["/knowledge/save", "/save", "/lenses/save"].contains(&p))
    {
        524288
    } else {
        32768
    };
    if length > limit {
        let _=stream.write_all(b"HTTP/1.1 413 Content Too Large\r\nContent-Length: 17\r\nConnection: close\r\n\r\nRequest too large");
        return;
    }
    while data.len() < header_end + length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method == "GET" && path.starts_with("/tool-window/") {
        web_assets::serve(&mut stream, path);
        return;
    }
    let query = fields(query);
    let lens = lens_ui::selected(&header, query.get("lens").map(String::as_str));
    if method == "POST" && path == "/lens" {
        let f = fields(&String::from_utf8_lossy(
            &data[header_end..header_end + length],
        ));
        let selected = lens_ui::normalize(f.get("lens").map(String::as_str).unwrap_or("none"));
        let location = lens_ui::return_to(f.get("next").map(String::as_str).unwrap_or("/"));
        let _ = write!(stream,"HTTP/1.1 303 See Other\r\nLocation: {location}\r\n{}Content-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",lens_ui::cookie(selected));
        return;
    }
    let mut state = state.lock().unwrap();
    if method == "POST" && path == "/providers" {
        let f = fields(&String::from_utf8_lossy(
            &data[header_end..header_end + length],
        ));
        let result = providers::save_with(&mut state, &f, storage::save);
        let (status, extra, body) = match result {
            Ok(()) => (
                "303 See Other",
                "Location: /?view=providers#saved\r\n",
                String::new(),
            ),
            Err(message) => ("400 Bad Request", "", message.to_owned()),
        };
        let _ = write!(stream,"HTTP/1.1 {status}\r\n{extra}Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",body.len());
        return;
    }
    if lens_library::handle(
        &mut stream,
        &mut state,
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
        &mut state,
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
        &mut state,
        method,
        target,
        lens,
        &header,
        &data[header_end..header_end + length],
    ) {
        return;
    }
    let (status, mime, body, extra) = if method == "POST" {
        let f = fields(&String::from_utf8_lossy(
            &data[header_end..header_end + length],
        ));
        let location = match path {
            "/improve" | "/task" | "/retry-run" => state.runtime.action(path, &f),
            "/room" | "/propose" | "/review" | "/request" | "/decide" => {
                state.collective.action(path, &f)
            }
            "/chat" => {
                if let Some(msg) = f.get("message").filter(|s| !s.trim().is_empty()) {
                    let message: String = msg.chars().take(2000).collect();
                    let source = format!("website-chat:{}", state.messages.len());
                    state.runtime.enqueue(message.clone(), "chat", &source);
                    state.messages.push(message);
                }
                if f.get("lens").is_some_and(|v| v == "research") {
                    "/?lens=research"
                } else {
                    "/"
                }
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
        if storage::save(&state).is_err() {
            let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\nContent-Length: 35\r\n\r\nCould not persist the local change.");
            return;
        }
        (
            "303 See Other",
            "text/plain",
            String::new(),
            format!("Location: {location}\r\n"),
        )
    } else if path == "/navigation.css" {
        (
            "200 OK",
            "text/css; charset=utf-8",
            include_str!("../assets/navigation.css").to_owned(),
            String::new(),
        )
    } else if path == "/workspace.css" {
        (
            "200 OK",
            "text/css",
            include_str!("../assets/workspace.css").to_owned(),
            String::new(),
        )
    } else if path == "/runtime.css" {
        (
            "200 OK",
            "text/css",
            include_str!("../assets/runtime.css").to_owned(),
            String::new(),
        )
    } else if path == "/collective.css" {
        (
            "200 OK",
            "text/css",
            include_str!("../assets/collective.css").to_owned(),
            String::new(),
        )
    } else if path == "/style.css" {
        (
            "200 OK",
            "text/css",
            include_str!("../assets/style.css").to_owned(),
            String::new(),
        )
    } else if path == "/" {
        (
            "200 OK",
            "text/html; charset=utf-8",
            render(
                &state,
                query.get("view").map(String::as_str).unwrap_or("hub"),
                lens,
                query
                    .get("channel")
                    .map(String::as_str)
                    .unwrap_or("general"),
            ),
            String::new(),
        )
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
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}{preference}Connection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n\r\n{body}", body.len());
    let _ = stream.write_all(response.as_bytes());
}
fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "federation") {
        return federation::run(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "mcp") {
        return mcp_stdio::run(&args[2..]);
    }
    if args.get(1).is_some_and(|s| s == "browser") {
        return mcp_stdio::browser_cli(&args[2..]);
    }
    let addr = std::env::var("CONTROL_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let socket: std::net::SocketAddr = addr.parse().map_err(std::io::Error::other)?;
    if !socket.ip().is_loopback() {
        return Err(std::io::Error::other(
            "Control currently requires a loopback bind address.",
        ));
    }
    if args.get(1).is_some_and(|s| s == "tui") {
        return harness::run();
    }
    if args.get(1).is_some_and(|s| s == "--help" || s == "help") {
        println!("control [serve | tui | mcp | browser | federation | open | --check-worker]\nserve (default): local web/MCP server\ntui: Rust terminal client for the local server\nmcp --root DIRECTORY: local read-only Hive tools and offline browser capture\nbrowser capture PREVIEW.html [WIDTH HEIGHT]: Nix Firefox screenshot\nfederation: signed offline lens exchange (use federation help)\nopen: sign in to the local website\n--check-worker: exercise one authenticated container task\n\nWorker execution requires the configured Linux container engine and pinned Nix worker image.");
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
        return access::open(&addr);
    }
    if args
        .get(1)
        .is_some_and(|s| s == "--bind-discord" || s == "--discord-channel")
    {
        let result = (|| -> Result<(), &'static str> {
            let mut policy = identity::Policy::load()?;
            if args[1] == "--bind-discord" {
                policy.bind(
                    args.get(2).ok_or("Missing principal label")?,
                    args.get(3).ok_or("Missing Discord ID")?.parse()?,
                )?;
            } else {
                policy.channel_id = Some(args.get(2).ok_or("Missing channel ID")?.parse()?);
            }
            policy.save()
        })();
        return result.map_err(std::io::Error::other);
    }
    let listener = TcpListener::bind(&addr)?;
    println!("CONTROL · local UI prototype · http://{addr}");
    storage::initialize()?;
    let access = Arc::new(access::Access::load()?);
    let state = Arc::new(Mutex::new(storage::load()?));
    worker::start(state.clone());
    for stream in listener.incoming().flatten() {
        let state = state.clone();
        let access = access.clone();
        std::thread::spawn(move || handle(stream, state, access));
    }
    Ok(())
}
