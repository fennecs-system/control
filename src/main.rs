mod access;
mod artifacts;
#[path = "../tools/browser/main.rs"]
mod browser;
mod browser_ui;
mod chat;
mod collective;
use control::config;
mod discord;
mod discord_bridge;
mod discord_permissions;
mod doctor;
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
mod tool_catalog;
mod toolbox;
mod tools_http;
mod usage;
mod web_assets;
mod worker;
mod worker_tools;
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
        "tools" => tool_catalog::render(state, if channel == "general" { "" } else { channel }),
        "session" => chat::render(state,channel).unwrap_or_else(escape),
        "session-list" | "artifact-page" | "browser" | "tool-page" => channel.to_owned(),
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
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(3)));
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
    if tools_http::handle(
        &mut stream,
        method,
        target,
        &header,
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
    if method == "GET" && serve_stylesheet(&mut stream, path) {
        return;
    }
    state_response(&mut stream, &state, |mut stream, state| {
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
            let _ = write!(stream,"HTTP/1.1 {status}\r\n{extra}Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",body.len());
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
            if storage::save(state).is_err() {
                let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\nContent-Length: 35\r\n\r\nCould not persist the local change.");
                return;
            }
            (
                "303 See Other",
                "text/plain",
                String::new(),
                format!("Location: {location}\r\n"),
            )
        } else if path == "/" {
            (
                "200 OK",
                "text/html; charset=utf-8",
                render(
                    state,
                    query.get("view").map(String::as_str).unwrap_or("hub"),
                    lens,
                    query
                        .get(if query.get("view").is_some_and(|v| v == "tools") {
                            "q"
                        } else {
                            "channel"
                        })
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
            let _ = write!(response, "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}", body.len());
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
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/css; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}", body.len());
    true
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--version") {
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
        println!("control [serve | tui | mcp | discord | doctor | config | open | browser | federation]\nserve: local web/MCP server\ntui: Rust terminal client\nmcp [--root DIRECTORY]: local tools for an external harness\ndiscord --enable: listen to this node's configured DMs\ndoctor: offline prerequisites; no model calls\nconfig init | path: initialize or locate personal configuration\nopen: sign in to the local website\nbrowser capture FILE [WIDTH HEIGHT]: offline Nix Firefox capture\nfederation help: signed offline lens exchange\n--check-worker: one authenticated sandbox test (uses model usage)\n--version: version and platform");
        println!("config init: create private node configuration\nconfig path: show the node configuration directory\n--bind-discord LABEL ID: bind a configured principal locally");
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "config") {
        let directory = config::directory().map_err(std::io::Error::other)?;
        match args.get(2).map(String::as_str) {
            Some("path") => println!("{}", directory.display()),
            Some("init") => {
                config::initialize(include_bytes!("../discord-access.json"))?;
                storage::initialize()?;
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
        return access::open(&addr);
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
    let listener = TcpListener::bind(&addr)?;
    storage::initialize()?;
    let _state_lock = storage::lock()?;
    let access = Arc::new(access::Access::load()?);
    let state = Arc::new(Mutex::new(storage::load()?));
    worker::start(state.clone());
    println!("control · http://{addr}");
    for stream in listener.incoming().flatten() {
        let state = state.clone();
        let access = access.clone();
        std::thread::spawn(move || handle(stream, state, access));
    }
    Ok(())
}

#[cfg(test)]
mod responsiveness_tests {
    use super::*;

    #[test]
    fn socket_writes_release_state_even_when_the_client_stalls_or_disconnects() {
        struct DisconnectedClient<'a>(&'a Mutex<State>);
        impl Write for DisconnectedClient<'_> {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                let state = self.0.try_lock().expect("socket write held shared state");
                assert_eq!(state.runtime.notice, "committed");
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let state = Mutex::new(State::default());
        state_response(
            &mut DisconnectedClient(&state),
            &state,
            |response, state| {
                state.runtime.notice = "committed".into();
                session_http::handle(response, state, "GET", "/chats", "none", "", b"");
            },
        );
        assert_eq!(state.lock().unwrap().runtime.notice, "committed");
    }

    #[test]
    fn poisoned_state_returns_503_without_rendering_or_mutating() {
        let state = Mutex::new(State::default());
        let _ = std::panic::catch_unwind(|| {
            let _guard = state.lock().unwrap();
            panic!("simulate interrupted state mutation");
        });
        let mut response = Vec::new();
        state_response(&mut response, &state, |_, _| panic!("poisoned state used"));
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        let (header, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(header.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(state.is_poisoned());
        // Static UI dependencies remain available without touching mutable state.
        let mut stylesheet = Vec::new();
        assert!(serve_stylesheet(&mut stylesheet, "/style.css"));
        assert!(stylesheet.starts_with(b"HTTP/1.1 200 OK\r\n"));
    }
}
