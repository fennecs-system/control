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
#[cfg(test)]
use reqwest::Url;
use serde_json::{json, Value};
use std::{
    io,
    time::{Duration, Instant},
};

#[cfg(test)]
use reqwest::blocking::Client;
#[cfg(test)]
use std::fs;
use control_context::commands;
mod command_menu;
mod history;

fn command_line(text: &str) -> Line<'static> {
    if let Some(highlight) = command_menu::highlight(text) {
        Line::from(vec![Span::styled(text[highlight.clone()].to_owned(), Style::default().fg(Color::Rgb(181, 160, 255))), Span::raw(text[highlight.end..].to_owned())])
    } else {
        Line::from(text.to_owned())
    }
}
use crate::client::Hive;
fn failure(message:impl std::fmt::Display)->io::Error {io::Error::other(message.to_string())}
#[cfg(test)]
use crate::client::local_server;
#[derive(Default)]
struct App {
    history:history::layout::Cache,
    show_activity:bool,
    command_selection: usize,
    commands_hidden: bool,
    menu: Option<Menu>,
    inspector: Option<(String, String)>,
    follow: bool,
    chats: Vec<Value>,
    active: String,
    detail: Value,
    input: String,
    status: String,
    scroll: usize,
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
fn folded_activity(lines:&mut Vec<Line<'static>>,events:&[Value]) {
    let mut count=0;let mut failed=0;let mut latest=None;let mut failure=None;
    for event in events.iter().filter(|event|event["kind"]!="message") {
        count+=1;latest=Some(event);
        if matches!(event["status"].as_str(),Some("failed"|"error"|"interrupted"|"cancelled")) {failed+=1;failure=Some(event);}
    }
    if let Some(latest)=latest {
        let detail=clean(latest["text"].as_str().or(latest["command"].as_str()).unwrap_or(""));
        let detail=detail.lines().next().unwrap_or("").chars().take(48).collect::<String>();
        let failures=if failed>0 {format!(" · {failed} failed")}else{String::new()};
        lines.push(Line::styled(format!("  ↳ {count} actions{failures} · {detail}"),Style::default().fg(Color::Rgb(133,133,143))));
    }
    if let Some(event)=failure {
        let output=clean(event["output"].as_str().unwrap_or(""));
        let output=output.lines().find(|line|!line.trim().is_empty()).unwrap_or("See activity details").chars().take(240).collect::<String>();
        lines.push(Line::styled(format!("  ! {output}"),Style::default().fg(Color::Rgb(237,139,155))));
    }
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
    fn set_detail(&mut self,detail:Value) {self.detail=detail;self.history.invalidate();}
    fn layout_history(&mut self,width:u16) {
        let width=history::content_width(width);
        if self.history.dirty() {let text=self.transcript();self.history.update(text,width);}
        else if self.history.width()!=width {self.history.resize(width);}
    }
    fn command_matches(&self) -> Vec<command_menu::Entry> {
        if self.commands_hidden || self.menu.is_some() { return Vec::new(); }
        command_menu::matching(&self.input).collect()
    }
    /// Completion edits the draft only. Sending remains an explicit Enter.
    fn complete_command(&mut self) {
        if let Some(entry) = self.command_matches().get(self.command_selection).copied() {
            self.input = format!("{} ", entry.text);
            self.command_selection = 0;
            self.status.clear();
        }
    }
    fn input_changed(&mut self) {
        self.command_selection = 0;
        self.commands_hidden = false;
    }
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
                || self.chats.iter().find(|s|s["id"]==self.active).map(|s|&s["port_revision"]) != Some(&self.detail["port_revision"])
                || self.chats.iter().find(|s|s["id"]==self.active).map(|s|&s["bot_revision"]) != Some(&self.detail["bot_revision"]))
        {
            self.set_detail(hive.tool("control.chat.read", json!({"id":self.active}))?);
        }
        Ok(())
    }
    fn new_chat(&mut self, hive: &mut Hive) -> io::Result<()> {
        self.history.invalidate();
        self.menu = None;
        self.inspector = None;
        let value = hive.tool("control.chat.create", json!({}))?;
        self.active = value["id"].as_str().unwrap_or("").into();
        self.set_detail(value);
        self.scroll = 0;
        self.follow = true;
        self.refresh(hive)
    }
    fn submit(&mut self, hive: &mut Hive) -> io::Result<bool> {
        self.history.invalidate();
        let text = self.input.trim().to_owned();
        self.menu = None;
        match text.as_str() {
            _ if text=="/activity" || text.starts_with("/activity ") => {
                self.show_activity=match text.strip_prefix("/activity").unwrap().trim() {
                    ""=>!self.show_activity,"full"=>true,"compact"=>false,_=>return Err(failure("Use /activity, /activity full or /activity compact."))
                };
                self.status=if self.show_activity {"Tool details expanded · /activity folds them"}else{"Tool activity compact · failures remain visible"}.into();
            }
            "/compact" => {
                if self.active.is_empty() { return Err(failure("Open a conversation first")); }
                self.set_detail(hive.tool("control.chat.read", json!({"id":self.active}))?);
                self.inspector = Some(("Compaction tree".into(), serde_json::to_string_pretty(&self.detail["compaction"]).map_err(failure)?));
                self.scroll = 0; self.follow = false;
                self.status = "Compaction is independent of the chat model · /chat returns".into();
            }
            _ if text.starts_with("/port ") => {
                let parts=text.split_whitespace().collect::<Vec<_>>();
                if !(3..=4).contains(&parts.len()) || !matches!(parts[1],"allow"|"deny") || (parts.len()==4 && parts[3]!="--loopback") {
                    return Err(failure("Use /port allow ID [--loopback] or /port deny ID. Review the exact target before allowing loopback."));
                }
                let request=self.detail["pending_ports"].as_array().and_then(|ports|ports.iter().find(|port|port["port_id"]==parts[2]))
                    .ok_or_else(||failure("Port request changed. Refresh the conversation before approving it."))?;
                let loopback=parts.get(3)==Some(&"--loopback");
                if parts[1]=="allow" && request["loopback_required"]==true && !loopback {return Err(failure("This exact loopback target requires /port allow ID --loopback."));}
                let decision = if parts[1] == "allow" { crate::client::PortDecision::Allow } else { crate::client::PortDecision::Deny };
                hive.decide_port(&self.active, parts[2], decision, loopback)?;
                self.set_detail(hive.tool("control.chat.read",json!({"id":self.active}))?);
                self.status="Port decision saved".into();
            }
            "/open" => {
                if !self.active.is_empty() {
                    self.set_detail(hive.tool("control.chat.read",json!({"id":self.active}))?);
                }
                self.inspector=None;self.follow=true;self.scroll=usize::MAX;
                self.status=if pending_project(&self.detail).is_some() {"Review the folder request below; /trust confirms it"}else{"Use /open /absolute/path or /open --as NAME /absolute/path"}.into();
            }
            _ if text=="/trust" || text.starts_with("/trust ")=>{
                let project=pending_project(&self.detail).ok_or_else(||failure("No folder request to approve. Use /open /absolute/path."))?;
                let displayed=project["request"].as_str().filter(|request|!request.is_empty()).ok_or_else(||failure("Project request changed. Use /open to review it."))?;
                let request=text.strip_prefix("/trust ").map(str::trim).unwrap_or(displayed).to_owned();
                if request!=displayed {return Err(failure("Project request changed. Use /open to review it."));}
                let approval = hive.approve_project(&self.active, &request)?;
                self.set_detail(approval.detail);
                approval.result.map_err(failure)?;
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
            "/help" => {
                self.inspector = Some(("Help".into(), command_menu::help()));
                self.scroll = 0;
                self.follow = false;
                self.status = "PageUp / PageDown scroll · Esc or /chat returns".into();
            }
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
            "/chat"=>{self.inspector=None;self.follow=true;self.scroll=usize::MAX;}
            _ if text.starts_with("/use ")=>{
                let names=text[5..].split(|c:char|c.is_whitespace()||c==',').filter(|s|!s.is_empty()).collect::<Vec<_>>();
                let lenses=if names==["none"]{Vec::new()}else{names};
                // Resolve before creating a conversation: invalid input has no side effects.
                hive.tool("control.lenses.resolve",json!({"lenses":lenses}))?;
                if self.active.is_empty(){self.new_chat(hive)?;}
                self.set_detail(hive.tool("control.chat.lenses",json!({"id":self.active,"lenses":lenses}))?);
                self.inspector=None;self.status=format!("Future messages: {}",lens_label(&self.detail));self.follow=true;
            }
            _ if text.starts_with("/resume ") => { self.active = text[8..].trim().into(); self.inspector=None;self.refresh(hive)?; }
            _ if text.starts_with('/') && commands::parse(&text).is_none()=>return Err(failure("Unknown command. Use /help.")),
            _ if !text.is_empty() => {
                if control_context::limits::message_too_long(&text) { return Err(failure(control_context::limits::CHAT_MESSAGE_ERROR)); }
                if self.active.is_empty() { self.new_chat(hive)?; }
                self.set_detail(hive.tool("control.chat.send",json!({"id":self.active,"message":text}))?);
                self.inspector=None;
                self.scroll = usize::MAX;
                self.follow = true;
                self.status = if text.starts_with("/steer ") { "Queued for next turn" } else if self.detail["mode"] == "improve" { "Improve · sent" } else { "Sent" }.into();
            }
            _ => {}
        }
        self.input.clear();
        self.input_changed();
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
                let user=control_context::agent_policy::MessageSpeaker::for_run_kind(message["kind"].as_str())==control_context::agent_policy::MessageSpeaker::User;
                if user {
                    lines.push(history::user_line(Line::from(" ")));
                    lines.extend(clean(message["text"].as_str().unwrap_or("")).lines().map(command_line).map(history::user_line));
                    lines.push(history::user_line(Line::from(" ")));
                } else {
                    lines.push(Line::styled("◇ Event",Style::default().fg(Color::Rgb(133,133,143))));
                    lines.extend(clean(message["text"].as_str().unwrap_or("")).lines().map(command_line));
                }
                lines.push(Line::from(""));
                let status=message["status"].as_str().unwrap_or("");
                if !matches!(status,""|"completed") {
                    let status=if status=="queued" && message["kind"]=="steering" {"queued · next turn"} else {status};
                    let color=if matches!(status,"failed"|"interrupted"|"cancelled") {Color::Rgb(237,139,155)}else{history::PURPLE};
                    lines.push(Line::styled(format!("● {}",clean(status)),Style::default().fg(color)));
                }
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
                            if !self.show_activity {folded_activity(&mut lines,events);}
                            for event in events {
                                if event["kind"]=="message" {
                                    let text=event["text"].as_str().unwrap_or("");
                                    if text.trim()!=message["reply"].as_str().unwrap_or("").trim() {
                                        lines.extend(clean(text).lines().map(|line|Line::from(line.to_owned())));
                                    }
                                    continue;
                                }
                                if !self.show_activity {continue;}
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
                                            let color=if patch {match crate::diff::kind(s) {
                                                crate::diff::Kind::Add=>{let (r,g,b)=crate::diff::ADD_RGB;Some(Color::Rgb(r,g,b))},
                                                crate::diff::Kind::Remove=>{let (r,g,b)=crate::diff::REMOVE_RGB;Some(Color::Rgb(r,g,b))},
                                                crate::diff::Kind::Header=>Some(Color::Rgb(133,133,143)),
                                                crate::diff::Kind::Context=>None,
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
                    if self.show_activity {for command in commands {service_command_lines(&mut lines,command);}}
                    else {folded_activity(&mut lines,commands);for command in commands.iter().filter(|command|command["kind"]=="message") {service_command_lines(&mut lines,command);}}
                }
                lines.push(Line::from(""));
            }
        }
        if let Some(commands)=self.detail["agent_activity"].as_array() {
            for command in commands {
                lines.push(Line::styled(format!("Agent · {}",clean(command["chat"].as_str().unwrap_or(""))),Style::default().fg(Color::Rgb(181,160,255))));
                if self.show_activity || command["activity"]["kind"]=="message" {service_command_lines(&mut lines,&command["activity"]);}
                else {folded_activity(&mut lines,std::slice::from_ref(&command["activity"]));}
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
        if let Some(bots)=self.detail["pending_bots"].as_array() {for bot in bots {
            lines.push(Line::styled(format!("Bot request · {}",clean(bot["chat_id"].as_str().unwrap_or(""))),Style::default().fg(Color::Rgb(181,160,255))));
            let id=clean(bot["request_id"].as_str().unwrap_or(""));
            lines.push(Line::from(format!("/bot allow {id} · /bot deny {id}")));
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
        app.set_detail(hive.tool("control.project.request",json!({"id":app.active,"path":path.to_string_lossy()}))?);
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
            let matches = app.command_matches();
            let selection = app.command_selection.min(matches.len().saturating_sub(1));
            let suggestions_height = if matches.is_empty() { 0 } else { matches.len().min(5) as u16 + 1 };
            terminal.draw(|frame| {
                let layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(5),
                        Constraint::Length(suggestions_height),
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
                app.layout_history(layout[1].width);
                let max_scroll=app.history.len().saturating_sub(usize::from(layout[1].height));
                if app.follow || app.scroll == usize::MAX {
                    app.scroll = max_scroll;
                }
                else {app.scroll=app.scroll.min(max_scroll);}
                let text=app.history.window(app.scroll.min(max_scroll),usize::from(layout[1].height));
                frame.render_widget(history::History{paragraph:Paragraph::new(text),scroll:0}, layout[1]);
                let composer_style = Style::default()
                    .bg(Color::Rgb(36, 32, 47))
                    .fg(Color::Rgb(233, 232, 237));
                if !matches.is_empty() {
                    let start = selection.saturating_sub(4);
                    let mut lines = matches.iter().enumerate().skip(start).take(5).map(|(index, entry)| {
                        Line::from(vec![Span::styled(format!("{} {}", if index == selection { "›" } else { " " }, entry.text), Style::default().fg(Color::Rgb(181, 160, 255))), Span::raw(format!("  {}", entry.description))])
                    }).collect::<Vec<_>>();
                    lines.push(Line::styled("↑ ↓ choose · Tab completes · Esc dismisses", Style::default().fg(Color::Rgb(133, 133, 143))));
                    frame.render_widget(Paragraph::new(lines), layout[2]);
                }
                frame.render_widget(Block::default().style(composer_style), layout[3]);
                let input_area = layout[3].inner(ratatui::layout::Margin {
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
                    layout[4],
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
                let matches = app.command_matches();
                if !matches.is_empty() && key.modifiers.is_empty() {
                    match key.code {
                        KeyCode::Up => { app.command_selection = app.command_selection.saturating_sub(1); continue; }
                        KeyCode::Down => { app.command_selection = (app.command_selection + 1).min(matches.len() - 1); continue; }
                        KeyCode::Esc => { app.commands_hidden = true; continue; }
                        _ => {}
                    }
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
                            app.scroll = menu.selected.saturating_sub(6);
                            app.history.invalidate();
                            continue;
                        }
                        KeyCode::Char(' ') if app.menu.as_ref().is_some_and(|m| m.multiple) => {
                            app.menu.as_mut().unwrap().toggle();
                            app.history.invalidate();
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
                            app.history.invalidate();
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
                        app.input.push('\n');
                        app.input_changed();
                    }
                    KeyCode::Enter => match app.submit(&mut hive) {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(e) => app.status = e.to_string(),
                    },
                    KeyCode::Backspace => {
                        app.input.pop();
                        app.input_changed();
                    }
                    KeyCode::Tab => app.complete_command(),
                    KeyCode::Esc => {
                        app.history.invalidate();
                        app.menu = None;
                        app.inspector = None;
                        app.follow = true;
                        app.status.clear();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.input.chars().count() < control_context::limits::CHAT_MESSAGE_CHARS {
                            app.input.push(c);
                            app.input_changed();
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
                        app.scroll = usize::MAX;
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
mod tests;



#[cfg(test)]
#[path = "terminal/tests/profiling.rs"]
mod profiling;
