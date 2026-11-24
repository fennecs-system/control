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
            let actual = mask(exchange(state, method, target, cookie, body));
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pages")
                .join(format!("{name}.http"));
            let pinned = std::fs::read_to_string(&path).unwrap_or_default();
            (actual != pinned).then(|| {
                let fresh = std::env::temp_dir().join(format!("control-page-{name}.http"));
                std::fs::write(&fresh, &actual).unwrap();
                format!("{method} {target}: cp {} {}", fresh.display(), path.display())
            })
        })
        .collect()
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
    let call = |id: &str, name: &str, arguments: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
        )
    };
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

/// The operator pages under `/?view=`: DMs and runs with usage, genes, a
/// room thread, knowledge entries and access requests.
fn operator_fixture() -> State {
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
    state.note = "Notes link [[Plan]] & <more>".into();
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
