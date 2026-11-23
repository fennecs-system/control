use crate::{chat, storage, State};
use serde_json::{json, Value};
use std::io::Write;

fn respond(stream: &mut impl Write, status: &str, mime: &str, body: &str, extra: &str) {
    respond_with_policy(stream, status, mime, body, extra, "'none'");
}
fn respond_chat(stream: &mut impl Write, html: &str) {
    respond_chat_status(stream, "200 OK", html);
}
fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.lines().filter_map(|line|line.split_once(':'))
        .filter(|(key,_)|key.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_,value)|value.split(';'))
        .filter_map(|cookie|cookie.trim().split_once('='))
        .find_map(|(key,value)|(key==name).then_some(value))
}
/// Render the remembered conversations panel state so it never flashes after load.
fn session_page(page: &str, header: &str) -> String {
    let page = page.replace("<body>", "<body class='session-view'>");
    if cookie(header, "control_conversations") == Some("open") {
        page.replacen("<details class='conversation-toggle'>", "<details class='conversation-toggle' open>", 1)
    } else {
        page
    }
}
/// A chat page; a rejected submission shows its escaped error and keeps the draft.
fn chat_page(state: &State, lens: &str, id: &str, error: &str, draft: &str, header: &str) -> String {
    let page = session_page(&crate::render(state, "session", lens, id), header);
    if error.is_empty() { page } else { chat::rejected(&page, error, draft) }
}
fn respond_chat_status(stream: &mut impl Write, status: &str, html: &str) {
    respond_chat_headers(stream,status,html,"");
}
fn respond_chat_headers(stream:&mut impl Write,status:&str,html:&str,extra:&str) {
    let page = crate::web_assets::with_client(html);
    respond_with_policy(
        stream,
        status,
        "text/html; charset=utf-8",
        &page,
        extra,
        "'self' 'wasm-unsafe-eval'",
    );
}
fn respond_with_policy(
    stream: &mut impl Write,
    status: &str,
    mime: &str,
    body: &str,
    extra: &str,
    scripts: &str,
) {
    let policy = if scripts == "'none'" {control_http::Policy::Embedded} else {control_http::Policy::Chat};
    let _ = control_http::respond(stream, status, mime, body.as_bytes(), extra, policy);
}

fn change(state: &mut State, name: &str, arguments: &Value) -> Result<Value, &'static str> {
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
    let asynchronous = header.lines().any(|line| line.split_once(':').is_some_and(|(name,value)|
        name.eq_ignore_ascii_case("accept") && value.split(',').any(|mime|mime.trim()=="application/json")));
    if fields.get("message").is_some_and(|message|message.trim()=="/open") {
        match chat::detail(state,id) {
            Ok(_) => respond(stream,"303 See Other","text/plain","",&format!("Location: /dm/{id}/project\r\n")),
            Err(message) => respond_chat_status(stream,"400 Bad Request",&chat_page(state,lens,id,message,"/open",header)),
        }
        return;
    }
    let mut arguments=json!({"id":id,"message":fields.get("message").map(String::as_str).unwrap_or("")});
    if fields.contains_key("composition") {arguments["lenses"]=chat::form_lenses(fields,"none");}
    let result=change_with_save(state,"control.chat.send",&arguments,save);
    if asynchronous {
        let (status,value)=match result {
            Ok(_) => {
                let session=state.chats.iter().find(|chat|chat.id==id).unwrap();
                ("200 OK",json!({"title":crate::callsigns::name_or_id(&state.chats,id),"improve":session.improve_mode}))
            },
            Err(message) => ("400 Bad Request",json!({"error":message})),
        };
        respond(stream,status,"application/json",&value.to_string(),"");
    } else {
        match result {
            Ok(_) => respond(stream,"303 See Other","text/plain","",&format!("Location: /dm/{id}\r\n")),
            Err(message) => respond_chat_status(stream,"400 Bad Request",&chat_page(state,lens,id,message,fields.get("message").map(String::as_str).unwrap_or(""),header)),
        }
    }
}
pub(crate) fn change_with_save(
    state: &mut State,
    name: &str,
    arguments: &Value,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    let related = if name=="control.chat.send" {
        arguments["message"].as_str().and_then(|text|control_context::commands::tail(text.trim(),control_context::commands::Kind::Answer))
            .and_then(|answer|answer.trim().split_once(char::is_whitespace))
            .and_then(|(question,_)|crate::design::answer_target(state,arguments["id"].as_str().unwrap_or(""),question).ok())
            .map(str::to_owned)
    } else if name=="control.chat.ask" && arguments["id"].is_null() {
        crate::design::desk(state).map(str::to_owned)
    } else { None };
    transact(state, arguments["id"].as_str(), related.as_deref(), |state| if crate::tool_registry::lookup(name).is_some_and(|tool|tool.route==crate::tool_registry::Route::Project) {
        crate::projects::request_named(state,arguments["id"].as_str().unwrap_or(""),arguments["path"].as_str().unwrap_or(""),arguments["name"].as_str(),crate::projects::Requester::Agent{run:None})
    } else if crate::tool_registry::lookup(name).is_some_and(|tool|tool.route==crate::tool_registry::Route::Room) {
        crate::rooms::tool(state, name, arguments)
    } else {
        chat::tool(state, name, arguments)
    }, save)
}
/// Apply `operation` and checkpoint once; any failure restores chats, runs and posts together.
fn transact(
    state: &mut State,
    id: Option<&str>,
    related: Option<&str>,
    operation: impl FnOnce(&mut State) -> Result<Value, &'static str>,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    let targets: std::collections::HashSet<_> = id.into_iter().chain(related).collect();
    let checkpoint = crate::chat::Checkpoint::capture(state, &targets);
    let result = operation(state);
    if result.is_err() || save(state).is_err() {
        checkpoint.restore(state);
        return result.and(Err("Could not save the chat. Try again."));
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
        let result = if crate::knowledge_tools::known(name) {
            crate::knowledge_tools::local(state, name, arguments, storage::save)
        } else if crate::tool_registry::checkpoint(name) {
            change(state, name, arguments)
        } else {
            match crate::tool_registry::lookup(name).map(|tool|tool.route) {
                Some(crate::tool_registry::Route::Artifacts) => crate::artifacts::tool(state, name, arguments),
                Some(crate::tool_registry::Route::Room) => crate::rooms::tool(state, name, arguments),
                Some(crate::tool_registry::Route::Lens) => crate::lens_library::tool(state, name, arguments),
                // Served by `mcp_http` before the state lock is taken.
                Some(crate::tool_registry::Route::Package) => Err("Package tools are served by the HTTP MCP endpoint."),
                _ => chat::tool(state, name, arguments),
            }
        };
        match result {
            Ok(value) => control_mcp::data(&value),
            Err(message) => control_mcp::failure(message),
        }
    }
}
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
    if method == "GET"
        && path == "/"
        && crate::fields(query)
            .get("view")
            .is_some_and(|v| v == "evolution")
    {
        respond(
            stream,
            "303 See Other",
            "text/plain",
            "",
            "Location: /dms?improve=1\r\n",
        );
        return true;
    }
    if path==crate::history_view::PATH && method=="GET" {
        // Older history URLs redirect here (`url_names::history`). The client
        // loads a DM's turns when its row opens.
        let html=crate::history_view::page(state,&crate::history_view::Filter::from(&crate::fields(query)));
        let page=crate::web_assets::with_client(&crate::render(state,"history",lens,&html));
        let _=control_http::respond(stream,"200 OK","text/html; charset=utf-8",page.as_bytes(),"",control_http::Policy::Client);
        return true;
    }
    if path=="/bots/history/turns" && method=="GET" {
        match crate::history_view::turns(state,&crate::history_view::Filter::from(&crate::fields(query))) {
            Ok(html)=>respond(stream,"200 OK","text/html; charset=utf-8",&html,""),
            Err(message)=>respond(stream,"404 Not Found","text/plain",message,""),
        }
        return true;
    }
    if path=="/bots/graveyard" && method=="GET" {
        // The scroll loader's next page: rows only, no page shell.
        let query=crate::graveyard::Query::from(&crate::fields(query));
        let graves=crate::graveyard::dead(state,&query.search);
        respond(stream,"200 OK","text/html; charset=utf-8",&crate::graveyard::rows(&graves,&query),"");
        return true;
    }
    if path == "/dms" && method == "GET" {
        let query = crate::fields(query);
        if !query.contains_key("new") && !query.contains_key("discuss") && !query.contains_key("improve") {
            let recent=cookie(header,"control_recent_chat").filter(|id|chat::valid_id(id));
            if let Some(id)=recent.filter(|id|chat::detail(state,id).is_ok()) {
                respond(stream,"303 See Other","text/plain","",&format!("Location: /dm/{id}\r\n"));
                return true;
            }
        }
        let html = chat::landing_mode(
            state,
            lens,
            query.get("discuss").map(String::as_str),
            query.get("improve").is_some_and(|v| v == "1"),
        );
        respond_chat(stream, &session_page(&crate::render(state, "session-list", lens, &html), header));
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
        let result = transact(state, None, None, |state| {
            let value = chat::tool(state, "control.chat.create", &arguments)?;
            if let Some(text) = text {
                chat::tool(state, "control.chat.send", &json!({"id":value["id"],"message":text}))?;
            }
            Ok(value)
        }, storage::save);
        match result {
            Ok(value) => respond(
                stream,
                "303 See Other",
                "text/plain",
                "",
                &format!("Location: /dm/{}\r\n{}", value["id"].as_str().unwrap(), crate::lens_ui::cookie(chosen)),
            ),
            Err(message) => {
                let html = chat::rejected(&chat::landing_mode(state, lens, fields.get("design_context").map(String::as_str), false), message, text.map(String::as_str).unwrap_or(""));
                respond_chat_status(stream, "400 Bad Request", &session_page(&crate::render(state, "session-list", lens, &html), header));
            }
        }
        return true;
    }
    let Some(route) = path.strip_prefix("/dm/") else {
        return false;
    };
    let (id, operation) = route.split_once('/').unwrap_or((route, ""));
    if !chat::valid_id(id) {
        respond(stream, "404 Not Found", "text/plain", "Chat not found.", "");
        return true;
    }
    if method == "POST" && operation == "lenses" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        match change(
            state,
            "control.chat.lenses",
            &json!({"id":id,"lenses":chat::form_lenses(&fields,"none")}),
        ) {
            Ok(_) => respond(
                stream,
                "303 See Other",
                "text/plain",
                "",
                &format!("Location: /dm/{id}\r\n"),
            ),
            Err(message) => respond_chat_status(stream, "400 Bad Request", &chat_page(state, lens, id, message, "", header)),
        }
    } else if method == "POST" && operation == "messages" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        send_message(stream,state,id,lens,header,&fields,storage::save);
    } else if method == "GET" && operation == "conversations" {
        let allowed=state.chats.iter().find(|chat|chat.id==id).ok_or("Chat not found.")
            .and_then(|chat|crate::identity::authorize(&chat.actor,crate::identity::Action::Task).map(|_|()));
        match allowed {
            Ok(_) => respond(stream,"200 OK","text/html; charset=utf-8",&crate::navigation::conversations(state,id),""),
            Err(message) => respond(stream,"404 Not Found","text/plain",message,""),
        }
    } else if method == "GET" && operation == "artifacts" {
        match crate::artifacts::render(state, id, &crate::fields(query)) {
            Ok(html) => respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                &crate::render(state, "artifact-page", lens, &html),
                "",
            ),
            Err(message) => respond(stream, "404 Not Found", "text/plain", message, ""),
        }
    } else if method == "GET" && operation == "memory" {
        match crate::compaction::render(state,id,query) {
            Ok(html)=>respond(stream,"200 OK","text/html; charset=utf-8",&crate::render(state,"artifact-page",lens,&html),""),
            Err(error)=>respond(stream,"404 Not Found","text/plain",error,""),
        }
    } else if method == "GET" && operation == "context" {
        match crate::turn_context::view(state,id,&crate::fields(query)) {
            Ok(html)=>respond(stream,"200 OK","text/html; charset=utf-8",&crate::render(state,"artifact-page",lens,&html),""),
            Err(error)=>respond(stream,"404 Not Found","text/plain",&error.to_string(),""),
        }
    } else if method == "GET" && matches!(operation, "transcript" | "requests") {
        match if operation == "requests" { chat::requests(state,id) } else { chat::transcript(state, id) } {
            Ok(html) => respond(stream, "200 OK", "text/html; charset=utf-8", &html, ""),
            Err(message) => respond(stream, "404 Not Found", "text/plain", message, ""),
        }
    } else if method == "GET" && operation.is_empty() {
        match chat::detail(state, id) {
            Ok(_) => respond_chat_headers(stream,"200 OK",&chat_page(state,lens,id,"","",header),
                &format!("Set-Cookie: control_recent_chat={id}; Path=/; HttpOnly; SameSite=Strict\r\n")),
            Err(_) if chat::discord_actor(id).is_some_and(|actor|crate::identity::authorize(&actor,crate::identity::Action::Task).is_ok()) => respond(stream,"202 Accepted","text/html; charset=utf-8","<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><meta http-equiv='refresh' content='2'><title>Control / DM</title></head><body><p>Waiting for the Discord message…</p></body></html>",""),
            Err(message) => respond(stream, "404 Not Found", "text/plain", message, ""),
        }
    } else {
        respond(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            "Unsupported chat operation.",
            "",
        );
    }
    true
}

#[cfg(test)]
mod tests;
