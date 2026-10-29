use crate::{chat, storage, State};
use serde_json::{json, Value};
use std::{io::Write, net::TcpStream};

fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &str, extra: &str) {
    let _ = write!(stream,"HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}Connection: close\r\nCache-Control: no-store\r\nReferrer-Policy: same-origin\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'\r\n\r\n{body}",body.len());
}

fn change(state: &mut State, name: &str, arguments: &Value) -> Result<Value, &'static str> {
    change_with_save(state, name, arguments, storage::save)
}
fn change_with_save(
    state: &mut State,
    name: &str,
    arguments: &Value,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    let count = state.chats.len();
    let previous = state
        .chats
        .iter()
        .position(|s| Some(s.id.as_str()) == arguments["id"].as_str())
        .map(|i| (i, state.chats[i].clone()));
    let run_count = state.runtime.runs.len();
    let notice = state.runtime.notice.clone();
    let post_count = state.collective.posts.len();
    let result = if name.starts_with("control.room") {
        crate::rooms::tool(state, name, arguments)
    } else {
        chat::tool(state, name, arguments)
    };
    if result.is_err() || save(state).is_err() {
        state.chats.truncate(count);
        if let Some((index, session)) = previous {
            state.chats[index] = session;
        }
        state.runtime.runs.truncate(run_count);
        state.runtime.notice = notice;
        state.collective.posts.truncate(post_count);
        return result.and(Err("Could not save the chat. Try again."));
    }
    result
}

pub fn rpc(state: &mut State, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let error =
        |code, message| json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
    if request["jsonrpc"] != "2.0" || !request.is_object() {
        return error(-32600, "Invalid request.");
    }
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => {
            let version = match request["params"]["protocolVersion"].as_str() {
                Some("2025-03-26") => "2025-03-26",
                _ => "2025-11-25",
            };
            json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"control","version":env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":[
            {"name":"control.chats.list","description":"List local chats.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.chat.create","description":"Create a local operator chat with lenses from the library and an optional room or design topic.","inputSchema":{"type":"object","properties":{"room":{"type":"string","enum":["general","engineering","research"]},"lens":{"type":"string"},"lenses":{"type":"array","items":{"type":"string"}},"design_context":{"type":"string"}}}},
            {"name":"control.chat.lenses","description":"Set lenses from the library for future messages. Existing runs retain their resolved context.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"lenses":{"type":"array","items":{"type":"string"}}},"required":["id","lenses"]}},
            {"name":"control.lenses.list","description":"List current declarative lens definitions.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.lens.read","description":"Read a lens definition and its current revision.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.lenses.resolve","description":"Resolve nested lenses against explicit local document snapshots. Does not grant tool authority.","inputSchema":{"type":"object","properties":{"lenses":{"type":"array","items":{"type":"string"}}},"required":["lenses"]}},
            {"name":"control.rooms.list","description":"List collaboration rooms.","inputSchema":{"type":"object","properties":{}}},
            {"name":"control.room.read","description":"Read recent room posts and linked chats.","inputSchema":{"type":"object","properties":{"room":{"type":"string"}},"required":["room"]}},
            {"name":"control.room.post","description":"Post a concise shared update. This does not send a chat prompt or grant authority.","inputSchema":{"type":"object","properties":{"room":{"type":"string"},"message":{"type":"string","minLength":1,"maxLength":2000}},"required":["room","message"]}},
            {"name":"control.chat.read","description":"Read a chat and its run status.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}},
            {"name":"control.chat.send","description":"Send a message as the chat's original actor.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"message":{"type":"string","minLength":1,"maxLength":2000}},"required":["id","message"]}}
        ]}),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or("");
            let arguments = &request["params"]["arguments"];
            let result = if [
                "control.chat.create",
                "control.chat.send",
                "control.room.post",
                "control.chat.lenses",
            ]
            .contains(&name)
            {
                change(state, name, arguments)
            } else if name.starts_with("control.room") {
                crate::rooms::tool(state, name, arguments)
            } else if name.starts_with("control.lens") {
                crate::lens_library::tool(state, name, arguments)
            } else {
                chat::tool(state, name, arguments)
            };
            match result {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":{"data":value},"isError":false})
                }
                Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
            }
        }
        _ => return error(-32601, "Method not found."),
    };
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

pub fn handle(
    stream: &mut TcpStream,
    state: &mut State,
    method: &str,
    target: &str,
    lens: &str,
    header: &str,
    body: &[u8],
) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path == "/mcp" {
        if method != "POST" {
            respond(
                stream,
                "405 Method Not Allowed",
                "text/plain",
                "Use MCP POST requests.",
                "Allow: POST\r\n",
            );
            return true;
        }
        let version = header
            .lines()
            .filter_map(|s| s.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case("mcp-protocol-version"))
            .map(|(_, v)| v.trim());
        if version.is_some_and(|v| !["2025-03-26", "2025-11-25"].contains(&v)) {
            respond(
                stream,
                "400 Bad Request",
                "text/plain",
                "Unsupported MCP protocol version.",
                "",
            );
            return true;
        }
        let request = match serde_json::from_slice::<Value>(body) {
            Ok(value) => value,
            Err(_) => {
                respond(stream,"400 Bad Request","application/json",&json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON."}}).to_string(),"");
                return true;
            }
        };
        if request.get("id").is_none()
            && request["method"]
                .as_str()
                .is_some_and(|m| m.starts_with("notifications/"))
        {
            respond(stream, "202 Accepted", "application/json", "", "");
            return true;
        }
        // A notification must never execute a mutating tool and silently lose its acknowledgement.
        if request.get("id").is_none() {
            respond(
                stream,
                "400 Bad Request",
                "text/plain",
                "A request ID is required.",
                "",
            );
            return true;
        }
        respond(
            stream,
            "200 OK",
            "application/json",
            &rpc(state, &request).to_string(),
            "",
        );
        return true;
    }
    if path == "/chats" && method == "GET" {
        let query = crate::fields(query);
        let html = chat::landing(state, lens, query.get("discuss").map(String::as_str));
        let page = crate::render(state, "session-list", lens, &html)
            .replace("<body>", "<body class='session-view'>");
        respond(stream, "200 OK", "text/html; charset=utf-8", &page, "");
        return true;
    }
    if ["/chats", "/chat"].contains(&path) && method == "POST" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let chosen = fields.get("lens").map(String::as_str).unwrap_or(lens);
        let arguments = json!({"lenses":chat::form_lenses(&fields,lens),"room":fields.get("room"),"design_context":fields.get("design_context")});
        let result = change(state, "control.chat.create", &arguments);
        match result {
            Ok(value) => {
                let id = value["id"].as_str().unwrap();
                if let Some(text) = fields.get("message").filter(|text| !text.trim().is_empty()) {
                    if let Err(message) =
                        change(state, "control.chat.send", &json!({"id":id,"message":text}))
                    {
                        respond(stream, "400 Bad Request", "text/plain", message, "");
                        return true;
                    }
                }
                respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    "",
                    &format!("Location: /chat/{id}\r\n{}", crate::lens_ui::cookie(chosen)),
                );
            }
            Err(message) => respond(
                stream,
                "500 Internal Server Error",
                "text/plain",
                message,
                "",
            ),
        }
        return true;
    }
    let Some(route) = path.strip_prefix("/chat/") else {
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
                &format!("Location: /chat/{id}\r\n"),
            ),
            Err(message) => respond(stream, "400 Bad Request", "text/plain", message, ""),
        }
    } else if method == "POST" && operation == "messages" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let mut arguments =
            json!({"id":id,"message":fields.get("message").map(String::as_str).unwrap_or("")});
        if fields.contains_key("composition") {
            arguments["lenses"] = chat::form_lenses(&fields, "none");
        }
        match change(state, "control.chat.send", &arguments) {
            Ok(_) => respond(
                stream,
                "303 See Other",
                "text/plain",
                "",
                &format!("Location: /chat/{id}\r\n"),
            ),
            Err(message) => respond(stream, "400 Bad Request", "text/plain", message, ""),
        }
    } else if method == "GET" && operation == "transcript" {
        match chat::transcript(state, id) {
            Ok(html) => respond(stream, "200 OK", "text/html; charset=utf-8", &html, ""),
            Err(message) => respond(stream, "404 Not Found", "text/plain", message, ""),
        }
    } else if method == "GET" && operation.is_empty() {
        match chat::detail(state, id) {
            Ok(_) => respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                &crate::render(state, "session", lens, id)
                    .replace("<body>", "<body class='session-view'>"),
                "",
            ),
            Err(_) if chat::discord_actor(id).is_some_and(|actor|crate::identity::authorize(&actor,crate::identity::Action::Task).is_ok()) => respond(stream,"202 Accepted","text/html; charset=utf-8","<!doctype html><html lang='en'><head><meta http-equiv='refresh' content='2'><title>Control / Chat</title></head><body><p>Waiting for the Discord message…</p></body></html>",""),
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
mod tests {
    use super::*;
    #[test]
    fn failed_chat_checkpoint_rolls_back_message_and_run() {
        let mut state = State::default();
        let id = chat::create(&mut state);
        let error = change_with_save(
            &mut state,
            "control.chat.send",
            &json!({"id":id,"message":"Test request"}),
            |_| Err(std::io::Error::other("disk full")),
        );
        assert!(error.is_err());
        assert!(state.runtime.runs.is_empty());
        assert!(state.chats[0].messages.is_empty());
        assert_eq!(state.chats[0].revision, 0);
        assert!(change_with_save(
            &mut state,
            "control.chat.send",
            &json!({"id":id,"message":"Test request"}),
            |_| Ok(())
        )
        .is_ok());
        assert_eq!(state.runtime.runs.len(), 1);
    }
    #[test]
    fn mcp_negotiation_catalog_and_unknown_methods() {
        let mut state = State::default();
        let init = rpc(
            &mut state,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(
            rpc(
                &mut state,
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
            )["result"]["tools"]
                .as_array()
                .unwrap()
                .len(),
            11
        );
        assert_eq!(
            rpc(
                &mut state,
                &json!({"jsonrpc":"2.0","id":3,"method":"unknown"})
            )["error"]["code"],
            -32601
        );
    }
    #[test]
    fn room_checkpoint_failure_rolls_back_and_invalid_membership_creates_nothing() {
        let mut state = State::default();
        assert!(change_with_save(
            &mut state,
            "control.room.post",
            &json!({"room":"research","message":"A finding"}),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err());
        assert!(state.collective.posts.is_empty());
        assert!(change_with_save(
            &mut state,
            "control.chat.create",
            &json!({"room":"../../private"}),
            |_| Ok(())
        )
        .is_err());
        assert!(state.chats.is_empty());
        let chat = change_with_save(
            &mut state,
            "control.chat.create",
            &json!({"room":"research"}),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(chat["room"], "research");
        assert!(crate::rooms::read(&state, "research")["chats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == chat["id"]));
    }
}
