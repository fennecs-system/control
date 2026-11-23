//! A DM's transcript in two steps: [`transcript`] copies what it shows out of
//! state and the history journal under the state lock, and
//! [`Transcript::render`] builds the page from that copy alone.
//!
//! The panels other modules own (previews, GitHub publications, the project
//! folder panel) are copied as the HTML those modules render; giving them
//! their own views is later work.
use super::{RunStatus, command_html, command_text, escape, response_from_run, valid_id};
use crate::{
    State,
    runtime::RunKind,
    usage::{Activity, StatusPart},
};
use control_context::{
    agent_policy::MessageSpeaker,
    commands::{self, Kind},
};

pub struct Transcript {
    id: String,
    /// Preview links and the GitHub panel, rendered by their modules.
    panels: String,
    listener: Option<(String, String)>,
    bot: Option<String>,
    entries: Vec<Entry>,
    agents: Vec<(String, Activity)>,
    /// The project panel when no message carries it.
    project: Option<String>,
}
enum Entry {
    Note { label: String, note: String },
    Notice(String),
    Turn(Box<Turn>),
}
struct Turn {
    position: usize,
    input: (MessageSpeaker, String),
    status: &'static str,
    /// Why a queued turn waits: dispatch is paused, or its account is held.
    hold: Option<String>,
    body: Body,
    /// The folder request's panel, on the message that asked for it.
    project: Option<Project>,
    delegated: Option<String>,
    run: Option<String>,
    /// The run's reply, which its own message events repeat.
    reply: String,
    attempts: Vec<Attempt>,
    commands: Vec<Activity>,
}
enum Body {
    Decision,
    Question(Box<crate::design::Question>),
    Child(String),
    Reply(String),
}
struct Project {
    /// A `/open` request shows only the folder panel.
    replaces_body: bool,
    part: ProjectPart,
}
enum ProjectPart {
    Importing,
    Requested,
    Panel(String),
}
struct Attempt {
    parts: Vec<StatusPart>,
    traced: bool,
    events: Result<Vec<Activity>, String>,
}

/// Everything the transcript of chat `id` shows, read in one critical section.
pub fn transcript(state: &State, id: &str) -> Result<Transcript, &'static str> {
    let session = super::viewable(&state.chats, id)?;
    let runs = crate::conversation::Conversation::of(state, session);
    let project_message = crate::projects::message_index(session);
    let entries = session
        .messages
        .iter()
        .enumerate()
        .map(|(position, message)| {
            let run = message.run_id.as_deref().and_then(|id| runs.run(id));
            let (run_status, run_reply) = response_from_run(message, run);
            // An observer's note is retained evidence with no reply: one distinct box.
            if run.is_some_and(|r| r.source.as_str() == "advisor-note") {
                let (label, note) = message.text.split_once('\n').unwrap_or((message.text.as_str(), ""));
                return Entry::Note {
                    label: label.to_owned(),
                    note: note.to_owned(),
                };
            }
            // A Control notice without a run (operator decisions): one status line.
            if message.is_notice(run) {
                return Entry::Notice(control_context::agent_policy::notice_line(&message.text));
            }
            let status = match run_status {
                RunStatus::Queued if run.is_some_and(|r| r.kind == RunKind::Steering) => "Queued · next turn",
                RunStatus::Queued if state.runtime.paused.is_some() => "Queued · dispatch paused",
                RunStatus::Queued => "Queued",
                RunStatus::Running => "Working…",
                RunStatus::Waiting if run.is_some_and(|r| r.delegation.is_some()) => "Delegated work",
                RunStatus::Waiting => "Your decision",
                RunStatus::Cancelling => "Stopping…",
                RunStatus::Cancelled => "Cancelled",
                RunStatus::Blocked => "Blocked",
                RunStatus::Failed => "Failed",
                RunStatus::Interrupted => "Interrupted",
                RunStatus::Completed | RunStatus::Continued | RunStatus::Unrecognised(_) => "",
            };
            let held = run.filter(|_| *run_status == RunStatus::Queued).and_then(|run| {
                crate::providers::accounts::hold(&state.runtime, run)
                    .map(|hold| crate::providers::accounts::held(&hold, run))
            });
            let hold = state
                .runtime
                .paused
                .clone()
                .filter(|_| *run_status == RunStatus::Queued)
                .or(held);
            let folder_request = message.run_id.is_none() && commands::tail(message.text.trim(), Kind::Open).is_some();
            let opened = session
                .project
                .as_ref()
                .and_then(|p| p.mounts.iter().find(|m| m.message_index == Some(position)));
            let folder_reply = folder_request.then(|| {
                opened
                    .map(|m| format!("Opened /workspace/{}", m.name))
                    .unwrap_or_else(|| {
                        if message.reply == crate::projects::DISMISSED {
                            message.reply.clone()
                        } else {
                            "Folder request replaced by a later request.".into()
                        }
                    })
            });
            let reply = folder_reply.as_deref().unwrap_or(run_reply);
            let question = run
                .filter(|r| matches!(r.status, RunStatus::Running | RunStatus::Waiting | RunStatus::Continued))
                .and_then(|r| {
                    r.question.as_ref().filter(|q| {
                        r.attempts
                            .last()
                            .is_some_and(|a| a.workspace_id.as_deref() == Some(&q.generation))
                    })
                });
            let body = match question {
                Some(_) if *run_status == RunStatus::Waiting => Body::Decision,
                Some(question) => Body::Question(Box::new(question.clone())),
                None => reply
                    .strip_prefix("Agent conversation: ")
                    .and_then(crate::url_names::dm_route)
                    .filter(|child| {
                        valid_id(child)
                            && state
                                .chats
                                .iter()
                                .any(|c| c.id == *child && c.parent_chat.as_deref() == Some(id))
                    })
                    .map_or_else(|| Body::Reply(reply.to_owned()), |child| Body::Child(child.to_owned())),
            };
            let project = (project_message == Some(position)).then(|| Project {
                replaces_body: folder_request,
                part: if session.project.as_ref().is_some_and(|p| p.pending || p.grant.is_none()) {
                    if crate::projects::importing(id).is_some() {
                        ProjectPart::Importing
                    } else {
                        ProjectPart::Requested
                    }
                } else {
                    ProjectPart::Panel(crate::projects::panel(&state.chats, id))
                },
            });
            let delegated = run
                .and_then(|r| r.delegation.as_ref())
                .and_then(|d| d.child.as_deref())
                .and_then(|run_id| state.runtime.runs.get(run_id))
                .and_then(|r| r.chat_id.as_deref())
                .filter(|child| valid_id(child))
                .map(str::to_owned);
            let attempts = run
                .map(|run| {
                    run.attempts
                        .iter()
                        .enumerate()
                        .map(|(index, attempt)| Attempt {
                            parts: attempt.status(index),
                            traced: attempt.context_trace.is_some(),
                            events: crate::history::activity(run.chat_id.as_deref(), attempt)
                                .map(std::borrow::Cow::into_owned)
                                .map_err(|error| error.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Entry::Turn(Box::new(Turn {
                position,
                input: (message.speaker(run), message.text.clone()),
                status,
                hold,
                body,
                project,
                delegated,
                run: run.map(|r| r.id().to_string()),
                reply: run_reply.to_owned(),
                attempts,
                commands: message
                    .run_id
                    .as_deref()
                    .map(|run| crate::services::command_activity(state, run).collect())
                    .unwrap_or_default(),
            }))
        })
        .collect();
    Ok(Transcript {
        id: id.to_owned(),
        panels: format!(
            "{}{}",
            crate::previews::links(&state.previews, id),
            crate::github::panel(&state.github_publications, &state.runtime.runs, id)
        ),
        listener: crate::rooms::listener_status(state, id).map(|listener| {
            (
                listener["room"].as_str().unwrap_or("").to_owned(),
                listener["state"].as_str().unwrap_or("").to_owned(),
            )
        }),
        bot: crate::bots::status(state, id),
        entries,
        agents: super::agent_activity(state, id)
            .into_iter()
            .map(|entry| (entry.chat.to_owned(), entry.activity))
            .collect(),
        project: (project_message.is_none()
            && session
                .project
                .as_ref()
                .is_some_and(|p| !p.pending && p.grant.is_some()))
        .then(|| crate::projects::panel(&state.chats, id)),
    })
}

impl Transcript {
    pub fn render(&self) -> String {
        let id = self.id.as_str();
        let mut html = "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/runtime.css'><link rel='stylesheet' href='/workspace.css'></head><body class='transcript'>".to_owned();
        html.push_str(&self.panels);
        if let Some((room, state)) = &self.listener {
            html.push_str(&format!(
                "<p class='message-status'>Room #{} · {}</p>",
                escape(room),
                escape(state)
            ));
        }
        if let Some(bot) = &self.bot {
            html.push_str(&format!("<p class='message-status'>{}</p>", escape(bot)));
        }
        for entry in &self.entries {
            match entry {
                Entry::Note { label, note } => html.push_str(&format!("<article class='advisor-note' aria-label='Observer note'><p class='advisor-note-label'>{} · read-only evidence</p><p class='preserve'>{}</p></article>", escape(label), escape(note))),
                Entry::Notice(line) => html.push_str(&format!(
                    "<p class='message-status preserve' aria-label='Control notice'>{}</p>",
                    escape(line)
                )),
                Entry::Turn(turn) => turn.render(id, &mut html),
            }
        }
        if !self.agents.is_empty() {
            let agents = self
                .agents
                .iter()
                .map(|(chat, _)| chat)
                .collect::<std::collections::HashSet<_>>()
                .len();
            html.push_str(&format!("<details class='run-activity' data-transcript-key='agent-activity'><summary>Agent activity · {agents} active {}</summary>",if agents == 1 {"agent"} else {"agents"}));
            for (chat, activity) in &self.agents {
                html.push_str(&format!(
                    "<p class='message-status'><a target='_top' href='/dm/{}'>View agent ↗</a></p>{}",
                    escape(chat),
                    command_html(activity)
                ));
            }
            html.push_str("</details>");
        }
        if let Some(panel) = &self.project {
            html.push_str(panel);
        }
        html.push_str("<div id='latest'></div></body></html>");
        html
    }
}

impl Turn {
    fn render(&self, id: &str, html: &mut String) {
        let position = self.position;
        let status = if self.status.is_empty() {
            String::new()
        } else {
            format!("<span class='message-status' role='status'>{}</span>", self.status)
        };
        let status = match &self.hold {
            Some(reason) => format!("{status}<p class='message-status'>{}</p>", escape(reason)),
            None => status,
        };
        let body = match &self.body {
            Body::Decision => "<p class='message-status'>Decision requested · see Requests</p>".to_owned(),
            Body::Question(question) => crate::design::render(question, id, false),
            Body::Child(child) => {
                format!("<p class='preserve'><a href='/dm/{child}' target='_top'>Agent conversation ↗</a></p>")
            }
            Body::Reply(reply) => format!("<p class='preserve'>{}</p>", escape(reply)),
        };
        let body = match &self.project {
            Some(project) => format!(
                "{}{}",
                if project.replaces_body { String::new() } else { body },
                match &project.part {
                    ProjectPart::Importing => {
                        "<p class='message-status' role='status'>Importing folder… · see Requests</p>".to_owned()
                    }
                    ProjectPart::Requested => "<p class='message-status'>Folder request · see Requests</p>".to_owned(),
                    ProjectPart::Panel(panel) => panel.clone(),
                }
            ),
            None => body,
        };
        let input = match &self.input {
            (MessageSpeaker::User, text) => format!(
                "<article class='user-message' aria-label='You'><p class='preserve'>{}</p></article>",
                command_text(text)
            ),
            (MessageSpeaker::Control, text) => format!(
                "<p class='message-status' aria-label='Control update'>{}</p>",
                escape(text)
            ),
        };
        html.push_str(&format!(
            "{input}<article class='assistant-message' aria-label='Control'>{status}{body}</article>"
        ));
        if let Some(child) = &self.delegated {
            html.push_str(&format!(
                "<p class='message-status'><a href='/dm/{}' target='_top'>Delegated conversation ↗</a></p>",
                escape(child)
            ));
        }
        for (index, attempt) in self.attempts.iter().enumerate() {
            // One compact line per attempt; titles carry the full wording.
            let parts = &attempt.parts;
            let input = self.run.as_deref().filter(|_| attempt.traced).map(|run| format!("<a target='_top' href='/dm/{}/context?run={}&amp;attempt={}' title='Control input for attempt {}'>input ↗</a>",escape(id),escape(run),index+1,index+1));
            if parts.len() > 1 || input.is_some() {
                let mut line = parts
                    .iter()
                    .map(|p| format!("<span title='{}'>{}</span>", escape(&p.title), escape(&p.text)))
                    .collect::<Vec<_>>();
                line.extend(input);
                html.push_str(&format!(
                    "<p class='message-status attempt-status'>{}</p>",
                    line.join(" · ")
                ));
            }
            let events = match &attempt.events {
                Ok(events) => events,
                Err(error) => {
                    html.push_str(&format!("<p class='message-status' role='alert'>{}</p>", escape(error)));
                    continue;
                }
            };
            for event in events.iter().filter(|event| event.kind == "file_change") {
                let patch = &event.output;
                if patch.is_empty() {
                    continue;
                }
                let mut body = String::new();
                for line in patch.lines() {
                    let class = match control_diff::kind(line) {
                        control_diff::Kind::Add => "diff-add",
                        control_diff::Kind::Remove => "diff-remove",
                        control_diff::Kind::Header => "diff-header",
                        control_diff::Kind::Context => "diff-context",
                    };
                    body.push_str(&format!("<span class='diff-line {class}'>{}</span>", escape(line)));
                }
                html.push_str(&format!("<details class='edit-diff' data-transcript-key='{position}-{index}-diff-{}'><summary>Changes · attempt {} <span>{}</span></summary><pre aria-label='File changes'>{body}</pre></details>",escape(&event.id),index+1,escape(&event.text)));
            }
            for event in events
                .iter()
                .filter(|event| event.kind == "message" && event.text.trim() != self.reply.trim())
            {
                html.push_str(&format!("<article class='assistant-message' data-transcript-key='{position}-{index}-message-{}'><p class='preserve'>{}</p></article>",escape(&event.id),escape(&event.text)));
            }
            let mut events = events
                .iter()
                .filter(|event| event.kind != "file_change" && event.kind != "message")
                .enumerate()
                .peekable();
            if events.peek().is_none() {
                continue;
            }
            html.push_str(&format!(
                "<details class='run-activity' data-transcript-key='{position}-{index}-activity'><summary>Recent activity · attempt {}</summary>", index + 1
            ));
            for (event_index, event) in events {
                html.push_str(&format!("<details class='command-activity' data-transcript-key='{position}-{index}-event-{event_index}'><summary><span>{}</span> <code>{}</code></summary><pre>{}</pre></details>", escape(&event.status), escape(&event.text), escape(&event.output)));
            }
            html.push_str("</details>");
        }
        if !self.commands.is_empty() {
            html.push_str(&format!("<details class='run-activity' data-transcript-key='{position}-commands'><summary>Commands · {}</summary>",self.commands.len()));
            for event in &self.commands {
                html.push_str(&command_html(event));
            }
            html.push_str("</details>");
        }
    }
}
