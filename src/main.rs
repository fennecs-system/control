mod access;
mod collective;
mod discord;
mod identity;
mod runtime;
mod storage;
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
    discord_seen: Vec<String>,
    runtime: runtime::Runtime,
    collective: collective::Collective,
    messages: Vec<String>,
    agents: Vec<String>,
    note: String,
    saved: bool,
}
impl State {
    fn initial() -> Self {
        Self {
            note: include_str!("../DESIGN.md").to_owned(),
            ..Self::default()
        }
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
    let research = lens == "research";
    let title = if research {
        "Research & signals"
    } else {
        "Systems engineering"
    };
    let mut page = include_str!("../assets/index.html")
        .replace("</nav>", "<a href='/?view=rooms'># <span>Rooms</span></a><a href='/?view=knowledge'>▧ <span>Knowledge inbox</span></a><a href='/?view=authority'>◇ <span>Authority queue</span></a><a href='/?view=personas'>⌘ <span>Personas & runs</span></a></nav>")
        .replace("</head>", "<link rel='stylesheet' href='/collective.css'><link rel='stylesheet' href='/runtime.css'></head>")
        .replace("</nav>", "<a href='/?view=discord-access'>◇ <span>Discord access</span></a></nav>")
        .replace("LIVE CONTEXT", "EXAMPLE CONTEXT")
        .replace("<span class=\"tag\">DEMO</span>", "<span class=\"tag\">CODEX</span>")
        .replace("Messages stay in server memory. Model responses are not connected.", "Messages persist locally. Runs wait for a configured Codex container; refresh to see their status.")
        .replace("＋ Spawn an agent", "＋ Queue an agent")
        .replace("</nav>", "<a href='/?view=evolution'>↻ <span>Evolution</span></a><a href='/?view=tools'>⊞ <span>Tool windows</span></a><a href='/?view=integrations'>⌁ <span>Integrations</span></a></nav>")
        .replace("{{title}}", title)
        .replace(
            "{{lens}}",
            if research { "research" } else { "engineering" },
        );
    let content = state.runtime.render(view,lens).or_else(|| state.collective.render(view, channel)).unwrap_or_else(|| match view {
        "editor" => format!("<div class='section-heading'><div><span class='eyebrow'>KNOWLEDGE / CONTROL</span><h2>Architecture.md</h2></div><span class='tag'>{} · local storage</span></div><form method='post' action='/save'><textarea class='editor' name='note' aria-label='Architecture document'>{}</textarea><button class='primary'>Save document ↗</button></form>", if state.saved { "Saved" } else { "Draft" }, escape(&state.note)),
        "agents" => format!("<div class='section-heading'><div><span class='eyebrow'>EXECUTION / SIMULATED</span><h2>Agent sessions</h2></div></div><p class='muted'>Spawn a local demonstration session. Remote execution is not connected.</p>{}<form method='post' action='/spawn'><label>Session objective<input required name='objective' placeholder='Map the knowledge graph…' maxlength='300'></label><label>Provider<select name='provider'><option>Anthropic</option><option>OpenAI</option><option>Self-hosted</option></select></label><button class='primary'>＋ Spawn demo agent</button></form>", state.agents.iter().enumerate().map(|(i,a)| format!("<div class='session'><span class='dot'></span><div><strong>agent / {:03}</strong><p>{}</p></div><span class='tag'>simulated</span></div>", i+1, escape(a))).collect::<String>()),
        "nodes" => "<span class='eyebrow'>FEDERATION / PLANNED TOPOLOGY</span><h2>A collective, with boundaries.</h2><p class='muted'>Each node owns its knowledge, tools, and execution. Lenses select context across nodes; node policy remains authoritative.</p><div class='node-grid'><article><span class='node-icon'>⌘</span><h3>control.local</h3><p>Primary hub · this prototype</p><span class='tag'>local</span></article><article><span class='node-icon'>◇</span><h3>forge.tailnet</h3><p>Development · proposed node</p><span class='tag'>not connected</span></article><article><span class='node-icon'>◈</span><h3>archive.tailnet</h3><p>Knowledge · proposed node</p><span class='tag'>not connected</span></article></div>".into(),
        "monitor" => "<span class='eyebrow'>OBSERVABILITY / PROVIDER ADAPTERS</span><h2>One view. Any backend.</h2><p class='muted'>Provider configuration and live telemetry are not connected in this prototype.</p><div class='provider'><b>Anthropic</b><span>API adapter</span><span class='tag'>not configured</span></div><div class='provider'><b>OpenAI</b><span>API adapter</span><span class='tag'>not configured</span></div><div class='provider'><b>Self-hosted</b><span>Local endpoint</span><span class='tag'>not configured</span></div><div class='callout'>Planned telemetry: token usage, cost, latency, tool calls, and an audit trail per session and node.</div>".into(),
        _ => format!("<div class='section-heading'><div><span class='eyebrow'>WORKSPACE / ACTIVE LENS</span><h2>{title}<span class='small-arrow'>↗</span></h2></div><span class='tag cyan'>context ready</span></div><p class='intro'>The right knowledge. The right tools. A shared direction.</p><div class='metrics'><div><b>03</b><span>KNOWLEDGE SOURCES</span></div><div><b>{}</b><span>SELECTED TOOLS</span></div><div><b>{:02}</b><span>DEMO AGENTS</span></div></div><div class='section-heading compact'><h3>Context constellation</h3><span class='muted'>01 / LENS MAP</span></div><div class='constellation'><div class='orbit orbit-one'></div><div class='orbit orbit-two'></div><span class='map-node n1'>▤ architecture.md</span><span class='map-node n2'>◇ {}</span><div class='core'><span>⌘</span> {title}<small>ACTIVE LENS</small></div><span class='map-node n3'>⌁ {}</span><span class='map-node n4'>▤ decisions/</span><span class='map-caption'>KNOWLEDGE + TOOLS + INSTRUCTIONS</span></div><div class='section-heading compact'><h3>Lens library</h3><span class='muted'>COMPOSE YOUR CONTEXT</span></div><div class='lens-grid'><a class='lens-card {}' href='/?lens=engineering'><span class='card-symbol'>⌘</span><span class='tag'>ENGINEERING</span><h3>Systems engineering</h3><p>Architecture, repositories, and the tools to build.</p><footer>3 sources · 4 tools <span>↗</span></footer></a><a class='lens-card {}' href='/?lens=research'><span class='card-symbol'>◈</span><span class='tag'>RESEARCH</span><h3>Research & signals</h3><p>Connect discoveries. Turn information into direction.</p><footer>3 sources · 2 tools <span>↗</span></footer></a></div>", control_context::lens(if research {"research"} else {"engineering"}, &control_context::TOOLS).len(), state.runtime.runs.len(), if research {"search.read"} else {"repo.read"}, if research {"knowledge.propose"} else {"repo.patch"}, if research {""} else {"selected"}, if research {"selected"} else {""}),
    });
    page = page.replace("{{content}}", &content);
    page = page.replace("{{messages}}", &state.messages.iter().enumerate().map(|(i,s)| {
        let source = format!("website-chat:{i}");
        let response = state.runtime.runs.iter().find(|r|r.source == source).map(|r|format!("{}\n{}",r.status,r.result)).unwrap_or_else(||"Recorded before container routing was enabled.".into());
        format!("<div class='user-message'>{}</div><div class='assistant-message'><span class='eyebrow'>CONTROL / CODEX</span><p class='preserve'>{}</p><a href='/?view=agents'>Open run ledger ↗</a></div>",escape(s),escape(&response))
    }).collect::<String>());
    let category = if research { "research" } else { "engineering" };
    let tools = control_context::lens(category, &control_context::TOOLS)
        .iter()
        .map(|tool| {
            format!(
                "<li>{} <span>{}</span></li>",
                tool.name,
                if tool.available {
                    "local"
                } else {
                    "unconnected"
                }
            )
        })
        .collect::<String>();
    page.replace("{{tools}}", &tools)
        .replace("DEMO AGENTS", "RECORDED RUNS")
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
    if !local_host || (method == "POST" && origin.is_some_and(|v| v != format!("http://{host}"))) {
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
    if length > 32768 {
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
    let lens = query
        .get("lens")
        .map(String::as_str)
        .unwrap_or("engineering");
    let mut state = state.lock().unwrap();
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
            "/save" => {
                if let Some(note) = f.get("note") {
                    state.note = note.clone();
                    state.saved = true;
                }
                "/?view=editor"
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
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}Connection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n\r\n{body}", body.len());
    let _ = stream.write_all(response.as_bytes());
}
fn main() -> std::io::Result<()> {
    let addr = std::env::var("CONTROL_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let socket: std::net::SocketAddr = addr.parse().map_err(std::io::Error::other)?;
    if !socket.ip().is_loopback() {
        return Err(std::io::Error::other(
            "Control currently requires a loopback bind address.",
        ));
    }
    let args: Vec<String> = std::env::args().collect();
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
