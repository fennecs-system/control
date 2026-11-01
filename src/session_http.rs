use crate::{chat, storage, State};
use serde_json::{json, Value};
use std::io::Write;

fn respond(stream: &mut impl Write, status: &str, mime: &str, body: &str, extra: &str) {
    respond_with_policy(stream, status, mime, body, extra, "'none'");
}
fn respond_chat(stream: &mut impl Write, html: &str) {
    respond_chat_status(stream, "200 OK", html);
}
/// A chat page; a rejected submission shows its escaped error and keeps the draft.
fn chat_page(state: &State, lens: &str, id: &str, error: &str, draft: &str) -> String {
    let page = crate::render(state, "session", lens, id).replace("<body>", "<body class='session-view'>");
    if error.is_empty() { page } else { chat::rejected(&page, error, draft) }
}
fn respond_chat_status(stream: &mut impl Write, status: &str, html: &str) {
    respond_chat_headers(stream,status,html,"");
}
fn respond_chat_headers(stream:&mut impl Write,status:&str,html:&str,extra:&str) {
    let page = html.replace(
        "</head>",
        "<script type='module' src='/tool-window/chat-bootstrap.js'></script></head>",
    );
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
            Ok(_) => respond(stream,"303 See Other","text/plain","",&format!("Location: /chat/{id}/project\r\n")),
            Err(message) => respond_chat_status(stream,"400 Bad Request",&chat_page(state,lens,id,message,"/open")),
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
                ("200 OK",json!({"title":session.title,"improve":session.improve_mode}))
            },
            Err(message) => ("400 Bad Request",json!({"error":message})),
        };
        respond(stream,status,"application/json",&value.to_string(),"");
    } else {
        match result {
            Ok(_) => respond(stream,"303 See Other","text/plain","",&format!("Location: /chat/{id}\r\n")),
            Err(message) => respond_chat_status(stream,"400 Bad Request",&chat_page(state,lens,id,message,fields.get("message").map(String::as_str).unwrap_or(""))),
        }
    }
}
fn change_with_save(
    state: &mut State,
    name: &str,
    arguments: &Value,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    transact(state, arguments["id"].as_str(), |state| if crate::tool_registry::lookup(name).is_some_and(|tool|tool.route==crate::tool_registry::Route::Project) {
        crate::projects::request_named(state,arguments["id"].as_str().unwrap_or(""),arguments["path"].as_str().unwrap_or(""),arguments["name"].as_str())
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
    operation: impl FnOnce(&mut State) -> Result<Value, &'static str>,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    let count = state.chats.len();
    let previous = state
        .chats
        .iter()
        .position(|s| Some(s.id.as_str()) == id)
        .map(|i| (i, state.chats[i].clone()));
    let run_count = state.runtime.runs.len();
    let pending = crate::runtime::RunStatusCheckpoint::capture(&state.runtime, |run| run.chat_id.as_deref() == id);
    let notice = state.runtime.notice.clone();
    let post_count = state.collective.posts.len();
    let result = operation(state);
    if result.is_err() || save(state).is_err() {
        state.chats.truncate(count);
        if let Some((index, session)) = previous {
            state.chats[index] = session;
        }
        state.runtime.runs.truncate(run_count);
        pending.restore(&mut state.runtime);
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
        "tools/list" => json!({"tools":crate::tool_registry::http_definitions()}),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or("");
            let arguments = &request["params"]["arguments"];
            let result = if crate::knowledge_tools::known(name) {
                crate::knowledge_tools::local(state, name, arguments, storage::save)
            } else if crate::tool_registry::checkpoint(name)
            {
                change(state, name, arguments)
            } else {
                match crate::tool_registry::lookup(name).map(|tool|tool.route) {
                    Some(crate::tool_registry::Route::Artifacts) => crate::artifacts::tool(state, name, arguments),
                    Some(crate::tool_registry::Route::Room) => crate::rooms::tool(state, name, arguments),
                    Some(crate::tool_registry::Route::Lens) => crate::lens_library::tool(state, name, arguments),
                    _ => chat::tool(state, name, arguments),
                }
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
            "Location: /chats?improve=1\r\n",
        );
        return true;
    }
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
    if path=="/chats/history" && method=="GET" {
        let query=crate::fields(query);
        let html=crate::navigation::history(state,query.get("older").map(String::as_str),query.get("newer").map(String::as_str));
        respond(stream,"200 OK","text/html; charset=utf-8",&crate::render(state,"session-list",lens,&html),"");
        return true;
    }
    if path == "/chats" && method == "GET" {
        let query = crate::fields(query);
        if !query.contains_key("new") && !query.contains_key("discuss") && !query.contains_key("improve") {
            let recent=header.lines().filter_map(|line|line.split_once(':'))
                .filter(|(name,_)|name.eq_ignore_ascii_case("cookie"))
                .flat_map(|(_,value)|value.split(';'))
                .filter_map(|cookie|cookie.trim().split_once('='))
                .find_map(|(name,id)|(name=="control_recent_chat" && chat::valid_id(id)).then_some(id));
            if let Some(id)=recent.filter(|id|chat::detail(state,id).is_ok()) {
                respond(stream,"303 See Other","text/plain","",&format!("Location: /chat/{id}\r\n"));
                return true;
            }
        }
        let html = chat::landing_mode(
            state,
            lens,
            query.get("discuss").map(String::as_str),
            query.get("improve").is_some_and(|v| v == "1"),
        );
        let page = crate::render(state, "session-list", lens, &html)
            .replace("<body>", "<body class='session-view'>");
        respond_chat(stream, &page);
        return true;
    }
    if ["/chats", "/chat", "/improve"].contains(&path) && method == "POST" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let chosen = fields.get("lens").map(String::as_str).unwrap_or(lens);
        let arguments = json!({"lenses":chat::form_lenses(&fields,lens),"room":fields.get("room"),"design_context":fields.get("design_context"),"mode":if path == "/improve" {json!("improve")} else {Value::Null}});
        let text = fields
            .get(if path == "/improve" { "objective" } else { "message" })
            .filter(|text| !text.trim().is_empty());
        // Create and first message commit together, so a rejected message leaves no empty chat.
        let result = transact(state, None, |state| {
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
                &format!("Location: /chat/{}\r\n{}", value["id"].as_str().unwrap(), crate::lens_ui::cookie(chosen)),
            ),
            Err(message) => {
                let html = chat::rejected(&chat::landing_mode(state, lens, fields.get("design_context").map(String::as_str), false), message, text.map(String::as_str).unwrap_or(""));
                respond_chat_status(stream, "400 Bad Request", &crate::render(state, "session-list", lens, &html).replace("<body>", "<body class='session-view'>"));
            }
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
            Err(message) => respond_chat_status(stream, "400 Bad Request", &chat_page(state, lens, id, message, "")),
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
    } else if method == "GET" && operation == "transcript" {
        match chat::transcript(state, id) {
            Ok(html) => respond(stream, "200 OK", "text/html; charset=utf-8", &html, ""),
            Err(message) => respond(stream, "404 Not Found", "text/plain", message, ""),
        }
    } else if method == "GET" && operation.is_empty() {
        match chat::detail(state, id) {
            Ok(_) => respond_chat_headers(stream,"200 OK",&chat_page(state,lens,id,"",""),
                &format!("Set-Cookie: control_recent_chat={id}; Path=/; HttpOnly; SameSite=Strict\r\n")),
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
    #[test]
    fn returning_to_chat_remembers_only_an_existing_authorized_conversation() {
        let mut state=crate::State::default();
        let id=crate::chat::create(&mut state);
        let mut response=Vec::new();
        handle(&mut response,&mut state,"GET",&format!("/chat/{id}"),"none","",&[]);
        assert!(String::from_utf8(response).unwrap().to_ascii_lowercase().contains(&format!("set-cookie: control_recent_chat={id}; path=/; httponly; samesite=strict")));
        let cookie=format!("Cookie: control_recent_chat={id}\r\n");
        let mut response=Vec::new();
        handle(&mut response,&mut state,"GET","/chats","none",&cookie,&[]);
        assert!(String::from_utf8(response).unwrap().to_ascii_lowercase().contains(&format!("location: /chat/{id}")));
        for path in ["/chats?new=1","/chats?discuss=tools","/chats?improve=1"] {
            let mut response=Vec::new();handle(&mut response,&mut state,"GET",path,"none",&cookie,&[]);
            assert!(String::from_utf8(response).unwrap().starts_with("HTTP/1.1 200 OK"));
        }
        state.chats[0].actor=crate::identity::Actor::Unknown;
        for header in [&cookie,"Cookie: control_recent_chat=c-missing\r\n","Cookie: control_recent_chat=../../tools\r\n"] {
            let mut response=Vec::new();handle(&mut response,&mut state,"GET","/chats","none",header,&[]);
            assert!(!String::from_utf8(response).unwrap().to_ascii_lowercase().contains("location:"));
        }
    }
    #[test]
    fn web_folder_picker_is_opt_in_and_bare_open_only_navigates() {
        let mut state = State::default();
        let id = chat::create(&mut state);
        assert!(!chat::transcript(&state, &id).unwrap().contains("project-picker"));
        chat::send(&mut state, &id, "Discuss the design", "test").unwrap();
        assert!(!chat::transcript(&state, &id).unwrap().contains("project-picker"));
        let before = serde_json::to_vec(&state).unwrap();
        let mut response = Vec::new();
        assert!(handle(&mut response, &mut state, "POST", &format!("/chat/{id}/messages"), "", "engineering", b"message=%2Fopen"));
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 303 See Other\r\n"));
        assert!(response.lines().filter_map(|line|line.split_once(':')).any(|(name,value)|name.eq_ignore_ascii_case("location") && value.trim()==format!("/chat/{id}/project")));
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        // An explicit path still produces an approval next to its own message.
        chat::send(&mut state, &id, "/open /fixture/source", "test").unwrap();
        let transcript = chat::transcript(&state, &id).unwrap();
        assert_eq!(transcript.matches("project-picker").count(), 1);
        assert!(transcript.contains("Trust folder and open"));
        assert!(state.chats[0].project.as_ref().unwrap().grant.is_none());
    }
    use super::*;
    use reqwest::blocking::Client;
    use std::time::{Duration, Instant};
    #[test]
    #[ignore = "requires built WASM and a local Chrome or Firefox WebDriver; isolated in-memory fixture, no credentials or models"]
    fn live_transcript_updates_without_navigation() {
        use std::io::{BufRead, Read, Write};
        use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
        let (mut initial, chat) = crate::design::tests::paused();
        let question = initial.runtime.runs[0].question.take().unwrap();
        initial.runtime.runs[0].status = "running".into();
        initial.runtime.runs[0].result.clear();
        crate::projects::request(&mut initial,&chat,"/fixture/project").unwrap();
        let long_chat=crate::chat::create(&mut initial);
        let state = Arc::new(Mutex::new(initial));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let rejected=Arc::new(AtomicBool::new(false));let reject=rejected.clone();
        let served = state.clone(); let stopped = stop.clone(); let fail = failed.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else { std::thread::sleep(Duration::from_millis(10)); continue; };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new(); if reader.read_line(&mut line).unwrap_or(0)==0 {continue;}
                let method=line.split_whitespace().next().unwrap_or("GET").to_owned();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let mut headers=String::new();let mut length=0;
                loop {
                    let mut header=String::new();
                    if reader.read_line(&mut header).unwrap_or(0)==0 || header=="\r\n" {break;}
                    if let Some((key,value))=header.split_once(':') && key.eq_ignore_ascii_case("content-length"){length=value.trim().parse::<usize>().unwrap();}
                    headers.push_str(&header);
                }
                assert!(length<64*1024);let mut body=vec![0;length];reader.read_exact(&mut body).unwrap();
                if path.starts_with("/tool-window/") { crate::web_assets::serve(&mut stream, &path); }
                else if crate::serve_stylesheet(&mut stream, &path) {}
                else if path.contains("/transcript") && fail.load(Ordering::Relaxed) { let _=stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); }
                else if method=="POST" && path.ends_with("/messages") {
                    let id=path.trim_start_matches("/chat/").trim_end_matches("/messages");
                    let fields=crate::fields(&String::from_utf8_lossy(&body));
                    let mut state=served.lock().unwrap();
                    send_message(&mut stream,&mut state,id,"engineering",&headers,&fields,|_|if reject.load(Ordering::Relaxed){Err(std::io::Error::other("fixture disk full"))}else{Ok(())});
                    for run in &mut state.runtime.runs {if run.chat_id.as_deref()==Some(id) && run.status=="queued" {
                        run.status="completed".into();
                        run.result=format!("{}\n\n{}",run.objective.replace("Prompt","Response"),"A retained paragraph exercises a longer conversation, including **formatted text**, `code`, and evidence without navigating the composer.\n\n".repeat(32));
                    }}
                }
                else if let Some(query)=path.strip_prefix("/?") {
                    let html=crate::public_render(&served.lock().unwrap(),&crate::fields(query),"none");
                    respond(&mut stream,"200 OK","text/html; charset=utf-8",&html,"");
                }
                else { crate::session_http::handle(&mut stream, &mut served.lock().unwrap(), &method, &path, "engineering", &headers, &body); }
            }
        });
        struct Fixture { stop: Arc<AtomicBool>, thread: Option<std::thread::JoinHandle<()>> }
        impl Drop for Fixture { fn drop(&mut self) { self.stop.store(true,Ordering::Relaxed); let _=self.thread.take().unwrap().join(); } }
        let _fixture = Fixture {stop, thread:Some(thread)};
        let driver=std::env::var("CONTROL_WEBDRIVER_URL").expect("local WebDriver URL");
        let chrome=std::env::var("CONTROL_QA_BROWSER").is_ok_and(|s|s=="chrome");
        let capabilities=if chrome {json!({"browserName":"chrome","goog:chromeOptions":{"binary":"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome","args":["--headless=new","--no-first-run","--no-default-browser-check"]}})} else {json!({"browserName":"firefox","moz:firefoxOptions":{"binary":"/Applications/Firefox.app/Contents/MacOS/firefox","args":["-headless"]}})};
        let client=Client::builder().timeout(Duration::from_secs(20)).build().unwrap();
        let session:Value=client.post(format!("{driver}/session")).json(&json!({"capabilities":{"alwaysMatch":capabilities}})).send().unwrap().json().unwrap();
        let id=session["value"]["sessionId"].as_str().expect("Browser session failed");
        struct Browser { client:Client, url:String }
        impl Browser {
            fn request(&self, method:reqwest::Method, path:&str, args:Value)->Value {let mut r=self.client.request(method.clone(),format!("{}{path}",self.url));if method==reqwest::Method::POST {r=r.json(&args);} let response=r.send().unwrap();let status=response.status();let value:Value=response.json().unwrap(); assert!(status.is_success(),"WebDriver {path}: {}",value["value"]["error"]);value["value"].clone()}
            fn post(&self,path:&str,args:Value)->Value {self.request(reqwest::Method::POST,path,args)}
            fn get(&self,path:&str)->Value {self.request(reqwest::Method::GET,path,Value::Null)}
            fn element(&self,selector:&str)->String {self.post("/element",json!({"using":"css selector","value":selector}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned()}
            fn text(&self,selector:&str)->String {let id=self.element(selector);self.get(&format!("/element/{id}/text")).as_str().unwrap().to_owned()}
            fn wait(&self,text:&str) {let start=Instant::now(); loop {if self.text("body").contains(text) {break;} assert!(start.elapsed()<Duration::from_secs(12),"Transcript never displayed {text}"); std::thread::sleep(Duration::from_millis(100));}}
        }
        impl Drop for Browser {fn drop(&mut self){let _=self.client.delete(&self.url).send();}}
        let b=Browser{client,url:format!("{driver}/session/{id}")};
        b.post("/timeouts",json!({"implicit":5000,"pageLoad":15000}));
        let child={let mut state=state.lock().unwrap();let mut parent=String::new();
            for index in 0..25 {
                parent=crate::chat::create(&mut state);state.chats.last_mut().unwrap().title=format!("History thread {index}");
                state.runtime.enqueue(format!("History run {index}"),"chat","test");state.runtime.runs.last_mut().unwrap().status="completed".into();
            }
            let child=crate::chat::create(&mut state);let session=state.chats.last_mut().unwrap();session.parent_chat=Some(parent);session.title="Nested history agent".into();child
        };
        b.post("/url",json!({"url":format!("{server}/?view=runs")}));
        assert_eq!(b.post("/elements",json!({"using":"css selector","value":"article.record"})).as_array().unwrap().len(),10);
        b.wait("History run 24");let older=b.element(".history-controls a[href*='older=']");b.post(&format!("/element/{older}/click"),json!({}));b.wait("History run 14");
        assert!(!b.text("body").contains("History run 24"));
        b.post("/url",json!({"url":format!("{server}/chats/history")}));
        assert_eq!(b.post("/elements",json!({"using":"css selector","value":".conversation-history nav a[href^='/chat/']"})).as_array().unwrap().len(),11);
        let nested=b.element(&format!("a[href='/chat/{child}']"));assert_eq!(b.get(&format!("/element/{nested}/displayed")),false,"Child agents start collapsed under their parent");
        let older=b.element(".history-controls a[href*='older=']");b.post(&format!("/element/{older}/click"),json!({}));b.wait("History thread 14");assert!(!b.text("body").contains("History thread 24"));
        b.post("/url",json!({"url":format!("{server}/chat/{child}")}));
        let fresh_input=b.element("textarea[data-enter-sends=true]");let toggle=b.element(".conversation-toggle > summary");b.post(&format!("/element/{toggle}/click"),json!({}));
        let branch=b.element(".conversation-branch");assert_eq!(b.get(&format!("/element/{branch}/property/open")),true,"The active child's parent must be expanded");
        {let mut state=state.lock().unwrap();crate::chat::create(&mut state);state.chats.last_mut().unwrap().title="Live recent thread".into();}
        b.wait("Live recent thread");assert_eq!(b.get(&format!("/element/{fresh_input}/name")),"textarea","Refreshing the disclosed picker must not reload chat");
        b.post("/url",json!({"url":format!("{server}/chat/{chat}")}));
        let input=b.element("textarea[data-enter-sends=true]");
        b.post(&format!("/element/{input}/value"),json!({"text":"/op"}));
        let option=b.element("#command-open");
        assert_eq!(b.get(&format!("/element/{option}/displayed")),true);
        b.post(&format!("/element/{option}/click"),json!({}));
        assert_eq!(b.get(&format!("/element/{input}/property/value")),"/open");
        assert_eq!(b.text(".composer-highlight .chat-command"),"/open");
        b.post(&format!("/element/{input}/clear"),json!({}));
        b.post(&format!("/element/{input}/value"),json!({"text":"Keep this unfinished draft"}));
        let frame=b.element("iframe.chat-transcript");
        b.post("/frame",json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
        let body=b.element("body");
        let approval=b.element(".project-picker button[name=approve]");
        let approval_color=b.get(&format!("/element/{approval}/css/background-color"));
        assert!(approval_color=="rgb(181, 160, 255)"||approval_color=="rgba(181, 160, 255, 1)","Approval must use the theme purple: {approval_color}");
        assert_eq!(b.get(&format!("/element/{approval}/css/border-radius")),"0px");
        let previous_project=b.element(".project-picker > summary");
        b.post(&format!("/element/{previous_project}/click"),json!({}));
        {crate::chat::send(&mut state.lock().unwrap(),&chat,"/open /fixture/replacement","test").unwrap();}
        b.wait("/fixture/replacement");
        let replacement=b.element(".assistant-message .project-picker");
        assert_eq!(b.get(&format!("/element/{replacement}/property/open")),true,"A new request must not inherit the old collapsed state");
        assert_eq!(b.get(&format!("/element/{body}/name")),"body");
        {let mut state=state.lock().unwrap();state.runtime.runs[0].attempts[0].activity.push(crate::usage::Activity{id:"command-1".into(),kind:"command_execution".into(),text:"cargo test".into(),output:"Streaming output alpha".into(),status:"running".into()});}
        b.wait("Streaming output alpha");
        let details=b.element(".run-activity > summary");
        b.post(&format!("/element/{details}/click"),json!({}));
        b.post("/frame",json!({"id":null}));
        b.post(&format!("/element/{input}/click"),json!({}));
        b.post("/frame",json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
        failed.store(true,Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(1600));
        { let mut state=state.lock().unwrap(); let run=&mut state.runtime.runs[0]; run.status="completed".into();run.result="Finished without another user message".into();run.attempts[0].activity.push(crate::usage::Activity{id:"workspace-diff".into(),kind:"file_change".into(),text:"1 file · +1 −1".into(),output:"--- a/demo.rs\n+++ b/demo.rs\n@@ -1 +1 @@\n-old\n+new".into(),status:"completed".into()}); }
        failed.store(false,Ordering::Relaxed);
        b.wait("Finished without another user message");
        let diff=b.element(".edit-diff > summary");
        b.post(&format!("/element/{diff}/click"),json!({}));
        assert_eq!(b.text(".diff-add"),"+new");
        assert_eq!(b.text(".diff-remove"),"-old");
        b.post("/frame",json!({"id":null}));
        b.post(&format!("/element/{input}/click"),json!({}));
        b.post("/frame",json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
        let details=b.element(".run-activity");
        assert_eq!(b.get(&format!("/element/{details}/property/open")),false);
        assert_eq!(b.get(&format!("/element/{body}/name")),"body","The iframe must never navigate");
        let article=b.element(".assistant-message");
        std::thread::sleep(Duration::from_millis(2200));
        assert_eq!(b.get(&format!("/element/{article}/name")),"article","Unchanged polls must retain DOM nodes");
        {let mut state=state.lock().unwrap();let run=&mut state.runtime.runs[0];run.status="waiting".into();run.question=Some(question);}
        b.wait("Which format?");
        let activity=b.element(".run-activity");
        assert_eq!(b.get(&format!("/element/{activity}/property/open")),false,"Collapsed activity must survive a changed transcript in both browser realms");
        assert!(b.text("body").contains("Plain text"));
        let answer=b.element("form");
        assert_eq!(b.get(&format!("/element/{answer}/attribute/target")),"_top");
        b.post("/frame",json!({"id":null}));
        assert_eq!(b.get(&format!("/element/{input}/property/value")),"Keep this unfinished draft");
        assert_eq!(b.get("/element/active")["element-6066-11e4-a52e-4f735466cecf"],input);
        // Fifteen real form submissions through the WASM client and HTTP command;
        // deterministic worker results isolate browser costs from model latency.
        b.post("/url",json!({"url":format!("{server}/chat/{long_chat}")}));
        let page=b.element("body");let input=b.element("textarea[data-enter-sends=true]");let frame=b.element("iframe.chat-transcript");
        rejected.store(true,Ordering::Relaxed);
        b.post(&format!("/element/{input}/value"),json!({"text":"Prompt rejected\u{E007}"}));
        b.wait("Could not save the chat");
        assert_eq!(b.get(&format!("/element/{input}/property/value")),"Prompt rejected");
        assert!(state.lock().unwrap().chats.iter().find(|chat|chat.id==long_chat).unwrap().messages.is_empty());
        rejected.store(false,Ordering::Relaxed);b.post(&format!("/element/{input}/clear"),json!({}));
        let mut first=None;let mut first_html=Value::Null;let mut timings=Vec::new();
        for turn in 1..=15 {
            let start=Instant::now();
            b.post(&format!("/element/{input}/value"),json!({"text":format!("Prompt {turn}\u{E007}")}));
            b.post("/frame",json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
            b.wait(&format!("Response {turn}"));
            let scroller=b.element("html");
            let height=b.get(&format!("/element/{scroller}/property/scrollHeight")).as_f64().unwrap();
            let viewport=b.get(&format!("/element/{scroller}/property/clientHeight")).as_f64().unwrap();
            let top=b.get(&format!("/element/{scroller}/property/scrollTop")).as_f64().unwrap();
            assert!(height-viewport-top<80.0,"Turn {turn} should follow the latest reply");
            let current=b.element("article");
            if let Some(first)=&first {assert_eq!(&current,first,"Earlier messages changed at turn {turn}; before {first_html}, after {}",b.get(&format!("/element/{current}/property/outerHTML")));}
            else {first_html=b.get(&format!("/element/{current}/property/outerHTML"));first=Some(current);}
            b.post("/frame",json!({"id":null}));
            assert_eq!(b.get(&format!("/element/{page}/name")),"body","Sending must not navigate the page");
            assert_eq!(b.get(&format!("/element/{input}/property/value")),"");
            timings.push(start.elapsed());
        }
        assert_eq!(state.lock().unwrap().chats.iter().find(|chat|chat.id==long_chat).unwrap().messages.len(),15);
        timings.sort();println!("{}: 15 turns, median {:?}, maximum {:?}",if chrome{"Chrome"}else{"Firefox"},timings[7],timings[14]);
        assert!(timings[14]<Duration::from_secs(5),"Local fixture responses should stay responsive");
    }

    #[test]
    fn asynchronous_send_commits_before_ack_and_preserves_native_navigation() {
        let mut state=State::default();let id=chat::create(&mut state);
        let fields=crate::fields("message=hello&composition=1&lens=research");let before=serde_json::to_vec(&state).unwrap();
        let mut response=Vec::new();
        send_message(&mut response,&mut state,&id,"engineering","Accept: application/json",&fields,|_|Err(std::io::Error::other("disk full")));
        assert_eq!(serde_json::to_vec(&state).unwrap(),before);
        assert!(String::from_utf8(response).unwrap().contains("\"error\":\"Could not save the chat"));
        let mut response=Vec::new();
        send_message(&mut response,&mut state,&id,"engineering","Accept: application/json",&fields,|_|Ok(()));
        let response=String::from_utf8(response).unwrap();assert!(response.starts_with("HTTP/1.1 200 OK"));assert!(!response.contains("Location:"));assert!(response.contains("application/json"));
        assert_eq!(state.chats[0].messages.len(),1);
        let mut response=Vec::new();send_message(&mut response,&mut state,&id,"engineering","",&crate::fields("message=%2Fopen"),|_|panic!("Picker navigation must not save"));
        assert!(String::from_utf8(response).unwrap().to_ascii_lowercase().contains(&format!("location: /chat/{id}/project")));
        let mut response=Vec::new();send_message(&mut response,&mut state,&id,"engineering","",&crate::fields("message=%2Fcancel"),|_|Ok(()));
        assert!(String::from_utf8(response).unwrap().starts_with("HTTP/1.1 303 See Other"));
    }

    #[test]
    fn failed_design_answer_checkpoint_restores_question_and_does_not_resume() {
        let (mut state,id) = crate::design::tests::paused();
        let before = serde_json::to_vec(&state).unwrap();
        assert!(change_with_save(&mut state,"control.chat.send",&json!({"id":id,"message":"2"}),|_|Err(std::io::Error::other("disk full"))).is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(),before);
        assert!(change_with_save(&mut state,"control.chat.send",&json!({"id":id,"message":"2"}),|_|Ok(())).is_ok());
        assert_eq!(state.runtime.runs[0].status,"continued");
        assert_eq!(state.runtime.runs.len(),2);
    }
    #[test]
    fn rejected_first_message_creates_no_chat_and_keeps_the_escaped_draft() {
        let mut state = State::default();
        let mut response = Vec::new();
        assert!(handle(&mut response, &mut state, "POST", "/chats", "none", "", b"message=%2Fanswer+%3Cb%3Edraft"));
        let response = String::from_utf8(response).unwrap();
        assert!(state.chats.is_empty() && state.runtime.runs.is_empty());
        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(response.contains("role='alert'>Use /answer"));
        assert!(response.contains("/answer &lt;b&gt;draft</textarea>") && !response.contains("<b>draft"));
    }
    #[test]
    fn failed_cancel_checkpoint_restores_existing_run_and_conversation() {
        for status in ["queued", "running"] {
            let mut state = State::default();
            let id = chat::create(&mut state);
            chat::send(&mut state, &id, "Keep my work", "test").unwrap();
            state.runtime.runs[0].status = status.into();
            state.runtime.runs[0].result = "Existing progress".into();
            let before = serde_json::to_vec(&state).unwrap();
            assert!(change_with_save(&mut state, "control.chat.send", &json!({"id":id,"message":"/cancel"}), |_| Err(std::io::Error::other("disk full"))).is_err());
            assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        }
    }
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
    fn failed_mode_change_does_not_persist_mode_or_new_source_work() {
        let mut state = State::default();
        let id = chat::create(&mut state);
        let before = serde_json::to_vec(&state).unwrap();
        assert!(change_with_save(
            &mut state,
            "control.chat.send",
            &json!({"id":id,"mode":"improve","message":"Discuss a simpler UI"}),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }
    #[test]
    fn mcp_negotiation_catalog_and_unknown_methods() {
        let mut state = State::default();
        let init = rpc(
            &mut state,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        let catalogue = rpc(
                &mut state,
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
            );
        let tools=catalogue["result"]["tools"].as_array().unwrap();
        let names:std::collections::BTreeSet<_>=tools.iter().filter_map(|t|t["name"].as_str()).collect();
        assert_eq!(names.len(),tools.len(),"MCP tools must have unique names");
        for expected in ["control.chat.create","control.room.read","control.project.request","control_github_open_pr"] {assert!(names.contains(expected));}
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
