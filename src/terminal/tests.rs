
#[test]
fn github_inspection_uses_the_typed_read_without_sending_a_chat_message() {
    use std::io::{BufRead, Read, Write};
    for (input,tool,arguments) in [
        ("/github 170 checks 2", "control_github_read",json!({"chat_id":"c-inspection","query":{"pull_request":170,"section":"checks","page":2}})),
        ("/github 170 merge 1234567890123456789012345678901234567890", "control_github_write",json!({"chat_id":"c-inspection","operation":{"action":"merge","pull_request":170,"expected_head_sha":"1234567890123456789012345678901234567890","method":"squash"}})),
        ("/github 170 comment Review findings\n\nPlease check this.", "control_github_write",json!({"chat_id":"c-inspection","operation":{"action":"comment","pull_request":170,"body":"Review findings\n\nPlease check this."}})),
    ] {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut reader = io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("POST /mcp "));
        let mut length = 0;
        loop {
            line.clear();reader.read_line(&mut line).unwrap();
            if line=="\r\n" {break;}
            if let Some((key,value))=line.split_once(':') && key.eq_ignore_ascii_case("content-length") {length=value.trim().parse().unwrap();}
        }
        let mut body=vec![0;length];reader.read_exact(&mut body).unwrap();
        let request:Value=serde_json::from_slice(&body).unwrap();
        assert_eq!(request["params"]["name"],tool);
        assert_eq!(request["params"]["arguments"],arguments);
        let response=json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"head_sha":"verified-head","items":[]}}}}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
    });
    let mut hive=Hive::fixture(Client::new(),format!("http://{address}/mcp"));
    let mut app=App {active:"c-inspection".into(),input:input.into(),detail:json!({"messages":[{"text":"Retain this conversation"}]}),..Default::default()};
    let before=app.detail.clone();
    assert!(!app.submit(&mut hive).unwrap());
    assert!(app.inspector.as_ref().unwrap().1.contains("verified-head"));
    assert_eq!(app.detail,before);
    assert!(app.input.is_empty());
    server.join().unwrap();
    app.input="/github 170 merge".into();
    assert!(app.submit(&mut hive).is_err(),"invalid commands fail before contacting the closed server");
    }
}

#[test]
fn folder_prompt_stays_at_the_live_tail_and_tracks_replaced_requests() {
    let mut app = App {
        active: "c-fixture".into(),
        detail: json!({
            "project":{"request":"p-current","path":"/node/current","name":"current","pending":true,"grant":"previous-grant","message_index":2,"mounts":[{"name":"approved","message_index":0}]},
            "messages":[
                {"text":"/open /node/approved","reply":"Review and approve folder access: /chat/c-fixture/project"},
                {"text":"/open /node/replaced","reply":"Review and approve folder access: /chat/c-fixture/project"},
                {"text":"/open /node/current","reply":"Review and approve folder access: /chat/c-fixture/project"},
                {"text":"A later message","reply":"Latest response"},
                {"text":"/open /node/invalid","reply":"The folder could not be read"}
            ]
        }),
        ..Default::default()
    };
    let transcript = app.transcript().to_string();
    assert!(transcript.contains("Opened /workspace/approved"));
    assert!(transcript.contains("Folder request replaced by a later request."));
    assert!(transcript.contains("/trust p-current to approve"));
    assert!(transcript.contains("Open as /workspace/current"));
    assert!(transcript.contains("The folder could not be read"));
    assert!(
        transcript.find("Latest response").unwrap()
            < transcript
                .find("Folder access requested · /node/current")
                .unwrap()
    );
    assert!(!transcript.contains("/chat/c-fixture/project"));
    app.detail["project"]["pending"] = json!(false);
    assert!(!app.transcript().to_string().contains("/trust"));
}

#[test]
fn terminal_folder_approval_uses_displayed_identity_and_retains_errors_without_a_browser() {
    use std::io::{BufRead, Read};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            let mut length = 0;
            let mut origin = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    if name.eq_ignore_ascii_case("origin") {
                        origin = value.trim().to_owned();
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            if index == 0 || index == 2 {
                assert_eq!(path, "/chat/c-fixture/project");
                assert_eq!(origin, format!("http://{addr}"));
                let fields: std::collections::HashMap<_, _> = Url::parse(&format!(
                    "http://fixture/?{}",
                    String::from_utf8(bytes).unwrap()
                ))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
                assert_eq!(fields.len(), 2);
                assert_eq!(fields["request"], "p-fixture");
                assert_eq!(fields["approve"], "yes");
                control_http::respond(
                    &mut stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    "Location: /chat/c-fixture\r\n",
                    control_http::Policy::Embedded,
                )
                .unwrap();
            } else {
                assert_eq!(path, "/mcp");
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(request["params"]["name"], "control.chat.read");
                assert_eq!(request["params"]["arguments"], json!({"id":"c-fixture"}));
                let project = if index == 1 {
                    json!({"request":"p-fixture","path":"/node/project","name":"project","pending":true,"error":"Source is not a directory"})
                } else {
                    json!({"request":"p-fixture","path":"/node/project","name":"project","pending":false,"grant":"g-fixture"})
                };
                let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"id":"c-fixture","project":project}}}});
                control_http::respond(
                    &mut stream,
                    "200 OK",
                    "application/json",
                    response.to_string().as_bytes(),
                    "",
                    control_http::Policy::Embedded,
                )
                .unwrap();
            }
        }
    });
    let mut hive = Hive::fixture(Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(), format!("http://{addr}/mcp"));
    let mut app = App {
        active: "c-fixture".into(),
        detail: json!({"project":{"request":"p-fixture","path":"/node/project","name":"project","pending":true}}),
        input: "/trust p-obsolete".into(),
        ..Default::default()
    };
    assert!(
        app.submit(&mut hive)
            .unwrap_err()
            .to_string()
            .contains("request changed")
    );
    app.input = "/trust".into();
    assert_eq!(
        app.submit(&mut hive).unwrap_err().to_string(),
        "Source is not a directory"
    );
    assert!(
        app.transcript()
            .to_string()
            .contains("Source is not a directory")
    );
    app.input = "/trust p-fixture".into();
    app.submit(&mut hive).unwrap();
    assert_eq!(app.status, "Folder ready · isolated worktree");
    assert!(!app.transcript().to_string().contains("/trust"));
    app.input = "/trust".into();
    assert!(
        app.submit(&mut hive)
            .unwrap_err()
            .to_string()
            .contains("No folder request")
    );
    app.input = "/open".into();
    app.submit(&mut hive).unwrap();
    assert_eq!(
        app.status,
        "Use /open /absolute/path or /open --as NAME /absolute/path"
    );
    server.join().unwrap();
}

#[test]
fn listen_commands_reach_the_server_and_render_returned_state() {
    use std::io::{BufRead, Read};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for command in ["/listen", "/listen off"] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /mcp "));
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["params"]["name"], "control.chat.send");
            assert_eq!(
                request["params"]["arguments"],
                json!({"id":"c-fixture","message":command})
            );
            let status = if command == "/listen" {
                json!({"room":"research","state":"listening"})
            } else {
                Value::Null
            };
            let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"id":"c-fixture","room_listener":status}}}});
            control_http::respond(
                &mut stream,
                "200 OK",
                "application/json",
                response.to_string().as_bytes(),
                "",
                control_http::Policy::Embedded,
            )
            .unwrap();
        }
    });
    let mut hive = Hive::fixture(Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(), format!("http://{addr}/mcp"));
    let mut app = App {
        active: "c-fixture".into(),
        input: "/listen".into(),
        ..Default::default()
    };
    app.submit(&mut hive).unwrap();
    assert!(
        app.transcript()
            .to_string()
            .contains("Room #research · listening")
    );
    app.input = "/listen off".into();
    app.submit(&mut hive).unwrap();
    assert!(!app.transcript().to_string().contains("listening"));
    server.join().unwrap();
}

#[test]
fn parent_transcript_exposes_child_decisions_and_answer_command() {
    let app = super::App {
        detail: serde_json::json!({"room_listener":{"room":"research","state":"listening"},"messages":[{"text":"Earlier prompt","reply":"Earlier response"}],"pending_questions":[{"summary":"Choose a route\n1. Relay\n2. Stop\n/answer q-child YOUR_ANSWER"}],"pending_bots":[{"request_id":"bot-child","chat_id":"c-child"}]}),
        ..Default::default()
    };
    let transcript = app.transcript().to_string();
    assert!(transcript.contains("Choose a route"));
    assert!(transcript.contains("/answer q-child YOUR_ANSWER"));
    assert!(transcript.contains("Room #research · listening"));
    assert!(
        transcript.contains("Bot request · c-child") && transcript.contains("/bot allow bot-child")
    );
    assert!(
        transcript.find("Earlier response").unwrap()
            < transcript.find("Decision requested").unwrap(),
        "Pending decisions must remain visible at the live tail"
    );
}
#[test]
fn terminal_port_decisions_require_explicit_loopback_and_use_operator_http() {
    use std::io::{BufRead, Read};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            let mut length = 0;
            let mut origin = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    if name.eq_ignore_ascii_case("origin") {
                        origin = value.trim().to_owned();
                    }
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let value = if index == 0 {
                assert_eq!(path, "/tools/ports");
                assert_eq!(origin, format!("http://{addr}"));
                let fields: std::collections::HashMap<_, _> = Url::parse(&format!(
                    "http://fixture/?{}",
                    String::from_utf8(bytes).unwrap()
                ))
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
                assert_eq!(fields["id"], "port-fixture");
                assert_eq!(fields["revoke"], "yes");
                assert_eq!(fields["seconds"], "-1");
                assert_eq!(fields["return_chat"], "c-fixture");
                assert!(!fields.contains_key("loopback"));
                json!({"ok":true})
            } else {
                assert_eq!(path, "/mcp");
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(request["params"]["name"], "control.chat.read");
                json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":{"id":"c-fixture","pending_ports":[]}}}})
            };
            control_http::respond(
                &mut stream,
                "200 OK",
                "application/json",
                value.to_string().as_bytes(),
                "",
                control_http::Policy::Embedded,
            )
            .unwrap();
        }
    });
    let mut hive = Hive::fixture(Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(), format!("http://{addr}/mcp"));
    let mut app = App {
        active: "c-fixture".into(),
        detail: json!({"messages":[{"text":"Earlier prompt","reply":"Earlier response"}],"pending_ports":[{"port_id":"port-fixture","name":"relay","target":{"host":"127.0.0.1","port":25565},"loopback_required":true}]}),
        input: "/port allow port-fixture".into(),
        ..Default::default()
    };
    let transcript = app.transcript().to_string();
    assert!(
        transcript.find("Earlier response").unwrap() < transcript.find("Port request").unwrap()
    );
    assert!(
        app.submit(&mut hive)
            .unwrap_err()
            .to_string()
            .contains("--loopback")
    );
    app.input = "/port deny port-fixture".into();
    app.submit(&mut hive).unwrap();
    assert_eq!(app.detail["pending_ports"], json!([]));
    server.join().unwrap();
}
#[test]
fn startup_only_resumes_an_explicit_selection() {
    let args = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(resume_selection(&args(&["control", "tui"])).unwrap(), None);
    for option in ["--resume", "--session"] {
        assert_eq!(
            resume_selection(&args(&["control", option, "c-selected"])).unwrap(),
            Some("c-selected")
        );
        assert!(resume_selection(&args(&["control", option])).is_err());
        assert!(resume_selection(&args(&["control", option, "--check"])).is_err());
    }
    assert!(resume_selection(&args(&["control", "--resume", "c-a", "--session", "c-b"])).is_err());
}
#[test]
fn refresh_keeps_a_new_chat_blank_and_preserves_explicit_resume() {
    use std::io::{BufRead, Read};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut calls = Vec::new();
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let tool = request["params"]["name"].as_str().unwrap();
            calls.push(tool.to_owned());
            let value = match tool {
                "control.chats.list" => {
                    json!([{"id":"c-existing","title":"Existing","revision":7}])
                }
                "control.chat.read" => json!({"id":"c-existing","title":"Existing","revision":7}),
                _ => panic!("Refreshing must not create or send chats"),
            };
            let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{"isError":false,"structuredContent":{"data":value}}});
            control_http::respond(
                &mut stream,
                "200 OK",
                "application/json",
                response.to_string().as_bytes(),
                "",
                control_http::Policy::Embedded,
            )
            .unwrap();
        }
        calls
    });
    let mut hive = Hive::fixture(Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(), format!("http://{addr}/mcp"));
    let mut app = App::default();
    app.refresh(&mut hive).unwrap();
    app.refresh(&mut hive).unwrap();
    assert!(app.active.is_empty());
    assert!(app.detail.is_null());
    assert_eq!(app.chats.len(), 1);
    app.active = "c-existing".into();
    app.refresh(&mut hive).unwrap();
    app.refresh(&mut hive).unwrap();
    assert_eq!(app.active, "c-existing");
    assert_eq!(app.detail["title"], "Existing");
    assert_eq!(
        server.join().unwrap(),
        [
            "control.chats.list",
            "control.chats.list",
            "control.chats.list",
            "control.chat.read",
            "control.chats.list"
        ]
    );
}
use super::*;

#[test]
fn completing_commands_only_edits_the_draft_and_never_changes_the_conversation() {
    let mut app = App { input: "/con".into(), active: "chat-a".into(), detail: json!({"revision":3,"messages":[]}), ..Default::default() };
    let before = app.detail.clone();
    app.complete_command();
    assert_eq!(app.input, "/context ");
    assert_eq!(app.active, "chat-a");
    assert_eq!(app.detail, before);
    app.input = "/".into();
    app.input_changed();
    let candidates = app.command_matches();
    app.command_selection = candidates.iter().position(|entry| entry.text == "/cancel all").unwrap();
    app.complete_command();
    assert_eq!(app.input, "/cancel all ");
    assert_eq!(app.detail, before, "completion must never execute cancellation");
    app.input = "/con".into();
    app.commands_hidden = true;
    assert!(app.command_matches().is_empty());
    app.input_changed();
    assert_eq!(app.command_matches().len(), 1);
    assert_eq!(command_line("/con").spans[0].style.fg, Some(Color::Rgb(181, 160, 255)));
}
#[test]
#[ignore = "requires isolated control-bench-qa server with benchmark worker image; no model calls"]
fn live_rust_benchmarks_use_mcp_and_compare_html_escaping() {
    let directory = crate::config::directory().unwrap();
    assert!(directory.parent().is_some_and(|p| {
        p.parent() == Some(std::path::Path::new("/private/tmp"))
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("control-bench-qa."))
    }));
    let server = std::env::var("CONTROL_QA_URL").unwrap();
    let mut hive = Hive::connect(&server).unwrap();
    let tools = hive.rpc("tools/list", json!({})).unwrap();
    assert!(
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control.rust.bench")
    );
    let current = include_str!("../../tools/rust-analysis/escape-candidate.rs")
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    let previous = r#"fn previous(s:&str)->String {s.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&#39;")}"#;
    for (name, input) in [
        (
            "plain",
            "ordinary command output without markup\n".repeat(48),
        ),
        ("escaped", "Synthetic output <&> λ\n".repeat(48)),
        ("sparse", format!("{}<", "x".repeat(1800))),
        ("short", "é<&>λ'🦊\"".into()),
    ] {
        let setup = format!(
            "{current}\n{previous}\nlet input=String::from({}); assert_eq!(escape_html(&input),previous(&input));",
            serde_json::to_string(&input).unwrap()
        );
        for implementation in ["previous", "escape_html"] {
            let result=hive.tool("control.rust.bench",json!({"setup":setup,"code":format!("{implementation}(std::hint::black_box(input.as_str()))"),"iterations":1000,"samples":9})).unwrap();
            assert_eq!(result["success"], true, "{}", result["output"]);
            assert_eq!(result["timings"]["samples"], 9);
            assert_eq!(result["opt_level"], 3);
            println!(
                "{}",
                json!({"workload":"html_escape","case":name,"implementation":implementation,"timings":result["timings"],"image":result["image"]})
            );
        }
    }
    for (code, status) in [("not valid Rust", "compile 1"), ("loop {}", "run timeout")] {
        let result = hive
            .tool("control.rust.bench", json!({"code":code}))
            .unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(result["status"], status);
        assert!(result["timings"].is_null());
    }
}
#[test]
#[ignore = "requires isolated control-selfdev-qa server, declared build image, own authenticated development model; three or more model calls"]
fn live_improve_build_and_independent_judge_roundtrip() {
    let state_dir = crate::config::state_directory().unwrap();
    assert!(state_dir.parent().is_some_and(|p| {
        p.parent() == Some(std::path::Path::new("/private/tmp"))
            && p.file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with("control-selfdev-qa."))
    }));
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated self-development server");
    let mut hive = Hive::connect(&server).unwrap();
    let chat = hive
        .tool("control.chat.create", json!({"mode":"improve"}))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let submitted=hive.tool("control.chat.send",json!({"id":chat,"message":"Implement this small self-development verification task in the isolated Control source. Create docs/self-development-proof.md containing exactly: Control self-development verified. Do not change any other source file. Then use control_workspace_start with environment build and timeout_seconds 1800 to run cargo test --locked --bin control github::tests. Use the status tool until the job completes and verify exit code zero. Next call control_agent_spawn_blocking with role judge and objective: Review docs/self-development-proof.md against the requested exact text Control self-development verified. Inspect the actual file in the read-only source; report findings and do not modify files. Stop after blocking-spawn acknowledgement. When Control resumes you with the judge feedback, use control_agent_result section summary to inspect its full report, verify the proof file remains correct and finish briefly. Do not publish a PR, deploy, inspect credentials or modify the host checkout."})).unwrap();
    let root = submitted["messages"][0]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let started = Instant::now();
    loop {
        let detail = hive.tool("control.chat.read", json!({"id":chat})).unwrap();
        let last = detail["messages"].as_array().unwrap().last().unwrap();
        let status = last["status"].as_str().unwrap();
        if status == "completed" {
            break;
        }
        assert!(
            ["queued", "running", "waiting", "continued"].contains(&status),
            "Self-development run ended with {status}: {}",
            last["reply"]
        );
        assert!(
            started.elapsed() < Duration::from_secs(900),
            "Self-development workflow timed out"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    let saved: Value =
        serde_json::from_slice(&fs::read(state_dir.join("state.json")).unwrap()).unwrap();
    let runs = saved["runtime"]["runs"].as_array().unwrap();
    let parent = runs.iter().find(|r| r["id"] == root).unwrap();
    assert_eq!(
        parent["status"], "continued",
        "The developer must actually delegate, not merely describe a review"
    );
    let judge = runs
        .iter()
        .find(|r| r["persona"]["role"] == "judge" && r["persona"]["root"] == root)
        .expect("No independent judge was run");
    assert_eq!(judge["backend"], parent["backend"]);
    assert_eq!(judge["model"], parent["model"]);
    assert_eq!(judge["status"], "completed");
    assert!(
        judge["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["review_only"] == true && a["artifact"].is_null())
    );
    assert!(
        runs.iter()
            .filter(|r| r["chat_id"] == chat)
            .flat_map(|r| r["attempts"].as_array().unwrap())
            .flat_map(|a| a["command_jobs"].as_array().into_iter().flatten())
            .any(|job| job["build_environment"] == true
                && job["exit_code"] == 0
                && job["status"] == "completed"),
        "A real declared build job must pass"
    );
    let final_run = runs
        .iter()
        .rev()
        .find(|r| r["chat_id"] == chat && r["status"] == "completed")
        .unwrap();
    let artifact = final_run["attempts"].as_array().unwrap().last().unwrap()["artifact"]
        .as_str()
        .unwrap();
    let proof = hive
        .tool(
            "control.artifact.read",
            json!({"id":chat,"artifact":artifact,"path":"docs/self-development-proof.md"}),
        )
        .unwrap();
    assert_eq!(
        proof["text"].as_str().unwrap().trim(),
        "Control self-development verified."
    );
    assert!(
        !crate::config::source_root()
            .unwrap()
            .join("docs/self-development-proof.md")
            .exists(),
        "The original checkout must remain unchanged"
    );
}
#[test]
#[ignore = "requires isolated server and authenticated worker; two small model calls, optional reload mode makes no calls"]
fn live_workspace_diff_tracks_real_edits_and_persists() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let fixture = crate::config::state("diff-qa-chat").unwrap();
    let id = if std::env::var_os("CONTROL_QA_DIFF_RELOAD").is_some() {
        fs::read_to_string(&fixture).unwrap()
    } else {
        let id = hive.tool("control.chat.create", json!({})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        for request in [
            "Create edit-demo.txt containing exactly two lines: alpha and beta, each ending with a newline. Do not create any other files. Reply briefly.",
            "Edit edit-demo.txt: replace only the second line beta with gamma, retaining alpha and the final newline. Use the edit/patch tool if available. Do not create any other files. Reply briefly.",
        ] {
            let submitted = hive
                .tool("control.chat.send", json!({"id":id,"message":request}))
                .unwrap();
            let index = submitted["messages"].as_array().unwrap().len() - 1;
            let deadline = Instant::now() + Duration::from_secs(180);
            loop {
                let detail = hive.tool("control.chat.read", json!({"id":id})).unwrap();
                let status = detail["messages"][index]["status"].as_str().unwrap();
                if status == "completed" {
                    break;
                }
                assert!(
                    ["queued", "running"].contains(&status),
                    "Diff task ended with {status}"
                );
                assert!(Instant::now() < deadline, "Diff task timed out");
                std::thread::sleep(Duration::from_millis(300));
            }
            if index == 0 {
                let first = hive.tool("control.chat.read", json!({"id":id})).unwrap();
                let changes = first["messages"][0]["attempts"][0]["activity"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|event| event["id"] == "workspace-diff")
                    .expect("New file creation must also have a diff");
                assert_eq!(changes["text"], "1 file · +2 −0");
                assert!(
                    changes["output"]
                        .as_str()
                        .unwrap()
                        .contains("+alpha\n+beta\n")
                );
            }
        }
        fs::write(&fixture, &id).unwrap();
        id
    };
    let detail = hive.tool("control.chat.read", json!({"id":id})).unwrap();
    let events = detail["messages"][1]["attempts"][0]["activity"]
        .as_array()
        .unwrap();
    let patch = events
        .iter()
        .find(|e| e["id"] == "workspace-diff")
        .expect("No observed workspace diff");
    assert_eq!(patch["text"], "1 file · +1 −1");
    let output = patch["output"].as_str().unwrap();
    assert!(output.contains("edit-demo.txt") && output.contains(" alpha\n-beta\n+gamma\n"));
    let page = hive
        .http()
        .get(format!("{server}/chat/{id}/transcript"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(
        page.contains("class='edit-diff' open")
            && page.contains("diff-line diff-add'>+gamma")
            && page.contains("diff-line diff-remove'>-beta")
    );
}
#[test]
#[ignore = "requires isolated server, authenticated container worker and two small model calls; optional pause/resume phases test server restart"]
fn live_design_question_pauses_and_resumes_checkpoint() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let fixture = crate::config::state("design-question-qa-id").unwrap();
    let id = if std::env::var_os("CONTROL_QA_DESIGN_RESUME").is_some() {
        fs::read_to_string(&fixture).unwrap()
    } else {
        let id = hive.tool("control.chat.create", json!({})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        hive.tool("control.chat.send",json!({"id":id,"message":"Checkpoint test: first write checkpoint-proof.txt containing exactly design-before. Then use control_design_ask to ask Which output format? with options Plain text and JSON, recommended index 1. Stop after asking. When the user answers, continue this original task: verify checkpoint-proof.txt still contains design-before and write answer-proof.txt containing exactly the chosen option text. Do not ask again. Reply briefly."})).unwrap();
        id
    };
    let deadline = Instant::now() + Duration::from_secs(180);
    let paused = loop {
        let detail = hive.tool("control.chat.read", json!({"id":id})).unwrap();
        let status = detail["messages"][0]["status"].as_str().unwrap_or("");
        if status == "waiting" {
            break detail;
        }
        assert!(
            ["queued", "running"].contains(&status),
            "Design ask ended with {status}"
        );
        assert!(Instant::now() < deadline, "Design ask did not pause");
        std::thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(
        paused["messages"][0]["question"]["input"]["options"],
        json!(["Plain text", "JSON"])
    );
    let artifact = paused["messages"][0]["attempts"][0]["artifact"]
        .as_str()
        .unwrap();
    let before = hive
        .tool(
            "control.artifact.read",
            json!({"id":id,"artifact":artifact,"path":"checkpoint-proof.txt"}),
        )
        .unwrap();
    assert_eq!(before["text"], "design-before");
    let page = hive
        .http()
        .get(format!("{server}/chat/{id}/transcript"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(page.contains("value='/answer") && page.contains("JSON · recommended"));
    if std::env::var_os("CONTROL_QA_DESIGN_PAUSE_ONLY").is_some() {
        fs::write(fixture, &id).unwrap();
        return;
    }
    let question_id = paused["messages"][0]["question"]["id"].as_str().unwrap();
    let response = hive
        .http()
        .post(format!("{server}/chat/{id}/messages"))
        .header("Origin", &server)
        .form(&[("message", format!("/answer {question_id} 2"))])
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 303);
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let detail = hive.tool("control.chat.read", json!({"id":id})).unwrap();
        assert_eq!(detail["messages"][0]["question"]["answer"], "JSON");
        let message = &detail["messages"][1];
        let status = message["status"].as_str().unwrap_or("");
        if status == "completed" {
            let artifact = message["attempts"][0]["artifact"].as_str().unwrap();
            let after = hive
                .tool(
                    "control.artifact.read",
                    json!({"id":id,"artifact":artifact,"path":"answer-proof.txt"}),
                )
                .unwrap();
            assert_eq!(after["text"], "JSON");
            break;
        }
        assert!(
            ["queued", "running"].contains(&status),
            "Design continuation ended with {status}"
        );
        assert!(Instant::now() < deadline, "Design answer did not complete");
        std::thread::sleep(Duration::from_millis(300));
    }
}
#[test]
#[ignore = "requires isolated local server with workers disabled; no model calls"]
fn live_harness_commands_create_children_and_cancel_without_losing_history() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let parent = hive.tool("control.chat.create", json!({})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    hive.tool(
        "control.chat.send",
        json!({"id":parent,"message":"Build a parser"}),
    )
    .unwrap();
    hive.tool(
        "control.chat.send",
        json!({"id":parent,"message":"/steer Include tests"}),
    )
    .unwrap();
    hive.tool(
        "control.chat.send",
        json!({"id":parent,"message":"/btw Explain the grammar"}),
    )
    .unwrap();
    let detail = hive
        .tool(
            "control.chat.send",
            json!({"id":parent,"message":"/spawn Review the grammar"}),
        )
        .unwrap();
    assert_eq!(detail["messages"].as_array().unwrap().len(), 4);
    for message in &detail["messages"].as_array().unwrap()[2..] {
        let child = message["reply"]
            .as_str()
            .unwrap()
            .strip_prefix("Agent conversation: /chat/")
            .unwrap();
        let child = hive.tool("control.chat.read", json!({"id":child})).unwrap();
        assert_eq!(child["parent_chat"], parent);
        assert_eq!(child["mode"], "chat");
        assert_eq!(child["messages"][0]["status"], "queued");
    }
    let first = detail["messages"][0]["run_id"].as_str().unwrap();
    let detail = hive
        .tool(
            "control.chat.send",
            json!({"id":parent,"message":format!("/cancel {first}")}),
        )
        .unwrap();
    assert_eq!(detail["messages"][0]["status"], "cancelled");
    let revision = detail["revision"].as_u64().unwrap();
    let steering = detail["messages"][1]["run_id"].as_str().unwrap();
    let response = hive
        .http()
        .post(format!("{server}/cancel-run"))
        .header("Origin", &server)
        .form(&[("id", steering)])
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 303);
    let detail = hive
        .tool("control.chat.read", json!({"id":parent}))
        .unwrap();
    assert_eq!(detail["messages"][1]["status"], "cancelled");
    assert!(detail["revision"].as_u64().unwrap() > revision);
    assert_eq!(detail["messages"][0]["text"], "Build a parser");
}
#[test]
#[ignore = "requires isolated server with workers disabled, built WASM assets, and local Firefox WebDriver"]
fn live_chat_enter_sends_and_shift_enter_adds_newline() {
    let server =
        std::env::var("CONTROL_QA_URL").expect("Use isolated server with workers disabled");
    let mut hive = Hive::connect(&server).unwrap();
    let driver =
        std::env::var("CONTROL_WEBDRIVER_URL").unwrap_or_else(|_| "http://127.0.0.1:18082".into());
    local_server(&driver).unwrap();
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let session: Value = client.post(format!("{driver}/session")).json(&json!({"capabilities":{"alwaysMatch":{"browserName":"firefox","moz:firefoxOptions":{"args":["-headless"],"binary":"/Applications/Firefox.app/Contents/MacOS/firefox"}}}})).send().unwrap().json().unwrap();
    let id = session["value"]["sessionId"]
        .as_str()
        .expect("Firefox session failed");
    struct Browser {
        client: Client,
        url: String,
    }
    impl Browser {
        fn request(&self, method: reqwest::Method, path: &str, args: Value) -> Value {
            let mut request = self
                .client
                .request(method.clone(), format!("{}{path}", self.url));
            if method == reqwest::Method::POST {
                request = request.json(&args);
            }
            let response = request.send().unwrap();
            let status = response.status();
            let value: Value = response.json().unwrap();
            assert!(
                status.is_success(),
                "WebDriver {method} {path}: {}",
                value["value"]["error"].as_str().unwrap_or("request failed")
            );
            value["value"].clone()
        }
        fn post(&self, path: &str, args: Value) -> Value {
            self.request(reqwest::Method::POST, path, args)
        }
        fn get(&self, path: &str) -> Value {
            self.request(reqwest::Method::GET, path, Value::Null)
        }
        fn enter(&self) {
            self.post("/actions", json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\u{e007}"},{"type":"keyUp","value":"\u{e007}"}]}]}));
        }
        fn input(&self) -> String {
            self.post("/element", json!({"using":"css selector","value":".conversation-composer textarea[data-enter-sends=true]"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().expect("Chat WASM did not initialize").to_owned()
        }
    }
    impl Drop for Browser {
        fn drop(&mut self) {
            let _ = self.client.delete(&self.url).send();
        }
    }
    let browser = Browser {
        client,
        url: format!("{driver}/session/{id}"),
    };
    browser.post("/timeouts", json!({"implicit":5000,"pageLoad":15000}));
    browser.post("/url", json!({"url":format!("{server}/health")}));
    // The existing native client consumes the operator credential internally.
    // Pass it directly to the browser cookie store; never print it or put it in a URL.
    let cookie = hive
        .operator_cookie()
        .to_str()
        .unwrap()
        .strip_prefix("control_operator=")
        .unwrap();
    browser.post("/cookie", json!({"cookie":{"name":"control_operator","value":cookie,"path":"/","httpOnly":true,"sameSite":"Strict"}}));
    browser.post("/url", json!({"url":format!("{server}/chats")}));
    let before = hive.tool("control.chats.list", json!({})).unwrap();
    let input = browser.input();
    browser.post(&format!("/element/{input}/value"), json!({"text":"first"}));
    browser.post("/actions", json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\u{e008}"},{"type":"keyDown","value":"\u{e007}"},{"type":"keyUp","value":"\u{e007}"},{"type":"keyUp","value":"\u{e008}"}]}]}));
    browser.post(&format!("/element/{input}/value"), json!({"text":"second"}));
    assert_eq!(
        browser.get(&format!("/element/{input}/property/value")),
        "first\nsecond"
    );
    assert_eq!(hive.tool("control.chats.list", json!({})).unwrap(), before);
    browser.enter();
    let start = Instant::now();
    let chat_id = loop {
        let url = browser.get("/url");
        let path = Url::parse(url.as_str().unwrap()).unwrap().path().to_owned();
        if let Some(id) = path.strip_prefix("/chat/c-") {
            break format!("c-{id}");
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Enter did not submit"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let chat = hive
        .tool("control.chat.read", json!({"id":chat_id}))
        .unwrap();
    assert_eq!(chat["messages"][0]["text"], "first\r\nsecond");
    let transcript = hive
        .http()
        .get(format!("{server}/chat/{chat_id}/transcript"))
        .send()
        .unwrap();
    assert!(
        transcript.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'none'")
    );
    let input = browser.input();
    browser.post(
        &format!("/element/{input}/value"),
        json!({"text":"followup"}),
    );
    browser.enter();
    let start = Instant::now();
    loop {
        let chat = hive
            .tool("control.chat.read", json!({"id":chat_id}))
            .unwrap();
        if chat["messages"].as_array().unwrap().len() == 2 {
            assert_eq!(chat["messages"][1]["text"], "followup");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Existing chat did not submit"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
    let input = browser.input();
    browser.post(
        &format!("/element/{input}/value"),
        json!({"text":"\u{e007}"}),
    );
    assert_eq!(browser.get(&format!("/element/{input}/property/value")), "");
    assert_eq!(
        hive.tool("control.chat.read", json!({"id":chat_id}))
            .unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // Completing a slash command changes only the draft. A second Enter sends it.
    browser.post(&format!("/element/{input}/value"), json!({"text":"/imp"}));
    let command = browser.post("/element", json!({"using":"css selector","value":"#chat-commands:not([hidden]) [data-command='/improve']"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
    assert_eq!(
        browser.get(&format!("/element/{command}/css/color")),
        "rgb(181, 160, 255)"
    );
    browser.enter();
    assert_eq!(
        browser.get(&format!("/element/{input}/property/value")),
        "/improve"
    );
    assert_eq!(
        hive.tool("control.chat.read", json!({"id":chat_id}))
            .unwrap()["mode"],
        "chat"
    );
    let highlighted = browser.post(
        "/element",
        json!({"using":"css selector","value":".composer-highlight .chat-command"}),
    )["element-6066-11e4-a52e-4f735466cecf"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        browser.get(&format!("/element/{highlighted}/css/color")),
        "rgb(181, 160, 255)"
    );
    browser.enter();
    let start = Instant::now();
    loop {
        if hive
            .tool("control.chat.read", json!({"id":chat_id}))
            .unwrap()["mode"]
            == "improve"
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Slash command did not enable improve mode"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
    let input = browser.input();
    // Tab completes an off command, and mouse completion remains available.
    browser.post(
        &format!("/element/{input}/value"),
        json!({"text":"/improve o"}),
    );
    browser.post("/actions", json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\u{e004}"},{"type":"keyUp","value":"\u{e004}"}]}]}));
    assert_eq!(
        browser.get(&format!("/element/{input}/property/value")),
        "/improve off"
    );
    browser.enter();
    let start = Instant::now();
    loop {
        if hive
            .tool("control.chat.read", json!({"id":chat_id}))
            .unwrap()["mode"]
            == "chat"
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Slash command did not exit improve mode"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
    let input = browser.input();
    browser.post(&format!("/element/{input}/value"), json!({"text":"/"}));
    let off = browser.post("/element", json!({"using":"css selector","value":"#chat-commands:not([hidden]) [data-command='/improve off']"}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
    browser.post(&format!("/element/{off}/click"), json!({}));
    assert_eq!(
        browser.get(&format!("/element/{input}/property/value")),
        "/improve off"
    );
    for (draft, command) in [
        ("/bt", "/btw"),
        ("/spa", "/spawn"),
        ("/ste", "/steer"),
        ("/ans", "/answer"),
    ] {
        browser.post("/url", json!({"url":format!("{server}/chat/{chat_id}")}));
        let input = browser.input();
        browser.post(&format!("/element/{input}/value"), json!({"text":draft}));
        let option = browser.post("/element", json!({"using":"css selector","value":format!("#chat-commands:not([hidden]) [data-command='{command}']")}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
        assert_eq!(
            browser.get(&format!("/element/{option}/css/color")),
            "rgb(181, 160, 255)"
        );
        browser.enter();
        assert_eq!(
            browser.get(&format!("/element/{input}/property/value")),
            command
        );
        let prefix = browser.post(
            "/element",
            json!({"using":"css selector","value":".composer-highlight .chat-command"}),
        )["element-6066-11e4-a52e-4f735466cecf"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            browser.get(&format!("/element/{prefix}/css/color")),
            "rgb(181, 160, 255)"
        );
    }
    if std::env::var_os("CONTROL_QA_DIFF_BROWSER").is_some() {
        let diff_chat = fs::read_to_string(crate::config::state("diff-qa-chat").unwrap()).unwrap();
        browser.post(
            "/url",
            json!({"url":format!("{server}/chat/{diff_chat}/transcript")}),
        );
        for (selector, color) in [
            (".edit-diff[open] .diff-add", "rgb(181, 160, 255)"),
            (".edit-diff[open] .diff-remove", "rgb(217, 154, 171)"),
        ] {
            let element=browser.post("/element",json!({"using":"css selector","value":selector}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
            assert_eq!(browser.get(&format!("/element/{element}/css/color")), color);
        }
        if let Ok(path) = std::env::var("CONTROL_QA_DIFF_SCREENSHOT") {
            use base64::Engine;
            let screenshot = browser.get("/screenshot");
            fs::write(
                path,
                base64::engine::general_purpose::STANDARD
                    .decode(screenshot.as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
        }
        browser.post(
            "/url",
            json!({"url":format!("{server}/?view=providers#saved")}),
        );
        for (selector, property) in [
            (".provider-saved:target", "color"),
            (
                ".model-choice[aria-pressed=true] .model-dot",
                "background-color",
            ),
        ] {
            let element=browser.post("/element",json!({"using":"css selector","value":selector}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
            assert_eq!(
                browser.get(&format!("/element/{element}/css/{property}")),
                "rgb(181, 160, 255)"
            );
        }
    }
}
#[test]
#[ignore = "requires an isolated Control server and pinned Nix development image; no model calls"]
fn live_tools_catalog_and_rust_roundtrip() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let page = hive
        .http()
        .get(format!("{server}/?view=tools"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert_eq!(page.matches("href='/tools/browser'").count(), 1);
    assert!(!page.contains("Lens tools"));
    assert!(!page.contains("<iframe"));
    let filtered = hive
        .http()
        .get(format!("{server}/?view=tools&q=lens%3Aresearch"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(filtered.contains("href='/tools/search'"));
    assert!(!filtered.contains("href='/tools/rust'"));
    let catalog = hive.rpc("tools/list", json!({})).unwrap();
    assert!(
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "control.rust.run")
    );
    let result = hive
        .tool(
            "control.rust.run",
            json!({"code":"fn main() { println!(\"tool-roundtrip-ok\"); }"}),
        )
        .unwrap();
    assert_eq!(result["success"], true);
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .contains("tool-roundtrip-ok")
    );
    let result = hive
        .http()
        .post(format!("{server}/tools/rust"))
        .timeout(Duration::from_secs(25))
        .header("Origin", &server)
        .form(&[(
            "code",
            "fn main() { println!(\"<script>escaped</script>\"); }",
        )])
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(result.contains("&lt;script&gt;escaped&lt;/script&gt;"));
    assert!(!result.contains("<script>escaped</script>"));
    let rejected = hive.http().post(format!("{server}/mcp")).json(&json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"control.rust.run","arguments":{"code":"fn main() {}"}}})).send().unwrap();
    assert_eq!(rejected.status().as_u16(), 400);
}
#[test]
#[ignore = "requires isolated Control server, authenticated worker image, and one model call"]
fn live_worker_uses_credential_free_playground_tool() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let chat = hive.tool("control.chat.create", json!({})).unwrap();
    let id = chat["id"].as_str().unwrap();
    hive.tool("control.chat.send", json!({"id":id,"message":"Use the control_rust_run MCP tool to compile and run fn main() { println!(\"worker-playground-ok\"); }. Do not use shell commands. Reply briefly with its output."})).unwrap();
    let start = Instant::now();
    loop {
        let chat = hive.tool("control.chat.read", json!({"id":id})).unwrap();
        let message = chat["messages"].as_array().unwrap().last().unwrap();
        match message["status"].as_str().unwrap() {
            "queued" | "running" if start.elapsed() < Duration::from_secs(180) => {
                std::thread::sleep(Duration::from_millis(500))
            }
            "completed" => {
                assert!(
                    message["reply"]
                        .as_str()
                        .unwrap()
                        .contains("worker-playground-ok")
                );
                assert!(
                    message["attempts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(
                            |attempt| attempt["activity"].as_array().unwrap().iter().any(
                                |a| a["kind"] == "mcp_tool_call"
                                    && a["text"]
                                        .as_str()
                                        .unwrap_or("")
                                        .contains("control_rust_run")
                            )
                        )
                );
                break;
            }
            status => panic!(
                "Worker tool check ended with {status}: {}",
                message["reply"]
            ),
        }
    }
}
#[test]
fn automatic_updates_stay_distinct_from_human_message_cards() {
    let app = App {
        detail: json!({"messages":[
            {"kind":"delegation-result","text":"Delegated task · completed"},
            {"kind":"room-notification","text":"New room posts"},
            {"kind":"design-answer","text":"Use the smaller option"}
        ]}),
        ..App::default()
    };
    let text = app.transcript();
    let labels = text
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(labels.iter().filter(|line| *line == "◇ Event").count(), 2);
    assert!(!labels.iter().any(|line|matches!(line.as_str(),"You"|"Control")));
    assert_eq!(text.lines.iter().find(|line|line.to_string()=="Use the smaller option").unwrap().style.bg,Some(history::CARD));
    assert!(text.lines.iter().filter(|line|matches!(line.to_string().as_str(),"New room posts"|"Delegated task · completed")).all(|line|line.style.bg!=Some(history::CARD)));
}
#[test]
fn file_diffs_use_purple_additions_and_pastel_red_removals_in_terminal() {
    let app = App {
        show_activity:true,
        detail: json!({"messages":[{"text":"Edit","status":"completed","reply":"Done","attempts":[{"activity":[{"kind":"file_change","status":"completed","text":"1 file · +1 −1","output":"@@ -1 +1 @@\n-old\n+new\n"},{"kind":"command_execution","text":"echo","output":"+ordinary output"}]}]}]}),
        ..App::default()
    };
    let text = app.transcript();
    let add = text
        .lines
        .iter()
        .find(|line| line.to_string() == "  +new")
        .unwrap();
    let removed = text
        .lines
        .iter()
        .find(|line| line.to_string() == "  -old")
        .unwrap();
    assert_eq!(add.style.fg, Some(Color::Rgb(181, 160, 255)));
    assert_eq!(removed.style.fg, Some(Color::Rgb(217, 154, 171)));
    let ordinary = text
        .lines
        .iter()
        .find(|line| line.to_string() == "  +ordinary output")
        .unwrap();
    assert_eq!(ordinary.style.fg, None);
}
#[test]
fn folded_activity_retains_failures_and_expansion_is_a_local_action() {
    let mut app=App {detail:json!({"messages":[{"text":"Build","reply":"Working","attempts":[{"activity":[
        {"kind":"command_execution","text":"cargo check","status":"completed","output":"private verbose success detail"},
        {"kind":"command_execution","text":"cargo test","status":"failed","output":"E42: test failed\nlong stack trace"}
    ]}]}]}),input:"/activity full".into(),..Default::default()};
    let compact=app.transcript().to_string();assert!(compact.contains("2 actions · 1 failed"));assert!(compact.contains("E42: test failed"));assert!(!compact.contains("private verbose success detail"));
    let mut hive=Hive::fixture(Client::new(),"http://127.0.0.1:1/".into());
    app.submit(&mut hive).unwrap();
    assert!(app.show_activity);assert!(app.transcript().to_string().contains("private verbose success detail"));
    app.layout_history(80);assert!(!app.history.dirty());
    app.set_detail(json!({"messages":[{"text":"New result","reply":"Verified"}]}));
    assert!(app.history.dirty());app.layout_history(80);
    assert!(app.history.window(0,30).to_string().contains("Verified"));
}
#[test]
fn activity_and_context_boundary_render_in_terminal() {
    let app = App {
        show_activity:true,
        detail: json!({"messages":[{"text":"Build", "status":"running", "reply":"", "attempts":[{"omitted_history":3,"activity":[{"status":"completed","text":"cargo check", "output":"finished\u{001b}[31m"}]}]}]}),
        ..App::default()
    };
    let text = app.transcript().to_string();
    assert!(text.contains("cargo check"));
    assert!(text.contains("finished"));
    assert!(text.contains("3 earlier exchanges excluded"));
    assert!(!text.contains('\u{001b}'));
}
#[test]
fn service_commands_and_child_stdout_render_in_terminal() {
    let app = App {
        show_activity:true,
        detail: json!({"messages":[{"text":"Run","commands":[{"text":"echo own","status":"completed","output":"own output\nExit 0"}]}],"agent_activity":[{"chat":"child-chat","activity":{"text":"npm install","status":"running","output":"downloaded\u{001b}[31m\n"}}]}),
        ..App::default()
    };
    let text = app.transcript().to_string();
    for expected in [
        "echo own",
        "own output",
        "Exit 0",
        "Agent · child-chat",
        "npm install",
        "downloaded",
    ] {
        assert!(text.contains(expected), "Missing {expected}: {text}");
    }
    assert!(!text.contains('\u{001b}'));
}
#[test]
fn visible_messages_render_as_chat_and_do_not_duplicate_the_final_reply() {
    let app = App {
        detail: json!({"messages":[{"text":"Check","reply":"Done","attempts":[{"activity":[{"kind":"message","text":"Checking inventory."},{"kind":"message","text":"Done"}]}]}],"agent_activity":[{"chat":"child","activity":{"kind":"message","text":"Building the shelter."}}]}),
        ..App::default()
    };
    let text = app.transcript().to_string();
    assert!(text.contains("Checking inventory."));
    assert!(text.contains("Building the shelter."));
    assert_eq!(text.matches("Done").count(), 1);
}
#[test]
#[ignore = "requires an isolated Control server and pinned browser image; no model calls"]
fn live_browser_ui_capture_and_protected_artifact() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let hive = Hive::connect(&server).unwrap();
    let page = hive
        .http()
        .get(format!("{server}/tools/browser"))
        .send()
        .unwrap();
    assert_eq!(page.status().as_u16(), 200);
    assert_eq!(page.headers()["referrer-policy"], "same-origin");
    assert!(page.text().unwrap().contains("/tools/browser/capture"));
    let outside = hive
        .http()
        .post(format!("{server}/tools/browser/capture"))
        .form(&[
            ("input", "../outside.html"),
            ("width", "390"),
            ("height", "844"),
        ])
        .send()
        .unwrap();
    assert_eq!(outside.status().as_u16(), 400);
    let capture = hive
        .http()
        .post(format!("{server}/tools/browser/capture"))
        .header("Origin", &server)
        .form(&[
            ("input", "preview.html"),
            ("width", "390"),
            ("height", "844"),
        ])
        .send()
        .unwrap();
    assert_eq!(capture.status().as_u16(), 303);
    let location = capture.headers()["location"].to_str().unwrap();
    let page = hive
        .http()
        .get(format!("{server}{location}"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    let name = location.split_once("capture=").unwrap().1;
    let url = format!("{server}/tools/browser/artifacts/{name}");
    assert!(page.contains(&format!("src='/tools/browser/artifacts/{name}'")));
    let image = hive.http().get(&url).send().unwrap();
    assert_eq!(image.headers()["content-type"], "image/png");
    assert!(image.bytes().unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
    let unauthenticated = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(url)
        .send()
        .unwrap();
    assert_ne!(unauthenticated.status().as_u16(), 200);
}
#[test]
fn terminal_control_injection_is_rejected_and_lenses_remain_readable() {
    assert!(!clean("hi\x1b[2J\x07").contains('\x1b'));
    assert_eq!(
        lens_label(&json!({"lenses":["engineering","evidence"],"lens":null})),
        "engineering + evidence"
    );
    assert_eq!(lens_label(&json!({"lenses":[]})), "No lenses");
    assert_eq!(lens_label(&json!({"lens":"research"})), "research");
}
#[test]
#[ignore = "requires an isolated local Control server; no model calls"]
fn live_tui_lens_commands() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let mut app = App {
        input: "/use missing-lens".into(),
        ..App::default()
    };
    assert!(app.submit(&mut hive).is_err());
    assert!(app.active.is_empty());
    app.input = "/use engineering evidence".into();
    app.submit(&mut hive).unwrap();
    assert_eq!(app.detail["lenses"], json!(["engineering", "evidence"]));
    app.input = "/lenses".into();
    app.submit(&mut hive).unwrap();
    assert!(
        app.inspector
            .as_ref()
            .unwrap()
            .1
            .contains("repository-read")
    );
    let menu = app.menu.as_mut().unwrap();
    menu.selected = menu
        .entries
        .iter()
        .position(|(_, _, id)| id == "research")
        .unwrap();
    menu.toggle();
    app.input = menu.command().unwrap();
    app.submit(&mut hive).unwrap();
    assert!(
        app.detail["lenses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "research")
    );
    app.input = "/context".into();
    app.submit(&mut hive).unwrap();
    assert!(app.inspector.as_ref().unwrap().1.contains("repo.read"));
    app.input = "/chat".into();
    app.submit(&mut hive).unwrap();
    assert!(app.inspector.is_none());
    app.input = "/use none".into();
    app.submit(&mut hive).unwrap();
    assert_eq!(app.detail["lenses"], json!([]));
    assert!(app.detail["messages"].as_array().unwrap().is_empty());
}
#[test]
#[ignore = "requires an isolated local Control server; no model calls"]
fn live_declarative_lens_library() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let lens_id = format!("fixture-{stamp:x}");
    let note = hive
        .http()
        .post(format!("{server}/knowledge/save"))
        .form(&[
            ("id", ""),
            ("revision", "0"),
            ("title", &format!("Lens fixture {stamp}")),
            ("body", "A pinned knowledge fixture."),
        ])
        .send()
        .unwrap();
    assert_eq!(note.status().as_u16(), 303);
    let url = Url::parse(&format!(
        "{server}{}",
        note.headers()["location"].to_str().unwrap()
    ))
    .unwrap();
    let doc_id = url
        .query_pairs()
        .find(|(k, _)| k == "doc")
        .unwrap()
        .1
        .into_owned();
    let mut fields = vec![
        ("id".to_string(), lens_id.clone()),
        ("name".into(), "Fixture composition".into()),
        ("revision".into(), "0".into()),
        ("include_engineering".into(), "on".into()),
        ("include_research".into(), "on".into()),
        (format!("document_{doc_id}"), "on".into()),
        ("instructions".into(), "Fixture instructions.".into()),
    ];
    let saved = hive
        .http()
        .post(format!("{server}/lenses/save"))
        .form(&fields)
        .send()
        .unwrap();
    assert_eq!(saved.status().as_u16(), 303);
    let resolved = hive
        .tool("control.lenses.resolve", json!({"lenses":[lens_id]}))
        .unwrap();
    assert_eq!(resolved["tools"].as_array().unwrap().len(), 5);
    assert_eq!(
        resolved["documents"][0]["body"],
        "A pinned knowledge fixture."
    );
    assert!(
        resolved["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "Fixture instructions.")
    );
    let chat = hive
        .tool("control.chat.create", json!({"lenses":[lens_id]}))
        .unwrap();
    assert_eq!(chat["lenses"], json!([lens_id]));
    let page = hive
        .http()
        .get(format!("{server}/chat/{}", chat["id"].as_str().unwrap()))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(page.contains(&format!("name='lens_{lens_id}'")));
    assert!(page.contains("conversation-composer"));
    assert!(!page.contains("chat-lenses"));
    fields.iter_mut().find(|(k, _)| k == "revision").unwrap().1 = "1".into();
    let saved = hive
        .http()
        .post(format!("{server}/lenses/save"))
        .form(&fields)
        .send()
        .unwrap();
    assert_eq!(saved.status().as_u16(), 303);
    let stale = hive
        .http()
        .post(format!("{server}/lenses/save"))
        .form(&fields)
        .send()
        .unwrap();
    assert_eq!(stale.status().as_u16(), 409);
    let exported: Value = hive
        .http()
        .get(format!("{server}/lenses/export"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        exported["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["id"] == lens_id)
            .count(),
        2
    );
}
#[test]
#[ignore = "requires an isolated local Control server; no model calls"]
fn live_chat_composition_and_design_entry() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let before = hive.tool("control.chats.list", json!({})).unwrap();
    let response = hive
        .http()
        .get(format!("{server}/chats?discuss=knowledge"))
        .send()
        .unwrap();
    assert_eq!(response.headers()["referrer-policy"], "same-origin");
    let page = response.text().unwrap();
    assert!(page.contains("design of Knowledge"));
    assert!(!page.contains("right-panel"));
    assert_eq!(hive.tool("control.chats.list", json!({})).unwrap(), before);
    for origin in ["null", "https://foreign.example"] {
        let rejected = hive
            .http()
            .post(format!("{server}/chats"))
            .header("origin", origin)
            .form(&[("composition", "1")])
            .send()
            .unwrap();
        assert_eq!(rejected.status().as_u16(), 403);
    }
    assert_eq!(hive.tool("control.chats.list", json!({})).unwrap(), before);
    let result = hive
        .http()
        .post(format!("{server}/chats"))
        .header("origin", &server)
        .form(&[
            ("composition", "1"),
            ("lens_engineering", "engineering"),
            ("lens_research", "research"),
            ("design_context", "knowledge"),
        ])
        .send()
        .unwrap();
    assert_eq!(result.status().as_u16(), 303);
    let path = result.headers()["location"].to_str().unwrap();
    let id = path.strip_prefix("/chat/").unwrap();
    let page = hive.http().get(format!("{server}{path}")).send().unwrap();
    assert_eq!(page.headers()["referrer-policy"], "same-origin");
    let invalid = hive
        .http()
        .post(format!("{server}/chat/{id}/messages"))
        .header("origin", &server)
        .form(&[("message", "")])
        .send()
        .unwrap();
    assert_eq!(invalid.status().as_u16(), 400); // Reaches validation, not the origin rejection.
    let detail = hive.tool("control.chat.read", json!({"id":id})).unwrap();
    assert_eq!(detail["lenses"], json!(["engineering", "research"]));
    assert_eq!(detail["design_context"], "knowledge");
    assert!(detail["messages"].as_array().unwrap().is_empty());
    let result = hive
        .http()
        .post(format!("{server}/chat/{id}/lenses"))
        .header("origin", &server)
        .form(&[("composition", "1")])
        .send()
        .unwrap();
    assert_eq!(result.status().as_u16(), 303);
    assert_eq!(
        hive.tool("control.chat.read", json!({"id":id})).unwrap()["lenses"],
        json!([])
    );
    let page = hive
        .http()
        .get(format!("{server}/knowledge"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(!page.contains("right-panel"));
    assert!(page.contains("/chats?discuss=knowledge"));
}
#[test]
#[ignore = "requires an isolated local Control server; no model calls"]
fn live_knowledge_save_and_graph() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let hive = Hive::connect(&server).unwrap();
    if std::env::var_os("CONTROL_QA_VERIFY_RELOAD").is_some() {
        let page = hive
            .http()
            .get(format!("{server}/knowledge?mode=graph"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert!(page.contains("Durability A.md"));
        assert!(page.contains("Durability B.md"));
        assert!(page.contains("<line"));
        return;
    }
    let create = |title: &str, body: &str| {
        let result = hive
            .http()
            .post(format!("{server}/knowledge/save"))
            .form(&[
                ("id", ""),
                ("title", title),
                ("body", body),
                ("revision", "0"),
            ])
            .send()
            .unwrap();
        assert_eq!(result.status().as_u16(), 303);
        result.headers()["location"].to_str().unwrap().to_owned()
    };
    let location = create("Durability A", "[[Durability B]]\nA saved note.");
    let url = Url::parse(&format!("{server}{location}")).unwrap();
    let id = url
        .query_pairs()
        .find(|(key, _)| key == "doc")
        .unwrap()
        .1
        .into_owned();
    let page = hive.http().get(url).send().unwrap().text().unwrap();
    assert!(page.contains("✓ Saved Durability A.md · revision 1"));
    assert!(page.contains("A saved note."));
    create("Durability B", "Backlink: [[Durability A]]");
    let stale = hive
        .http()
        .post(format!("{server}/knowledge/save"))
        .form(&[
            ("id", id.as_str()),
            ("title", "Durability A"),
            ("body", "Retain this stale edit"),
            ("revision", "0"),
        ])
        .send()
        .unwrap();
    assert_eq!(stale.status().as_u16(), 409);
    assert!(stale.text().unwrap().contains("Retain this stale edit"));
    let graph = hive
        .http()
        .get(format!("{server}/knowledge?mode=graph"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(graph.contains("<line"));
    let note = hive
        .http()
        .get(format!("{server}/knowledge?doc={id}"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(note.contains("A saved note."));
    assert!(!note.contains("Retain this stale edit"));
}
#[test]
#[ignore = "requires an isolated Control server and authenticated Nix worker image with room MCP"]
fn live_room_tools_connect_worker_chat_and_ui() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let chat = hive
        .tool(
            "control.chat.create",
            json!({"room":"research","lens":"research"}),
        )
        .unwrap();
    let id = chat["id"].as_str().unwrap();
    let thread = chat["room"].as_str().unwrap();
    hive.tool(
        "control.room.post",
        json!({"room":thread,"message":"The shared test marker is ROOM_CEDAR."}),
    )
    .unwrap();
    hive.tool("control.chat.send",json!({"id":id,"message":"Use the control_rooms MCP tools: read this room, then post exactly the shared test marker as a concise update. Do not use shell files to post it. Reply briefly."})).unwrap();
    let started = Instant::now();
    loop {
        let chat = hive.tool("control.chat.read", json!({"id":id})).unwrap();
        let last = chat["messages"].as_array().unwrap().last().unwrap();
        match last["status"].as_str().unwrap() {
            "completed" => break,
            "queued" | "running" if started.elapsed() < Duration::from_secs(150) => {
                std::thread::sleep(Duration::from_millis(500))
            }
            _ => panic!("Room worker did not complete; no private diagnostics displayed"),
        }
    }
    let room = hive
        .tool("control.room.read", json!({"room":thread}))
        .unwrap();
    assert!(
        room["posts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["chat"] == id && p["text"] == "ROOM_CEDAR" && p["author"] == "agent")
    );
    let other = hive
        .tool("control.room.read", json!({"room":"engineering"}))
        .unwrap();
    assert!(!other.to_string().contains("ROOM_CEDAR"));
    let room_page = format!("/?view=rooms&channel={thread}");
    for (path, expected) in [
        (room_page.as_str(), "ROOM_CEDAR"),
        ("/?view=lenses", "Context and tool"),
        ("/?view=editor", "Scratchpad"),
    ] {
        let page = hive
            .http()
            .get(format!("{server}{path}"))
            .send()
            .unwrap()
            .text()
            .unwrap();
        if path.contains("lenses") {
            assert!(page.contains("Compose lenses"));
        } else {
            assert!(page.contains(expected));
        }
        assert!(!page.contains("<select id=\"new-chat-lens\""));
    }
}

#[test]
#[ignore = "requires an isolated Control server; no model calls or containers"]
fn live_task_threads_isolate_operator_posts_and_ui() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let first = hive
        .tool("control.chat.create", json!({"room":"general"}))
        .unwrap();
    let second = hive
        .tool("control.chat.create", json!({"room":"general"}))
        .unwrap();
    let first_room = first["room"].as_str().unwrap();
    let second_room = second["room"].as_str().unwrap();
    assert_ne!(first_room, second_room);
    hive.tool(
        "control.room.post",
        json!({"room":first_room,"message":"FIRST_THREAD_ONLY"}),
    )
    .unwrap();
    hive.tool(
        "control.room.post",
        json!({"room":second_room,"message":"SECOND_THREAD_ONLY"}),
    )
    .unwrap();
    let first_posts = hive
        .tool("control.room.read", json!({"room":first_room}))
        .unwrap();
    assert!(first_posts.to_string().contains("FIRST_THREAD_ONLY"));
    assert!(!first_posts.to_string().contains("SECOND_THREAD_ONLY"));
    let page = hive
        .http()
        .get(format!("{server}/?view=rooms&channel={first_room}"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(page.contains("FIRST_THREAD_ONLY") && !page.contains("SECOND_THREAD_ONLY"));
    assert!(page.contains(&format!("name='channel' value='{first_room}'")));
    assert!(page.contains(&format!("name='room' value='{first_room}'")));
    let response = hive
        .http()
        .post(format!("{server}/room"))
        .header("Origin", &server)
        .form(&[("channel", first_room), ("message", "OPERATOR_REPLY")])
        .send()
        .unwrap();
    assert_eq!(response.status().as_u16(), 303);
    assert_eq!(
        response
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("/?view=rooms&channel={first_room}")
    );
    let posts = hive
        .tool("control.room.read", json!({"room":first_room}))
        .unwrap();
    assert!(posts.to_string().contains("OPERATOR_REPLY"));
    assert!(
        !hive
            .tool("control.room.read", json!({"room":second_room}))
            .unwrap()
            .to_string()
            .contains("OPERATOR_REPLY")
    );
}
#[test]
#[ignore = "requires an isolated local Control server and authenticated container workers"]
fn live_shared_chat_keeps_context_and_web_transcript() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server URL");
    let mut hive = Hive::connect(&server).unwrap();
    let value = hive.tool("control.chat.create", json!({})).unwrap();
    let id = value["id"].as_str().unwrap().to_string();
    for request in [
        "Remember the codeword RIVER_CEDAR. Reply with the codeword.",
        "What codeword did I give you in the previous message? Reply with that codeword.",
    ] {
        hive.tool("control.chat.send", json!({"id":id,"message":request}))
            .unwrap();
        let started = Instant::now();
        loop {
            let data = hive.tool("control.chat.read", json!({"id":id})).unwrap();
            let last = data["messages"].as_array().unwrap().last().unwrap();
            match last["status"].as_str().unwrap() {
                "completed" => {
                    assert!(last["reply"].as_str().unwrap().contains("RIVER_CEDAR"));
                    break;
                }
                "queued" | "running" if started.elapsed() < Duration::from_secs(120) => {
                    std::thread::sleep(Duration::from_millis(500))
                }
                _ => panic!(
                    "Isolated session did not complete successfully; no raw diagnostics displayed."
                ),
            }
        }
    }
    let url = format!("{}/chat/{id}", server.trim_end_matches('/'));
    let page = hive.http().get(&url).send().unwrap();
    assert!(page.status().is_success());
    assert!(page.text().unwrap().contains("chat-transcript"));
    let page = hive
        .http()
        .get(format!("{url}/transcript"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(page.contains("RIVER_CEDAR"));
    assert!(!page.contains("http-equiv='refresh'"));
}

#[test]
#[ignore = "requires an isolated Control server, authenticated Nix worker image and two small model calls; reload mode uses one call"]
fn live_artifacts_preserve_project_files_and_viewer_after_restart() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let mut hive = Hive::connect(&server).unwrap();
    let reload = std::env::var_os("CONTROL_QA_VERIFY_RELOAD").is_some();
    let improve = std::env::var_os("CONTROL_QA_IMPROVE_MODE").is_some();
    let chat = if reload {
        hive.tool("control.chats.list", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        hive.tool("control.chat.create", json!({})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    if improve {
        if !reload {
            hive.tool("control.chat.send", json!({"id":chat,"message":"/improve"}))
                .unwrap();
        }
        assert_eq!(
            hive.tool("control.chat.read", json!({"id":chat})).unwrap()["mode"],
            "improve"
        );
    }
    let messages = if reload {
        vec![
            "Read counter.txt in the existing workspace, increment its integer by one and write it back. Preserve preview.txt. Actually use a shell command to read the file. Reply briefly.",
        ]
    } else {
        vec![
            "Use shell commands to create counter.txt containing 1 and preview.txt containing exactly <script>alert(1)</script>. Do not inspect credentials. Reply briefly.",
            "Read counter.txt in the existing workspace, increment its integer by one and write it back. Preserve preview.txt. Actually use a shell command to read the file. Reply briefly.",
        ]
    };
    for (index, message) in messages.iter().enumerate() {
        hive.tool("control.chat.send", json!({"id":chat,"message":message}))
            .unwrap();
        let started = Instant::now();
        loop {
            let detail = hive.tool("control.chat.read", json!({"id":chat})).unwrap();
            let current = detail["messages"].as_array().unwrap().last().unwrap();
            match current["status"].as_str().unwrap() {
                "queued" | "running" if started.elapsed() < Duration::from_secs(180) => {
                    std::thread::sleep(Duration::from_millis(500))
                }
                "completed" => break,
                status => panic!("Artifact test ended with {status}: {}", current["reply"]),
            }
        }
        let listed = hive
            .tool("control.artifacts.list", json!({"id":chat}))
            .unwrap();
        let artifact = listed["artifacts"][0]["id"].as_str().unwrap();
        if improve {
            let source = hive
                .tool(
                    "control.artifact.read",
                    json!({"id":chat,"artifact":artifact,"path":"src/main.rs"}),
                )
                .unwrap();
            assert!(source["text"].as_str().unwrap().contains("fixture"));
        }
        let read = hive
            .tool(
                "control.artifact.read",
                json!({"id":chat,"artifact":artifact,"path":"counter.txt"}),
            )
            .unwrap();
        let expected = if reload { 3 } else { index + 1 };
        assert_eq!(read["text"].as_str().unwrap().trim(), expected.to_string());
        let unsafe_path = hive.tool(
            "control.artifact.read",
            json!({"id":chat,"artifact":artifact,"path":"../operator-session"}),
        );
        assert!(unsafe_path.is_err());
        let other = hive.tool(
            "control.artifact.read",
            json!({"id":chat,"artifact":"unknown-output","path":"counter.txt"}),
        );
        assert!(other.is_err());
        let response = hive
            .http()
            .get(format!(
                "{server}/chat/{chat}/artifacts?artifact={artifact}&file=preview.txt"
            ))
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("script-src 'none'")
        );
        let page = response.text().unwrap();
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!page.contains("<script>alert(1)</script>"));
        let unauthenticated =
            reqwest::blocking::get(format!("{server}/chat/{chat}/artifacts")).unwrap();
        assert_eq!(unauthenticated.status().as_u16(), 401);
    }
}

#[test]
#[ignore = "requires an isolated Control server; modifies only its model configuration, no model calls"]
fn live_model_library_selection_and_reload() {
    let server = std::env::var("CONTROL_QA_URL").expect("Use an isolated server");
    let hive = Hive::connect(&server).unwrap();
    let page = || {
        hive.http()
            .get(format!("{server}/?view=providers"))
            .send()
            .unwrap()
            .text()
            .unwrap()
    };
    if std::env::var_os("CONTROL_QA_VERIFY_RELOAD").is_none() {
        for (provider, model) in [("codex", "fixture-codex"), ("claude", "fixture-claude")] {
            let response = hive
                .http()
                .post(format!("{server}/providers"))
                .header("Origin", &server)
                .form(&[("action", "add"), ("provider", provider), ("model", model)])
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 303);
        }
        for (provider, model) in [("claude", "fixture-claude"), ("codex", "fixture-codex")] {
            let response = hive
                .http()
                .post(format!("{server}/providers"))
                .header("Origin", &server)
                .form(&[
                    ("action", "select"),
                    ("provider", provider),
                    ("model", model),
                ])
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 303);
            let html = page();
            assert!(html.contains("fixture-codex"));
            assert!(html.contains("fixture-claude"));
            assert_eq!(html.matches("aria-pressed='true'").count(), 1);
        }
    }
    let html = page();
    assert!(html.contains("value='fixture-codex'") && html.contains("value='fixture-claude'"));
    assert!(
        html.contains("value='fixture-codex'><button class='model-choice' aria-pressed='true'")
    );
    assert!(!html.contains("<select"));
    assert!(!html.contains("provider-new"));
}
