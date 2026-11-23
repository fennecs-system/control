//! Chat pages pinned byte for byte through the whole request handler, so
//! moving their rendering out of the state lock cannot change a response.
//! An intended page change regenerates the fixtures (see the panic message).
use super::*;
use std::io::Read;

const SESSION: &str = "page-bytes-operator-session";

fn request(state: &Arc<Mutex<State>>, target: &str, cookie: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    let (state, access) = (state.clone(), Arc::new(access::Access::for_tests(SESSION)));
    let handler = std::thread::spawn(move || handle(server, state, access));
    write!(
        client,
        "GET {target} HTTP/1.1\r\nHost: {address}\r\nCookie: control_operator={SESSION}{cookie}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    handler.join().unwrap();
    response
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
    // Rename clock-derived ids through the saved form.
    let mut text = serde_json::to_string(&state).unwrap();
    let runs = state
        .runtime
        .runs
        .iter()
        .map(|run| run.id().to_string())
        .collect::<Vec<_>>();
    for (index, id) in runs.iter().enumerate() {
        text = text.replace(id, &format!("fixture-run-{index}"));
    }
    for (index, id) in [&lead, &child, &other].into_iter().enumerate() {
        text = text.replace(id.as_str(), &format!("c-100-{index}"));
    }
    let mut state: State = serde_json::from_str(&text).unwrap();
    // Not saved, so set after the rename.
    state.runtime.paused = Some("Paused for <maintenance>".into());
    state
}

#[test]
fn chat_pages_match_their_pinned_bytes() {
    let state = Arc::new(Mutex::new(fixture()));
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
    let changed = pages
        .iter()
        .filter_map(|(name, target, cookie)| {
            let actual = request(&state, target, cookie);
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pages")
                .join(format!("{name}.http"));
            let pinned = std::fs::read_to_string(&path).unwrap_or_default();
            (actual != pinned).then(|| {
                let fresh = std::env::temp_dir().join(format!("control-page-{name}.http"));
                std::fs::write(&fresh, &actual).unwrap();
                format!("{target}: cp {} {}", fresh.display(), path.display())
            })
        })
        .collect::<Vec<_>>();
    assert!(
        changed.is_empty(),
        "Page bytes changed. If the change is intended:\n{}",
        changed.join("\n")
    );
}
