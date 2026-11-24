use crate::application::{effects::Effects, runs::Tx};
use crate::store::{Applied, Commit};
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
/// Whether the client asked for the composer's JSON reply.
fn accepts_json(header: &str) -> bool {
    header.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("accept") && value.split(',').any(|mime| mime.trim() == "application/json")
        })
    })
}
/// What a send decided under the lock.
enum Sent {
    /// A change to save: its reply once durable, the wake to release then,
    /// and the rollback if storage refuses it while the lock is held.
    Changed {
        reply: Vec<u8>,
        effects: Effects,
        checkpoint: Box<chat::Checkpoint>,
    },
    /// Nothing changed (`/open`, or a refusal already rolled back).
    Answered(Vec<u8>),
}
/// A refused send: the chat page with the error and the draft kept, or the
/// composer's JSON error.
fn refused_send(
    state: &State,
    lens: &str,
    id: &str,
    header: &str,
    fields: &std::collections::HashMap<String, String>,
    message: &str,
) -> Vec<u8> {
    let mut reply = Vec::new();
    if accepts_json(header) {
        let _ = control_http::respond(
            &mut reply,
            "400 Bad Request",
            "application/json",
            json!({"error":message}).to_string().as_bytes(),
            "",
            control_http::Policy::Embedded,
        );
    } else {
        respond_chat(
            &mut reply,
            "400 Bad Request",
            &chat_page(
                state,
                lens,
                id,
                message,
                fields.get("message").map(String::as_str).unwrap_or(""),
                header,
            ),
            "",
        );
    }
    reply
}
// Both native forms and the WASM composer use the same checkpointed command.
fn send_change(
    state: &mut State,
    id: &str,
    lens: &str,
    header: &str,
    fields: &std::collections::HashMap<String, String>,
    tx: Tx,
) -> Sent {
    let mut reply = Vec::new();
    if fields.get("message").is_some_and(|message| message.trim() == "/open") {
        match chat::viewable(&state.chats, id) {
            Ok(_) => {
                let _ = control_http::respond(
                    &mut reply,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!("Location: /dm/{id}/project\r\n"),
                    control_http::Policy::Embedded,
                );
            }
            Err(message) => respond_chat(
                &mut reply,
                "400 Bad Request",
                &chat_page(state, lens, id, message, "/open", header),
                "",
            ),
        }
        return Sent::Answered(reply);
    }
    let mut arguments = json!({"id":id,"message":fields.get("message").map(String::as_str).unwrap_or("")});
    if fields.contains_key("composition") {
        arguments["lenses"] = chat::form_lenses(fields, "none");
    }
    let (effects, checkpoint) = match change_staged(state, "control.chat.send", &arguments, tx) {
        Ok((_, effects, checkpoint)) => (effects, Box::new(checkpoint)),
        Err(message) => return Sent::Answered(refused_send(state, lens, id, header, fields, &message)),
    };
    if accepts_json(header) {
        let session = state.chats.iter().find(|chat| chat.id == id).unwrap();
        let value = json!({"title":crate::callsigns::name_or_id(&state.chats,id),"improve":session.improve_mode});
        let _ = control_http::respond(
            &mut reply,
            "200 OK",
            "application/json",
            value.to_string().as_bytes(),
            "",
            control_http::Policy::Embedded,
        );
    } else {
        let _ = control_http::respond(
            &mut reply,
            "303 See Other",
            "text/plain",
            b"",
            &format!("Location: /dm/{id}\r\n"),
            control_http::Policy::Embedded,
        );
    }
    Sent::Changed {
        reply,
        effects,
        checkpoint,
    }
}
/// `send_change` saved under the lock by `save`, for a caller that already
/// holds the state; the server uses `messages`, which releases it first.
fn send_message(
    stream: &mut impl Write,
    state: &mut State,
    id: &str,
    lens: &str,
    header: &str,
    fields: &std::collections::HashMap<String, String>,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) {
    let reply = match send_change(state, id, lens, header, fields, crate::store::legacy_tx()) {
        Sent::Answered(reply) => reply,
        Sent::Changed {
            reply,
            effects,
            checkpoint,
        } => {
            let saved = save(state);
            crate::store::perform(effects.after_held_save(&saved));
            if saved.is_ok() {
                reply
            } else {
                checkpoint.restore(state);
                refused_send(state, lens, id, header, fields, SAVE_FAILED)
            }
        }
    };
    let _ = stream.write_all(&reply);
}
/// What a caller is told when its change was refused and rolled back.
const SAVE_FAILED: &str = "Could not save the chat. Try again.";

/// `POST /dm/{id}/messages`, from the native form and the WASM composer. The
/// message is applied under the lock and saved after it is released
/// (`Store::commit`). The caller hears of success only once it is durable.
/// If storage fails after the release, others may already see the message,
/// so it stays and is saved when storage recovers: the caller gets a 503
/// saying "not saved yet". If storage refuses before the release (it was
/// already unavailable), nothing changed: the usual rejection, saying "not
/// saved". Other `/dm/` routes fall through to `handle`.
pub(crate) fn messages(
    stream: &mut impl Write,
    store: &crate::Store,
    method: &str,
    path: &str,
    header: &str,
    requested_lens: Option<&str>,
    body: &[u8],
) -> bool {
    let Some(id) = path
        .strip_prefix("/dm/")
        .and_then(|route| route.strip_suffix("/messages"))
        .filter(|id| method == "POST" && chat::valid_id(id))
    else {
        return false;
    };
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let lens = |state: &State| crate::lens_ui::selected(&state.lens_library, header, requested_lens).to_owned();
    let committed = send_commit(
        store,
        id,
        header,
        requested_lens,
        &fields,
        crate::storage::submit,
        crate::worker::wake,
    );
    let unavailable = || {
        let mut reply = Vec::new();
        crate::unavailable(&mut reply);
        reply
    };
    let reply = match committed {
        Ok(Commit::Saved(reply)) => reply,
        Ok(Commit::Refused(reply, seen)) => {
            seen.wait();
            reply
        }
        Ok(Commit::NotYet(_, unsaved)) => not_saved_yet(header, &unsaved.to_string()),
        Ok(Commit::NotSaved(error)) => {
            let refused =
                store.read_seen(|state| refused_send(state, &lens(state), id, header, &fields, &error.to_string()));
            refused.map_or_else(
                |_| unavailable(),
                |(reply, seen)| {
                    seen.wait();
                    reply
                },
            )
        }
        Err(_) => unavailable(),
    };
    let _ = stream.write_all(&reply);
    true
}
/// A send as a transaction, with the stage and the wake passed in (tests
/// script the disk). Success, and refusals rendered under the lock, carry
/// the reply bytes.
pub(crate) fn send_commit(
    store: &crate::Store,
    id: &str,
    header: &str,
    requested_lens: Option<&str>,
    fields: &std::collections::HashMap<String, String>,
    submit: impl FnOnce(&State, crate::store::Staging) -> std::io::Result<storage::Saving>,
    wake: impl FnOnce(),
) -> Result<Commit<Vec<u8>, Vec<u8>>, crate::store::Unavailable> {
    store.commit_with(
        |state, tx| {
            let lens = crate::lens_ui::selected(&state.lens_library, header, requested_lens).to_owned();
            match send_change(state, id, &lens, header, fields, tx) {
                Sent::Changed {
                    reply,
                    effects,
                    checkpoint,
                } => Ok(Applied {
                    answer: reply,
                    effects,
                    undo: move |state: &mut State| checkpoint.restore(state),
                }),
                Sent::Answered(reply) => Err(reply),
            }
        },
        submit,
        wake,
    )
}
/// The message is in the chat but not durable: storage failed after others
/// could see it, so it stays and is saved once storage recovers.
pub(crate) fn not_saved_yet(header: &str, message: &str) -> Vec<u8> {
    let mut reply = Vec::new();
    let _ = if accepts_json(header) {
        control_http::respond(
            &mut reply,
            "503 Service Unavailable",
            "application/json",
            json!({"error":message}).to_string().as_bytes(),
            "",
            control_http::Policy::Embedded,
        )
    } else {
        control_http::respond(
            &mut reply,
            "503 Service Unavailable",
            "text/plain; charset=utf-8",
            message.as_bytes(),
            "",
            control_http::Policy::Embedded,
        )
    };
    reply
}
pub(crate) fn change_with_save(
    state: &mut State,
    name: &str,
    arguments: &Value,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, std::borrow::Cow<'static, str>> {
    let (value, effects, checkpoint) = change_staged(state, name, arguments, crate::store::legacy_tx())?;
    let saved = save(state);
    crate::store::perform(effects.after_held_save(&saved));
    if saved.is_err() {
        checkpoint.restore(state);
        return Err(SAVE_FAILED.into());
    }
    Ok(value)
}
/// Applies a checkpointed command. A refusal restores chats, runs and posts
/// together and is returned; otherwise the caller saves, restoring with the
/// checkpoint if the save is refused.
fn change_staged(
    state: &mut State,
    name: &str,
    arguments: &Value,
    tx: Tx,
) -> Result<(Value, Effects, chat::Checkpoint), std::borrow::Cow<'static, str>> {
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
    let targets: std::collections::HashSet<_> =
        arguments["id"].as_str().into_iter().chain(related.as_deref()).collect();
    let checkpoint = match (name, arguments["message"].as_str()) {
        ("control.chat.send", Some(text)) => chat::Checkpoint::for_send(state, &targets, text),
        _ => chat::Checkpoint::capture(state, &targets),
    };
    let result = if crate::tool_registry::lookup(name)
        .is_some_and(|tool| tool.route == crate::tool_registry::Route::Project)
    {
        crate::projects::request_named(
            state,
            arguments["id"].as_str().unwrap_or(""),
            arguments["path"].as_str().unwrap_or(""),
            arguments["name"].as_str(),
            crate::projects::Requester::Agent { run: None },
        )
        .map(|value| (value, Effects::none()))
        .map_err(Into::into)
    } else if crate::tool_registry::lookup(name).is_some_and(|tool| tool.route == crate::tool_registry::Route::Room) {
        crate::rooms::tool(state, name, arguments)
            .map(|value| (value, Effects::none()))
            .map_err(Into::into)
    } else {
        chat::tool_queued(state, name, arguments, tx)
    };
    match result {
        Ok((value, effects)) => Ok((value, effects, checkpoint)),
        Err(error) => {
            checkpoint.restore(state);
            Err(error)
        }
    }
}
/// Apply `operation` and checkpoint once; any failure restores chats, runs and posts together.
fn transact(
    state: &mut State,
    operation: impl FnOnce(&mut State) -> Result<(Value, Effects), std::borrow::Cow<'static, str>>,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, std::borrow::Cow<'static, str>> {
    let checkpoint = chat::Checkpoint::capture(state, &std::collections::HashSet::new());
    let (value, effects) = match operation(state) {
        Ok(applied) => applied,
        Err(error) => {
            checkpoint.restore(state);
            return Err(error);
        }
    };
    let saved = save(state);
    crate::store::perform(effects.after_held_save(&saved));
    if saved.is_err() {
        checkpoint.restore(state);
        return Err(SAVE_FAILED.into());
    }
    Ok(value)
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
                // Only `control.chat.send` queues work, and it is a checkpointed
                // tool (`change`): these have nothing to release.
                _ => chat::tool_queued(state, name, arguments, crate::store::legacy_tx()).map(|(value, effects)| {
                    effects.forgo();
                    value
                }),
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

/// `GET /dms`: the New DM page, or the DM the browser remembers.
pub(crate) struct Dms {
    /// The remembered DM, unless the request asks for a new one.
    recent: Option<String>,
    /// The lens the request names (`lens_ui::requested`); the view keeps it
    /// only when the library has it.
    lens: String,
    discuss: Option<String>,
    improve: bool,
    picker_open: bool,
}
impl Dms {
    pub(crate) fn of(query: &std::collections::HashMap<String, String>, header: &str, lens: String) -> Self {
        let fresh = ["new", "discuss", "improve"].iter().any(|key| query.contains_key(*key));
        Self {
            recent: cookie(header, "control_recent_chat")
                .filter(|id| !fresh && chat::valid_id(id))
                .map(str::to_owned),
            lens,
            discuss: query.get("discuss").cloned(),
            improve: query.get("improve").is_some_and(|v| v == "1"),
            picker_open: picker_open(header),
        }
    }
}
pub(crate) enum DmsView {
    Recent(String),
    New { landing: chat::Landing, picker_open: bool },
}
pub(crate) const DMS: crate::Read<Dms, DmsView> = crate::Read {
    view: |state, dms| match dms.recent.filter(|id| chat::viewable(&state.chats, id).is_ok()) {
        Some(id) => DmsView::Recent(id),
        None => DmsView::New {
            landing: chat::Landing::of(
                state,
                crate::lens_ui::known(&state.lens_library, &dms.lens),
                dms.discuss.as_deref(),
                dms.improve,
            ),
            picker_open: dms.picker_open,
        },
    },
    render: |view| match view {
        DmsView::Recent(id) => crate::Reply {
            status: "303 See Other",
            mime: "text/plain",
            body: String::new(),
            extra: format!("Location: /dm/{id}\r\n"),
            policy: control_http::Policy::Embedded,
        },
        DmsView::New { landing, picker_open } => crate::Reply {
            status: "200 OK",
            mime: "text/html; charset=utf-8",
            body: crate::web_assets::with_client(&session_page(
                &crate::shell("session-list", &landing.render()),
                picker_open,
            )),
            extra: String::new(),
            policy: control_http::Policy::Chat,
        },
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
        // The server serves this before taking the lock; a caller holding
        // state gets the same bytes.
        let landing = Dms::of(&crate::fields(query), header, lens.to_owned());
        let _ = DMS.reply(state, landing).write(stream);
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
            |state| {
                let tx = crate::store::legacy_tx();
                let (value, created) = chat::tool_queued(state, "control.chat.create", &arguments, tx)?;
                let effects = match text {
                    Some(text) => {
                        let first = json!({"id":value["id"],"message":text});
                        created.and(chat::tool_queued(state, "control.chat.send", &first, tx)?.1)
                    }
                    None => created,
                };
                Ok((value, effects))
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
