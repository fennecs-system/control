use crate::{State, chat, storage};
use serde_json::{Value, json};
use std::io::Write;

fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, value)| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}
/// Whether the browser remembers the DMs picker as open.
fn picker_open(header: &str) -> bool {
    cookie(header, "control_conversations") == Some("open")
}
/// Render the remembered conversations panel state so it never flashes after load.
fn session_page(page: &str, picker_open: bool) -> String {
    let page = page.replace("<body>", "<body class='session-view'>");
    if picker_open {
        page.replacen(
            "<details class='conversation-toggle'>",
            "<details class='conversation-toggle' open>",
            1,
        )
    } else {
        page
    }
}
/// A chat page; a rejected submission shows its escaped error and keeps the draft.
fn chat_page(state: &State, lens: &str, id: &str, error: &str, draft: &str, header: &str) -> String {
    let page = session_page(&crate::render(state, "session", lens, id), picker_open(header));
    if error.is_empty() {
        page
    } else {
        chat::rejected(&page, error, draft)
    }
}
/// A chat page that loads the Rust/WASM client.
fn respond_chat(stream: &mut impl Write, status: &str, html: &str, extra: &str) {
    let page = crate::web_assets::with_client(html);
    let _ = control_http::respond(
        stream,
        status,
        "text/html; charset=utf-8",
        page.as_bytes(),
        extra,
        control_http::Policy::Chat,
    );
}

fn change(state: &mut State, name: &str, arguments: &Value) -> Result<Value, std::borrow::Cow<'static, str>> {
    change_with_save(state, name, arguments, storage::save)
}
// Both native forms and the WASM composer use the same checkpointed command.
fn send_message(
    stream: &mut impl Write,
    state: &mut State,
    id: &str,
    lens: &str,
    header: &str,
    fields: &std::collections::HashMap<String, String>,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) {
    let asynchronous = header.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("accept") && value.split(',').any(|mime| mime.trim() == "application/json")
        })
    });
    if fields.get("message").is_some_and(|message| message.trim() == "/open") {
        match chat::viewable(&state.chats, id) {
            Ok(_) => {
                let _ = control_http::respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!("Location: /dm/{id}/project\r\n"),
                    control_http::Policy::Embedded,
                );
            }
            Err(message) => respond_chat(
                stream,
                "400 Bad Request",
                &chat_page(state, lens, id, message, "/open", header),
                "",
            ),
        }
        return;
    }
    let mut arguments = json!({"id":id,"message":fields.get("message").map(String::as_str).unwrap_or("")});
    if fields.contains_key("composition") {
        arguments["lenses"] = chat::form_lenses(fields, "none");
    }
    let result = change_with_save(state, "control.chat.send", &arguments, save);
    if asynchronous {
        let (status, value) = match result {
            Ok(_) => {
                let session = state.chats.iter().find(|chat| chat.id == id).unwrap();
                (
                    "200 OK",
                    json!({"title":crate::callsigns::name_or_id(&state.chats,id),"improve":session.improve_mode}),
                )
            }
            Err(message) => ("400 Bad Request", json!({"error":message})),
        };
        let _ = control_http::respond(
            stream,
            status,
            "application/json",
            value.to_string().as_bytes(),
            "",
            control_http::Policy::Embedded,
        );
    } else {
        match result {
            Ok(_) => {
                let _ = control_http::respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!("Location: /dm/{id}\r\n"),
                    control_http::Policy::Embedded,
                );
            }
            Err(message) => respond_chat(
                stream,
                "400 Bad Request",
                &chat_page(
                    state,
                    lens,
                    id,
                    &message,
                    fields.get("message").map(String::as_str).unwrap_or(""),
                    header,
                ),
                "",
            ),
        }
    }
}
pub(crate) fn change_with_save(
    state: &mut State,
    name: &str,
    arguments: &Value,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, std::borrow::Cow<'static, str>> {
    let related = if name == "control.chat.send" {
        arguments["message"]
            .as_str()
            .and_then(|text| control_context::commands::tail(text.trim(), control_context::commands::Kind::Answer))
            .and_then(|answer| answer.trim().split_once(char::is_whitespace))
            .and_then(|(question, _)| {
                crate::design::answer_target(state, arguments["id"].as_str().unwrap_or(""), question).ok()
            })
            .map(str::to_owned)
    } else if name == "control.chat.ask" && arguments["id"].is_null() {
        crate::design::desk(state).map(str::to_owned)
    } else if name == "control.chat.withdraw" {
        crate::design::question_chat(state, arguments["question_id"].as_str().unwrap_or("")).map(str::to_owned)
    } else {
        None
    };
    transact(
        state,
        arguments["id"].as_str(),
        related.as_deref(),
        |state| {
            if crate::tool_registry::lookup(name).is_some_and(|tool| tool.route == crate::tool_registry::Route::Project)
            {
                crate::projects::request_named(
                    state,
                    arguments["id"].as_str().unwrap_or(""),
                    arguments["path"].as_str().unwrap_or(""),
                    arguments["name"].as_str(),
                    crate::projects::Requester::Agent { run: None },
                )
                .map_err(Into::into)
            } else if crate::tool_registry::lookup(name)
                .is_some_and(|tool| tool.route == crate::tool_registry::Route::Room)
            {
                crate::rooms::tool(state, name, arguments).map_err(Into::into)
            } else {
                chat::tool(state, name, arguments)
            }
        },
        save,
    )
}
/// Apply `operation` and checkpoint once; any failure restores chats, runs and posts together.
fn transact(
    state: &mut State,
    id: Option<&str>,
    related: Option<&str>,
    operation: impl FnOnce(&mut State) -> Result<Value, std::borrow::Cow<'static, str>>,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, std::borrow::Cow<'static, str>> {
    let targets: std::collections::HashSet<_> = id.into_iter().chain(related).collect();
    let checkpoint = crate::chat::Checkpoint::capture(state, &targets);
    let result = operation(state);
    if result.is_err() || save(state).is_err() {
        checkpoint.restore(state);
        return result.and(Err("Could not save the chat. Try again.".into()));
    }
    result
}

const OPERATOR: control_mcp::Server = control_mcp::Server {
    name: "control",
    version: env!("CARGO_PKG_VERSION"),
    instructions: None,
    invalid: (control_mcp::INVALID_REQUEST, "Invalid request."),
    unknown: (control_mcp::METHOD_NOT_FOUND, "Method not found."),
};
/// The operator's tools, run under the state lock the caller holds.
struct Operator<'a>(&'a mut State);
impl control_mcp::Tools for Operator<'_> {
    fn list(&mut self) -> Vec<Value> {
        crate::tool_registry::http_definitions()
    }
    fn call(&mut self, call: control_mcp::Call<'_>) -> Value {
        let (state, name, arguments) = (&mut *self.0, call.name, call.arguments);
        let result: Result<Value, std::borrow::Cow<'static, str>> = if crate::knowledge_tools::known(name) {
            crate::knowledge_tools::local(state, name, arguments, storage::save).map_err(Into::into)
        } else if crate::tool_registry::checkpoint(name) {
            change(state, name, arguments)
        } else {
            match crate::tool_registry::lookup(name).map(|tool| tool.route) {
                Some(crate::tool_registry::Route::Artifacts) => {
                    crate::artifacts::tool(state, name, arguments).map_err(Into::into)
                }
                Some(crate::tool_registry::Route::Room) => {
                    crate::rooms::tool(state, name, arguments).map_err(Into::into)
                }
                Some(crate::tool_registry::Route::Lens) => {
                    crate::lens_library::tool(crate::lens_library::Sources::of(state), name, arguments)
                        .map_err(Into::into)
                }
                // Served by `mcp_http` before the state lock is taken.
                Some(crate::tool_registry::Route::Package) => {
                    Err("Package tools are served by the HTTP MCP endpoint.".into())
                }
                _ => chat::tool(state, name, arguments),
            }
        };
        match result {
            Ok(value) => control_mcp::data(&value),
            Err(message) => control_mcp::failure(&message),
        }
    }
}

/// The chat pages that only read state: `GET /dm/{id}`, its transcript and
/// its DM picker. The server copies their view under the state lock and
/// renders it after the lock is released (`crate::Read`).
pub(crate) enum ChatPage {
    Page { id: String, picker_open: bool },
    Transcript(String),
    Conversations(String),
}
impl ChatPage {
    pub(crate) fn of(method: &str, path: &str, header: &str) -> Option<Self> {
        let route = path.strip_prefix("/dm/").filter(|_| method == "GET")?;
        let (id, operation) = route.split_once('/').unwrap_or((route, ""));
        let id = Some(id).filter(|id| chat::valid_id(id))?.to_owned();
        match operation {
            "" => Some(Self::Page {
                id,
                picker_open: picker_open(header),
            }),
            "transcript" => Some(Self::Transcript(id)),
            "conversations" => Some(Self::Conversations(id)),
            _ => None,
        }
    }
}
pub(crate) enum ChatView {
    Page {
        id: String,
        picker_open: bool,
        page: Result<chat::Page, Absent>,
    },
    Transcript(Result<chat::Transcript, &'static str>),
    Conversations(Result<crate::navigation::Picker, &'static str>),
}
/// Why a DM page is not shown.
pub(crate) enum Absent {
    /// A Discord DM whose first message has not arrived yet.
    Waiting,
    Refused(&'static str),
}
pub(crate) const CHAT_PAGES: crate::Read<ChatPage, ChatView> = crate::Read {
    view: |state, page| match page {
        ChatPage::Page { id, picker_open } => ChatView::Page {
            page: chat::Page::of(&state.chats, &state.scores, &state.lens_library, &id).map_err(|message| {
                if chat::discord_actor(&id)
                    .is_some_and(|actor| crate::identity::authorize(&actor, crate::identity::Action::Task).is_ok())
                {
                    Absent::Waiting
                } else {
                    Absent::Refused(message)
                }
            }),
            id,
            picker_open,
        },
        ChatPage::Transcript(id) => ChatView::Transcript(chat::transcript(state, &id)),
        ChatPage::Conversations(id) => ChatView::Conversations(
            chat::viewable(&state.chats, &id).map(|_| crate::navigation::Picker::of(&state.chats, &id)),
        ),
    },
    render: |view| {
        let embedded = |status, mime, body: String| crate::Reply {
            status,
            mime,
            body,
            extra: String::new(),
            policy: control_http::Policy::Embedded,
        };
        let html = |result: Result<String, &'static str>| match result {
            Ok(html) => embedded("200 OK", "text/html; charset=utf-8", html),
            Err(message) => embedded("404 Not Found", "text/plain", message.to_owned()),
        };
        match view {
            ChatView::Page {
                id,
                picker_open,
                page: Ok(page),
            } => crate::Reply {
                status: "200 OK",
                mime: "text/html; charset=utf-8",
                body: crate::web_assets::with_client(&session_page(
                    &crate::shell("session", &page.render()),
                    picker_open,
                )),
                extra: format!("Set-Cookie: control_recent_chat={id}; Path=/; HttpOnly; SameSite=Strict\r\n"),
                policy: control_http::Policy::Chat,
            },
            ChatView::Page {
                page: Err(Absent::Waiting),
                ..
            } => embedded("202 Accepted", "text/html; charset=utf-8", "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><meta http-equiv='refresh' content='2'><title>Control / DM</title></head><body><p>Waiting for the Discord message…</p></body></html>".to_owned()),
            ChatView::Page {
                page: Err(Absent::Refused(message)),
                ..
            } => embedded("404 Not Found", "text/plain", message.to_owned()),
            ChatView::Transcript(transcript) => html(transcript.map(|transcript| transcript.render())),
            ChatView::Conversations(picker) => html(picker.map(|picker| picker.render())),
        }
    },
};

/// The operator MCP: `/mcp` over HTTP and the chat bridge's in-process calls.
pub fn rpc(state: &mut State, request: &Value) -> Value {
    control_mcp::serve(&OPERATOR, request, &mut Operator(state))
}

pub fn handle(
    stream: &mut impl Write,
    state: &mut State,
    method: &str,
    target: &str,
    lens: &str,
    header: &str,
    body: &[u8],
) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method == "GET" && path == "/" && crate::fields(query).get("view").is_some_and(|v| v == "evolution") {
        let _ = control_http::respond(
            stream,
            "303 See Other",
            "text/plain",
            b"",
            "Location: /dms?improve=1\r\n",
            control_http::Policy::Embedded,
        );
        return true;
    }
    if path == crate::history_view::PATH && method == "GET" {
        // Older history URLs redirect here (`url_names::history`). The client
        // loads a DM's turns when its row opens. The server serves this and
        // the turns before taking the lock; a caller holding state gets the
        // same bytes.
        let filter = crate::history_view::Filter::from(&crate::fields(query));
        let _ = crate::history_view::HISTORY.reply(state, filter).write(stream);
        return true;
    }
    if path == "/bots/history/turns" && method == "GET" {
        let filter = crate::history_view::Filter::from(&crate::fields(query));
        let _ = crate::history_view::DM_TURNS.reply(state, filter).write(stream);
        return true;
    }
    if path == "/bots/graveyard" && method == "GET" {
        // The scroll loader's next page: rows only, no page shell. The server
        // serves it before taking the lock; a caller holding state gets the
        // same bytes.
        let query = crate::graveyard::Query::from(&crate::fields(query));
        let _ = crate::graveyard::NEXT_PAGE.reply(state, query).write(stream);
        return true;
    }
    if path == "/dms" && method == "GET" {
        let query = crate::fields(query);
        if !query.contains_key("new") && !query.contains_key("discuss") && !query.contains_key("improve") {
            let recent = cookie(header, "control_recent_chat").filter(|id| chat::valid_id(id));
            if let Some(id) = recent.filter(|id| chat::viewable(&state.chats, id).is_ok()) {
                let _ = control_http::respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!("Location: /dm/{id}\r\n"),
                    control_http::Policy::Embedded,
                );
                return true;
            }
        }
        let html = chat::landing_mode(
            state,
            lens,
            query.get("discuss").map(String::as_str),
            query.get("improve").is_some_and(|v| v == "1"),
        );
        respond_chat(
            stream,
            "200 OK",
            &session_page(&crate::shell("session-list", &html), picker_open(header)),
            "",
        );
        return true;
    }
    if ["/dms", "/improve"].contains(&path) && method == "POST" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let chosen = fields.get("lens").map(String::as_str).unwrap_or(lens);
        let arguments = json!({"lenses":chat::form_lenses(&fields,lens),"room":fields.get("room").filter(|room|!room.is_empty()),"design_context":fields.get("design_context"),"mode":if path == "/improve" {json!("improve")} else {Value::Null}});
        let text = fields
            .get(if path == "/improve" { "objective" } else { "message" })
            .filter(|text| !text.trim().is_empty());
        // Create and first message commit together, so a rejected message leaves no empty chat.
        let result = transact(
            state,
            None,
            None,
            |state| {
                let value = chat::tool(state, "control.chat.create", &arguments)?;
                if let Some(text) = text {
                    chat::tool(state, "control.chat.send", &json!({"id":value["id"],"message":text}))?;
                }
                Ok(value)
            },
            storage::save,
        );
        match result {
            Ok(value) => {
                let _ = control_http::respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!(
                        "Location: /dm/{}\r\n{}",
                        value["id"].as_str().unwrap(),
                        crate::lens_ui::cookie(chosen)
                    ),
                    control_http::Policy::Embedded,
                );
            }
            Err(message) => {
                let html = chat::rejected(
                    &chat::landing_mode(state, lens, fields.get("design_context").map(String::as_str), false),
                    &message,
                    text.map(String::as_str).unwrap_or(""),
                );
                respond_chat(
                    stream,
                    "400 Bad Request",
                    &session_page(&crate::shell("session-list", &html), picker_open(header)),
                    "",
                );
            }
        }
        return true;
    }
    let Some(route) = path.strip_prefix("/dm/") else {
        return false;
    };
    // The server serves these before taking the lock (`crate::read_response`);
    // a caller already holding the state, such as a test, gets the same bytes.
    if let Some(page) = ChatPage::of(method, path, header) {
        let _ = CHAT_PAGES.reply(state, page).write(stream);
        return true;
    }
    let (id, operation) = route.split_once('/').unwrap_or((route, ""));
    if !chat::valid_id(id) {
        let _ = control_http::respond(
            stream,
            "404 Not Found",
            "text/plain",
            b"Chat not found.",
            "",
            control_http::Policy::Embedded,
        );
        return true;
    }
    if method == "POST" && operation == "lenses" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        match change(
            state,
            "control.chat.lenses",
            &json!({"id":id,"lenses":chat::form_lenses(&fields,"none")}),
        ) {
            Ok(_) => {
                let _ = control_http::respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!("Location: /dm/{id}\r\n"),
                    control_http::Policy::Embedded,
                );
            }
            Err(message) => respond_chat(
                stream,
                "400 Bad Request",
                &chat_page(state, lens, id, &message, "", header),
                "",
            ),
        }
    } else if method == "POST" && operation == "messages" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        send_message(stream, state, id, lens, header, &fields, storage::save);
    } else if method == "GET" && operation == "artifacts" {
        let _ = match crate::artifacts::render(state, id, &crate::fields(query)) {
            Ok(html) => control_http::respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                crate::shell("artifact-page", &html).as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
            Err(message) => control_http::respond(
                stream,
                "404 Not Found",
                "text/plain",
                message.as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
        };
    } else if method == "GET" && operation == "memory" {
        let _ = match crate::compaction::render(state, id, query) {
            Ok(html) => control_http::respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                crate::shell("artifact-page", &html).as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
            Err(error) => control_http::respond(
                stream,
                "404 Not Found",
                "text/plain",
                error.as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
        };
    } else if method == "GET" && operation == "context" {
        let _ = match crate::turn_context::view(state, id, &crate::fields(query)) {
            Ok(html) => control_http::respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                crate::shell("artifact-page", &html).as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
            Err(error) => control_http::respond(
                stream,
                "404 Not Found",
                "text/plain",
                error.to_string().as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
        };
    } else if method == "GET" && operation == "requests" {
        let _ = match chat::requests(state, id) {
            Ok(html) => control_http::respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
            Err(message) => control_http::respond(
                stream,
                "404 Not Found",
                "text/plain",
                message.as_bytes(),
                "",
                control_http::Policy::Embedded,
            ),
        };
    } else {
        let _ = control_http::respond(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            b"Unsupported chat operation.",
            "",
            control_http::Policy::Embedded,
        );
    }
    true
}

#[cfg(test)]
mod tests;
