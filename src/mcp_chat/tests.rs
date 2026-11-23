use super::*;

fn run(remote: &mut impl Remote, name: &str, args: &Value) -> io::Result<String> {
    call(remote, name, args, Approvals::default())
}

/// The operator endpoint served in process: every call runs the server's own
/// `session_http::rpc`, as `Hive` reaches it over loopback.
struct Local {
    state: crate::State,
    calls: Vec<(String, Value)>,
    decisions: Vec<String>,
}
impl Local {
    fn new() -> Self {
        Self { state: crate::State::default(), calls: Vec::new(), decisions: Vec::new() }
    }
}
impl Remote for Local {
    fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value> {
        self.calls.push((name.to_owned(), arguments.clone()));
        let response = crate::session_http::rpc(&mut self.state, &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}));
        let result = &response["result"];
        if result["isError"] == true {
            return Err(fail(result["content"][0]["text"].as_str().unwrap_or("Tool request failed.")));
        }
        Ok(result["structuredContent"]["data"].clone())
    }
    /// Records the confirmed-form decision; folder approval grants it here.
    fn decide(&mut self, chat: &str, kind: &str, request: &str, allow: bool, loopback: bool) -> io::Result<String> {
        self.decisions.push(format!("{kind} {request} {allow} {loopback}"));
        if let Some(project) = self.state.chats.iter_mut().find(|c| c.id == chat).and_then(|c| c.project.as_mut()).filter(|p| p.request == request) {
            project.pending = false;
            project.grant = allow.then(|| "grant".to_owned());
        }
        Ok("Decided.".into())
    }
}
fn created(text: &str) -> String {
    text.strip_prefix("Created chat ").unwrap().split(['.', ' ']).next().unwrap().to_owned()
}

#[test]
fn tool_set_offers_decisions_only_when_the_operator_enables_them() {
    let all = definitions(false, Approvals::default());
    let names = all.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(names.len(), names.iter().collect::<std::collections::HashSet<_>>().len());
    for name in &names {
        assert!(!["approve", "allow", "deny", "decide", "answer", "grant", "bot"].iter().any(|word| name.contains(word)), "{name}");
    }
    let enabled = definitions(false, Approvals { port: true, ..Default::default() });
    assert!(enabled.iter().any(|t| t["name"] == "chat_decide") && enabled.len() == all.len() + 1);
    let read = definitions(true, Approvals::default());
    assert_eq!(read.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["chats_list", "chat_read", "chat_team", "chat_requests"]);
    assert!(read.iter().all(|t| t["annotations"]["readOnlyHint"] == true));
    assert!(all.iter().filter(|t| t["annotations"]["readOnlyHint"] == false).count() == names.len() - read.len());
    assert!(run(&mut Local::new(), "approve_port", &json!({})).is_err());
}

#[test]
fn operator_loop_creates_sends_reads_and_cancels_through_the_server() {
    let mut remote = Local::new();
    // A name /rename would refuse is refused before any chat exists.
    let error = run(&mut remote, "chat_create", &json!({"title":"A title that is longer than thirty-two characters"})).unwrap_err();
    assert!(error.to_string().contains("at most 32") && remote.state.chats.is_empty(), "{error}");
    let text = run(&mut remote, "chat_create", &json!({"title":"Run A","genes":["coordination"],"model":"default"})).unwrap();
    let id = created(&text);
    assert!(text.contains("Future turns use"), "{text}");
    let session = remote.state.chats.iter().find(|c| c.id == id).unwrap();
    assert_eq!(session.name.as_deref(), Some("Run A"));
    assert_eq!(session.lenses.as_deref(), Some(&["coordination".to_owned()][..]));
    assert!(session.model.is_some());

    let listed = run(&mut remote, "chats_list", &json!({})).unwrap();
    assert!(listed.contains(&format!("{id} [idle] Run A")), "{listed}");

    let sent = run(&mut remote, "chat_send", &json!({"id":id,"message":"Build a farm"})).unwrap();
    assert!(sent.contains("[queued]"), "{sent}");
    assert!(run(&mut remote, "chats_list", &json!({})).unwrap().contains(&format!("{id} [queued]")));
    let read = run(&mut remote, "chat_read", &json!({"id":id})).unwrap();
    assert!(read.contains("> Build a farm") && read.contains("genes coordination"), "{read}");

    let genes = run(&mut remote, "chat_genes", &json!({"id":id,"genes":[]})).unwrap();
    assert!(genes.contains("none"), "{genes}");
    assert!(run(&mut remote, "chat_genes", &json!({"id":id,"genes":["no-such-gene"]})).is_err());

    let cancelled = run(&mut remote, "chat_cancel", &json!({"id":id})).unwrap();
    assert!(cancelled.contains("cancelled") || cancelled.contains("Stopping"), "{cancelled}");
    let all = run(&mut remote, "chat_cancel", &json!({"id":id,"all":true})).unwrap();
    assert!(all.starts_with("Cancelled 0 run(s)"), "{all}");
    assert!(run(&mut remote, "chat_cancel", &json!({"id":id,"all":true,"run_id":"r-1"})).is_err());
    assert_eq!(run(&mut remote, "chat_stop_services", &json!({"id":id,"team":true})).unwrap(), "No live services to stop.");
}

#[test]
fn send_refuses_commands_improve_chats_and_bad_input_before_the_server() {
    let mut remote = Local::new();
    let id = created(&run(&mut remote, "chat_create", &json!({})).unwrap());
    remote.calls.clear();
    for message in ["/open /etc", " /bot on", "/answer q-1 yes", "/improve", "/model x"] {
        assert!(run(&mut remote, "chat_send", &json!({"id":id,"message":message})).is_err(), "{message}");
    }
    assert!(remote.calls.is_empty(), "slash commands never reach the server: {:?}", remote.calls);
    // Command arguments are single tokens, so a model or run cannot smuggle a command.
    assert!(run(&mut remote, "chat_model", &json!({"id":id,"model":"x\n/open /etc"})).is_err());
    assert!(run(&mut remote, "chat_cancel", &json!({"id":id,"run_id":"r 1"})).is_err());
    assert!(run(&mut remote, "chat_send", &json!({"id":"../x","message":"hi"})).is_err());
    assert!(remote.calls.is_empty());

    remote.state.chats.iter_mut().find(|c| c.id == id).unwrap().improve_mode = true;
    let refused = run(&mut remote, "chat_send", &json!({"id":id,"message":"change source"})).unwrap_err();
    assert!(refused.to_string().contains("improve mode"));
    assert!(remote.calls.iter().all(|(name, _)| name != "control.chat.send"));
}

#[test]
fn team_and_requests_cover_spawned_chats_without_deciding_them() {
    let mut remote = Local::new();
    let lead = created(&run(&mut remote, "chat_create", &json!({"title":"Lead"})).unwrap());
    let child = crate::chat::create(&mut remote.state);
    remote.state.chats.last_mut().unwrap().parent_chat = Some(lead.clone());
    let team = run(&mut remote, "chat_team", &json!({"id":lead})).unwrap();
    assert!(team.contains(&format!("\n  {child} [idle]")) && team.contains("No live services."), "{team}");

    assert!(run(&mut remote, "chat_requests", &json!({"id":child})).unwrap().starts_with("Nothing in task"));
    remote.state.chats.last_mut().unwrap().project = Some(serde_json::from_value(json!({"request":"req-1","path":"/srv/data","grant":null,"head":null,"pending":true})).unwrap());
    let requests = run(&mut remote, "chat_requests", &json!({"id":lead})).unwrap();
    assert!(requests.contains(&format!("folder req-1 (chat {child}) · Folder request · /srv/data")) && requests.contains("cannot approve"), "{requests}");
    assert!(run(&mut remote, "chats_list", &json!({"roots_only":true})).unwrap().lines().count() == 1);
}

#[test]
fn reads_are_paged_and_bounded() {
    let mut remote = Local::new();
    let id = created(&run(&mut remote, "chat_create", &json!({})).unwrap());
    for n in 0..7 {
        run(&mut remote, "chat_send", &json!({"id":id,"message":format!("step {n} {}", "x".repeat(3000))})).unwrap();
    }
    let page = run(&mut remote, "chat_read", &json!({"id":id,"limit":2,"max_chars":200})).unwrap();
    assert!(page.contains("Messages 5–7 of 7; older: before=5") && page.contains("#6") && !page.contains("#4"), "{page}");
    assert!(page.contains("more chars]") && page.len() < 2000, "{}", page.len());
    let older = run(&mut remote, "chat_read", &json!({"id":id,"before":5,"limit":2})).unwrap();
    assert!(older.contains("#3") && older.contains("#4") && !older.contains("#5"), "{older}");
    assert!(run(&mut remote, "chat_read", &json!({"id":id,"limit":500})).is_err());
    assert!(clip(&"🦊".repeat(10), 3).starts_with("🦊🦊🦊… [7 more"));
}

#[test]
fn json_rpc_lists_tools_and_reports_unavailable_control_as_a_tool_error() {
    let mut connect = || -> io::Result<Local> { Err(fail("Start the server first")) };
    let mut connection = None;
    let listed = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), TOOLS.len() - 1, "chat_decide is hidden by default");
    let init = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "control-chat");
    let failed = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"chats_list","arguments":{}}}));
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed["result"]["content"][0]["text"].as_str().unwrap().contains("Start the server"));
    let mut connect = || -> io::Result<Local> { Ok(Local::new()) };
    let ok = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chats_list"}}));
    assert_eq!(ok["result"]["isError"], false);
    assert!(connection.is_some(), "a working session is kept");
    assert!(dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"1.0","id":5,"method":"ping"}))["error"].is_object());
}

#[test]
fn a_waiting_design_question_stays_with_the_operator() {
    let (state, chat) = crate::design::tests::paused();
    let mut remote = Local { state, calls: Vec::new(), decisions: Vec::new() };
    let refused = run(&mut remote, "chat_send", &json!({"id":chat,"message":"Use JSON"})).unwrap_err();
    assert!(refused.to_string().contains("design question"), "{refused}");
    assert!(remote.calls.iter().all(|(name, _)| name != "control.chat.send"));
    assert!(remote.state.runtime.runs[0].question.as_ref().unwrap().answer.is_none());
    let requests = run(&mut remote, "chat_requests", &json!({"id":chat})).unwrap();
    assert!(requests.starts_with("question ") && requests.contains("cannot approve"), "{requests}");
    assert!(run(&mut remote, "chat_read", &json!({"id":chat})).unwrap().contains("1 questions"));
}

#[test]
fn folder_requests_wait_for_the_operator_unless_folder_approvals_are_enabled() {
    let mut remote = Local::new();
    let id = created(&run(&mut remote, "chat_create", &json!({})).unwrap());
    let raised = run(&mut remote, "chat_open", &json!({"id":id,"paths":["/srv/one"]})).unwrap();
    assert!(raised.contains("Requested /srv/one"), "{raised}");
    let project = remote.state.chats[0].project.clone().unwrap();
    assert!(project.pending && project.grant.is_none() && remote.decisions.is_empty());
    let listed = run(&mut remote, "chat_requests", &json!({"id":id})).unwrap();
    assert!(listed.contains(&format!("folder {} (chat {id})", project.request)) && listed.contains("cannot approve"), "{listed}");
    assert!(run(&mut remote, "chat_open", &json!({"id":id,"paths":["/srv/one","/srv/two"]})).unwrap_err().to_string().contains("Permissions"));
    assert!(run(&mut remote, "chat_open", &json!({"id":id,"paths":["relative"]})).is_err());
    // Not enabled: no decision of any kind.
    for kind in Approvals::KINDS {
        assert!(run(&mut remote, "chat_decide", &json!({"id":id,"kind":kind,"request":project.request,"decision":"allow","answer":"1"})).is_err());
    }
    assert!(remote.decisions.is_empty());

    let folders = Approvals { folder: true, ..Default::default() };
    let opened = call(&mut remote, "chat_open", &json!({"id":id,"paths":["/srv/one","/srv/two"]}), folders).unwrap();
    assert_eq!(remote.decisions.len(), 2, "{opened}");
    assert!(remote.decisions.iter().all(|d| d.starts_with("folder ") && d.ends_with(" true false")));
    // Only the enabled kind.
    assert!(call(&mut remote, "chat_decide", &json!({"id":id,"kind":"port","request":"p-1","decision":"allow"}), folders).is_err());
    let ports = Approvals { port: true, ..Default::default() };
    call(&mut remote, "chat_decide", &json!({"id":id,"kind":"port","request":"p-1","decision":"deny"}), ports).unwrap();
    assert_eq!(remote.decisions.last().unwrap(), "port p-1 false false");
    // Knowledge proposals are their own switch, off by default (reviewed in Control).
    assert!(call(&mut remote, "chat_decide", &json!({"id":id,"kind":"knowledge","request":"claim:0","decision":"allow"}), ports).is_err());
    call(&mut remote, "chat_decide", &json!({"id":id,"kind":"knowledge","request":"claim:0","decision":"allow"}), Approvals { knowledge: true, ..Default::default() }).unwrap();
    assert_eq!(remote.decisions.last().unwrap(), "knowledge claim:0 true false");
}

#[test]
fn every_pending_kind_reaches_the_channel_notification_path() {
    let (mut state, chat) = crate::design::tests::paused();
    state.chats[0].project = Some(serde_json::from_value(json!({"request":"req-9","path":"/srv/data","grant":null,"head":null,"pending":true})).unwrap());
    let groups = crate::chat::pending_requests(&state);
    let group = groups.as_array().unwrap().iter().find(|g| g["chat_id"] == chat.as_str()).unwrap();
    let kinds = control_client::operator::pending(group).iter().map(|p| p.kind).collect::<Vec<_>>();
    use control_client::operator::Kind;
    assert!(kinds.contains(&Kind::Question) && kinds.contains(&Kind::Folder), "{kinds:?}");
    // Saved settings that predate folder forwarding still forward it with ports.
    let settings = control_client::channel::Settings { enabled: true, kinds: vec![Kind::Question, Kind::Bot, Kind::Port, Kind::Notice], ..Default::default() };
    assert!(settings.forwards(&chat, Kind::Folder));
    let folder = control_client::operator::pending(group).into_iter().find(|p| p.kind == Kind::Folder).unwrap();
    assert!(control_client::operator::reply(&folder, "allow").is_err(), "folders are approved in Control, not by reply");
}

fn question(text: &str) -> String {
    text.strip_prefix("Asked question ").unwrap().split(' ').next().unwrap().to_owned()
}

#[test]
fn design_ask_from_outside_a_run_raises_an_operator_question_on_the_decisions_desk() {
    use control_client::operator::{self, Command, Kind};
    let mut remote = Local::new();
    let before = crate::design::decision_revision(&remote.state);
    let asked = run(&mut remote, "control_design_ask", &json!({"question":"Ship the parser now?","options":["Ship","Wait a day"],"recommended":0})).unwrap();
    let first = question(&asked);
    let desk = crate::design::desk(&remote.state).unwrap().to_owned();
    assert!(asked.contains(&format!("in chat {desk}")) && asked.contains("approves nothing"), "{asked}");
    // One desk, reused: a second question lands in the same DM.
    let second = question(&run(&mut remote, "control_design_ask", &json!({"question":"Which name?","options":["alpha","beta","gamma"]})).unwrap());
    assert_eq!(remote.state.chats.len(), 1);
    assert_ne!(crate::design::decision_revision(&remote.state), before, "channels re-read requests on this revision");

    // Listed like an agent's question, and in the kinds the Discord notifier forwards.
    let listed = run(&mut remote, "chat_requests", &json!({"id":desk})).unwrap();
    assert!(listed.contains(&format!("question {first} (chat {desk})")) && listed.contains(&format!("question {second}")), "{listed}");
    let groups = remote.tool("control.requests.list", json!({})).unwrap();
    let group = groups.as_array().unwrap().iter().find(|g| g["chat_id"] == desk.as_str()).unwrap();
    let pending = operator::pending(group);
    assert!(pending.iter().filter(|p| p.kind == Kind::Question).count() == 2 && pending[0].body.contains("Ship the parser now?"), "{pending:?}");
    assert!(crate::chat::requests(&remote.state, &desk).unwrap().contains("Wait a day"));
    // Plain text to a chat waiting on a question still stays with the operator.
    assert!(run(&mut remote, "chat_send", &json!({"id":desk,"message":"Ship"})).unwrap_err().to_string().contains("design question"));

    // The operator replies on Discord to the second question first: option 2.
    let reply = pending.iter().find(|p| p.id == second).unwrap();
    let Command::Send(command) = operator::reply(reply, "2").unwrap() else { panic!("a question reply is a chat command") };
    remote.tool("control.chat.send", json!({"id":desk,"message":command})).unwrap();
    let read = run(&mut remote, "chat_read", &json!({"id":desk})).unwrap();
    assert!(read.contains("beta") && read.contains(&format!("Decision recorded for {second}")), "{read}");
    // Then their own direction for the first, typed in the DM (web or TUI).
    remote.tool("control.chat.send", json!({"id":desk,"message":format!("/answer {first} Ship it after the benchmarks")})).unwrap();
    let read = run(&mut remote, "chat_read", &json!({"id":desk})).unwrap();
    assert!(read.contains("Ship it after the benchmarks") && read.contains(&format!("Decision recorded for {first}")), "{read}");
    // Answering records the choice and starts nothing.
    assert!(remote.state.runtime.runs.iter().all(|r| r.status == "completed" && r.attempts.is_empty()));
    assert!(run(&mut remote, "chat_requests", &json!({"id":desk})).unwrap().contains("Nothing"));
}

#[test]
fn design_ask_in_a_named_chat_never_pauses_a_turn() {
    let mut remote = Local::new();
    let busy = created(&run(&mut remote, "chat_create", &json!({})).unwrap());
    run(&mut remote, "chat_send", &json!({"id":busy,"message":"Build the parser"})).unwrap();
    let refused = run(&mut remote, "control_design_ask", &json!({"id":busy,"question":"Now?","options":["Yes","No"]})).unwrap_err();
    assert!(refused.to_string().contains("Decisions DM"), "{refused}");
    let idle = created(&run(&mut remote, "chat_create", &json!({})).unwrap());
    let asked = question(&run(&mut remote, "control_design_ask", &json!({"id":idle,"question":"Now?","options":["Yes","No"]})).unwrap());
    assert!(crate::design::desk(&remote.state).is_none());
    assert!(run(&mut remote, "chat_requests", &json!({"id":idle})).unwrap().contains(&asked));
    // Invalid input is refused before anything is recorded.
    assert!(run(&mut remote, "control_design_ask", &json!({"question":"Only one?","options":["Yes"]})).is_err());
    // A cancelled question ends without an answer and is never retried as a turn.
    remote.tool("control.chat.send", json!({"id":idle,"message":"/cancel"})).unwrap();
    assert!(!run(&mut remote, "chat_requests", &json!({"id":idle})).unwrap().contains(&asked));
    let run_id = remote.state.runtime.runs.iter().find(|r| crate::design::asked(r)).unwrap().id.clone();
    remote.state.runtime.action("/retry-run", &[("id".to_owned(), run_id.clone())].into());
    assert_eq!(remote.state.runtime.runs.iter().find(|r| r.id == run_id).unwrap().status, "cancelled");
}

/// Pins the chat bridge's envelopes, including malformed input.
#[test]
fn chat_bridge_envelopes_are_pinned() {
    let mut connect = || -> io::Result<Local> { Ok(Local::new()) };
    let mut connection = None;
    let init = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}));
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init["result"]["capabilities"], json!({"tools":{}}));
    assert_eq!(init["result"]["serverInfo"], json!({"name":"control-chat","version":env!("CARGO_PKG_VERSION")}));
    assert!(init["result"]["instructions"].as_str().unwrap().starts_with("Drive Control chats as the local operator"));
    assert_eq!(init.as_object().unwrap().len(), 3);
    for (request, expected) in [
        (json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}), None),
        (json!({"jsonrpc":"2.0","id":3,"method":"ping"}), Some(json!({"jsonrpc":"2.0","id":3,"result":{}}))),
        (json!({"jsonrpc":"1.0","id":4,"method":"ping"}), Some(json!({"jsonrpc":"2.0","id":4,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}}))),
        (json!({"id":5,"method":"tools/list"}), Some(json!({"jsonrpc":"2.0","id":5,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}}))),
        (json!({"jsonrpc":"2.0","id":6,"method":"resources/list"}), Some(json!({"jsonrpc":"2.0","id":6,"error":{"code":-32601,"message":"Method not found"}}))),
        (json!({"jsonrpc":"2.0","method":"ping"}), Some(json!({"jsonrpc":"2.0","id":null,"result":{}}))),
        (json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"nope"}}),
            Some(json!({"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":"Unknown tool. This bridge serves chat tools only."}],"isError":true}}))),
        (json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"chats_list","arguments":[]}}),
            Some(json!({"jsonrpc":"2.0","id":8,"result":{"content":[{"type":"text","text":"Arguments must be an object."}],"isError":true}}))),
    ] {
        let response = dispatch(&mut connect, &mut connection, &request);
        match expected {
            Some(expected) => assert_eq!(response, expected, "{request}"),
            None => assert_eq!(response["result"]["protocolVersion"], "2025-11-25"),
        }
    }
    let listed = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}));
    assert_eq!(listed, json!({"jsonrpc":"2.0","id":9,"result":{"tools":definitions(false, control::permissions::read().mcp_approvals)}}));
    let ok = dispatch(&mut connect, &mut connection, &json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"chats_list"}}));
    assert_eq!(ok["result"]["content"][0]["type"], "text");
    assert_eq!((ok["result"]["isError"].clone(), ok["result"].as_object().unwrap().len()), (json!(false), 2));
}
