//! A thin Rust terminal client. Sessions, policy, tools and execution belong to Control.
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
    Terminal,
};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue, ACCEPT, COOKIE},
    Url,
};
use serde_json::{json, Value};
use std::{
    fs, io,
    time::{Duration, Instant},
};

const VERSION: &str = "2025-11-25";
const MUTATIONS: [&str; 4] = ["control.chat.create", "control.chat.send", "control.room.post", "control.chat.lenses"];
const CHAT_COMMANDS: [&str; 7] = ["/improve", "/improve off", "/steer", "/btw", "/spawn", "/cancel", "/answer"];

fn command_line(text: &str) -> Line<'static> {
    let prefix = CHAT_COMMANDS.iter().rev().find(|command| text.strip_prefix(**command).is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace)));
    if let Some(command) = prefix {
        Line::from(vec![Span::styled((*command).to_owned(), Style::default().fg(Color::Rgb(181, 160, 255))), Span::raw(text[command.len()..].to_owned())])
    } else {
        Line::from(text.to_owned())
    }
}
pub(crate) struct Hive {
    client: Client,
    endpoint: String,
    next: u64,
    #[cfg(test)]
    operator_cookie: HeaderValue,
}
fn failure(message: &str) -> io::Error {
    io::Error::other(message)
}
fn local_server(value: &str) -> io::Result<Url> {
    let url = Url::parse(value).map_err(|_| failure("Invalid server URL."))?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(failure(
            "The local harness requires a loopback HTTP origin without URL credentials.",
        ));
    }
    Ok(url)
}
impl Hive {
    pub(crate) fn connect(server: &str) -> io::Result<Self> {
        let url = local_server(server)?;
        let path = std::env::var_os("CONTROL_OPERATOR_FILE")
            .map(std::path::PathBuf::from)
            .map(Ok)
            .unwrap_or_else(|| crate::config::state("operator-session"))
            .map_err(failure)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| {
            failure("Start the server first: ./target/release/control serve, then ./target/release/control open.")
        })?;
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(failure(
                "Operator credential must be a private regular file.",
            ));
        }
        // Only this application consumes the local operator credential. It is never printed or placed in a URL.
        let token =
            fs::read_to_string(path).map_err(|_| failure("Cannot load operator credential."))?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("Invalid operator credential."));
        }
        let mut cookie = HeaderValue::from_str(&format!("control_operator={token}"))
            .map_err(|_| failure("Invalid operator credential."))?;
        cookie.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, cookie.clone());
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert("mcp-protocol-version", HeaderValue::from_static(VERSION));
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| failure("Cannot create the local MCP client."))?;
        let mut hive = Self {
            client,
            endpoint: url.join("mcp").unwrap().to_string(),
            next: 0,
            #[cfg(test)]
            operator_cookie: cookie,
        };
        let init = hive.rpc("initialize", json!({"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"control-harness","version":env!("CARGO_PKG_VERSION")}}))?;
        if init["protocolVersion"] != VERSION {
            return Err(failure("Server negotiated an unsupported MCP version."));
        }
        let response = hive
            .client
            .post(&hive.endpoint)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .map_err(|_| failure("Could not initialize the MCP session."))?;
        if response.status().as_u16() != 202 {
            return Err(failure("Server rejected MCP initialization."));
        }
        Ok(hive)
    }
    pub(crate) fn rpc(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.next += 1;
        let response = self
            .client
            .post(&self.endpoint)
            .timeout(
                if method == "tools/call"
                    && matches!(
                        params["name"].as_str(),
                        Some("control.web.search" | "control.rust.run")
                    )
                {
                    Duration::from_secs(25)
                } else if method == "tools/call" && MUTATIONS.contains(&params["name"].as_str().unwrap_or("")) {
                    // A slow checkpoint must not look like a failure after the server committed it.
                    Duration::from_secs(30)
                } else {
                    Duration::from_secs(3)
                },
            )
            .json(&json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params}))
            .send()
            .map_err(|_| failure("Control is unavailable. Reconnecting on the next refresh."))?;
        if !response.status().is_success() {
            return Err(failure(
                "Control rejected the request. Check local operator access.",
            ));
        }
        let data: Value = response
            .json()
            .map_err(|_| failure("Invalid MCP response."))?;
        if data["id"] != self.next || data.get("error").is_some() {
            return Err(failure("MCP request failed."));
        }
        Ok(data["result"].clone())
    }
    fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value> {
        let result = self.rpc("tools/call", json!({"name":name,"arguments":arguments}))?;
        if result["isError"] == true {
            return Err(failure(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap_or("Tool request failed."),
            ));
        }
        Ok(result["structuredContent"]["data"].clone())
    }
}

#[derive(Default)]
struct App {
    menu: Option<Menu>,
    inspector: Option<(String, String)>,
    follow: bool,
    chats: Vec<Value>,
    active: String,
    detail: Value,
    input: String,
    status: String,
    scroll: u16,
}
struct Menu {
    entries: Vec<(String, String, String)>,
    selected: usize,
    multiple: bool,
    checked: Vec<String>,
}
impl Menu {
    fn command(&self) -> Option<String> {
        if self.multiple {
            Some(format!(
                "/use {}",
                if self.checked.is_empty() {
                    "none".into()
                } else {
                    self.checked.join(" ")
                }
            ))
        } else {
            self.entries
                .get(self.selected)
                .map(|(_, _, id)| format!("/resume {id}"))
        }
    }
    fn toggle(&mut self) {
        if let Some((_, _, id)) = self.entries.get(self.selected) {
            if self.checked.contains(id) {
                self.checked.retain(|s| s != id);
            } else {
                self.checked.push(id.clone());
            }
        }
    }
}
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
fn lens_label(detail: &Value) -> String {
    let lenses = detail["lenses"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| detail["lens"].as_str().into_iter().collect());
    if lenses.is_empty() {
        "No lenses".into()
    } else {
        clean(&lenses.join(" + "))
    }
}
impl App {
    fn refresh(&mut self, hive: &mut Hive) -> io::Result<()> {
        self.chats = hive
            .tool("control.chats.list", json!({}))?
            .as_array()
            .cloned()
            .unwrap_or_default();
        self.chats.reverse();
        if self.active.is_empty() {
            self.active = self
                .chats
                .first()
                .and_then(|s| s["id"].as_str())
                .unwrap_or("")
                .into();
        }
        let revision = self
            .chats
            .iter()
            .find(|s| s["id"] == self.active)
            .map(|s| s["revision"].clone());
        if !self.active.is_empty()
            && (self.detail["id"] != self.active
                || revision.as_ref() != Some(&self.detail["revision"]))
        {
            self.detail = hive.tool("control.chat.read", json!({"id":self.active}))?;
        }
        Ok(())
    }
    fn new_chat(&mut self, hive: &mut Hive) -> io::Result<()> {
        self.menu = None;
        self.inspector = None;
        let value = hive.tool("control.chat.create", json!({}))?;
        self.active = value["id"].as_str().unwrap_or("").into();
        self.detail = value;
        self.scroll = 0;
        self.follow = true;
        self.refresh(hive)
    }
    fn submit(&mut self, hive: &mut Hive) -> io::Result<bool> {
        let text = self.input.trim().to_owned();
        self.menu = None;
        match text.as_str() {
            "/quit" => return Ok(true),
            "/new" => self.new_chat(hive)?,
            "/chats" => {
                self.refresh(hive)?;
                self.menu=Some(Menu {entries:self.chats.iter().map(|c| (c["title"].as_str().unwrap_or("Chat").into(),c["lenses"].as_array().map(|a|a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + ")).unwrap_or_default(),c["id"].as_str().unwrap_or("").into())).collect(),selected:0,multiple:false,checked:Vec::new()});
                let lines=self.chats.iter().map(|c|format!("{}\n  /resume {}",c["title"].as_str().unwrap_or("Chat"),c["id"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n\n");
                self.inspector=Some(("Conversations".into(),lines));self.scroll=0;self.follow=false;self.status="↑ ↓ choose · Enter open · Esc back".into();
            }
            "/help" => self.status = "/chats · /lenses · /use ID ID · /use none · /context · /chat · /new · /resume ID · /improve request · /improve off · /answer QUESTION_ID choice · /steer direction · /btw question · /spawn task · /cancel · Ctrl+Q quit".into(),
            "/lenses"=>{
                let definitions=hive.tool("control.lenses.list",json!({}))?;
                let entries=definitions.as_array().ok_or_else(||failure("Invalid lens library response."))?.iter().map(|d| (d["name"].as_str().unwrap_or("Lens").into(), d["includes"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + ")).unwrap_or_default(), d["id"].as_str().unwrap_or("").into())).collect();
                let checked=self.detail["lenses"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
                self.menu=Some(Menu{entries,selected:0,multiple:true,checked});
                let mut lines=vec!["Compose with /use engineering evidence; clear with /use none. Return with /chat.\n".to_owned()];
                for d in definitions.as_array().ok_or_else(||failure("Invalid lens library response."))? {
                    lines.push(format!("{} · {} · v{}",d["id"].as_str().unwrap_or("?"),d["name"].as_str().unwrap_or("?"),d["revision"].as_u64().unwrap_or(0)));
                    if let Some(includes)=d["includes"].as_array().filter(|a|!a.is_empty()) {lines.push(format!("  includes {}",includes.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + ")));}
                }
                self.inspector=Some(("Lens library".into(),lines.join("\n")));self.scroll=0;self.follow=false;self.status="↑ ↓ choose · Space toggle · Enter apply · Esc back".into();
            }
            "/context"=>{
                let lenses=self.detail.get("lenses").cloned().unwrap_or_else(||json!([]));
                let resolved=hive.tool("control.lenses.resolve",json!({"lenses":lenses}))?;
                self.inspector=Some(("Next-message context preview".into(),serde_json::to_string_pretty(&resolved).map_err(|_|failure("Could not display context."))?));self.scroll=0;self.follow=false;self.status="Live library preview; existing runs retain their captured snapshots. /chat to return.".into();
            }
            "/chat"=>{self.inspector=None;self.follow=true;self.scroll=u16::MAX;}
            _ if text.starts_with("/use ")=>{
                let names=text[5..].split(|c:char|c.is_whitespace()||c==',').filter(|s|!s.is_empty()).collect::<Vec<_>>();
                let lenses=if names==["none"]{Vec::new()}else{names};
                // Resolve before creating a conversation: invalid input has no side effects.
                hive.tool("control.lenses.resolve",json!({"lenses":lenses}))?;
                if self.active.is_empty(){self.new_chat(hive)?;}
                self.detail=hive.tool("control.chat.lenses",json!({"id":self.active,"lenses":lenses}))?;
                self.inspector=None;self.status=format!("Future messages: {}",lens_label(&self.detail));self.follow=true;
            }
            _ if text.starts_with("/resume ") => { self.active = text[8..].trim().into(); self.inspector=None;self.refresh(hive)?; }
            _ if text.starts_with('/') && !CHAT_COMMANDS.iter().any(|command| text.strip_prefix(command).is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace)))=>return Err(failure("Unknown command. Use /help.")),
            _ if !text.is_empty() => {
                if text.chars().count() > 2000 { return Err(failure("Messages are limited to 2000 characters.")); }
                if self.active.is_empty() { self.new_chat(hive)?; }
                self.detail = hive.tool("control.chat.send",json!({"id":self.active,"message":text}))?;
                self.inspector=None;
                self.scroll = u16::MAX;
                self.follow = true;
                self.status = if text.starts_with("/steer ") { "Queued for next turn" } else if self.detail["mode"] == "improve" { "Improve · sent" } else { "Sent" }.into();
            }
            _ => {}
        }
        self.input.clear();
        Ok(false)
    }
    fn transcript(&self) -> Text<'static> {
        if let Some(menu) = &self.menu {
            let mut lines = vec![Line::from("")];
            for (i, (label, detail, id)) in menu.entries.iter().enumerate() {
                let active = i == menu.selected;
                let style = Style::default()
                    .fg(if active {
                        Color::Rgb(233, 232, 237)
                    } else {
                        Color::Rgb(133, 133, 143)
                    })
                    .bg(if active {
                        Color::Rgb(36, 32, 47)
                    } else {
                        Color::Rgb(12, 13, 16)
                    });
                let marker = if menu.multiple {
                    if menu.checked.contains(id) {
                        "[✓]"
                    } else {
                        "[ ]"
                    }
                } else {
                    ""
                };
                lines.push(Line::styled(
                    format!(
                        " {} {marker}  {}",
                        if active { "›" } else { " " },
                        clean(label)
                    ),
                    style,
                ));
                if active && !detail.is_empty() {
                    lines.push(Line::styled(
                        format!("      {}", clean(detail)),
                        Style::default().fg(Color::Rgb(133, 133, 143)),
                    ));
                }
                if active {
                    lines.push(Line::from(""));
                }
            }
            if menu.entries.is_empty() {
                lines.push(Line::from("No conversations yet. /new to start."));
            }
            return Text::from(lines);
        }
        if let Some((_, body)) = &self.inspector {
            let body = clean(body);
            return Text::from(
                body.lines()
                    .map(|line| {
                        let color = if line.starts_with("  ") {
                            Color::Rgb(133, 133, 143)
                        } else {
                            Color::Rgb(181, 160, 255)
                        };
                        Line::styled(line.to_owned(), Style::default().fg(color))
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let mut lines = Vec::new();
        if let Some(messages) = self.detail["messages"].as_array() {
            for message in messages {
                lines.push(Line::styled(
                    "You",
                    Style::default().fg(Color::Rgb(181, 160, 255)),
                ));
                lines.extend(
                    clean(message["text"].as_str().unwrap_or(""))
                        .lines()
                        .map(command_line),
                );
                lines.push(Line::styled(
                    format!(
                        "Control · {}",
                        if message["status"] == "queued" && message["kind"] == "steering" { "queued · next turn".into() } else { clean(message["status"].as_str().unwrap_or("")) }
                    ),
                    Style::default().fg(Color::Rgb(140, 218, 203)),
                ));
                lines.extend(
                    clean(message["reply"].as_str().unwrap_or(""))
                        .lines()
                        .map(|s| Line::from(s.to_owned())),
                );
                if let Some(attempts) = message["attempts"].as_array() {
                    for (index, attempt) in attempts.iter().enumerate() {
                        if let Some(count) = attempt["omitted_history"].as_u64().filter(|n| *n > 0)
                        {
                            lines.push(Line::styled(format!("Context window · {count} earlier exchanges excluded from attempt {}. History retained.", index + 1), Style::default().fg(Color::Rgb(133, 133, 143))));
                        }
                        if let Some(events) = attempt["activity"].as_array() {
                            for event in events {
                                let patch=event["kind"] == "file_change";
                                if patch && event["output"].as_str().unwrap_or("").is_empty() {continue;}
                                lines.push(Line::styled(
                                    format!(
                                        "  [{} · attempt {}] {}",
                                        clean(event["status"].as_str().unwrap_or("")),
                                        index + 1,
                                        clean(event["text"].as_str().unwrap_or(""))
                                    ),
                                    Style::default().fg(Color::Rgb(133, 133, 143)),
                                ));
                                lines.extend(
                                    clean(event["output"].as_str().unwrap_or(""))
                                        .lines()
                                        .map(|s| {
                                            let color=if patch {match control::diff::kind(s) {
                                                control::diff::Kind::Add=>{let (r,g,b)=control::diff::ADD_RGB;Some(Color::Rgb(r,g,b))},
                                                control::diff::Kind::Remove=>{let (r,g,b)=control::diff::REMOVE_RGB;Some(Color::Rgb(r,g,b))},
                                                control::diff::Kind::Header=>Some(Color::Rgb(133,133,143)),
                                                control::diff::Kind::Context=>None,
                                            }}else{None};
                                            let style=color.map(|color|Style::default().fg(color)).unwrap_or_default();
                                            Line::styled(format!("  {s}"),style)
                                        }),
                                );
                            }
                        }
                    }
                }
                lines.push(Line::from(""));
            }
        }
        Text::from(lines)
    }
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show, LeaveAlternateScreen);
    }
}
pub fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|s| s == "--help") {
        println!("control tui [--server {}] [--session ID] [--check]\nAlso available as control-harness. Rust TUI for the local Control server (CONTROL_ADDR sets the default).\n\n/lenses · /use ID ID · /use none · /context · /chat · /help", crate::config::server_url());
        return Ok(());
    }
    let option = |name: &str| {
        args.iter()
            .position(|s| s == name)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    let default = crate::config::server_url();
    let mut hive = Hive::connect(option("--server").unwrap_or(&default))?;
    let mut app = App {
        follow: true,
        active: option("--session").unwrap_or("").into(),
        ..App::default()
    };
    app.refresh(&mut hive)?;
    if args.iter().any(|s| s == "--check") {
        println!(
            "Connected to Control MCP. {} chats available.",
            app.chats.len()
        );
        return Ok(());
    }
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut refreshed = Instant::now();
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| {
                let layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(5),
                        Constraint::Length(5),
                        Constraint::Length(2),
                    ])
                    .split(frame.area());
                frame.render_widget(
                    Block::default().style(
                        Style::default()
                            .bg(Color::Rgb(12, 13, 16))
                            .fg(Color::Rgb(233, 232, 237)),
                    ),
                    frame.area(),
                );
                frame.render_widget(
                    Paragraph::new(format!(
                        "Control  /  {}",
                        clean(
                            app.inspector
                                .as_ref()
                                .map(|(title, _)| title.as_str())
                                .unwrap_or_else(|| app.detail["title"]
                                    .as_str()
                                    .unwrap_or("New chat"))
                        )
                    ))
                    .style(Style::default().fg(Color::Rgb(133, 133, 143))),
                    layout[0],
                );
                let paragraph = Paragraph::new(app.transcript()).wrap(Wrap { trim: false });
                let max_scroll = paragraph
                    .line_count(layout[1].width)
                    .saturating_sub(layout[1].height as usize)
                    .min(u16::MAX as usize) as u16;
                if app.follow || app.scroll == u16::MAX {
                    app.scroll = max_scroll;
                }
                frame.render_widget(paragraph.scroll((app.scroll.min(max_scroll), 0)), layout[1]);
                let composer_style = Style::default()
                    .bg(Color::Rgb(36, 32, 47))
                    .fg(Color::Rgb(233, 232, 237));
                frame.render_widget(Block::default().style(composer_style), layout[2]);
                let input_area = layout[2].inner(ratatui::layout::Margin {
                    horizontal: 1,
                    vertical: 1,
                });
                let input = clean(&app.input);
                let input_lines: Vec<_> = input
                    .split('\n')
                    .enumerate()
                    .map(|(i, line)| {
                        let mut spans = vec![Span::styled(
                                if i == 0 { "> " } else { "  " },
                                Style::default().fg(Color::Rgb(181, 160, 255)),
                            )];
                        spans.extend(command_line(line).spans);
                        Line::from(spans)
                    })
                    .collect();
                let input_text = Text::from(input_lines);
                let width = input_area.width.max(1) as usize;
                let last_line = input_text.lines.last().map(Line::width).unwrap_or(2);
                let cursor_row = input_text
                    .lines
                    .iter()
                    .take(input_text.lines.len().saturating_sub(1))
                    .map(|line| line.width().max(1).div_ceil(width))
                    .sum::<usize>()
                    + last_line / width;
                let scroll =
                    cursor_row.saturating_sub(input_area.height.saturating_sub(1) as usize);
                frame.render_widget(
                    Paragraph::new(input_text)
                        .style(composer_style)
                        .wrap(Wrap { trim: false })
                        .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
                    input_area,
                );
                if input_area.width > 0 && input_area.height > 0 {
                    frame.set_cursor_position((
                        input_area.x + (last_line % width) as u16,
                        input_area.y + (cursor_row - scroll) as u16,
                    ));
                }
                frame.render_widget(
                    Paragraph::new(format!(
                        "{}\n{}",
                        lens_label(&app.detail),
                        if app.status.is_empty() {
                            "Enter send · Alt+Enter newline · /help".into()
                        } else {
                            clean(&app.status)
                        }
                    ))
                    .style(Style::default().fg(Color::Rgb(133, 133, 143)))
                    .wrap(Wrap { trim: false }),
                    layout[3],
                );
            })?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100))? {
            let event = event::read()?;
            dirty = true;
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.menu.is_some() && app.input.is_empty() {
                    match key.code {
                        KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
                            let menu = app.menu.as_mut().unwrap();
                            menu.selected = if key.code == KeyCode::Up {
                                menu.selected.saturating_sub(1)
                            } else {
                                (menu.selected + 1).min(menu.entries.len().saturating_sub(1))
                            };
                            app.scroll =
                                menu.selected.saturating_sub(6).min(u16::MAX as usize) as u16;
                            continue;
                        }
                        KeyCode::Char(' ') if app.menu.as_ref().is_some_and(|m| m.multiple) => {
                            app.menu.as_mut().unwrap().toggle();
                            continue;
                        }
                        KeyCode::Enter => {
                            if let Some(command) = app.menu.as_ref().and_then(Menu::command) {
                                app.input = command;
                                if let Err(e) = app.submit(&mut hive) {
                                    app.status = e.to_string();
                                }
                            }
                            continue;
                        }
                        KeyCode::Esc => {
                            app.menu = None;
                            app.inspector = None;
                            app.follow = true;
                            app.status.clear();
                            continue;
                        }
                        _ => {}
                    }
                }
                match key.code {
                    KeyCode::Char('q' | 'c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break
                    }
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Err(e) = app.new_chat(&mut hive) {
                            app.status = e.to_string();
                        }
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                        app.input.push('\n')
                    }
                    KeyCode::Enter => match app.submit(&mut hive) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(e) => app.status = e.to_string(),
                    },
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Tab => {
                        let matches: Vec<_> = CHAT_COMMANDS.iter().filter(|command| command.starts_with(&app.input)).collect();
                        if let [command] = matches.as_slice() {
                            app.input = format!("{command} ");
                            app.status.clear();
                        } else if !matches.is_empty() && app.input.starts_with('/') {
                            app.status = matches.iter().map(|command| **command).collect::<Vec<_>>().join(" · ");
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.input.chars().count() < 2100 {
                            app.input.push(c);
                        }
                    }
                    KeyCode::PageUp => {
                        app.follow = false;
                        app.scroll = app.scroll.saturating_sub(10);
                    }
                    KeyCode::End => app.follow = true,
                    KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
                    KeyCode::Up | KeyCode::Down
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && !app.chats.is_empty() =>
                    {
                        let index = app
                            .chats
                            .iter()
                            .position(|s| s["id"] == app.active)
                            .unwrap_or(0);
                        let next = if key.code == KeyCode::Down {
                            (index + 1).min(app.chats.len() - 1)
                        } else {
                            index.saturating_sub(1)
                        };
                        app.active = app.chats[next]["id"].as_str().unwrap_or("").into();
                        app.menu = None;
                        app.inspector = None;
                        app.scroll = u16::MAX;
                        app.follow = true;
                        if let Err(e) = app.refresh(&mut hive) {
                            app.status = e.to_string();
                        }
                    }
                    KeyCode::Up => {
                        app.follow = false;
                        app.scroll = app.scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        app.follow = false;
                        app.scroll = app.scroll.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        if refreshed.elapsed() > Duration::from_secs(2) {
            let old_chats = app.chats.clone();
            let old_revision = app.detail["revision"].clone();
            let old_status = app.status.clone();
            if let Err(e) = app.refresh(&mut hive) {
                app.status = e.to_string();
            }
            dirty |= app.chats != old_chats
                || app.detail["revision"] != old_revision
                || app.status != old_status;
            refreshed = Instant::now();
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires isolated server and authenticated worker; two small model calls, optional reload mode makes no calls"]
    fn live_workspace_diff_tracks_real_edits_and_persists() {
        let server=std::env::var("CONTROL_QA_URL").expect("Use isolated server");
        let mut hive=Hive::connect(&server).unwrap();
        let fixture=crate::config::state("diff-qa-chat").unwrap();
        let id=if std::env::var_os("CONTROL_QA_DIFF_RELOAD").is_some() {fs::read_to_string(&fixture).unwrap()}else{
            let id=hive.tool("control.chat.create",json!({})).unwrap()["id"].as_str().unwrap().to_owned();
            for request in ["Create edit-demo.txt containing exactly two lines: alpha and beta, each ending with a newline. Do not create any other files. Reply briefly.","Edit edit-demo.txt: replace only the second line beta with gamma, retaining alpha and the final newline. Use the edit/patch tool if available. Do not create any other files. Reply briefly."] {
                let submitted=hive.tool("control.chat.send",json!({"id":id,"message":request})).unwrap();
                let index=submitted["messages"].as_array().unwrap().len()-1;
                let deadline=Instant::now()+Duration::from_secs(180);
                loop {
                    let detail=hive.tool("control.chat.read",json!({"id":id})).unwrap();
                    let status=detail["messages"][index]["status"].as_str().unwrap();
                    if status=="completed" {break;}
                    assert!(["queued","running"].contains(&status),"Diff task ended with {status}");
                    assert!(Instant::now()<deadline,"Diff task timed out");
                    std::thread::sleep(Duration::from_millis(300));
                }
                if index==0 {
                    let first=hive.tool("control.chat.read",json!({"id":id})).unwrap();
                    let changes=first["messages"][0]["attempts"][0]["activity"].as_array().unwrap().iter().find(|event|event["id"]=="workspace-diff").expect("New file creation must also have a diff");
                    assert_eq!(changes["text"],"1 file · +2 −0");
                    assert!(changes["output"].as_str().unwrap().contains("+alpha\n+beta\n"));
                }
            }
            fs::write(&fixture,&id).unwrap();id
        };
        let detail=hive.tool("control.chat.read",json!({"id":id})).unwrap();
        let events=detail["messages"][1]["attempts"][0]["activity"].as_array().unwrap();
        let patch=events.iter().find(|e|e["id"]=="workspace-diff").expect("No observed workspace diff");
        assert_eq!(patch["text"],"1 file · +1 −1");
        let output=patch["output"].as_str().unwrap();
        assert!(output.contains("edit-demo.txt") && output.contains(" alpha\n-beta\n+gamma\n"));
        let page=hive.client.get(format!("{server}/chat/{id}/transcript")).send().unwrap().text().unwrap();
        assert!(page.contains("class='edit-diff' open") && page.contains("diff-line diff-add'>+gamma") && page.contains("diff-line diff-remove'>-beta"));
    }
    #[test]
    #[ignore = "requires isolated server, authenticated container worker and two small model calls; optional pause/resume phases test server restart"]
    fn live_design_question_pauses_and_resumes_checkpoint() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let fixture = crate::config::state("design-question-qa-id").unwrap();
        let id = if std::env::var_os("CONTROL_QA_DESIGN_RESUME").is_some() {
            fs::read_to_string(&fixture).unwrap()
        } else {
            let id = hive.tool("control.chat.create",json!({})).unwrap()["id"].as_str().unwrap().to_owned();
            hive.tool("control.chat.send",json!({"id":id,"message":"Checkpoint test: first write checkpoint-proof.txt containing exactly design-before. Then use control_design_ask to ask Which output format? with options Plain text and JSON, recommended index 1. Stop after asking. When the user answers, continue this original task: verify checkpoint-proof.txt still contains design-before and write answer-proof.txt containing exactly the chosen option text. Do not ask again. Reply briefly."})).unwrap();
            id
        };
        let deadline = Instant::now()+Duration::from_secs(180);
        let paused = loop {
            let detail = hive.tool("control.chat.read",json!({"id":id})).unwrap();
            let status = detail["messages"][0]["status"].as_str().unwrap_or("");
            if status == "waiting" { break detail; }
            assert!(["queued","running"].contains(&status),"Design ask ended with {status}");
            assert!(Instant::now()<deadline,"Design ask did not pause");
            std::thread::sleep(Duration::from_millis(300));
        };
        assert_eq!(paused["messages"][0]["question"]["input"]["options"],json!(["Plain text","JSON"]));
        let artifact = paused["messages"][0]["attempts"][0]["artifact"].as_str().unwrap();
        let before = hive.tool("control.artifact.read",json!({"id":id,"artifact":artifact,"path":"checkpoint-proof.txt"})).unwrap();
        assert_eq!(before["text"],"design-before");
        let page = hive.client.get(format!("{server}/chat/{id}/transcript")).send().unwrap().text().unwrap();
        assert!(page.contains("value='/answer") && page.contains("JSON · recommended"));
        if std::env::var_os("CONTROL_QA_DESIGN_PAUSE_ONLY").is_some() {
            fs::write(fixture,&id).unwrap();
            return;
        }
        let question_id = paused["messages"][0]["question"]["id"].as_str().unwrap();
        let response = hive.client.post(format!("{server}/chat/{id}/messages"))
            .header("Origin",&server).form(&[("message",format!("/answer {question_id} 2"))]).send().unwrap();
        assert_eq!(response.status().as_u16(),303);
        let deadline = Instant::now()+Duration::from_secs(180);
        loop {
            let detail = hive.tool("control.chat.read",json!({"id":id})).unwrap();
            assert_eq!(detail["messages"][0]["question"]["answer"],"JSON");
            let message = &detail["messages"][1];
            let status = message["status"].as_str().unwrap_or("");
            if status == "completed" {
                let artifact = message["attempts"][0]["artifact"].as_str().unwrap();
                let after = hive.tool("control.artifact.read",json!({"id":id,"artifact":artifact,"path":"answer-proof.txt"})).unwrap();
                assert_eq!(after["text"],"JSON");
                break;
            }
            assert!(["queued","running"].contains(&status),"Design continuation ended with {status}");
            assert!(Instant::now()<deadline,"Design answer did not complete");
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    #[test]
    #[ignore = "requires isolated local server with workers disabled; no model calls"]
    fn live_harness_commands_create_children_and_cancel_without_losing_history() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let parent = hive.tool("control.chat.create", json!({})).unwrap()["id"].as_str().unwrap().to_owned();
        hive.tool("control.chat.send", json!({"id":parent,"message":"Build a parser"})).unwrap();
        hive.tool("control.chat.send", json!({"id":parent,"message":"/steer Include tests"})).unwrap();
        hive.tool("control.chat.send", json!({"id":parent,"message":"/btw Explain the grammar"})).unwrap();
        let detail = hive.tool("control.chat.send", json!({"id":parent,"message":"/spawn Review the grammar"})).unwrap();
        assert_eq!(detail["messages"].as_array().unwrap().len(), 4);
        for message in &detail["messages"].as_array().unwrap()[2..] {
            let child = message["reply"].as_str().unwrap().strip_prefix("Agent conversation: /chat/").unwrap();
            let child = hive.tool("control.chat.read", json!({"id":child})).unwrap();
            assert_eq!(child["parent_chat"], parent);
            assert_eq!(child["mode"], "chat");
            assert_eq!(child["messages"][0]["status"], "queued");
        }
        let first = detail["messages"][0]["run_id"].as_str().unwrap();
        let detail = hive.tool("control.chat.send", json!({"id":parent,"message":format!("/cancel {first}")})).unwrap();
        assert_eq!(detail["messages"][0]["status"], "cancelled");
        let revision = detail["revision"].as_u64().unwrap();
        let steering = detail["messages"][1]["run_id"].as_str().unwrap();
        let response = hive.client.post(format!("{server}/cancel-run"))
            .header("Origin", &server).form(&[("id", steering)]).send().unwrap();
        assert_eq!(response.status().as_u16(), 303);
        let detail = hive.tool("control.chat.read", json!({"id":parent})).unwrap();
        assert_eq!(detail["messages"][1]["status"], "cancelled");
        assert!(detail["revision"].as_u64().unwrap() > revision);
        assert_eq!(detail["messages"][0]["text"], "Build a parser");
    }
    #[test]
    #[ignore = "requires isolated server with workers disabled, built WASM assets, and local Firefox WebDriver"]
    fn live_chat_enter_sends_and_shift_enter_adds_newline() {
        let server =
            std::env::var("CONTROL_QA_URL").expect("Use isolated server with workers disabled");
        let mut hive = Hive::connect(&server).unwrap();
        let driver = std::env::var("CONTROL_WEBDRIVER_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:18082".into());
        local_server(&driver).unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let session: Value = client.post(format!("{driver}/session")).json(&json!({"capabilities":{"alwaysMatch":{"browserName":"firefox","moz:firefoxOptions":{"args":["-headless"],"binary":"/Applications/Firefox.app/Contents/MacOS/firefox"}}}})).send().unwrap().json().unwrap();
        let id = session["value"]["sessionId"]
            .as_str()
            .expect("Firefox session failed");
        struct Browser {
            client: Client,
            url: String,
        }
        impl Browser {
            fn request(&self, method: reqwest::Method, path: &str, args: Value) -> Value {
                let mut request = self
                    .client
                    .request(method.clone(), format!("{}{path}", self.url));
                if method == reqwest::Method::POST {
                    request = request.json(&args);
                }
                let response = request.send().unwrap();
                let status = response.status();
                let value: Value = response.json().unwrap();
                assert!(
                    status.is_success(),
                    "WebDriver {method} {path}: {}",
                    value["value"]["error"].as_str().unwrap_or("request failed")
                );
                value["value"].clone()
            }
            fn post(&self, path: &str, args: Value) -> Value {
                self.request(reqwest::Method::POST, path, args)
            }
            fn get(&self, path: &str) -> Value {
                self.request(reqwest::Method::GET, path, Value::Null)
            }
            fn enter(&self) {
                self.post("/actions", json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\u{e007}"},{"type":"keyUp","value":"\u{e007}"}]}]}));
            }
            fn input(&self) -> String {
                self.post("/element", json!({"using":"css selector","value":".conversation-composer textarea[data-enter-sends=true]"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().expect("Chat WASM did not initialize").to_owned()
            }
        }
        impl Drop for Browser {
            fn drop(&mut self) {
                let _ = self.client.delete(&self.url).send();
            }
        }
        let browser = Browser {
            client,
            url: format!("{driver}/session/{id}"),
        };
        browser.post("/timeouts", json!({"implicit":5000,"pageLoad":15000}));
        browser.post("/url", json!({"url":format!("{server}/health")}));
        // The existing native client consumes the operator credential internally.
        // Pass it directly to the browser cookie store; never print it or put it in a URL.
        let cookie = hive
            .operator_cookie
            .to_str()
            .unwrap()
            .strip_prefix("control_operator=")
            .unwrap();
        browser.post("/cookie", json!({"cookie":{"name":"control_operator","value":cookie,"path":"/","httpOnly":true,"sameSite":"Strict"}}));
        browser.post("/url", json!({"url":format!("{server}/chats")}));
        let before = hive.tool("control.chats.list", json!({})).unwrap();
        let input = browser.input();
        browser.post(&format!("/element/{input}/value"), json!({"text":"first"}));
        browser.post("/actions", json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\u{e008}"},{"type":"keyDown","value":"\u{e007}"},{"type":"keyUp","value":"\u{e007}"},{"type":"keyUp","value":"\u{e008}"}]}]}));
        browser.post(&format!("/element/{input}/value"), json!({"text":"second"}));
        assert_eq!(
            browser.get(&format!("/element/{input}/property/value")),
            "first\nsecond"
        );
        assert_eq!(hive.tool("control.chats.list", json!({})).unwrap(), before);
        browser.enter();
        let start = Instant::now();
        let chat_id = loop {
            let url = browser.get("/url");
            let path = Url::parse(url.as_str().unwrap()).unwrap().path().to_owned();
            if let Some(id) = path.strip_prefix("/chat/c-") {
                break format!("c-{id}");
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Enter did not submit"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let chat = hive
            .tool("control.chat.read", json!({"id":chat_id}))
            .unwrap();
        assert_eq!(chat["messages"][0]["text"], "first\r\nsecond");
        let transcript = hive
            .client
            .get(format!("{server}/chat/{chat_id}/transcript"))
            .send()
            .unwrap();
        assert!(transcript.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'none'"));
        let input = browser.input();
        browser.post(
            &format!("/element/{input}/value"),
            json!({"text":"followup"}),
        );
        browser.enter();
        let start = Instant::now();
        loop {
            let chat = hive
                .tool("control.chat.read", json!({"id":chat_id}))
                .unwrap();
            if chat["messages"].as_array().unwrap().len() == 2 {
                assert_eq!(chat["messages"][1]["text"], "followup");
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Existing chat did not submit"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
        let input = browser.input();
        browser.post(
            &format!("/element/{input}/value"),
            json!({"text":"\u{e007}"}),
        );
        assert_eq!(browser.get(&format!("/element/{input}/property/value")), "");
        assert_eq!(
            hive.tool("control.chat.read", json!({"id":chat_id}))
                .unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // Completing a slash command changes only the draft. A second Enter sends it.
        browser.post(&format!("/element/{input}/value"), json!({"text":"/imp"}));
        let command = browser.post("/element", json!({"using":"css selector","value":"#chat-commands:not([hidden]) [data-command='/improve']"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
        assert_eq!(
            browser.get(&format!("/element/{command}/css/color")),
            "rgb(181, 160, 255)"
        );
        browser.enter();
        assert_eq!(
            browser.get(&format!("/element/{input}/property/value")),
            "/improve"
        );
        assert_eq!(
            hive.tool("control.chat.read", json!({"id":chat_id}))
                .unwrap()["mode"],
            "chat"
        );
        let highlighted = browser.post(
            "/element",
            json!({"using":"css selector","value":".composer-highlight .chat-command"}),
        )["element-6066-11e4-a52e-4f735466cecf"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            browser.get(&format!("/element/{highlighted}/css/color")),
            "rgb(181, 160, 255)"
        );
        browser.enter();
        let start = Instant::now();
        loop {
            if hive
                .tool("control.chat.read", json!({"id":chat_id}))
                .unwrap()["mode"]
                == "improve"
            {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Slash command did not enable improve mode"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
        let input = browser.input();
        // Tab completes an off command, and mouse completion remains available.
        browser.post(
            &format!("/element/{input}/value"),
            json!({"text":"/improve o"}),
        );
        browser.post("/actions", json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\u{e004}"},{"type":"keyUp","value":"\u{e004}"}]}]}));
        assert_eq!(
            browser.get(&format!("/element/{input}/property/value")),
            "/improve off"
        );
        browser.enter();
        let start = Instant::now();
        loop {
            if hive
                .tool("control.chat.read", json!({"id":chat_id}))
                .unwrap()["mode"]
                == "chat"
            {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "Slash command did not exit improve mode"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
        let input = browser.input();
        browser.post(&format!("/element/{input}/value"), json!({"text":"/"}));
        let off = browser.post("/element", json!({"using":"css selector","value":"#chat-commands:not([hidden]) [data-command='/improve off']"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
        browser.post(&format!("/element/{off}/click"), json!({}));
        assert_eq!(
            browser.get(&format!("/element/{input}/property/value")),
            "/improve off"
        );
        for (draft, command) in [("/bt", "/btw"), ("/spa", "/spawn"), ("/ste", "/steer"), ("/ans", "/answer")] {
            browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
            let input = browser.input();
            browser.post(&format!("/element/{input}/value"), json!({"text":draft}));
            let option = browser.post("/element", json!({"using":"css selector","value":format!("#chat-commands:not([hidden]) [data-command='{command}']")}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
            assert_eq!(browser.get(&format!("/element/{option}/css/color")), "rgb(181, 160, 255)");
            browser.enter();
            assert_eq!(browser.get(&format!("/element/{input}/property/value")), command);
            let prefix = browser.post("/element", json!({"using":"css selector","value":".composer-highlight .chat-command"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
            assert_eq!(browser.get(&format!("/element/{prefix}/css/color")), "rgb(181, 160, 255)");
        }
        if std::env::var_os("CONTROL_QA_DIFF_BROWSER").is_some() {
            let diff_chat=fs::read_to_string(crate::config::state("diff-qa-chat").unwrap()).unwrap();
            browser.post("/url",json!({"url":format!("{server}/chat/{diff_chat}/transcript")}));
            for (selector,color) in [(".edit-diff[open] .diff-add","rgb(181, 160, 255)"),(".edit-diff[open] .diff-remove","rgb(217, 154, 171)")] {
                let element=browser.post("/element",json!({"using":"css selector","value":selector}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
                assert_eq!(browser.get(&format!("/element/{element}/css/color")),color);
            }
            if let Ok(path)=std::env::var("CONTROL_QA_DIFF_SCREENSHOT") {
                use base64::Engine;
                let screenshot=browser.get("/screenshot");
                fs::write(path,base64::engine::general_purpose::STANDARD.decode(screenshot.as_str().unwrap()).unwrap()).unwrap();
            }
            browser.post("/url",json!({"url":format!("{server}/?view=providers#saved")}));
            for (selector,property) in [(".provider-saved:target","color"),(".model-choice[aria-pressed=true] .model-dot","background-color")] {
                let element=browser.post("/element",json!({"using":"css selector","value":selector}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
                assert_eq!(browser.get(&format!("/element/{element}/css/{property}")),"rgb(181, 160, 255)");
            }
        }
    }
    #[test]
    #[ignore = "requires an isolated Control server and pinned Nix development image; no model calls"]
    fn live_tools_catalog_and_rust_roundtrip() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let page = hive
            .client
            .get(format!("{server}/?view=tools"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert_eq!(page.matches("href='/tools/browser'").count(), 1);
        assert!(!page.contains("Lens tools"));
        assert!(!page.contains("<iframe"));
        let filtered = hive
            .client
            .get(format!("{server}/?view=tools&q=lens%3Aresearch"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(filtered.contains("href='/tools/search'"));
        assert!(!filtered.contains("href='/tools/rust'"));
        let catalog = hive.rpc("tools/list", json!({})).unwrap();
        assert!(catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control.rust.run"));
        let result = hive
            .tool(
                "control.rust.run",
                json!({"code":"fn main() { println!(\"tool-roundtrip-ok\"); }"}),
            )
            .unwrap();
        assert_eq!(result["success"], true);
        assert!(result["output"]
            .as_str()
            .unwrap()
            .contains("tool-roundtrip-ok"));
        let result = hive
            .client
            .post(format!("{server}/tools/rust"))
            .timeout(Duration::from_secs(25))
            .header("Origin", &server)
            .form(&[(
                "code",
                "fn main() { println!(\"<script>escaped</script>\"); }",
            )])
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(result.contains("&lt;script&gt;escaped&lt;/script&gt;"));
        assert!(!result.contains("<script>escaped</script>"));
        let rejected = hive.client.post(format!("{server}/mcp")).json(&json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"control.rust.run","arguments":{"code":"fn main() {}"}}})).send().unwrap();
        assert_eq!(rejected.status().as_u16(), 400);
    }
    #[test]
    #[ignore = "requires isolated Control server, authenticated worker image, and one model call"]
    fn live_worker_uses_credential_free_playground_tool() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let chat = hive.tool("control.chat.create", json!({})).unwrap();
        let id = chat["id"].as_str().unwrap();
        hive.tool("control.chat.send", json!({"id":id,"message":"Use the control_rust_run MCP tool to compile and run fn main() { println!(\"worker-playground-ok\"); }. Do not use shell commands. Reply briefly with its output."})).unwrap();
        let start = Instant::now();
        loop {
            let chat = hive.tool("control.chat.read", json!({"id":id})).unwrap();
            let message = chat["messages"].as_array().unwrap().last().unwrap();
            match message["status"].as_str().unwrap() {
                "queued" | "running" if start.elapsed() < Duration::from_secs(180) => {
                    std::thread::sleep(Duration::from_millis(500))
                }
                "completed" => {
                    assert!(message["reply"]
                        .as_str()
                        .unwrap()
                        .contains("worker-playground-ok"));
                    assert!(message["attempts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(
                            |attempt| attempt["activity"].as_array().unwrap().iter().any(|a| a
                                ["kind"]
                                == "mcp_tool_call"
                                && a["text"]
                                    .as_str()
                                    .unwrap_or("")
                                    .contains("control_rust_run"))
                        ));
                    break;
                }
                status => panic!(
                    "Worker tool check ended with {status}: {}",
                    message["reply"]
                ),
            }
        }
    }
    #[test]
    fn file_diffs_use_purple_additions_and_pastel_red_removals_in_terminal() {
        let app=App{detail:json!({"messages":[{"text":"Edit","status":"completed","reply":"Done","attempts":[{"activity":[{"kind":"file_change","status":"completed","text":"1 file · +1 −1","output":"@@ -1 +1 @@\n-old\n+new\n"},{"kind":"command_execution","text":"echo","output":"+ordinary output"}]}]}]}),..App::default()};
        let text=app.transcript();
        let add=text.lines.iter().find(|line|line.to_string()=="  +new").unwrap();
        let removed=text.lines.iter().find(|line|line.to_string()=="  -old").unwrap();
        assert_eq!(add.style.fg,Some(Color::Rgb(181,160,255)));
        assert_eq!(removed.style.fg,Some(Color::Rgb(217,154,171)));
        let ordinary=text.lines.iter().find(|line|line.to_string()=="  +ordinary output").unwrap();
        assert_eq!(ordinary.style.fg,None);
    }
    #[test]
    fn activity_and_context_boundary_render_in_terminal() {
        let app = App {
            detail: json!({"messages":[{"text":"Build", "status":"running", "reply":"", "attempts":[{"omitted_history":3,"activity":[{"status":"completed","text":"cargo check", "output":"finished\u{001b}[31m"}]}]}]}),
            ..App::default()
        };
        let text = app.transcript().to_string();
        assert!(text.contains("cargo check"));
        assert!(text.contains("finished"));
        assert!(text.contains("3 earlier exchanges excluded"));
        assert!(!text.contains('\u{001b}'));
    }
    #[test]
    #[ignore = "requires an isolated Control server and pinned browser image; no model calls"]
    fn live_browser_ui_capture_and_protected_artifact() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let hive = Hive::connect(&server).unwrap();
        let page = hive
            .client
            .get(format!("{server}/tools/browser"))
            .send()
            .unwrap();
        assert_eq!(page.status().as_u16(), 200);
        assert_eq!(page.headers()["referrer-policy"], "same-origin");
        assert!(page.text().unwrap().contains("/tools/browser/capture"));
        let outside = hive
            .client
            .post(format!("{server}/tools/browser/capture"))
            .form(&[
                ("input", "../outside.html"),
                ("width", "390"),
                ("height", "844"),
            ])
            .send()
            .unwrap();
        assert_eq!(outside.status().as_u16(), 400);
        let capture = hive
            .client
            .post(format!("{server}/tools/browser/capture"))
            .header("Origin", &server)
            .form(&[
                ("input", "preview.html"),
                ("width", "390"),
                ("height", "844"),
            ])
            .send()
            .unwrap();
        assert_eq!(capture.status().as_u16(), 303);
        let location = capture.headers()["location"].to_str().unwrap();
        let page = hive
            .client
            .get(format!("{server}{location}"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        let name = location.split_once("capture=").unwrap().1;
        let url = format!("{server}/tools/browser/artifacts/{name}");
        assert!(page.contains(&format!("src='/tools/browser/artifacts/{name}'")));
        let image = hive.client.get(&url).send().unwrap();
        assert_eq!(image.headers()["content-type"], "image/png");
        assert!(image.bytes().unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
        let unauthenticated = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(url)
            .send()
            .unwrap();
        assert_ne!(unauthenticated.status().as_u16(), 200);
    }
    #[test]
    fn remote_urls_and_terminal_control_injection_are_rejected() {
        assert!(local_server("http://localhost:8080").is_ok());
        for url in [
            "https://example.com",
            "http://localhost.evil.test",
            "http://secret@localhost",
            "http://localhost/path",
            "http://localhost/?token=secret",
        ] {
            assert!(local_server(url).is_err());
        }
        assert!(!clean("hi\x1b[2J\x07").contains('\x1b'));
        assert_eq!(
            lens_label(&json!({"lenses":["engineering","evidence"],"lens":null})),
            "engineering + evidence"
        );
        assert_eq!(lens_label(&json!({"lenses":[]})), "No lenses");
        assert_eq!(lens_label(&json!({"lens":"research"})), "research");
    }
    #[test]
    #[ignore = "requires an isolated local Control server; no model calls"]
    fn live_tui_lens_commands() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let mut app = App {
            input: "/use missing-lens".into(),
            ..App::default()
        };
        assert!(app.submit(&mut hive).is_err());
        assert!(app.active.is_empty());
        app.input = "/use engineering evidence".into();
        app.submit(&mut hive).unwrap();
        assert_eq!(app.detail["lenses"], json!(["engineering", "evidence"]));
        app.input = "/lenses".into();
        app.submit(&mut hive).unwrap();
        assert!(app
            .inspector
            .as_ref()
            .unwrap()
            .1
            .contains("repository-read"));
        let menu = app.menu.as_mut().unwrap();
        menu.selected = menu
            .entries
            .iter()
            .position(|(_, _, id)| id == "research")
            .unwrap();
        menu.toggle();
        app.input = menu.command().unwrap();
        app.submit(&mut hive).unwrap();
        assert!(app.detail["lenses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "research"));
        app.input = "/context".into();
        app.submit(&mut hive).unwrap();
        assert!(app.inspector.as_ref().unwrap().1.contains("repo.read"));
        app.input = "/chat".into();
        app.submit(&mut hive).unwrap();
        assert!(app.inspector.is_none());
        app.input = "/use none".into();
        app.submit(&mut hive).unwrap();
        assert_eq!(app.detail["lenses"], json!([]));
        assert!(app.detail["messages"].as_array().unwrap().is_empty());
    }
    #[test]
    #[ignore = "requires an isolated local Control server; no model calls"]
    fn live_declarative_lens_library() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let lens_id = format!("fixture-{stamp:x}");
        let note = hive
            .client
            .post(format!("{server}/knowledge/save"))
            .form(&[
                ("id", ""),
                ("revision", "0"),
                ("title", &format!("Lens fixture {stamp}")),
                ("body", "A pinned knowledge fixture."),
            ])
            .send()
            .unwrap();
        assert_eq!(note.status().as_u16(), 303);
        let url = Url::parse(&format!(
            "{server}{}",
            note.headers()["location"].to_str().unwrap()
        ))
        .unwrap();
        let doc_id = url
            .query_pairs()
            .find(|(k, _)| k == "doc")
            .unwrap()
            .1
            .into_owned();
        let mut fields = vec![
            ("id".to_string(), lens_id.clone()),
            ("name".into(), "Fixture composition".into()),
            ("revision".into(), "0".into()),
            ("include_engineering".into(), "on".into()),
            ("include_research".into(), "on".into()),
            (format!("document_{doc_id}"), "on".into()),
            ("instructions".into(), "Fixture instructions.".into()),
        ];
        let saved = hive
            .client
            .post(format!("{server}/lenses/save"))
            .form(&fields)
            .send()
            .unwrap();
        assert_eq!(saved.status().as_u16(), 303);
        let resolved = hive
            .tool("control.lenses.resolve", json!({"lenses":[lens_id]}))
            .unwrap();
        assert_eq!(resolved["tools"].as_array().unwrap().len(), 5);
        assert_eq!(
            resolved["documents"][0]["body"],
            "A pinned knowledge fixture."
        );
        assert!(resolved["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "Fixture instructions."));
        let chat = hive
            .tool("control.chat.create", json!({"lenses":[lens_id]}))
            .unwrap();
        assert_eq!(chat["lenses"], json!([lens_id]));
        let page = hive
            .client
            .get(format!("{server}/chat/{}", chat["id"].as_str().unwrap()))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(page.contains(&format!("name='lens_{lens_id}'")));
        assert!(page.contains("conversation-composer"));
        assert!(!page.contains("chat-lenses"));
        fields.iter_mut().find(|(k, _)| k == "revision").unwrap().1 = "1".into();
        let saved = hive
            .client
            .post(format!("{server}/lenses/save"))
            .form(&fields)
            .send()
            .unwrap();
        assert_eq!(saved.status().as_u16(), 303);
        let stale = hive
            .client
            .post(format!("{server}/lenses/save"))
            .form(&fields)
            .send()
            .unwrap();
        assert_eq!(stale.status().as_u16(), 409);
        let exported: Value = hive
            .client
            .get(format!("{server}/lenses/export"))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(
            exported["definitions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["id"] == lens_id)
                .count(),
            2
        );
    }
    #[test]
    #[ignore = "requires an isolated local Control server; no model calls"]
    fn live_chat_composition_and_design_entry() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        let before = hive.tool("control.chats.list", json!({})).unwrap();
        let response = hive
            .client
            .get(format!("{server}/chats?discuss=knowledge"))
            .send()
            .unwrap();
        assert_eq!(response.headers()["referrer-policy"], "same-origin");
        let page = response.text().unwrap();
        assert!(page.contains("design of Knowledge"));
        assert!(!page.contains("right-panel"));
        assert_eq!(hive.tool("control.chats.list", json!({})).unwrap(), before);
        for origin in ["null", "https://foreign.example"] {
            let rejected = hive
                .client
                .post(format!("{server}/chats"))
                .header("origin", origin)
                .form(&[("composition", "1")])
                .send()
                .unwrap();
            assert_eq!(rejected.status().as_u16(), 403);
        }
        assert_eq!(hive.tool("control.chats.list", json!({})).unwrap(), before);
        let result = hive
            .client
            .post(format!("{server}/chats"))
            .header("origin", &server)
            .form(&[
                ("composition", "1"),
                ("lens_engineering", "engineering"),
                ("lens_research", "research"),
                ("design_context", "knowledge"),
            ])
            .send()
            .unwrap();
        assert_eq!(result.status().as_u16(), 303);
        let path = result.headers()["location"].to_str().unwrap();
        let id = path.strip_prefix("/chat/").unwrap();
        let page = hive.client.get(format!("{server}{path}")).send().unwrap();
        assert_eq!(page.headers()["referrer-policy"], "same-origin");
        let invalid = hive
            .client
            .post(format!("{server}/chat/{id}/messages"))
            .header("origin", &server)
            .form(&[("message", "")])
            .send()
            .unwrap();
        assert_eq!(invalid.status().as_u16(), 400); // Reaches validation, not the origin rejection.
        let detail = hive.tool("control.chat.read", json!({"id":id})).unwrap();
        assert_eq!(detail["lenses"], json!(["engineering", "research"]));
        assert_eq!(detail["design_context"], "knowledge");
        assert!(detail["messages"].as_array().unwrap().is_empty());
        let result = hive
            .client
            .post(format!("{server}/chat/{id}/lenses"))
            .header("origin", &server)
            .form(&[("composition", "1")])
            .send()
            .unwrap();
        assert_eq!(result.status().as_u16(), 303);
        assert_eq!(
            hive.tool("control.chat.read", json!({"id":id})).unwrap()["lenses"],
            json!([])
        );
        let page = hive
            .client
            .get(format!("{server}/knowledge"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(!page.contains("right-panel"));
        assert!(page.contains("/chats?discuss=knowledge"));
    }
    #[test]
    #[ignore = "requires an isolated local Control server; no model calls"]
    fn live_knowledge_save_and_graph() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let hive = Hive::connect(&server).unwrap();
        if std::env::var_os("CONTROL_QA_VERIFY_RELOAD").is_some() {
            let page = hive
                .client
                .get(format!("{server}/knowledge?mode=graph"))
                .send()
                .unwrap()
                .text()
                .unwrap();
            assert!(page.contains("Durability A.md"));
            assert!(page.contains("Durability B.md"));
            assert!(page.contains("<line"));
            return;
        }
        let create = |title: &str, body: &str| {
            let result = hive
                .client
                .post(format!("{server}/knowledge/save"))
                .form(&[
                    ("id", ""),
                    ("title", title),
                    ("body", body),
                    ("revision", "0"),
                ])
                .send()
                .unwrap();
            assert_eq!(result.status().as_u16(), 303);
            result.headers()["location"].to_str().unwrap().to_owned()
        };
        let location = create("Durability A", "[[Durability B]]\nA saved note.");
        let url = Url::parse(&format!("{server}{location}")).unwrap();
        let id = url
            .query_pairs()
            .find(|(key, _)| key == "doc")
            .unwrap()
            .1
            .into_owned();
        let page = hive.client.get(url).send().unwrap().text().unwrap();
        assert!(page.contains("✓ Saved Durability A.md · revision 1"));
        assert!(page.contains("A saved note."));
        create("Durability B", "Backlink: [[Durability A]]");
        let stale = hive
            .client
            .post(format!("{server}/knowledge/save"))
            .form(&[
                ("id", id.as_str()),
                ("title", "Durability A"),
                ("body", "Retain this stale edit"),
                ("revision", "0"),
            ])
            .send()
            .unwrap();
        assert_eq!(stale.status().as_u16(), 409);
        assert!(stale.text().unwrap().contains("Retain this stale edit"));
        let graph = hive
            .client
            .get(format!("{server}/knowledge?mode=graph"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(graph.contains("<line"));
        let note = hive
            .client
            .get(format!("{server}/knowledge?doc={id}"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(note.contains("A saved note."));
        assert!(!note.contains("Retain this stale edit"));
    }
    #[test]
    #[ignore = "requires an isolated Control server and authenticated Nix worker image with room MCP"]
    fn live_room_tools_connect_worker_chat_and_ui() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
        let mut hive = Hive::connect(&server).unwrap();
        hive.tool(
            "control.room.post",
            json!({"room":"research","message":"The shared test marker is ROOM_CEDAR."}),
        )
        .unwrap();
        let chat = hive
            .tool(
                "control.chat.create",
                json!({"room":"research","lens":"research"}),
            )
            .unwrap();
        let id = chat["id"].as_str().unwrap();
        hive.tool("control.chat.send",json!({"id":id,"message":"Use the control_rooms MCP tools: read this room, then post exactly the shared test marker as a concise update. Do not use shell files to post it. Reply briefly."})).unwrap();
        let started = Instant::now();
        loop {
            let chat = hive.tool("control.chat.read", json!({"id":id})).unwrap();
            let last = chat["messages"].as_array().unwrap().last().unwrap();
            match last["status"].as_str().unwrap() {
                "completed" => break,
                "queued" | "running" if started.elapsed() < Duration::from_secs(150) => {
                    std::thread::sleep(Duration::from_millis(500))
                }
                _ => panic!("Room worker did not complete; no private diagnostics displayed"),
            }
        }
        let room = hive
            .tool("control.room.read", json!({"room":"research"}))
            .unwrap();
        assert!(room["posts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["chat"] == id && p["text"] == "ROOM_CEDAR" && p["author"] == "agent"));
        let other = hive
            .tool("control.room.read", json!({"room":"engineering"}))
            .unwrap();
        assert!(!other.to_string().contains("ROOM_CEDAR"));
        for (path, expected) in [
            ("/?view=rooms&channel=research", "ROOM_CEDAR"),
            ("/?view=lenses", "Context and tool"),
            ("/?view=editor", "Scratchpad"),
        ] {
            let page = hive
                .client
                .get(format!("{server}{path}"))
                .send()
                .unwrap()
                .text()
                .unwrap();
            if path.contains("lenses") {
                assert!(page.contains("Compose lenses"));
            } else {
                assert!(page.contains(expected));
            }
            assert!(!page.contains("<select id=\"new-chat-lens\""));
        }
    }
    #[test]
    #[ignore = "requires an isolated local Control server and authenticated container workers"]
    fn live_shared_chat_keeps_context_and_web_transcript() {
        let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server URL");
        let mut hive = Hive::connect(&server).unwrap();
        let value = hive.tool("control.chat.create", json!({})).unwrap();
        let id = value["id"].as_str().unwrap().to_string();
        for request in [
            "Remember the codeword RIVER_CEDAR. Reply with the codeword.",
            "What codeword did I give you in the previous message? Reply with that codeword.",
        ] {
            hive.tool("control.chat.send", json!({"id":id,"message":request}))
                .unwrap();
            let started = Instant::now();
            loop {
                let data = hive.tool("control.chat.read", json!({"id":id})).unwrap();
                let last = data["messages"].as_array().unwrap().last().unwrap();
                match last["status"].as_str().unwrap() {
                    "completed" => { assert!(last["reply"].as_str().unwrap().contains("RIVER_CEDAR")); break; }
                    "queued" | "running" if started.elapsed() < Duration::from_secs(120) => std::thread::sleep(Duration::from_millis(500)),
                    _ => panic!("Isolated session did not complete successfully; no raw diagnostics displayed."),
                }
            }
        }
        let url = format!("{}/chat/{id}", server.trim_end_matches('/'));
        let page = hive.client.get(&url).send().unwrap();
        assert!(page.status().is_success());
        assert!(page.text().unwrap().contains("chat-transcript"));
        let page = hive
            .client
            .get(format!("{url}/transcript"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(page.contains("RIVER_CEDAR"));
        assert!(!page.contains("http-equiv='refresh'"));
    }
}
#[test]
#[ignore = "requires an isolated Control server, authenticated Nix worker image and two small model calls; reload mode uses one call"]
fn live_artifacts_preserve_project_files_and_viewer_after_restart() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let reload = std::env::var_os("CONTROL_QA_VERIFY_RELOAD").is_some();
    let improve = std::env::var_os("CONTROL_QA_IMPROVE_MODE").is_some();
    let chat = if reload {
        hive.tool("control.chats.list", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        hive.tool("control.chat.create", json!({})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    if improve {
        if !reload {
            hive.tool("control.chat.send", json!({"id":chat,"message":"/improve"}))
                .unwrap();
        }
        assert_eq!(
            hive.tool("control.chat.read", json!({"id":chat})).unwrap()["mode"],
            "improve"
        );
    }
    let messages = if reload {
        vec!["Read counter.txt in the existing workspace, increment its integer by one and write it back. Preserve preview.txt. Actually use a shell command to read the file. Reply briefly."]
    } else {
        vec!["Use shell commands to create counter.txt containing 1 and preview.txt containing exactly <script>alert(1)</script>. Do not inspect credentials. Reply briefly.",
                "Read counter.txt in the existing workspace, increment its integer by one and write it back. Preserve preview.txt. Actually use a shell command to read the file. Reply briefly."]
    };
    for (index, message) in messages.iter().enumerate() {
        hive.tool("control.chat.send", json!({"id":chat,"message":message}))
            .unwrap();
        let started = Instant::now();
        loop {
            let detail = hive.tool("control.chat.read", json!({"id":chat})).unwrap();
            let current = detail["messages"].as_array().unwrap().last().unwrap();
            match current["status"].as_str().unwrap() {
                "queued" | "running" if started.elapsed() < Duration::from_secs(180) => {
                    std::thread::sleep(Duration::from_millis(500))
                }
                "completed" => break,
                status => panic!("Artifact test ended with {status}: {}", current["reply"]),
            }
        }
        let listed = hive
            .tool("control.artifacts.list", json!({"id":chat}))
            .unwrap();
        let artifact = listed["artifacts"][0]["id"].as_str().unwrap();
        if improve {
            let source = hive
                .tool(
                    "control.artifact.read",
                    json!({"id":chat,"artifact":artifact,"path":"src/main.rs"}),
                )
                .unwrap();
            assert!(source["text"].as_str().unwrap().contains("fixture"));
        }
        let read = hive
            .tool(
                "control.artifact.read",
                json!({"id":chat,"artifact":artifact,"path":"counter.txt"}),
            )
            .unwrap();
        let expected = if reload { 3 } else { index + 1 };
        assert_eq!(read["text"].as_str().unwrap().trim(), expected.to_string());
        let unsafe_path = hive.tool(
            "control.artifact.read",
            json!({"id":chat,"artifact":artifact,"path":"../operator-session"}),
        );
        assert!(unsafe_path.is_err());
        let other = hive.tool(
            "control.artifact.read",
            json!({"id":chat,"artifact":"unknown-output","path":"counter.txt"}),
        );
        assert!(other.is_err());
        let response = hive
            .client
            .get(format!(
                "{server}/chat/{chat}/artifacts?artifact={artifact}&file=preview.txt"
            ))
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert!(response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'none'"));
        let page = response.text().unwrap();
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!page.contains("<script>alert(1)</script>"));
        let unauthenticated =
            reqwest::blocking::get(format!("{server}/chat/{chat}/artifacts")).unwrap();
        assert_eq!(unauthenticated.status().as_u16(), 401);
    }
}

#[test]
#[ignore = "requires an isolated Control server; modifies only its model configuration, no model calls"]
fn live_model_library_selection_and_reload() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let hive = Hive::connect(&server).unwrap();
    let page = || {
        hive.client
            .get(format!("{server}/?view=providers"))
            .send()
            .unwrap()
            .text()
            .unwrap()
    };
    if std::env::var_os("CONTROL_QA_VERIFY_RELOAD").is_none() {
        for (provider, model) in [("codex", "fixture-codex"), ("claude", "fixture-claude")] {
            let response = hive
                .client
                .post(format!("{server}/providers"))
                .header("Origin", &server)
                .form(&[("action", "add"), ("provider", provider), ("model", model)])
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 303);
        }
        for (provider, model) in [("claude", "fixture-claude"), ("codex", "fixture-codex")] {
            let response = hive
                .client
                .post(format!("{server}/providers"))
                .header("Origin", &server)
                .form(&[
                    ("action", "select"),
                    ("provider", provider),
                    ("model", model),
                ])
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 303);
            let html = page();
            assert!(html.contains("fixture-codex"));
            assert!(html.contains("fixture-claude"));
            assert_eq!(html.matches("aria-pressed='true'").count(), 1);
        }
    }
    let html = page();
    assert!(html.contains("value='fixture-codex'") && html.contains("value='fixture-claude'"));
    assert!(html.contains("value='fixture-codex'><button class='model-choice' aria-pressed='true'"));
    assert!(!html.contains("<select"));
    assert!(!html.contains("provider-new"));
}
