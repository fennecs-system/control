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
use control_context::commands;

fn command_line(text: &str) -> Line<'static> {
    if let Some(command) = commands::parse(text).filter(|c| c.highlight.start == 0) {
        Line::from(vec![Span::styled(text[command.highlight.clone()].to_owned(), Style::default().fg(Color::Rgb(181, 160, 255))), Span::raw(text[command.highlight.end..].to_owned())])
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
fn failure(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
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
                if method == "tools/call" && control_context::tool_policy::lookup(params["name"].as_str().unwrap_or("")).is_some_and(|tool|tool.route==control_context::tool_policy::Route::Toolbox)
                {
                    Duration::from_secs(25)
                } else if method == "tools/call" && control_context::tool_policy::checkpoint(params["name"].as_str().unwrap_or("")) {
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
fn service_command_lines(lines: &mut Vec<Line<'static>>, command: &Value) {
    if command["kind"]=="message" {
        lines.extend(clean(command["text"].as_str().unwrap_or("")).lines().map(|line|Line::from(line.to_owned())));
        return;
    }
    lines.push(Line::styled(format!("[{}] > {}",clean(command["status"].as_str().unwrap_or("")),clean(command["text"].as_str().unwrap_or(""))),Style::default().fg(Color::Rgb(181,160,255))));
    lines.extend(clean(command["output"].as_str().unwrap_or("")).lines().map(|line|Line::from(format!("  {line}"))));
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
fn pending_project(detail: &Value) -> Option<&Value> {
    let project = detail.get("project").filter(|project| project.is_object())?;
    (project["grant"].is_null() || project["pending"] == true).then_some(project)
}
impl App {
    fn refresh(&mut self, hive: &mut Hive) -> io::Result<()> {
        self.chats = hive
            .tool("control.chats.list", json!({}))?
            .as_array()
            .cloned()
            .unwrap_or_default();
        let revision = self
            .chats
            .iter()
            .find(|s| s["id"] == self.active)
            .map(|s| s["revision"].clone());
        if !self.active.is_empty()
            && (self.detail["id"] != self.active
                || revision.as_ref() != Some(&self.detail["revision"])
                || self.chats.iter().find(|s|s["id"] == self.active).map(|s|&s["decision_revision"]) != Some(&self.detail["decision_revision"])
                || self.chats.iter().find(|s|s["id"]==self.active).map(|s|&s["port_revision"]) != Some(&self.detail["port_revision"]))
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
            _ if text.starts_with("/port ") => {
                let parts=text.split_whitespace().collect::<Vec<_>>();
                if !(3..=4).contains(&parts.len()) || !matches!(parts[1],"allow"|"deny") || (parts.len()==4 && parts[3]!="--loopback") {
                    return Err(failure("Use /port allow ID [--loopback] or /port deny ID. Review the exact target before allowing loopback."));
                }
                let request=self.detail["pending_ports"].as_array().and_then(|ports|ports.iter().find(|port|port["port_id"]==parts[2]))
                    .ok_or_else(||failure("Port request changed. Refresh the conversation before approving it."))?;
                let loopback=parts.get(3)==Some(&"--loopback");
                if parts[1]=="allow" && request["loopback_required"]==true && !loopback {return Err(failure("This exact loopback target requires /port allow ID --loopback."));}
                let endpoint=Url::parse(&hive.endpoint).map_err(failure)?;
                let mut fields=vec![("id",parts[2]),("return_chat",self.active.as_str()),("response","json"),("seconds","-1")];
                if parts[1]=="deny" {fields.push(("revoke","yes"));}
                if loopback {fields.push(("loopback","yes"));}
                let response=hive.client.post(endpoint.join("/tools/ports").map_err(failure)?)
                    .timeout(Duration::from_secs(15))
                    .header("Origin",endpoint.origin().ascii_serialization()).form(&fields).send().map_err(failure)?;
                let status=response.status();let value:Value=response.json().map_err(failure)?;
                if !status.is_success() {return Err(failure(value["error"].as_str().unwrap_or("Port decision failed")));}
                self.detail=hive.tool("control.chat.read",json!({"id":self.active}))?;
                self.status="Port decision saved".into();
            }
            "/open" => {
                if !self.active.is_empty() {
                    self.detail=hive.tool("control.chat.read",json!({"id":self.active}))?;
                }
                self.inspector=None;self.follow=true;self.scroll=u16::MAX;
                self.status=if pending_project(&self.detail).is_some() {"Review the folder request below; /trust confirms it"}else{"Use /open /absolute/path or /open --as NAME /absolute/path"}.into();
            }
            _ if text=="/trust" || text.starts_with("/trust ")=>{
                let project=pending_project(&self.detail).ok_or_else(||failure("No folder request to approve. Use /open /absolute/path."))?;
                let displayed=project["request"].as_str().filter(|request|!request.is_empty()).ok_or_else(||failure("Project request changed. Use /open to review it."))?;
                let request=text.strip_prefix("/trust ").map(str::trim).unwrap_or(displayed).to_owned();
                if request!=displayed {return Err(failure("Project request changed. Use /open to review it."));}
                let url=hive.endpoint.trim_end_matches("/mcp");
                let response=hive.client.post(format!("{url}/chat/{}/project",self.active)).timeout(Duration::from_secs(120))
                    .header("Origin",url).form(&[("approve","yes"),("request",request.as_str())]).send().map_err(|_|failure("Project approval did not finish; use /open to refresh before retrying."))?;
                let status=response.status();
                self.detail=hive.tool("control.chat.read",json!({"id":self.active}))?;
                if self.detail["project"]["request"]!=request {return Err(failure("Project request changed. Use /open to review it."));}
                if status.as_u16()!=303 || pending_project(&self.detail).is_some() || !self.detail["project"]["grant"].is_string() {return Err(failure(self.detail["project"]["error"].as_str().unwrap_or("Folder was not opened. Use /open to review it or /open /absolute/path to change it.")));}
                self.inspector=None;self.status="Folder ready · isolated worktree".into();
            }
            "/quit" => return Ok(true),
            "/new" => self.new_chat(hive)?,
            "/chats" | "/resume" => {
                self.refresh(hive)?;
                self.menu=Some(Menu {entries:self.chats.iter().map(|c| (format!("{}{}", "  ".repeat(c["depth"].as_u64().unwrap_or(0).min(8) as usize),c["title"].as_str().unwrap_or("Chat")),c["lenses"].as_array().map(|a|a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + ")).unwrap_or_default(),c["id"].as_str().unwrap_or("").into())).collect(),selected:0,multiple:false,checked:Vec::new()});
                let lines=self.chats.iter().map(|c|format!("{}\n  /resume {}",c["title"].as_str().unwrap_or("Chat"),c["id"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n\n");
                self.inspector=Some(("Conversations".into(),lines));self.scroll=0;self.follow=false;self.status="↑ ↓ choose · Enter open · Esc back".into();
            }
            "/help" => self.status = "/chats · /lenses · /use ID ID · /use none · /context · /chat · /new · /resume ID · /open /path · /trust · /port allow|deny ID · /improve request · /improve off · /answer QUESTION_ID choice · /steer direction · /btw question · /spawn task · /listen · /listen off · /cancel · Ctrl+Q quit".into(),
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
            _ if text.starts_with('/') && commands::parse(&text).is_none()=>return Err(failure("Unknown command. Use /help.")),
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
        if let Some(listener)=self.detail["room_listener"].as_object() {
            lines.push(Line::styled(format!("Room #{} · {}",listener.get("room").and_then(Value::as_str).unwrap_or(""),listener.get("state").and_then(Value::as_str).unwrap_or("")),Style::default().fg(Color::Rgb(181,160,255))));
        }
        if let Some(previews)=self.detail["previews"].as_array() {for preview in previews {if let Some(url)=preview["url"].as_str(){lines.push(Line::styled(format!("Preview · {}",clean(url)),Style::default().fg(Color::Rgb(181,160,255))));}}}
        if let Some(path)=self.detail["project_worktree"].as_str(){lines.push(Line::from(format!("Worktree · {}",clean(path))));}
        if let Some(project)=self.detail["project"].as_object() {
            let path=clean(project.get("path").and_then(Value::as_str).unwrap_or(""));
            lines.push(Line::styled(format!("Project · {path}"),Style::default().fg(Color::Rgb(181,160,255))));
        }
        if let Some(messages) = self.detail["messages"].as_array() {
            for (position, message) in messages.iter().enumerate() {
                lines.push(Line::styled(
                    control_context::agent_policy::MessageSpeaker::for_run_kind(message["kind"].as_str()).label(),
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
                let folder_reply = (message["run_id"].is_null()
                    && commands::tail(message["text"].as_str().unwrap_or("").trim(), commands::Kind::Open).is_some()
                    && message["reply"].as_str().and_then(|reply|reply.strip_prefix("Review and approve folder access: /chat/")).and_then(|path|path.strip_suffix("/project"))==Some(self.active.as_str())).then(|| {
                    if let Some(mount)=self.detail["project"]["mounts"].as_array().and_then(|mounts|mounts.iter().find(|mount|mount["message_index"].as_u64()==Some(position as u64))) {
                        format!("Opened /workspace/{}",clean(mount["name"].as_str().unwrap_or("")))
                    } else if pending_project(&self.detail).is_some_and(|project|project["message_index"].as_u64()==Some(position as u64)) {
                        "Folder access requested; review the prompt below.".into()
                    } else {
                        "Folder request replaced by a later request.".into()
                    }
                });
                lines.extend(
                    clean(folder_reply.as_deref().unwrap_or_else(||message["reply"].as_str().unwrap_or("")))
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
                                if event["kind"]=="message" {
                                    let text=event["text"].as_str().unwrap_or("");
                                    if text.trim()!=message["reply"].as_str().unwrap_or("").trim() {
                                        lines.extend(clean(text).lines().map(|line|Line::from(line.to_owned())));
                                    }
                                    continue;
                                }
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
                if let Some(commands)=message["commands"].as_array() {
                    for command in commands { service_command_lines(&mut lines,command); }
                }
                lines.push(Line::from(""));
            }
        }
        if let Some(commands)=self.detail["agent_activity"].as_array() {
            for command in commands {
                lines.push(Line::styled(format!("Agent · {}",clean(command["chat"].as_str().unwrap_or(""))),Style::default().fg(Color::Rgb(181,160,255))));
                service_command_lines(&mut lines,&command["activity"]);
            }
        }
        if let Some(questions) = self.detail["pending_questions"].as_array() {
            for question in questions {
                lines.push(Line::styled("Decision requested",Style::default().fg(Color::Rgb(181,160,255))));
                lines.extend(clean(question["summary"].as_str().unwrap_or("")).lines().map(|line|Line::from(line.to_owned())));
            }
        }
        if let Some(ports)=self.detail["pending_ports"].as_array() {for port in ports {
            lines.push(Line::styled(format!("Port request · {} · {}",clean(port["name"].as_str().unwrap_or("")),clean(&port["target"].to_string())),Style::default().fg(Color::Rgb(181,160,255))));
            let id=clean(port["port_id"].as_str().unwrap_or(""));
            lines.push(Line::from(format!("/port allow {id}{} · /port deny {id}",if port["loopback_required"]==true {" --loopback"}else{""})));
        }}
        if let Some(project)=pending_project(&self.detail) {
            lines.push(Line::styled(format!("Folder access requested · {}",clean(project["path"].as_str().unwrap_or(""))),Style::default().fg(Color::Rgb(181,160,255))));
            lines.push(Line::from(format!("Open as /workspace/{}",clean(project["name"].as_str().unwrap_or("")))));
            lines.push(Line::from("Trust this folder’s code and allow local previews? Control copies it into an isolated worktree; credentials, generated directories and symlinks are excluded. The original stays unchanged."));
            if let Some(error)=project["error"].as_str() {lines.push(Line::from(clean(error)));}
            lines.push(Line::from(format!("/trust {} to approve · /open /absolute/path to change",clean(project["request"].as_str().unwrap_or("")))));
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
fn resume_selection(args:&[String])->io::Result<Option<&str>> {
    let mut selected=None;
    for (index,arg) in args.iter().enumerate() {
        if arg=="--session"||arg=="--resume" {
            if selected.is_some(){return Err(failure("Choose one --resume ID or --session ID."));}
            selected=Some(args.get(index+1).filter(|value|!value.is_empty()&&!value.starts_with('-')).ok_or_else(||failure("Use --resume ID to select a conversation."))?.as_str());
        }
    }
    Ok(selected)
}
pub fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|s| s == "--help") {
        println!("control tui [--server {}] [--resume ID] [--directory PATH] [--check]\nAlso available as control-harness. Starts a fresh chat; --resume ID (or --session ID) explicitly resumes one. CONTROL_ADDR sets the default server.\n\n/resume · /open /absolute/path · /trust [REQUEST] · /port allow|deny ID · /answer QUESTION_ID choice · /lenses · /use ID ID · /use none · /context · /chat · /help", crate::config::server_url());
        return Ok(());
    }
    let option = |name: &str| {
        args.iter()
            .position(|s| s == name)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    let selected=resume_selection(&args)?;
    let default = crate::config::server_url();
    let mut hive = Hive::connect(option("--server").unwrap_or(&default))?;
    let mut app = App {
        follow: true,
        active: selected.unwrap_or("").into(),
        ..App::default()
    };
    app.refresh(&mut hive)?;
    if let Some(directory)=option("--directory") {
        let path=std::path::PathBuf::from(directory);
        let path=if path.is_absolute(){path}else{std::env::current_dir()?.join(path)};
        if app.active.is_empty(){app.new_chat(&mut hive)?;}
        app.detail=hive.tool("control.project.request",json!({"id":app.active,"path":path.to_string_lossy()}))?;
        app.refresh(&mut hive)?;
    }
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
                        let matches: Vec<_> = commands::completions(&app.input).into_iter().map(|i|commands::DEFINITIONS[i].text).collect();
                        if let [command] = matches.as_slice() {
                            app.input = format!("{command} ");
                            app.status.clear();
                        } else if !matches.is_empty() && app.input.starts_with('/') {
                            app.status = matches.join(" · ");
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
    #[test]
    fn folder_prompt_stays_at_the_live_tail_and_tracks_replaced_requests() {
        let mut app=App {active:"c-fixture".into(),detail:json!({
            "project":{"request":"p-current","path":"/node/current","name":"current","pending":true,"grant":"previous-grant","message_index":2,"mounts":[{"name":"approved","message_index":0}]},
            "messages":[
                {"text":"/open /node/approved","reply":"Review and approve folder access: /chat/c-fixture/project"},
                {"text":"/open /node/replaced","reply":"Review and approve folder access: /chat/c-fixture/project"},
                {"text":"/open /node/current","reply":"Review and approve folder access: /chat/c-fixture/project"},
                {"text":"A later message","reply":"Latest response"},
                {"text":"/open /node/invalid","reply":"The folder could not be read"}
            ]
        }),..Default::default()};
        let transcript=app.transcript().to_string();
        assert!(transcript.contains("Opened /workspace/approved"));
        assert!(transcript.contains("Folder request replaced by a later request."));
        assert!(transcript.contains("/trust p-current to approve"));
        assert!(transcript.contains("Open as /workspace/current"));
        assert!(transcript.contains("The folder could not be read"));
        assert!(transcript.find("Latest response").unwrap()<transcript.find("Folder access requested · /node/current").unwrap());
        assert!(!transcript.contains("/chat/c-fixture/project"));
        app.detail["project"]["pending"]=json!(false);
        assert!(!app.transcript().to_string().contains("/trust"));
    }

    #[test]
    fn terminal_folder_approval_uses_displayed_identity_and_retains_errors_without_a_browser() {
        use std::io::{BufRead,Read};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr=listener.local_addr().unwrap();
        let server=std::thread::spawn(move || {
            for index in 0..5 {
                let (mut stream,_)=listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader=io::BufReader::new(stream.try_clone().unwrap());
                let mut line=String::new();reader.read_line(&mut line).unwrap();
                let path=line.split_whitespace().nth(1).unwrap().to_owned();
                let mut length=0;let mut origin=String::new();
                loop {line.clear();reader.read_line(&mut line).unwrap();if line=="\r\n"{break;}
                    if let Some((name,value))=line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length"){length=value.trim().parse::<usize>().unwrap();}
                        if name.eq_ignore_ascii_case("origin"){origin=value.trim().to_owned();}
                    }
                }
                let mut bytes=vec![0;length];reader.read_exact(&mut bytes).unwrap();
                if index==0 || index==2 {
                    assert_eq!(path,"/chat/c-fixture/project");assert_eq!(origin,format!("http://{addr}"));
                    let fields:std::collections::HashMap<_,_>=Url::parse(&format!("http://fixture/?{}",String::from_utf8(bytes).unwrap())).unwrap().query_pairs().into_owned().collect();
                    assert_eq!(fields.len(),2);assert_eq!(fields["request"],"p-fixture");assert_eq!(fields["approve"],"yes");
                    control_http::respond(&mut stream,"303 See Other","text/plain",b"","Location: /chat/c-fixture\r\n",control_http::Policy::Embedded).unwrap();
                }else {
                    assert_eq!(path,"/mcp");let request:Value=serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(request["params"]["name"],"control.chat.read");assert_eq!(request["params"]["arguments"],json!({"id":"c-fixture"}));
                    let project=if index==1 {json!({"request":"p-fixture","path":"/node/project","name":"project","pending":true,"error":"Source is not a directory"})}else{json!({"request":"p-fixture","path":"/node/project","name":"project","pending":false,"grant":"g-fixture"})};
                    let response=json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"id":"c-fixture","project":project}}}});
                    control_http::respond(&mut stream,"200 OK","application/json",response.to_string().as_bytes(),"",control_http::Policy::Embedded).unwrap();
                }
            }
        });
        let mut hive=Hive {client:Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(3)).build().unwrap(),endpoint:format!("http://{addr}/mcp"),next:0,operator_cookie:HeaderValue::from_static("fixture")};
        let mut app=App {active:"c-fixture".into(),detail:json!({"project":{"request":"p-fixture","path":"/node/project","name":"project","pending":true}}),input:"/trust p-obsolete".into(),..Default::default()};
        assert!(app.submit(&mut hive).unwrap_err().to_string().contains("request changed"));
        app.input="/trust".into();
        assert_eq!(app.submit(&mut hive).unwrap_err().to_string(),"Source is not a directory");
        assert!(app.transcript().to_string().contains("Source is not a directory"));
        app.input="/trust p-fixture".into();app.submit(&mut hive).unwrap();
        assert_eq!(app.status,"Folder ready · isolated worktree");
        assert!(!app.transcript().to_string().contains("/trust"));
        app.input="/trust".into();assert!(app.submit(&mut hive).unwrap_err().to_string().contains("No folder request"));
        app.input="/open".into();app.submit(&mut hive).unwrap();
        assert_eq!(app.status,"Use /open /absolute/path or /open --as NAME /absolute/path");
        server.join().unwrap();
    }

    #[test]
    fn listen_commands_reach_the_server_and_render_returned_state() {
        use std::io::{BufRead, Read};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr=listener.local_addr().unwrap();
        let server=std::thread::spawn(move || {
            for command in ["/listen", "/listen off"] {
                let (mut stream,_)=listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader=io::BufReader::new(stream.try_clone().unwrap());
                let mut line=String::new(); reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /mcp "));
                let mut length=0;
                loop {
                    line.clear(); reader.read_line(&mut line).unwrap();
                    if line=="\r\n" {break;}
                    if let Some((name,value))=line.split_once(':') && name.eq_ignore_ascii_case("content-length") {length=value.trim().parse::<usize>().unwrap();}
                }
                let mut bytes=vec![0;length]; reader.read_exact(&mut bytes).unwrap();
                let request:Value=serde_json::from_slice(&bytes).unwrap();
                assert_eq!(request["params"]["name"], "control.chat.send");
                assert_eq!(request["params"]["arguments"], json!({"id":"c-fixture","message":command}));
                let status=if command=="/listen" {json!({"room":"research","state":"listening"})}else{Value::Null};
                let response=json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"id":"c-fixture","room_listener":status}}}});
                control_http::respond(&mut stream,"200 OK","application/json",response.to_string().as_bytes(),"",control_http::Policy::Embedded).unwrap();
            }
        });
        let mut hive=Hive {client:Client::builder().timeout(Duration::from_secs(3)).build().unwrap(),endpoint:format!("http://{addr}/mcp"),next:0,operator_cookie:HeaderValue::from_static("fixture")};
        let mut app=App {active:"c-fixture".into(),input:"/listen".into(),..Default::default()};
        app.submit(&mut hive).unwrap();
        assert!(app.transcript().to_string().contains("Room #research · listening"));
        app.input="/listen off".into(); app.submit(&mut hive).unwrap();
        assert!(!app.transcript().to_string().contains("listening"));
        server.join().unwrap();
    }

    #[test]
    fn parent_transcript_exposes_child_decisions_and_answer_command() {
        let app = super::App {
            detail:serde_json::json!({"room_listener":{"room":"research","state":"listening"},"messages":[{"text":"Earlier prompt","reply":"Earlier response"}],"pending_questions":[{"summary":"Choose a route\n1. Relay\n2. Stop\n/answer q-child YOUR_ANSWER"}]}),
            ..Default::default()
        };
        let transcript = app.transcript().to_string();
        assert!(transcript.contains("Choose a route"));
        assert!(transcript.contains("/answer q-child YOUR_ANSWER"));
        assert!(transcript.contains("Room #research · listening"));
        assert!(transcript.find("Earlier response").unwrap()<transcript.find("Decision requested").unwrap(),"Pending decisions must remain visible at the live tail");
    }
    #[test]
    fn terminal_port_decisions_require_explicit_loopback_and_use_operator_http() {
        use std::io::{BufRead,Read};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr=listener.local_addr().unwrap();
        let server=std::thread::spawn(move || {
            for index in 0..2 {
                let (mut stream,_)=listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader=io::BufReader::new(stream.try_clone().unwrap());
                let mut line=String::new();reader.read_line(&mut line).unwrap();
                let path=line.split_whitespace().nth(1).unwrap().to_owned();
                let mut length=0;let mut origin=String::new();
                loop {line.clear();reader.read_line(&mut line).unwrap();if line=="\r\n"{break;}
                    if let Some((name,value))=line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length"){length=value.trim().parse::<usize>().unwrap();}
                        if name.eq_ignore_ascii_case("origin"){origin=value.trim().to_owned();}
                    }
                }
                let mut bytes=vec![0;length];reader.read_exact(&mut bytes).unwrap();
                let value=if index==0 {
                    assert_eq!(path,"/tools/ports");assert_eq!(origin,format!("http://{addr}"));
                    let fields:std::collections::HashMap<_,_>=Url::parse(&format!("http://fixture/?{}",String::from_utf8(bytes).unwrap())).unwrap().query_pairs().into_owned().collect();
                    assert_eq!(fields["id"],"port-fixture");assert_eq!(fields["revoke"],"yes");
                    assert_eq!(fields["seconds"],"-1");assert_eq!(fields["return_chat"],"c-fixture");
                    assert!(!fields.contains_key("loopback"));json!({"ok":true})
                }else {
                    assert_eq!(path,"/mcp");let request:Value=serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(request["params"]["name"],"control.chat.read");
                    json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"id":"c-fixture","pending_ports":[]}}}})
                };
                control_http::respond(&mut stream,"200 OK","application/json",value.to_string().as_bytes(),"",control_http::Policy::Embedded).unwrap();
            }
        });
        let mut hive=Hive {client:Client::builder().timeout(Duration::from_secs(3)).build().unwrap(),endpoint:format!("http://{addr}/mcp"),next:0,operator_cookie:HeaderValue::from_static("fixture")};
        let mut app=App {active:"c-fixture".into(),detail:json!({"messages":[{"text":"Earlier prompt","reply":"Earlier response"}],"pending_ports":[{"port_id":"port-fixture","name":"relay","target":{"host":"127.0.0.1","port":25565},"loopback_required":true}]}),input:"/port allow port-fixture".into(),..Default::default()};
        let transcript=app.transcript().to_string();
        assert!(transcript.find("Earlier response").unwrap()<transcript.find("Port request").unwrap());
        assert!(app.submit(&mut hive).unwrap_err().to_string().contains("--loopback"));
        app.input="/port deny port-fixture".into();app.submit(&mut hive).unwrap();
        assert_eq!(app.detail["pending_ports"],json!([]));
        server.join().unwrap();
    }
    #[test]
    fn startup_only_resumes_an_explicit_selection() {
        let args=|values:&[&str]|values.iter().map(|s|s.to_string()).collect::<Vec<_>>();
        assert_eq!(resume_selection(&args(&["control","tui"])).unwrap(),None);
        for option in ["--resume","--session"] {
            assert_eq!(resume_selection(&args(&["control",option,"c-selected"])).unwrap(),Some("c-selected"));
            assert!(resume_selection(&args(&["control",option])).is_err());
            assert!(resume_selection(&args(&["control",option,"--check"])).is_err());
        }
        assert!(resume_selection(&args(&["control","--resume","c-a","--session","c-b"])).is_err());
    }
    #[test]
    fn refresh_keeps_a_new_chat_blank_and_preserves_explicit_resume() {
        use std::io::{BufRead,Read};
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let addr=listener.local_addr().unwrap();
        let server=std::thread::spawn(move || {
            let mut calls=Vec::new();
            for _ in 0..5 {
                let (mut stream,_)=listener.accept().unwrap();stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader=io::BufReader::new(stream.try_clone().unwrap());let mut line=String::new();reader.read_line(&mut line).unwrap();let mut length=0;
                loop {line.clear();reader.read_line(&mut line).unwrap();if line=="\r\n"{break;}
                    if let Some((name,value))=line.split_once(':') && name.eq_ignore_ascii_case("content-length"){length=value.trim().parse::<usize>().unwrap();}
                }
                let mut bytes=vec![0;length];reader.read_exact(&mut bytes).unwrap();let request:Value=serde_json::from_slice(&bytes).unwrap();
                let tool=request["params"]["name"].as_str().unwrap();calls.push(tool.to_owned());
                let value=match tool {
                    "control.chats.list"=>json!([{"id":"c-existing","title":"Existing","revision":7}]),
                    "control.chat.read"=>json!({"id":"c-existing","title":"Existing","revision":7}),
                    _=>panic!("Refreshing must not create or send chats"),
                };
                let response=json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":value}}});
                control_http::respond(&mut stream,"200 OK","application/json",response.to_string().as_bytes(),"",control_http::Policy::Embedded).unwrap();
            }
            calls
        });
        let mut hive=Hive{client:Client::builder().timeout(Duration::from_secs(3)).build().unwrap(),endpoint:format!("http://{addr}/mcp"),next:0,operator_cookie:HeaderValue::from_static("fixture")};
        let mut app=App::default();app.refresh(&mut hive).unwrap();app.refresh(&mut hive).unwrap();
        assert!(app.active.is_empty());assert!(app.detail.is_null());assert_eq!(app.chats.len(),1);
        app.active="c-existing".into();app.refresh(&mut hive).unwrap();app.refresh(&mut hive).unwrap();
        assert_eq!(app.active,"c-existing");assert_eq!(app.detail["title"],"Existing");
        assert_eq!(server.join().unwrap(),["control.chats.list","control.chats.list","control.chats.list","control.chat.read","control.chats.list"]);
    }
    use super::*;
    #[test]
    #[ignore = "requires isolated control-bench-qa server with benchmark worker image; no model calls"]
    fn live_rust_benchmarks_use_mcp_and_compare_html_escaping() {
        let directory=crate::config::directory().unwrap();
        assert!(directory.parent().is_some_and(|p|p.parent()==Some(std::path::Path::new("/private/tmp"))&&p.file_name().is_some_and(|n|n.to_string_lossy().starts_with("control-bench-qa."))));
        let server=std::env::var("CONTROL_QA_URL").unwrap();let mut hive=Hive::connect(&server).unwrap();
        let tools=hive.rpc("tools/list",json!({})).unwrap();
        assert!(tools["tools"].as_array().unwrap().iter().any(|t|t["name"]=="control.rust.bench"));
        let current=include_str!("../tools/rust-analysis/escape-candidate.rs").lines().skip(1).collect::<Vec<_>>().join("\n");
        let previous=r#"fn previous(s:&str)->String {s.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&#39;")}"#;
        for (name,input) in [("plain","ordinary command output without markup\n".repeat(48)),("escaped","Synthetic output <&> λ\n".repeat(48)),("sparse",format!("{}<","x".repeat(1800))),("short","é<&>λ'🦊\"".into())] {
            let setup=format!("{current}\n{previous}\nlet input=String::from({}); assert_eq!(escape_html(&input),previous(&input));",serde_json::to_string(&input).unwrap());
            for implementation in ["previous","escape_html"] {
                let result=hive.tool("control.rust.bench",json!({"setup":setup,"code":format!("{implementation}(std::hint::black_box(input.as_str()))"),"iterations":1000,"samples":9})).unwrap();
                assert_eq!(result["success"],true,"{}",result["output"]);
                assert_eq!(result["timings"]["samples"],9);
                assert_eq!(result["opt_level"],3);
                println!("{}",json!({"workload":"html_escape","case":name,"implementation":implementation,"timings":result["timings"],"image":result["image"]}));
            }
        }
        for (code,status) in [("not valid Rust","compile 1"),("loop {}","run timeout")] {
            let result=hive.tool("control.rust.bench",json!({"code":code})).unwrap();
            assert_eq!(result["success"],false);assert_eq!(result["status"],status);assert!(result["timings"].is_null());
        }
    }
    #[test]
    #[ignore = "requires isolated control-selfdev-qa server, declared build image, own authenticated development model; three or more model calls"]
    fn live_improve_build_and_independent_judge_roundtrip() {
        let state_dir=crate::config::state_directory().unwrap();
        assert!(state_dir.parent().is_some_and(|p|p.parent()==Some(std::path::Path::new("/private/tmp")) && p.file_name().is_some_and(|s|s.to_string_lossy().starts_with("control-selfdev-qa."))));
        let server=std::env::var("CONTROL_QA_URL").expect("Use an isolated self-development server");
        let mut hive=Hive::connect(&server).unwrap();
        let chat=hive.tool("control.chat.create",json!({"mode":"improve"})).unwrap()["id"].as_str().unwrap().to_owned();
        let submitted=hive.tool("control.chat.send",json!({"id":chat,"message":"Implement this small self-development verification task in the isolated Control source. Create docs/self-development-proof.md containing exactly: Control self-development verified. Do not change any other source file. Then use control_workspace_start with environment build and timeout_seconds 1800 to run cargo test --locked --bin control github::tests. Use the status tool until the job completes and verify exit code zero. Next call control_agent_spawn_blocking with role judge and objective: Review docs/self-development-proof.md against the requested exact text Control self-development verified. Inspect the actual file in the read-only source; report findings and do not modify files. Stop after blocking-spawn acknowledgement. When Control resumes you with the judge feedback, use control_agent_result section summary to inspect its full report, verify the proof file remains correct and finish briefly. Do not publish a PR, deploy, inspect credentials or modify the host checkout."})).unwrap();
        let root=submitted["messages"][0]["run_id"].as_str().unwrap().to_owned();
        let started=Instant::now();
        loop {
            let detail=hive.tool("control.chat.read",json!({"id":chat})).unwrap();
            let last=detail["messages"].as_array().unwrap().last().unwrap();
            let status=last["status"].as_str().unwrap();
            if status=="completed" {break;}
            assert!(["queued","running","waiting","continued"].contains(&status),"Self-development run ended with {status}: {}",last["reply"]);
            assert!(started.elapsed()<Duration::from_secs(900),"Self-development workflow timed out");
            std::thread::sleep(Duration::from_millis(500));
        }
        let saved:Value=serde_json::from_slice(&fs::read(state_dir.join("state.json")).unwrap()).unwrap();
        let runs=saved["runtime"]["runs"].as_array().unwrap();
        let parent=runs.iter().find(|r|r["id"]==root).unwrap();
        assert_eq!(parent["status"],"continued","The developer must actually delegate, not merely describe a review");
        let judge=runs.iter().find(|r|r["persona"]["role"]=="judge"&&r["persona"]["root"]==root).expect("No independent judge was run");
        assert_eq!(judge["backend"],parent["backend"]);assert_eq!(judge["model"],parent["model"]);assert_eq!(judge["status"],"completed");
        assert!(judge["attempts"].as_array().unwrap().iter().all(|a|a["review_only"]==true&&a["artifact"].is_null()));
        assert!(runs.iter().filter(|r|r["chat_id"]==chat).flat_map(|r|r["attempts"].as_array().unwrap()).flat_map(|a|a["command_jobs"].as_array().into_iter().flatten()).any(|job|job["build_environment"]==true&&job["exit_code"]==0&&job["status"]=="completed"),"A real declared build job must pass");
        let final_run=runs.iter().rev().find(|r|r["chat_id"]==chat&&r["status"]=="completed").unwrap();
        let artifact=final_run["attempts"].as_array().unwrap().last().unwrap()["artifact"].as_str().unwrap();
        let proof=hive.tool("control.artifact.read",json!({"id":chat,"artifact":artifact,"path":"docs/self-development-proof.md"})).unwrap();
        assert_eq!(proof["text"].as_str().unwrap().trim(),"Control self-development verified.");
        assert!(!crate::config::source_root().unwrap().join("docs/self-development-proof.md").exists(),"The original checkout must remain unchanged");
    }
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
    fn automatic_updates_are_attributed_to_control_in_terminal() {
        let app = App {
            detail: json!({"messages":[
                {"kind":"delegation-result","text":"Delegated task · completed"},
                {"kind":"room-notification","text":"New room posts"},
                {"kind":"design-answer","text":"Use the smaller option"}
            ]}),
            ..App::default()
        };
        let text = app.transcript();
        let labels = text.lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(labels.iter().filter(|line| *line == "You").count(), 1);
        assert_eq!(labels.iter().filter(|line| *line == "Control").count(), 2);
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
    fn service_commands_and_child_stdout_render_in_terminal() {
        let app=App{detail:json!({"messages":[{"text":"Run","commands":[{"text":"echo own","status":"completed","output":"own output\nExit 0"}]}],"agent_activity":[{"chat":"child-chat","activity":{"text":"npm install","status":"running","output":"downloaded\u{001b}[31m\n"}}]}),..App::default()};
        let text=app.transcript().to_string();
        for expected in ["echo own","own output","Exit 0","Agent · child-chat","npm install","downloaded"] {assert!(text.contains(expected),"Missing {expected}: {text}");}
        assert!(!text.contains('\u{001b}'));
    }
    #[test]
    fn visible_messages_render_as_chat_and_do_not_duplicate_the_final_reply() {
        let app=App{detail:json!({"messages":[{"text":"Check","reply":"Done","attempts":[{"activity":[{"kind":"message","text":"Checking inventory."},{"kind":"message","text":"Done"}]}]}],"agent_activity":[{"chat":"child","activity":{"kind":"message","text":"Building the shelter."}}]}),..App::default()};
        let text=app.transcript().to_string();
        assert!(text.contains("Checking inventory."));
        assert!(text.contains("Building the shelter."));
        assert_eq!(text.matches("Done").count(),1);
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
