//! Chat pages pinned byte for byte through the whole request handler, so
//! moving their rendering out of the state lock cannot change a response.
//! An intended page change regenerates the fixtures (see the panic message).
use super::*;
use std::io::Read;

const SESSION: &str = "page-bytes-operator-session";

fn exchange(state: &Store, method: &str, target: &str, cookie: &str, body: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    let (state, access) = (state.clone(), Arc::new(access::Access::for_tests(SESSION)));
    let handler = std::thread::spawn(move || handle(server, state, access));
    let kind = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    write!(
        client,
        "{method} {target} HTTP/1.1\r\nHost: {address}\r\nCookie: control_operator={SESSION}{cookie}\r\n{kind}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    handler.join().unwrap();
    response
}
/// Each `(fixture name, method, target, cookie, body)` against its pinned
/// bytes: the commands that refresh the changed ones, if any.
fn changed(state: &Store, pages: &[(&str, &str, &str, &str, &str)]) -> Vec<String> {
    changed_after(state, pages, |response| response)
}
/// [`changed`], comparing each response after `mask` hides its clock-derived part.
fn changed_after(state: &Store, pages: &[(&str, &str, &str, &str, &str)], mask: fn(String) -> String) -> Vec<String> {
    pages
        .iter()
        .filter_map(|(name, method, target, cookie, body)| {
            unpinned(name, &mask(exchange(state, method, target, cookie, body)))
                .map(|refresh| format!("{method} {target}: {refresh}"))
        })
        .collect()
}
/// The command that refreshes fixture `name` when `actual` differs from it.
fn unpinned(name: &str, actual: &str) -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pages")
        .join(format!("{name}.http"));
    let pinned = std::fs::read_to_string(&path).unwrap_or_default();
    (actual != pinned).then(|| {
        let fresh = std::env::temp_dir().join(format!("control-page-{name}.http"));
        std::fs::write(&fresh, actual).unwrap();
        format!("cp {} {}", fresh.display(), path.display())
    })
}
/// A `tools/call` request body for the operator MCP.
fn mcp_call(id: &str, name: &str, arguments: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
    )
}
/// Each `(fixture name, body)` posted to `/mcp`.
fn mcp_changed(state: &Store, bodies: &[(&str, String)]) -> Vec<String> {
    let pages = bodies
        .iter()
        .map(|(name, body)| (*name, "POST", "/mcp", "", body.as_str()))
        .collect::<Vec<_>>();
    changed(state, &pages)
}
fn assert_pinned(changed: Vec<String>) {
    assert!(
        changed.is_empty(),
        "Page bytes changed. If the change is intended:\n{}",
        changed.join("\n")
    );
}
/// Renames clock-derived run and chat ids through the saved form, so the
/// bytes are stable. `chats` are the fixture's clock-named chat ids in order.
fn renamed(state: &State, chats: &[&str]) -> State {
    let mut text = serde_json::to_string(state).unwrap();
    let runs = state
        .runtime
        .runs
        .iter()
        .map(|run| run.id().as_str().to_owned())
        .collect::<Vec<_>>();
    for (index, id) in runs.iter().enumerate() {
        text = text.replace(id, &format!("fixture-run-{index}"));
    }
    for (index, id) in chats.iter().enumerate() {
        text = text.replace(id, &format!("c-100-{index}"));
    }
    serde_json::from_str(&text).unwrap()
}

/// Three DMs (one a child thread), every transcript element that reads no
/// clock: finished attempts with a diff, a message and a command, a failed
/// and a queued turn while dispatch is paused, a Control notice, genes and a
/// room. Clock-derived ids are renamed so the bytes are stable.
fn fixture() -> State {
    use crate::runtime::RunStatus;
    let mut state = State::default();
    crate::ui_previews::genes(&mut state, 3);
    let lead = chat::create(&mut state);
    let child = chat::create(&mut state);
    let other = chat::create(&mut state);
    state.chats[1].parent_chat = Some(lead.clone());
    state.chats[0].title = "Lead <thread> & co".into();
    state.chats[0].room = Some("engineering".into());
    state.chats[0].lenses = Some(vec!["gene-0001".into()]);
    for text in ["Fix the <parser> & ship", "Try the risky path", "Queue this next"] {
        chat::send(&mut state, &lead, text, "test").unwrap();
    }
    chat::send(&mut state, &child, "Child work", "test").unwrap();
    let activity = |id: &str, kind: &str, text: &str, output: &str| crate::usage::Activity {
        id: id.into(),
        kind: kind.into(),
        text: text.into(),
        output: output.into(),
        status: "completed".into(),
    };
    let finished = |events| crate::usage::Attempt {
        status: crate::usage::AttemptStatus::Completed,
        workspace_id: Some("ws".into()),
        activity: events,
        ..Default::default()
    };
    {
        let run = &mut state.runtime.runs[0];
        run.status = RunStatus::Completed;
        *run.result = "Fixed <it> & shipped.".into();
        run.attempts = vec![finished(vec![
            activity("e0", "message", "Fixed <it> & shipped.", ""),
            activity(
                "e1",
                "file_change",
                "src/lib.rs",
                "@@ -1 +1 @@\n-old <a>\n+new & b\n context",
            ),
            activity("e2", "message", "Working on it", ""),
            activity("e3", "command_execution", "cargo test", "ok <1>"),
        ])];
        run.attempts[0].tokens = Some(crate::usage::Tokens {
            input_tokens: 1200,
            cached_input_tokens: 200,
            output_tokens: 300,
        });
    }
    {
        let run = &mut state.runtime.runs[1];
        run.status = RunStatus::Failed;
        *run.result = "It broke.".into();
        run.attempts = vec![
            finished(Vec::new()),
            finished(vec![activity("e4", "command_execution", "ls", "")]),
        ];
    }
    state.runtime.runs[3].status = RunStatus::Completed;
    // An observer's note in the child thread.
    state.runtime.runs[3].source = "advisor-note".into();
    state.chats[1].messages[0].text = "Advisor <note>\nCheck the & edge".into();
    state.chats[0].messages.push(chat::Message {
        speaker: control_context::agent_policy::MessageSpeaker::Control,
        text: "Port request approved".into(),
        run_id: None,
        reply: String::new(),
    });
    let mut state = renamed(&state, &[&lead, &child, &other]);
    // Not saved, so set after the rename.
    state.runtime.paused = Some("Paused for <maintenance>".into());
    state
}

#[test]
fn chat_pages_match_their_pinned_bytes() {
    let state = Store::new(fixture());
    let pages = [
        ("dm", "/dm/c-100-0", ""),
        ("dm_open_picker", "/dm/c-100-1", "; control_conversations=open"),
        ("dm_transcript", "/dm/c-100-0/transcript", ""),
        ("dm_child_transcript", "/dm/c-100-1/transcript", ""),
        ("dm_conversations", "/dm/c-100-1/conversations", ""),
        ("dm_requests", "/dm/c-100-0/requests", ""),
        ("dm_missing", "/dm/c-100-9", ""),
        ("dm_missing_transcript", "/dm/c-100-9/transcript", ""),
    ];
    let pages = pages.map(|(name, target, cookie)| (name, "GET", target, cookie, ""));
    assert_pinned(changed(&state, &pages));
}

/// The operator MCP's read tools answer with the same bytes whether or not
/// they hold the state lock while encoding.
#[test]
fn mcp_read_tools_match_their_pinned_bytes() {
    let state = Store::new(fixture());
    let call = mcp_call;
    let bodies = [
        ("mcp_chats_list", call("2", "control.chats.list", "{}")),
        (
            "mcp_chat_read",
            call(r#""read-1""#, "control.chat.read", r#"{"id":"c-100-0"}"#),
        ),
        (
            "mcp_chat_read_child",
            call("3", "control.chat.read", r#"{"id":"c-100-1"}"#),
        ),
        (
            "mcp_chat_read_missing",
            call("4", "control.chat.read", r#"{"id":"c-100-9"}"#),
        ),
        (
            "mcp_chat_read_invalid",
            call("5", "control.chat.read", r#"{"id":"../x"}"#),
        ),
    ];
    assert_pinned(mcp_changed(&state, &bodies));
}

/// The rest of the operator MCP's read-only tools and its tool list, on
/// the operator pages' state: genes, knowledge, room posts and personas.
#[test]
fn operator_read_tools_match_their_pinned_bytes() {
    let state = Store::new(operator_fixture());
    let call = mcp_call;
    let bodies = [
        (
            "mcp_tools_list",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_owned(),
        ),
        ("mcp_personas_list", call("2", "control.personas.list", "{}")),
        (
            "mcp_persona_read",
            call("3", "control.persona.read", r#"{"id":"lead"}"#),
        ),
        (
            "mcp_persona_read_revision",
            call("4", "control.persona.read", r#"{"id":"lead","revision":1}"#),
        ),
        (
            "mcp_persona_read_missing",
            call("5", "control.persona.read", r#"{"id":"nobody"}"#),
        ),
        ("mcp_persona_read_no_id", call("6", "control.persona.read", "{}")),
        ("mcp_lenses_list", call("7", "control.lenses.list", "{}")),
        ("mcp_lens_read", call("8", "control.lens.read", r#"{"id":"gene-0001"}"#)),
        (
            "mcp_lens_read_missing",
            call("9", "control.lens.read", r#"{"id":"nope"}"#),
        ),
        (
            "mcp_lenses_resolve",
            call(
                "10",
                "control.lenses.resolve",
                r#"{"lenses":["gene-0001","gene-0002"]}"#,
            ),
        ),
        (
            "mcp_lenses_resolve_unknown",
            call("11", "control.lenses.resolve", r#"{"lenses":["nope"]}"#),
        ),
        (
            "mcp_lenses_resolve_invalid",
            call("12", "control.lenses.resolve", r#"{"lenses":"gene-0001"}"#),
        ),
        (
            "mcp_knowledge_search",
            call("13", "control.knowledge.search", r#"{"query":"claim"}"#),
        ),
        (
            "mcp_knowledge_search_alias",
            call("14", "control_knowledge_search", r#"{"query":"notes","limit":2}"#),
        ),
        (
            "mcp_knowledge_search_invalid",
            call("15", "control.knowledge.search", r#"{"query":""}"#),
        ),
        (
            "mcp_knowledge_read_claim",
            call("16", "control.knowledge.read", r#"{"id":"claim:1"}"#),
        ),
        (
            "mcp_knowledge_read_document",
            call(
                "17",
                "control.knowledge.read",
                r#"{"id":"document:scratchpad","offset":3,"limit":5}"#,
            ),
        ),
        (
            "mcp_knowledge_read_missing",
            call("18", "control.knowledge.read", r#"{"id":"claim:9"}"#),
        ),
        ("mcp_rooms_list", call("19", "control.rooms.list", "{}")),
        (
            "mcp_room_read",
            call("20", "control.room.read", r#"{"room":"engineering"}"#),
        ),
        (
            "mcp_room_read_unknown",
            call("21", "control.room.read", r#"{"room":"nowhere"}"#),
        ),
        (
            "mcp_run_scores",
            call("22", "control.run.scores", r#"{"id":"c-100-1"}"#),
        ),
        (
            "mcp_run_scores_missing",
            call("23", "control.run.scores", r#"{"id":"c-100-9"}"#),
        ),
        (
            "mcp_run_scores_invalid",
            call("24", "control.run.scores", r#"{"id":"../x"}"#),
        ),
    ];
    assert_pinned(mcp_changed(&state, &bodies));
}

/// `control.artifacts.list` and `control.artifact.read` as far as they read
/// no files: the published outputs' files live outside the fixture.
#[test]
fn artifact_tools_match_their_pinned_bytes() {
    let mut state = fixture();
    // A published output whose manifest is not on disk.
    state.runtime.runs[3].attempts = vec![crate::usage::Attempt {
        artifact: Some("a-missing".into()),
        ..Default::default()
    }];
    let state = Store::new(state);
    let call = mcp_call;
    let bodies = [
        (
            "mcp_artifacts_none",
            call("1", "control.artifacts.list", r#"{"id":"c-100-0"}"#),
        ),
        (
            "mcp_artifacts_unreadable",
            call("2", "control.artifacts.list", r#"{"id":"c-100-1"}"#),
        ),
        (
            "mcp_artifacts_missing",
            call("3", "control.artifacts.list", r#"{"id":"c-100-9"}"#),
        ),
        (
            "mcp_artifacts_invalid",
            call("4", "control.artifacts.list", r#"{"id":"c-100-0","extra":1}"#),
        ),
        (
            "mcp_artifact_read_foreign",
            call(
                "5",
                "control.artifact.read",
                r#"{"id":"c-100-0","artifact":"a-missing","path":"x"}"#,
            ),
        ),
        (
            "mcp_artifact_read_unreadable",
            call(
                "6",
                "control.artifact.read",
                r#"{"id":"c-100-1","artifact":"a-missing","path":"x"}"#,
            ),
        ),
    ];
    assert_pinned(mcp_changed(&state, &bodies));
}

/// The chat MCP bridge's `Remote`, reaching the operator MCP through the
/// whole HTTP handler as `Hive` does over loopback.
struct Http<'a>(&'a Store);
impl crate::mcp_chat::Remote for Http<'_> {
    fn tool(&mut self, name: &str, arguments: serde_json::Value) -> std::io::Result<serde_json::Value> {
        let body = mcp_call("1", name, &arguments.to_string());
        let response = exchange(self.0, "POST", "/mcp", "", &body);
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        let result = serde_json::from_str::<serde_json::Value>(body).unwrap()["result"].take();
        if result["isError"] == true {
            return Err(std::io::Error::other(
                result["content"][0]["text"].as_str().unwrap().to_owned(),
            ));
        }
        Ok(result["structuredContent"]["data"].clone())
    }
}

/// `control.requests.list` and `control.services.list` with every kind of
/// request and a live service, and the chat MCP's `chat_requests` and
/// `chat_team`, which read them. A Discord DM's folder request is left out:
/// the test node authorizes no Discord principal.
#[test]
fn request_tools_match_their_pinned_bytes() {
    let mut state = requests_fixture();
    let mut discord = state.chats[0].clone();
    discord.id = "c-100-5".into();
    discord.actor = identity::Actor::DiscordDm {
        user_id: "7".parse().unwrap(),
        channel_id: "8".parse().unwrap(),
    };
    discord.project = Some(serde_json::from_value(serde_json::json!({"request":"req-5","path":"/srv/discord","grant":null,"head":null,"pending":true,"name":"discord"})).unwrap());
    state.chats.push(discord);
    let state = Store::new(state);
    let call = mcp_call;
    let bodies = [
        ("mcp_requests_list", call("1", "control.requests.list", "{}")),
        (
            "mcp_services_list",
            call("2", "control.services.list", r#"{"id":"c-100-0"}"#),
        ),
        (
            "mcp_services_list_team",
            call("3", "control.services.list", r#"{"id":"c-100-0","team":true}"#),
        ),
        (
            "mcp_services_list_discord",
            call("4", "control.services.list", r#"{"id":"c-100-5"}"#),
        ),
        (
            "mcp_services_list_missing",
            call("5", "control.services.list", r#"{"id":"c-100-9"}"#),
        ),
        (
            "mcp_services_list_invalid",
            call("6", "control.services.list", r#"{"id":"../x"}"#),
        ),
    ];
    let mut changed = mcp_changed(&state, &bodies);
    let bridge = |name: &str| {
        let text = crate::mcp_chat::call(
            &mut Http(&state),
            name,
            &serde_json::json!({"id":"c-100-0"}),
            control::permissions::Approvals::default(),
        );
        format!("{text:?}\n")
    };
    changed.extend(
        [("chat_requests", "chat_mcp_requests"), ("chat_team", "chat_mcp_team")]
            .into_iter()
            .filter_map(|(tool, name)| unpinned(name, &bridge(tool)).map(|refresh| format!("{tool}: {refresh}"))),
    );
    assert_pinned(changed);
}

/// The operator MCP's `control.chat.send` answers with the chat's detail,
/// built under the state lock. Commands only: a turn's run id reads the
/// clock.
#[test]
fn mcp_chat_send_matches_its_pinned_bytes() {
    let state = Store::new(fixture());
    let call = |id: &str, arguments: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"control.chat.send","arguments":{arguments}}}}}"#
        )
    };
    let bodies = [
        (
            "mcp_chat_send_command",
            call("1", r#"{"id":"c-100-2","message":"/improve off"}"#),
        ),
        ("mcp_chat_send_room", call("2", r#"{"id":"c-100-0","message":"/room"}"#)),
        (
            "mcp_chat_send_refused",
            call("3", r#"{"id":"c-100-2","message":"/steer"}"#),
        ),
        (
            "mcp_chat_send_missing",
            call("4", r#"{"id":"c-100-9","message":"Nobody"}"#),
        ),
        (
            "mcp_chat_send_invalid",
            call("5", r#"{"id":"../x","message":"Nobody"}"#),
        ),
    ];
    let pages = bodies
        .iter()
        .map(|(name, body)| (*name, "POST", "/mcp", "", body.as_str()))
        .collect::<Vec<_>>();
    assert_pinned(changed(&state, &pages));
}

/// The Bots page: live bots under a steering chat, a running agent, dead
/// bots in the graveyard and a queued turn.
fn bots_fixture() -> State {
    let mut state = crate::performance::dead_bots(2, 6);
    state
        .runtime
        .enqueue("Queued <survey> & report".into(), crate::runtime::RunKind::Chat, "test");
    // Only the steering chat's id is clock-derived.
    let steering = state.chats[0].id.clone();
    renamed(&state, &[&steering])
}

#[test]
fn bots_pages_match_their_pinned_bytes() {
    let state = Store::new(bots_fixture());
    let pages = [
        ("bots", "/?view=agents"),
        ("bots_search", "/?view=agents&q=ridge+1"),
        ("bots_search_both", "/?view=agents&q=tunnel"),
        ("bots_search_miss", "/?view=agents&q=zzzz"),
        ("bots_older", "/?view=agents&older=c-187000000000-0"),
        ("bots_graveyard", "/bots/graveyard?older=c-187000000000-0"),
    ]
    .map(|(name, target)| (name, "GET", target, "", ""));
    assert_pinned(changed(&state, &pages));
}

#[test]
fn history_pages_match_their_pinned_bytes() {
    let mut state = crate::performance::history_fixture(3, 24);
    state.runtime.runs[5].status = crate::runtime::RunStatus::Queued;
    state.chats[1].persona = Some("lead".into());
    state.runtime.notice = "Notice <for> & history".into();
    let dm = state.chats[0].id.clone();
    let state = crate::store::Store::new(state);
    let targets = [
        ("history", "/bots/history".to_owned()),
        ("history_expand", "/bots/history?expand=1".to_owned()),
        ("history_failed", "/bots/history?status=failed&kind=bot-wake".to_owned()),
        ("history_search", "/bots/history?q=tunnel+1".to_owned()),
        ("history_dm", format!("/bots/history?dm={dm}")),
        ("history_outside", "/bots/history?dm=-".to_owned()),
        ("history_turns", format!("/bots/history/turns?dm={dm}")),
        ("history_turns_missing", "/bots/history/turns?dm=c-100-9".to_owned()),
    ];
    let pages = targets
        .iter()
        .map(|(name, target)| (*name, "GET", target.as_str(), "", ""))
        .collect::<Vec<_>>();
    assert_pinned(changed(&state, &pages));
}

#[test]
fn dms_landing_matches_its_pinned_bytes() {
    let state = Store::new(fixture());
    let pages = [
        ("dms", "/dms", ""),
        ("dms_new", "/dms?new=1", "; control_recent_chat=c-100-0"),
        ("dms_discuss", "/dms?discuss=engineering", ""),
        ("dms_improve", "/dms?improve=1", ""),
        ("dms_recent", "/dms", "; control_recent_chat=c-100-1"),
    ]
    .map(|(name, target, cookie)| (name, "GET", target, cookie, ""));
    assert_pinned(changed(&state, &pages));
}

/// The write routes that release the state lock before waiting for the disk
/// answer with the bytes they did while waiting under it: redirects, the
/// JSON composer reply and the rejected pages. Each request runs on the state
/// the ones before it left.
#[test]
fn write_routes_match_their_pinned_bytes() {
    let state = Store::new(fixture());
    // The composer asks for JSON; the header rides after the cookie line.
    const JSON: &str = "\r\nAccept: application/json";
    let pages = [
        ("write_task", "/task", "", "objective=Pinned+%3Ctask%3E+%26+more"),
        ("write_task_empty", "/task", "", "objective="),
        ("write_retry_run", "/retry-run", "", "id=fixture-run-1"),
        ("write_retry_missing", "/retry-run", "", "id=fixture-run-9"),
        (
            "write_room",
            "/room",
            "",
            "channel=research&message=Pinned+room+%3Cpost%3E",
        ),
        ("write_room_rejected", "/room", "", "channel=nowhere&message=Lost"),
        (
            "write_spawn",
            "/spawn",
            "",
            "objective=Pinned+spawn&provider=Self-hosted",
        ),
        (
            "write_chat_send",
            "/dm/c-100-2/messages",
            "",
            "message=Pinned+%3Cmessage%3E",
        ),
        (
            "write_chat_send_json",
            "/dm/c-100-2/messages",
            JSON,
            "message=Pinned+json",
        ),
        (
            "write_chat_send_rejected",
            "/dm/c-100-2/messages",
            "",
            "message=%2Fsteer",
        ),
        (
            "write_chat_send_rejected_json",
            "/dm/c-100-2/messages",
            JSON,
            "message=%2Fsteer",
        ),
        ("write_chat_send_missing", "/dm/c-100-9/messages", "", "message=Nobody"),
    ]
    .map(|(name, target, cookie, body)| (name, "POST", target, cookie, body));
    assert_pinned(changed(&state, &pages));
}
/// Two DMs, one of 10 messages and one of 1,000 (turns and commands, each
/// turn finished), and three genes.
fn sends_fixture() -> State {
    let mut state = State::default();
    crate::ui_previews::genes(&mut state, 3);
    let small = chat::create(&mut state);
    let large = chat::create(&mut state);
    for (id, count) in [(&small, 10), (&large, 1000)] {
        for index in 0..count {
            if index % 2 == 0 {
                chat::send(&mut state, id, &format!("Turn <{index}> & more"), "test").unwrap();
                let run = state.runtime.runs.last_mut().unwrap();
                run.status = crate::runtime::RunStatus::Completed;
                *run.result = format!("Done <{index}>.");
            } else {
                chat::send(&mut state, id, "/improve off", "test").unwrap();
            }
        }
    }
    renamed(&state, &[&small, &large])
}
/// Hides the ids of the DMs `/dms` posts created, which read the clock and
/// count the chats this process made, and the length they change.
fn without_new_chats(response: String) -> String {
    let mut masked = String::new();
    let mut rest = response.as_str();
    while let Some(at) = rest.find("c-") {
        masked.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let clock = tail.bytes().take_while(u8::is_ascii_hexdigit).count();
        rest = if clock == 16 && tail[clock..].starts_with('-') {
            let count = tail[clock + 1..].bytes().take_while(u8::is_ascii_digit).count();
            masked.push_str("c-(new)");
            &tail[clock + 1 + count..]
        } else {
            masked.push_str("c-");
            tail
        };
    }
    masked.push_str(rest);
    let length = masked
        .lines()
        .find(|line| line.starts_with("Content-Length:"))
        .unwrap_or("")
        .to_owned();
    masked.replacen(&length, "Content-Length: (masked)", 1)
}

/// Chat sends and the other chat writes that answer without the chat's
/// detail: a turn and commands, as a form and from the composer (JSON), to
/// a short and a long DM, and their refusals. Each request runs on the
/// state the ones before it left.
#[test]
fn chat_sends_match_their_pinned_bytes() {
    let state = Store::new(sends_fixture());
    const JSON: &str = "\r\nAccept: application/json";
    let mut pages = Vec::new();
    for (size, id) in [("small", "c-100-0"), ("large", "c-100-1")] {
        for (what, body) in [
            ("turn", "message=Pinned+%3Cturn%3E+%26+more"),
            ("improve_off", "message=%2Fimprove+off"),
            ("improve_on", "message=%2Fimprove+on"),
            ("model", "message=%2Fmodel"),
            ("room", "message=%2Froom"),
            ("room_research", "message=%2Froom+research"),
            ("steer_rejected", "message=%2Fsteer"),
            ("lenses_turn", "message=With+genes&composition=1&lens_gene-0001=on"),
        ] {
            for (form, cookie) in [("form", ""), ("json", JSON)] {
                pages.push((
                    format!("send_{size}_{what}_{form}"),
                    format!("/dm/{id}/messages"),
                    cookie,
                    body,
                ));
            }
        }
        pages.push((
            format!("send_{size}_lenses"),
            format!("/dm/{id}/lenses"),
            "",
            "lens=gene-0002",
        ));
    }
    pages.push((
        "send_lenses_missing".into(),
        "/dm/c-100-9/lenses".into(),
        "",
        "lens=gene-0002",
    ));
    pages.push((
        "send_lenses_unknown".into(),
        "/dm/c-100-0/lenses".into(),
        "",
        "lens=gene-9999",
    ));
    let pages = pages
        .iter()
        .map(|(name, target, cookie, body)| (name.as_str(), "POST", target.as_str(), *cookie, *body))
        .collect::<Vec<_>>();
    assert_pinned(changed(&state, &pages));
    // The refusal first: the DM picker on its page names every DM.
    let created = [
        ("send_dms_create_rejected", "/dms", "", "message=%2Fsteer"),
        ("send_dms_create", "/dms", "", "message=First+%3Cturn%3E"),
        ("send_dms_create_empty", "/dms", "", "message="),
        ("send_improve_create", "/improve", "", "objective=Improve+%3Cit%3E"),
    ]
    .map(|(name, target, cookie, body)| (name, "POST", target, cookie, body));
    assert_pinned(changed_after(&state, &created, without_new_chats));
}
/// Three DMs whose actors may not run a task: no identity, a Discord server
/// channel and a Discord DM with no channel binding on this node.
fn unauthorized_fixture() -> State {
    let mut state = State::default();
    crate::ui_previews::genes(&mut state, 3);
    let ids: Vec<String> = (0..3).map(|_| chat::create(&mut state)).collect();
    chat::send(&mut state, &ids[0], "Before the actor changed", "test").unwrap();
    state.runtime.runs[0].status = crate::runtime::RunStatus::Completed;
    let actors = [
        crate::identity::Actor::Unknown,
        crate::identity::Actor::Discord {
            user_id: "5".parse().unwrap(),
            channel_id: "6".parse().unwrap(),
        },
        chat::discord_actor("d-7-8").unwrap(),
    ];
    for (session, actor) in state.chats.iter_mut().zip(actors) {
        session.actor = actor;
    }
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    renamed(&state, &ids)
}

/// Sends and gene changes to a DM whose actor may not run a task, and to a
/// chat that does not exist, answer as the base did and change nothing.
#[test]
fn chat_writes_without_task_authority_match_their_pinned_bytes() {
    let state = Store::new(unauthorized_fixture());
    let before = state.read(|state| serde_json::to_vec(state).unwrap()).unwrap();
    const JSON: &str = "\r\nAccept: application/json";
    let mut pages = Vec::new();
    for (who, id) in [
        ("unknown", "c-100-0"),
        ("channel", "c-100-1"),
        ("unbound", "c-100-2"),
        ("missing", "c-100-9"),
    ] {
        for (what, body) in [
            ("turn", "message=Not+allowed"),
            ("improve_on", "message=%2Fimprove+on"),
            ("model", "message=%2Fmodel"),
        ] {
            for (form, cookie) in [("form", ""), ("json", JSON)] {
                pages.push((
                    format!("refused_{who}_{what}_{form}"),
                    format!("/dm/{id}/messages"),
                    cookie,
                    body,
                ));
            }
        }
        pages.push((
            format!("refused_{who}_lenses"),
            format!("/dm/{id}/lenses"),
            "",
            "lens=gene-0001",
        ));
    }
    let pages = pages
        .iter()
        .map(|(name, target, cookie, body)| (name.as_str(), "POST", target.as_str(), *cookie, *body))
        .collect::<Vec<_>>();
    assert_pinned(changed(&state, &pages));
    assert!(state.read(|state| serde_json::to_vec(state).unwrap()).unwrap() == before);
}
/// One DM with every kind of pending request: a folder, a design question,
/// a port, a bot and a knowledge proposal.
fn requests_fixture() -> State {
    let (mut state, run, chat) = crate::ports::tests::pending_fixture();
    crate::design::tests::ask_without_io(&mut state, &run, "nonce-1", crate::design::tests::input()).unwrap();
    {
        let run = &mut state.runtime.runs[0];
        run.status = crate::runtime::RunStatus::Waiting;
        // Asked after any test clock, so its age reads zero.
        run.question.as_mut().unwrap().status = crate::design::Status::Pending {
            asked_at: Some(crate::design::UnixSeconds(4_000_000_000)),
        };
    }
    let proposal = [
        ("claim", "Builds need <nix>"),
        ("evidence", "The flake & lock"),
        ("scope", "This node"),
        ("validity", "When the flake changes"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    state.collective.action("/propose", &proposal);
    let mut value = serde_json::to_value(&state).unwrap();
    value["chats"][0]["project"] = serde_json::json!({"request":"req-9","path":"/srv/<data>","grant":null,"head":null,"pending":true,"name":"data"});
    value["bots"]["approvals"] = serde_json::json!([{"id":"bot-1","chat":chat,"requester":chat,"origin":run,"every_minutes":5,"room":true,"decision":"Pending","acknowledged":false}]);
    value["collective"]["proposals"][0]["source"] = serde_json::json!({"actor":value["chats"][0]["actor"],"chat":chat,"run":run,"attempt":null,"nonce":"n-1","lenses":[],"documents":[]});
    // The port's id hashes the clock-named run.
    let port = value["ports"][0]["id"].as_str().unwrap().to_owned();
    let text = value.to_string().replace(&port, &"f".repeat(port.len()));
    renamed(&serde_json::from_str(&text).unwrap(), &[&chat])
}

#[test]
fn request_panels_match_their_pinned_bytes() {
    let state = Store::new(requests_fixture());
    let pages = [
        ("requests_every_kind", "/dm/c-100-0/requests"),
        ("requests_missing", "/dm/c-100-9/requests"),
    ]
    .map(|(name, target)| (name, "GET", target, "", ""));
    assert_pinned(changed(&state, &pages));
}

/// Hides the Usage page's activity grid and the length it changes: the grid
/// ends today, read from the clock.
fn without_heatmap(response: String) -> String {
    let start = "<section class='usage-graph'>";
    let Some((before, rest)) = response.split_once(start) else {
        return response;
    };
    let after = rest.split_once("</section>").map_or("", |(_, after)| after);
    let length = before
        .lines()
        .find(|line| line.starts_with("Content-Length:"))
        .unwrap_or("");
    format!("{before}{start}…</section>{after}").replacen(length, "Content-Length: (masked)", 1)
}

/// Signs every backend's default account in as the worker tests do. The
/// Models page shows whether each has a login, and the tests of one binary
/// share the login folders: worker tests create these stand-ins (and none
/// removes them), so without this the page read "No login yet" or "Signed
/// in" depending on which tests ran first.
fn signed_in() {
    crate::backend::Backend::hosted().for_each(|backend| {
        crate::worker::tests::attempts::credential(backend);
    });
}

/// The operator pages under `/?view=`: DMs and runs with usage, genes, a
/// room thread, knowledge entries and access requests.
fn operator_fixture() -> State {
    signed_in();
    let mut state = fixture();
    let form = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>()
    };
    for (path, fields) in [
        (
            "/room",
            form(&[("channel", "engineering"), ("message", "Ship <it> & tell")]),
        ),
        ("/room", form(&[("channel", "general"), ("message", "Hello general")])),
        (
            "/propose",
            form(&[
                ("claim", "Pending <claim>"),
                ("evidence", "Logs & traces"),
                ("scope", "Node"),
                ("validity", "Weekly"),
            ]),
        ),
        (
            "/propose",
            form(&[
                ("claim", "Accepted claim"),
                ("evidence", "Tests"),
                ("scope", "All"),
                ("validity", "Never"),
            ]),
        ),
        ("/review", form(&[("id", "1"), ("decision", "accepted")])),
        (
            "/request",
            form(&[
                ("capability", "network.read"),
                ("resource", "example.org"),
                ("purpose", "Docs <fetch>"),
                ("authority", "human"),
            ]),
        ),
        (
            "/request",
            form(&[
                ("capability", "tool.access"),
                ("resource", "jq"),
                ("purpose", "Parse"),
                ("authority", "judge"),
            ]),
        ),
        ("/decide", form(&[("id", "1"), ("decision", "denied")])),
    ] {
        state.collective.action(path, &fields);
    }
    *state.note = "Notes link [[Plan]] & <more>".into();
    crate::knowledge::migrate(&mut state);
    state
}

#[test]
fn operator_pages_match_their_pinned_bytes() {
    let state = Store::new(operator_fixture());
    let pages = [
        ("view_authority", "/?view=authority", ""),
        ("view_knowledge_review", "/?view=knowledge-review", ""),
        ("view_providers", "/?view=providers", ""),
        ("view_tools", "/?view=tools", ""),
        ("view_tools_search", "/?view=tools&q=room+read", ""),
        ("view_tools_lens", "/?view=tools&q=lens:gene-0001", ""),
        ("view_tools_unknown_lens", "/?view=tools&q=lens:nope", ""),
        ("view_tools_lens_cookie", "/?view=tools&lens=gene-0001", ""),
        ("view_knowledge", "/?view=knowledge", ""),
        ("view_editor", "/?view=editor", ""),
        ("view_rooms", "/?view=rooms", ""),
        ("view_rooms_engineering", "/?view=rooms&channel=engineering", ""),
        ("view_nodes", "/?view=nodes", ""),
        ("view_manage", "/?view=manage", ""),
        ("view_root", "/", ""),
        ("view_hub", "/?view=hub", ""),
        ("view_runs", "/?view=runs", ""),
        ("view_session", "/?view=session&channel=c-100-0", ""),
        ("view_session_missing", "/?view=session&channel=c-100-9", ""),
        ("view_evolution", "/?view=evolution", ""),
    ]
    .map(|(name, target, cookie)| (name, "GET", target, cookie, ""));
    assert_pinned(changed(&state, &pages));
    let usage = [
        ("view_usage", "/?view=monitor"),
        ("view_internal", "/?view=history&channel=<b>"),
        ("view_unknown", "/?view=nowhere"),
    ]
    .map(|(name, target)| (name, "GET", target, "", ""));
    assert_pinned(changed_after(&state, &usage, without_heatmap));
}

/// The Permissions part of Access with something in every list: a live
/// service, a pending port, an automatic decision and requests waiting.
#[test]
fn permissions_overview_matches_its_pinned_bytes() {
    let mut state = requests_fixture();
    // Decided for the pending port, so its line carries the rule.
    let port = serde_json::to_value(&state.ports[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    state.auto_decisions.push(crate::auto_approval::Record {
        unix: 1_800_000_000,
        kind: "port".into(),
        id: port,
        chat: "c-100-0".into(),
        what: "egress <example.org>:443".into(),
        rule: "public-web".into(),
        decider: "policy".into(),
    });
    let state = Store::new(state);
    let pages = [("view_authority_requests", "GET", "/?view=authority", "", "")];
    assert_pinned(changed(&state, &pages));
}

/// The Models page with saved models, local agent and summary endpoints, a
/// CLI summary model and Memory Tree settings.
#[test]
fn models_page_matches_its_pinned_bytes() {
    let mut state = operator_fixture();
    let endpoint = |id: &str, url: &str, tools: bool, extra: serde_json::Value| {
        let mut value = serde_json::json!({"id":id,"model":format!("{id} <model>"),"url":url,"revision":"rev & 1","limits":{"context_tokens":32768,"output_tokens":1024,"input_bytes":28000,"output_bytes":8192},"timeout_seconds":1800,"tools":tools});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value::<crate::model_calls::Endpoint>(value).unwrap()
    };
    let runtime = &mut state.runtime;
    runtime.compaction.models = vec![
        endpoint(
            "qwen-agent",
            "http://127.0.0.1:8081/v1",
            true,
            serde_json::json!({"thinking":"low"}),
        ),
        endpoint(
            "summary-small",
            "http://127.0.0.1:8082/v1",
            false,
            serde_json::json!({}),
        ),
        endpoint("claude-summary", "", false, serde_json::json!({"cli":"claude"})),
    ];
    runtime.compaction.settings.enabled = true;
    runtime.compaction.settings.model = Some("summary-small".into());
    runtime.compaction.settings.chats = vec!["c-100-0".into(), "c-100-1".into()];
    runtime.models = serde_json::from_value(serde_json::json!([
        {"backend":"claude","model":"claude-<big>","capacity":{"context_tokens":200000,"output_tokens":8192,"overhead_tokens":4096}},
        {"backend":"grok","model":"grok-4"}
    ]))
    .unwrap();
    runtime.backend = crate::backend::Backend::Local;
    runtime.model = Some("qwen-agent".into());
    let state = Store::new(state);
    let pages = [("view_providers_models", "GET", "/?view=providers", "", "")];
    assert_pinned(changed(&state, &pages));
}

/// `GET /knowledge` in each mode, with a note that links to another and
/// one that links to nothing yet.
#[test]
fn knowledge_pages_match_their_pinned_bytes() {
    let mut state = operator_fixture();
    state.documents.push(crate::knowledge::Document {
        id: "plan".into(),
        title: "Plan".into(),
        body: "Back to [[Scratchpad.md]] & [[Missing <note>]]".into(),
        revision: 3,
        saved_at: Some(1_800_000_000),
    });
    let state = Store::new(state);
    let pages = [
        ("knowledge", "/knowledge"),
        ("knowledge_plan_saved", "/knowledge?doc=plan&saved=3"),
        ("knowledge_scratchpad", "/knowledge?doc=scratchpad&saved=9"),
        ("knowledge_graph", "/knowledge?mode=graph"),
        ("knowledge_review", "/knowledge?mode=review"),
        ("knowledge_new", "/knowledge?mode=new"),
        ("knowledge_missing", "/knowledge?doc=nowhere"),
        ("view_knowledge_plan", "/?view=knowledge"),
    ]
    .map(|(name, target)| (name, "GET", target, "", ""));
    assert_pinned(changed(&state, &pages));
}

/// A DM's artifacts, Memory Tree and Control input pages, as far as they
/// read no files: the files they read live outside the fixture.
#[test]
fn dm_file_pages_match_their_pinned_bytes() {
    let mut state = fixture();
    // A published output whose manifest is not on disk.
    state.runtime.runs[3].attempts = vec![crate::usage::Attempt {
        artifact: Some("a-missing".into()),
        ..Default::default()
    }];
    state.runtime.compaction.settings.chats = vec!["c-100-0".into()];
    let state = Store::new(state);
    let pages = [
        ("artifacts_none", "/dm/c-100-0/artifacts"),
        ("artifacts_unreadable", "/dm/c-100-1/artifacts"),
        ("artifacts_missing", "/dm/c-100-9/artifacts"),
        ("memory", "/dm/c-100-0/memory"),
        ("memory_block", "/dm/c-100-0/memory?block=0.1"),
        ("memory_entry", "/dm/c-100-0/memory?entry=2"),
        ("memory_child", "/dm/c-100-1/memory"),
        ("memory_missing", "/dm/c-100-9/memory"),
        ("context_no_trace", "/dm/c-100-0/context?run=fixture-run-0&attempt=1"),
        ("context_no_attempt", "/dm/c-100-0/context?run=fixture-run-0&attempt=0"),
        ("context_outside", "/dm/c-100-2/context?run=fixture-run-0&attempt=1"),
        ("context_missing", "/dm/c-100-9/context?run=fixture-run-0&attempt=1"),
    ]
    .map(|(name, target)| (name, "GET", target, "", ""));
    assert_pinned(changed(&state, &pages));
}
