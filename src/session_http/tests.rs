use crate::runtime::RunKind;
use crate::runtime::RunStatus;
/// Control's own work around a conversation, without model calls: send,
/// record the run's reply, save durably, then load the transcript and page,
/// for many 15-turn conversations in one growing state. Profile with perf.
#[test]
#[ignore = "profiling workload; set CONTROL_STATE_DIR to a scratch directory and optionally CONTROL_PROFILE_CONVERSATIONS"]
fn fifteen_turn_conversations_workload() {
    #[derive(Default)]
    struct Stage {
        wall_ns: u128,
        cpu_ns: u128,
        calls: u64,
    }
    fn cpu_ns() -> u128 {
        let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as u128 * 1_000_000_000 + time.tv_nsec as u128
    }
    fn measure<T>(stage: &mut Stage, action: impl FnOnce() -> T) -> T {
        let wall = std::time::Instant::now();
        let cpu = cpu_ns();
        let result = action();
        stage.cpu_ns += cpu_ns() - cpu;
        stage.wall_ns += wall.elapsed().as_nanos();
        stage.calls += 1;
        result
    }
    let mut stages: [Stage; 5] = std::array::from_fn(|_| Stage::default());
    println!(
        "{}",
        serde_json::json!({"layout":{"session_bytes":std::mem::size_of::<crate::chat::Session>(),"run_bytes":std::mem::size_of::<crate::runtime::Run>(),"attempt_bytes":std::mem::size_of::<crate::usage::Attempt>()}})
    );
    assert!(
        std::env::var_os("CONTROL_STATE_DIR").is_some(),
        "saves write to CONTROL_STATE_DIR"
    );
    let conversations = std::env::var("CONTROL_PROFILE_CONVERSATIONS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(20);
    let reply = "## Result\n\nThe server is **ready** and both bots joined.\n\n- `server.jar` verified\n- ViaProxy listening on 25566\n- Alice and Cass in the player list\n\n```sh\njava -jar server.jar nogui\n```\n\n".repeat(12);
    let mut state = crate::State::default();
    let start = std::time::Instant::now();
    for _ in 0..conversations {
        let id = crate::chat::create(&mut state);
        for turn in 0..15 {
            let mut out = Vec::new();
            let body = format!("message=Turn+{turn}%3A+keep+building+the+house+and+report+progress");
            assert!(measure(&mut stages[0], || super::handle(
                &mut out,
                &mut state,
                "POST",
                &format!("/dm/{id}/messages"),
                "engineering",
                "Accept: application/json\r\n",
                body.as_bytes()
            )));
            let run = state
                .runtime
                .runs
                .iter_mut()
                .rev()
                .find(|run| run.chat_id.as_deref() == Some(id.as_str()))
                .unwrap();
            run.status = RunStatus::Completed;
            *run.result = reply.clone();
            let mut attempt = crate::usage::Attempt::new();
            attempt.activity = (0..20)
                .map(|i| crate::usage::Activity {
                    id: format!("tool-{i}"),
                    kind: "command_execution".into(),
                    text: format!("cargo test --workspace -p crate-{i}"),
                    output: "test result: ok. 412 passed; 0 failed\n".repeat(8),
                    status: "completed".into(),
                })
                .collect();
            run.attempts.push(attempt);
            // As commit does when a run finishes.
            measure(&mut stages[1], || crate::history::archive_run(run)).unwrap();
            measure(&mut stages[2], || crate::storage::save(&mut state)).unwrap();
            for (stage, page) in ["transcript", ""].into_iter().enumerate() {
                out.clear();
                let target = if page.is_empty() {
                    format!("/dm/{id}")
                } else {
                    format!("/dm/{id}/{page}")
                };
                assert!(measure(&mut stages[stage + 3], || super::handle(
                    &mut out,
                    &mut state,
                    "GET",
                    &target,
                    "engineering",
                    "",
                    b""
                )));
            }
        }
    }
    let turns = conversations * 15;
    for (name, stage) in ["send_and_save", "archive", "save", "transcript", "page"]
        .into_iter()
        .zip(stages)
    {
        println!(
            "{}",
            serde_json::json!({"stage":name,"calls":stage.calls,"mean_wall_ns":stage.wall_ns/u128::from(stage.calls),"mean_cpu_ns":stage.cpu_ns/u128::from(stage.calls)})
        );
    }
    println!(
        "{turns} turns in {:?} = {:?}/turn; saved state {} KiB",
        start.elapsed(),
        start.elapsed() / turns as u32,
        crate::history::persisting(|| serde_json::to_vec(&state)).unwrap().len() / 1024
    );
}
#[test]
fn memory_tree_has_one_address_and_the_old_ones_are_renamed() {
    let mut state = crate::State::default();
    let id = crate::chat::create(&mut state);
    let get = |state: &mut crate::State, path: &str| {
        let mut response = Vec::new();
        handle(&mut response, state, "GET", path, "none", "", &[]);
        String::from_utf8(response).unwrap()
    };
    for old in [format!("/chat/{id}/compaction"), format!("/dm/{id}/compaction")] {
        assert_eq!(
            crate::url_names::renamed(&old),
            Some(crate::compaction::memory_url(&id))
        );
    }
    assert_eq!(crate::compaction::memory_url(&id), format!("/dm/{id}/memory"));
    let page = get(&mut state, &crate::compaction::memory_url(&id));
    assert!(page.starts_with("HTTP/1.1 200 OK"));
    assert!(page.contains("<h2>Memory Tree</h2>") && page.contains("name='memory-detail'"));
    assert!(!page.to_lowercase().contains("compaction tree"));
    assert!(get(&mut state, &crate::url_names::dm(&id)).contains("Memory Tree ↗"));
}
#[test]
fn returning_to_chat_remembers_only_an_existing_authorized_conversation() {
    let mut state = crate::State::default();
    let id = crate::chat::create(&mut state);
    let mut response = Vec::new();
    handle(&mut response, &mut state, "GET", &format!("/dm/{id}"), "none", "", &[]);
    assert!(
        String::from_utf8(response)
            .unwrap()
            .to_ascii_lowercase()
            .contains(&format!(
                "set-cookie: control_recent_chat={id}; path=/; httponly; samesite=strict"
            ))
    );
    let cookie = format!("Cookie: control_recent_chat={id}\r\n");
    let mut response = Vec::new();
    handle(&mut response, &mut state, "GET", "/dms", "none", &cookie, &[]);
    assert!(
        String::from_utf8(response)
            .unwrap()
            .to_ascii_lowercase()
            .contains(&format!("location: /dm/{id}"))
    );
    for path in ["/dms?new=1", "/dms?discuss=tools", "/dms?improve=1"] {
        let mut response = Vec::new();
        handle(&mut response, &mut state, "GET", path, "none", &cookie, &[]);
        assert!(String::from_utf8(response).unwrap().starts_with("HTTP/1.1 200 OK"));
    }
    let mut response = Vec::new();
    handle(&mut response, &mut state, "GET", "/dms?new=1", "none", &cookie, &[]);
    let page = String::from_utf8(response).unwrap();
    assert!(
        page.contains("name='room'")
            && crate::rooms::NAMES
                .iter()
                .all(|room| page.contains(&format!("value='{room}'"))),
        "new chats can choose a room"
    );
    state.chats[0].actor = crate::identity::Actor::Unknown;
    for header in [
        &cookie,
        "Cookie: control_recent_chat=c-missing\r\n",
        "Cookie: control_recent_chat=../../tools\r\n",
    ] {
        let mut response = Vec::new();
        handle(&mut response, &mut state, "GET", "/dms", "none", header, &[]);
        assert!(
            !String::from_utf8(response)
                .unwrap()
                .to_ascii_lowercase()
                .contains("location:")
        );
    }
}
#[test]
fn conversations_panel_is_rendered_from_the_remembered_cookie() {
    let mut state = State::default();
    let id = chat::create(&mut state);
    for target in [format!("/dm/{id}"), "/dms?new=1".into()] {
        for (cookie, open) in [
            ("", false),
            ("Cookie: control_conversations=closed\r\n", false),
            ("Cookie: control_conversations=open'><script>\r\n", false),
            ("Cookie: lens=x; control_conversations=open\r\n", true),
        ] {
            let mut response = Vec::new();
            handle(&mut response, &mut state, "GET", &target, "none", cookie, &[]);
            let page = String::from_utf8(response).unwrap();
            assert_eq!(
                page.matches("<details class='conversation-toggle").count(),
                1,
                "{target}"
            );
            assert_eq!(
                page.contains("<details class='conversation-toggle' open>"),
                open,
                "{target} {cookie}"
            );
            assert!(!page.contains("<script>"));
        }
    }
}
#[test]
fn web_folder_picker_is_opt_in_and_bare_open_only_navigates() {
    let mut state = State::default();
    let id = chat::create(&mut state);
    assert!(
        !chat::transcript(&state, &id)
            .unwrap()
            .render()
            .contains("project-picker")
    );
    chat::send(&mut state, &id, "Discuss the design", "test").unwrap();
    assert!(
        !chat::transcript(&state, &id)
            .unwrap()
            .render()
            .contains("project-picker")
    );
    let before = serde_json::to_vec(&state).unwrap();
    let mut response = Vec::new();
    assert!(handle(
        &mut response,
        &mut state,
        "POST",
        &format!("/dm/{id}/messages"),
        "",
        "engineering",
        b"message=%2Fopen"
    ));
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 303 See Other\r\n"));
    assert!(
        response
            .lines()
            .filter_map(|line| line.split_once(':'))
            .any(|(name, value)| name.eq_ignore_ascii_case("location") && value.trim() == format!("/dm/{id}/project"))
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    // An explicit path produces an approval in the separate request panel.
    // The operator's path is checked now; a missing folder fails at once.
    assert!(
        chat::send(&mut state, &id, "/open /fixture/missing-source", "test")
            .unwrap_err()
            .contains("No readable folder")
    );
    assert!(state.chats[0].project.is_none());
    let source = std::env::temp_dir();
    chat::send(&mut state, &id, &format!("/open {}", source.display()), "test").unwrap();
    let transcript = chat::requests(&state, &id).unwrap();
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
    browser_fixture(false);
}
#[test]
#[ignore = "requires built WASM and a local Chrome or Firefox WebDriver; isolated in-memory fixture, no credentials or models"]
fn live_long_messages_validate_and_deliver_without_truncation() {
    browser_fixture(true);
}
fn browser_fixture(long_messages_only: bool) {
    use std::io::{BufRead, Read, Write};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (mut initial, chat) = crate::design::tests::paused();
    let question = initial.runtime.runs[0].question.take().unwrap();
    initial.runtime.runs[0].status = RunStatus::Running;
    initial.runtime.runs[0].result.clear();
    crate::projects::request(
        &mut initial,
        &chat,
        "/fixture/project",
        crate::projects::Requester::Agent { run: None },
    )
    .unwrap();
    let long_chat = crate::chat::create(&mut initial);
    let (ports, _, port_chat) = crate::ports::tests::pending_fixture();
    initial.services.extend(ports.services);
    initial.ports.extend(ports.ports);
    initial.runtime.runs.extend(ports.runtime.into_inner().runs.into_vec());
    initial.chats.extend(ports.chats);
    // Retained asynchronous service output uses a different path from
    // provider Activity events. Exercise both through the real WASM poller.
    let mut service = serde_json::to_value(&initial.services[0]).unwrap();
    service["executions"] = json!([{"id":"browser-command","digest":"fixture","run":initial.runtime.runs[0].id(),"attempt":"fixture","command":"printf service-output","display_command":"printf service-output","timeout_seconds":120,"status":"running","output":"Service streaming alpha","truncated":false,"exit_code":null,"diagnostic":""}]);
    initial.services[0] = serde_json::from_value(service).unwrap();
    // In-memory mutations have no storage checkpoint. Publish the same hint
    // when the fixture's write guard drops; HTTP reads use the inner mutex.
    struct FixtureState(crate::store::Store);
    struct FixtureWrite<'a>(crate::store::Guard<'a>, bool);
    impl std::ops::Deref for FixtureWrite<'_> {
        type Target = crate::State;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl std::ops::DerefMut for FixtureWrite<'_> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.1 = true;
            &mut self.0
        }
    }
    impl Drop for FixtureWrite<'_> {
        fn drop(&mut self) {
            if self.1 {
                crate::live_updates::notify();
            }
        }
    }
    impl FixtureState {
        fn lock(&self) -> Result<FixtureWrite<'_>, ()> {
            self.0.guard().map(|guard| FixtureWrite(guard, false)).map_err(|_| ())
        }
    }
    let state = FixtureState(crate::store::Store::new(initial));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let rejected = Arc::new(AtomicBool::new(false));
    let reject = rejected.clone();
    let port_posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let posted_ports = port_posts.clone();
    let reject_port = Arc::new(AtomicBool::new(false));
    let rejected_port = reject_port.clone();
    let served = state.0.clone();
    let event_cancel = control_effect::sync::CancelToken::new();
    let events = event_cancel.clone();
    let stopped = stop.clone();
    let fail = failed.clone();
    let thread = std::thread::spawn(move || {
        while !stopped.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let method = line.split_whitespace().next().unwrap_or("GET").to_owned();
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
                if let Some((key, value)) = header.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
                headers.push_str(&header);
            }
            assert!(length < 64 * 1024);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            if path.starts_with("/tool-window/") {
                crate::web_assets::serve(&mut stream, &path);
            } else if path == "/events" {
                let token = events.clone();
                std::thread::spawn(move || {
                    let _ = crate::live_updates::fixture_stream(&mut stream, &token);
                });
            } else if crate::serve_stylesheet(&mut stream, &path) {
            } else if path == "/tools/ports" {
                posted_ports.fetch_add(1, Ordering::Relaxed);
                let fail_save = rejected_port.swap(false, Ordering::Relaxed);
                let served = served.clone();
                std::thread::spawn(move || {
                    // Leave transcript polling live while operator validation
                    // is slow, including one recoverable persistence error.
                    std::thread::sleep(Duration::from_millis(1800));
                    crate::ports::http_with_save(&mut stream, &served, &method, &path, &body, |_| {
                        if fail_save {
                            Err(std::io::Error::other("fixture port save failed"))
                        } else {
                            Ok(())
                        }
                    });
                });
            } else if path.contains("/transcript") && fail.load(Ordering::Relaxed) {
                let _ = stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            } else if method == "POST" && path.ends_with("/messages") {
                let id = path.trim_start_matches("/dm/").trim_end_matches("/messages");
                let fields = crate::fields(&String::from_utf8_lossy(&body));
                let mut state = served.guard().unwrap();
                send_message(&mut stream, &mut state, id, "engineering", &headers, &fields, |_| {
                    if reject.load(Ordering::Relaxed) {
                        Err(std::io::Error::other("fixture disk full"))
                    } else {
                        Ok(())
                    }
                });
                for run in state.runtime.runs.iter_mut() {
                    if run.chat_id.as_deref() == Some(id) && run.status == RunStatus::Queued {
                        run.status = RunStatus::Completed;
                        *run.result=format!("{}\n\n{}",run.objective.replace("Prompt","Response"),"A retained paragraph exercises a longer conversation, including **formatted text**, `code`, and evidence without navigating the composer.\n\n".repeat(32));
                    }
                }
            } else if let Some(query) = path.strip_prefix("/?") {
                let html = crate::public_render(&served.guard().unwrap(), &crate::fields(query), "none");
                let _ = control_http::respond(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    html.as_bytes(),
                    "",
                    control_http::Policy::Embedded,
                );
            } else {
                crate::session_http::handle(
                    &mut stream,
                    &mut served.guard().unwrap(),
                    &method,
                    &path,
                    "engineering",
                    &headers,
                    &body,
                );
            }
        }
    });
    struct Fixture {
        stop: Arc<AtomicBool>,
        events: control_effect::sync::CancelToken,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.events.cancel();
            let _ = self.thread.take().unwrap().join();
        }
    }
    let _fixture = Fixture {
        stop,
        events: event_cancel,
        thread: Some(thread),
    };
    let driver = std::env::var("CONTROL_WEBDRIVER_URL").expect("local WebDriver URL");
    let chrome = std::env::var("CONTROL_QA_BROWSER").is_ok_and(|s| s == "chrome");
    let capabilities = if chrome {
        json!({"browserName":"chrome","goog:chromeOptions":{"binary":"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome","args":["--headless=new","--no-first-run","--no-default-browser-check"]}})
    } else {
        json!({"browserName":"firefox","moz:firefoxOptions":{"binary":"/Applications/Firefox.app/Contents/MacOS/firefox","args":["-headless"]}})
    };
    let client = Client::builder().timeout(Duration::from_secs(20)).build().unwrap();
    let session: Value = client
        .post(format!("{driver}/session"))
        .json(&json!({"capabilities":{"alwaysMatch":capabilities}}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let id = session["value"]["sessionId"].as_str().expect("Browser session failed");
    struct Browser {
        client: Client,
        url: String,
    }
    impl Browser {
        #[track_caller]
        fn request(&self, method: reqwest::Method, path: &str, args: Value) -> Value {
            let mut r = self.client.request(method.clone(), format!("{}{path}", self.url));
            if method == reqwest::Method::POST {
                r = r.json(&args);
            }
            let response = r.send().unwrap();
            let status = response.status();
            let value: Value = response.json().unwrap();
            assert!(status.is_success(), "WebDriver {path}: {}", value["value"]["error"]);
            value["value"].clone()
        }
        #[track_caller]
        fn post(&self, path: &str, args: Value) -> Value {
            self.request(reqwest::Method::POST, path, args)
        }
        fn get(&self, path: &str) -> Value {
            self.request(reqwest::Method::GET, path, Value::Null)
        }
        fn element(&self, selector: &str) -> String {
            self.post("/element",json!({"using":"css selector","value":selector}))["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned()
        }
        fn text(&self, selector: &str) -> String {
            let start = Instant::now();
            loop {
                let id = self.element(selector);
                let response = self
                    .client
                    .get(format!("{}/element/{id}/text", self.url))
                    .send()
                    .unwrap();
                let status = response.status();
                let value: Value = response.json().unwrap();
                if status.is_success() {
                    return value["value"].as_str().unwrap().to_owned();
                }
                assert!(
                    value["value"]["error"] == "stale element reference" && start.elapsed() < Duration::from_secs(10),
                    "WebDriver text: {}",
                    value["value"]["error"]
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        fn wait(&self, text: &str) {
            let start = Instant::now();
            loop {
                if self.text("body").contains(text) {
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(12),
                    "Transcript never displayed {text}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    impl Drop for Browser {
        fn drop(&mut self) {
            let _ = self.client.delete(&self.url).send();
        }
    }
    let b = Browser {
        client,
        url: format!("{driver}/session/{id}"),
    };
    b.post("/timeouts", json!({"implicit":5000,"pageLoad":15000}));
    // Browser defaults differ; narrow layouts deliberately overlay the
    // composer with the open request drawer. Use the same desktop size
    // for both composer tests, then exercise responsive sizes below.
    b.post("/window/rect", json!({"width":1280,"height":900}));
    if long_messages_only {
        b.post("/url", json!({"url":format!("{server}/dm/{chat}")}));
        let input = b.element("textarea[data-enter-sends=true]");
        let text = format!("{} END", "long instructions ".repeat(250));
        b.post(&format!("/element/{input}/value"), json!({"text":text}));
        assert_eq!(b.get(&format!("/element/{input}/property/value")), text);
        let send = b.element(".send-message");
        b.post(&format!("/element/{send}/click"), json!({}));
        let started = Instant::now();
        while b.get(&format!("/element/{input}/property/value")) != "" {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "Long message was not acknowledged"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            state
                .lock()
                .unwrap()
                .chats
                .iter()
                .find(|c| c.id == chat)
                .unwrap()
                .messages
                .last()
                .unwrap()
                .text,
            text
        );
        for _ in 0..(control_context::limits::CHAT_MESSAGE_CHARS / 4000) {
            b.post(&format!("/element/{input}/value"), json!({"text":"x".repeat(4000)}));
        }
        assert_eq!(b.get(&format!("/element/{input}/property/validationMessage")), "");
        b.post(&format!("/element/{input}/value"), json!({"text":"x"}));
        assert_eq!(
            b.get(&format!("/element/{input}/property/validationMessage")),
            control_context::limits::CHAT_MESSAGE_ERROR
        );
        let count = state
            .lock()
            .unwrap()
            .chats
            .iter()
            .find(|c| c.id == chat)
            .unwrap()
            .messages
            .len();
        b.post(&format!("/element/{send}/click"), json!({}));
        assert_eq!(
            state
                .lock()
                .unwrap()
                .chats
                .iter()
                .find(|c| c.id == chat)
                .unwrap()
                .messages
                .len(),
            count
        );
        assert_eq!(
            b.get(&format!("/element/{input}/property/value"))
                .as_str()
                .unwrap()
                .len(),
            control_context::limits::CHAT_MESSAGE_CHARS + 1
        );
        b.post(&format!("/element/{input}/value"), json!({"text":"\u{e003}"}));
        assert_eq!(b.get(&format!("/element/{input}/property/validationMessage")), "");
        b.post(&format!("/element/{send}/click"), json!({}));
        let started = Instant::now();
        while b.get(&format!("/element/{input}/property/value")) != "" {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "Maximum message was not acknowledged"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            state
                .lock()
                .unwrap()
                .chats
                .iter()
                .find(|c| c.id == chat)
                .unwrap()
                .messages
                .last()
                .unwrap()
                .text,
            "x".repeat(control_context::limits::CHAT_MESSAGE_CHARS)
        );
        return;
    }
    let (child, newest) = {
        let mut state = state.lock().unwrap();
        let mut parent = String::new();
        for index in 0..55 {
            parent = crate::chat::create(&mut state);
            state.chats.last_mut().unwrap().name = Some(format!("History thread {index}"));
            state
                .runtime
                .enqueue(format!("History run {index}"), RunKind::Chat, "test");
            let run = state.runtime.runs.last_mut().unwrap();
            run.status = RunStatus::Completed;
            run.chat_id = Some(parent.clone());
        }
        let child = crate::chat::create(&mut state);
        let session = state.chats.last_mut().unwrap();
        session.parent_chat = Some(parent.clone());
        session.title = "Nested history agent".into();
        (child, parent)
    };
    let count = |b: &Browser, selector: &str| {
        b.post("/elements", json!({"using":"css selector","value":selector}))
            .as_array()
            .unwrap()
            .len()
    };
    // "Show turns", where the old run history redirects (the fixture server
    // does not redirect; `url_names` tests cover that), lists every DM's turns.
    b.post("/url", json!({"url":format!("{server}/bots/history?expand=1")}));
    b.wait("History run 54");
    assert_eq!(
        count(&b, "details.history-dm[open]"),
        50,
        "One page of DMs, every row open"
    );
    assert_eq!(count(&b, ".history-turn"), 49, "The newest DM, the child, has no turns");
    // Without filters, rows load their turns when opened.
    b.post("/url", json!({"url":format!("{server}/bots/history")}));
    b.element(".sidebar a[href='/?view=agents'][aria-current='page']");
    b.element("nav[aria-label='Bots'] a[href='/bots/history'][aria-current='page']");
    assert_eq!(count(&b, "nav[aria-label='History']"), 0, "One view, no tabs");
    assert_eq!(count(&b, "details.history-dm"), 50);
    assert_eq!(count(&b, ".history-turn"), 0, "Turns load on demand");
    b.element(&format!("details.history-dm[data-dm='{child}']"));
    let row = b.element(&format!("details[data-dm='{newest}'] > summary"));
    b.post(&format!("/element/{row}/click"), json!({}));
    b.wait("History run 54");
    assert_eq!(count(&b, &format!("details[data-dm='{newest}'] .history-turn")), 1);
    assert_eq!(
        count(&b, &format!("details[data-dm='{newest}'] [data-turns-slot]")),
        0,
        "The fragment replaced the link"
    );
    if let Ok(path) = std::env::var("CONTROL_QA_HISTORY_SCREENSHOT") {
        use base64::Engine;
        let screenshot = b.get("/screenshot");
        std::fs::write(
            path,
            base64::engine::general_purpose::STANDARD
                .decode(screenshot.as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
    }
    for width in [1280, 390] {
        b.post("/window/rect", json!({"width":width,"height":900}));
        let body = b.element("body");
        assert!(
            b.get(&format!("/element/{body}/property/scrollWidth"))
                .as_i64()
                .unwrap()
                <= b.get(&format!("/element/{body}/property/clientWidth"))
                    .as_i64()
                    .unwrap(),
            "History must fit narrow screens without horizontal scrolling"
        );
    }
    b.post("/window/rect", json!({"width":1280,"height":900}));
    let older = b.element(".history-controls a[href*='older=']");
    b.post(&format!("/element/{older}/click"), json!({}));
    b.wait("History thread 0");
    assert!(!b.text("body").contains("History thread 54"));
    let parent = {
        let mut state = state.lock().unwrap();
        let parent = state
            .chats
            .iter()
            .find(|session| session.id == child)
            .unwrap()
            .parent_chat
            .clone()
            .unwrap();
        for index in 0..64 {
            crate::chat::create(&mut state);
            let session = state.chats.last_mut().unwrap();
            session.parent_chat = Some(parent.clone());
            session.title = format!("Spawned worker {index}");
        }
        parent
    };
    b.post("/url", json!({"url":format!("{server}/dm/{child}")}));
    let menu = b.element(".conversation-toggle");
    assert_eq!(
        b.get(&format!("/element/{menu}/property/open")),
        false,
        "A new browser tab starts with the picker closed"
    );
    let fresh_input = b.element("textarea[data-enter-sends=true]");
    let toggle = b.element(".conversation-toggle > summary");
    b.post(&format!("/element/{toggle}/click"), json!({}));
    assert!(
        !b.post(
            "/elements",
            json!({"using":"css selector","value":".conversation-branch[open]"})
        )
        .as_array()
        .unwrap()
        .is_empty(),
        "The active child's parent must be expanded"
    );
    for width in [1280, 720] {
        b.post("/window/rect", json!({"width":width,"height":900}));
        let new_chat = b.element(".conversation-navigation nav > a:first-child");
        assert!(
            b.get(&format!("/element/{new_chat}/rect"))["height"].as_f64().unwrap() >= 32.0,
            "New chat must retain its height beside a long expanded team"
        );
        let nav = b.element(".conversation-navigation nav");
        assert!(
            b.get(&format!("/element/{nav}/property/scrollHeight"))
                .as_i64()
                .unwrap()
                > b.get(&format!("/element/{nav}/property/clientHeight"))
                    .as_i64()
                    .unwrap(),
            "The team list must scroll instead of squeezing controls"
        );
        let row = b.element(&format!(".conversation-navigation a[href='/dm/{child}']"));
        let row = b.get(&format!("/element/{row}/rect"));
        let nav = b.get(&format!("/element/{nav}/rect"));
        assert_eq!(
            row["x"], nav["x"],
            "The active child's highlight spans the picker, including its indentation"
        );
        assert!((row["width"].as_f64().unwrap() - nav["width"].as_f64().unwrap()).abs() < 1.0);
    }
    b.post("/window/rect", json!({"width":1280,"height":900}));
    {
        let mut state = state.lock().unwrap();
        crate::chat::create(&mut state);
        state.chats.last_mut().unwrap().title = "Live recent thread".into();
    }
    b.wait("Live recent thread");
    assert_eq!(
        b.get(&format!("/element/{fresh_input}/name")),
        "textarea",
        "Refreshing the disclosed picker must not reload chat"
    );
    for id in [&parent, &child] {
        let link = b.element(&format!(".conversation-navigation a[href='/dm/{id}']"));
        b.post(&format!("/element/{link}/click"), json!({}));
        b.element("textarea[data-enter-sends=true]");
        let menu = b.element(".conversation-toggle");
        assert_eq!(
            b.get(&format!("/element/{menu}/property/open")),
            true,
            "Following parent/child links must preserve the open picker"
        );
    }
    let new_chat = b.element(".conversation-navigation nav > a[href='/dms?new=1']");
    b.post(&format!("/element/{new_chat}/click"), json!({}));
    b.element("textarea[data-enter-sends=true]");
    let menu = b.element(".conversation-toggle");
    assert_eq!(
        b.get(&format!("/element/{menu}/property/open")),
        true,
        "New chat pages without a transcript must also restore the picker"
    );
    assert!(
        b.post(
            "/elements",
            json!({"using":"css selector","value":"iframe.chat-transcript"})
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
    let toggle = b.element(".conversation-toggle > summary");
    b.post(&format!("/element/{toggle}/click"), json!({}));
    b.post("/refresh", json!({}));
    b.element("textarea[data-enter-sends=true]");
    let menu = b.element(".conversation-toggle");
    assert_eq!(
        b.get(&format!("/element/{menu}/property/open")),
        false,
        "Explicitly closing the picker must survive reload"
    );
    {
        let mut state = state.lock().unwrap();
        let late = crate::chat::create(&mut state);
        let session = state.chats.last_mut().unwrap();
        session.parent_chat = Some(chat.clone());
        session.title = format!("Late background child {late}");
    }
    for path in ["/?view=tools", "/bots/history", "/dms"] {
        b.post("/url", json!({"url":format!("{server}{path}")}));
    }
    let toggle = b.element(".conversation-toggle > summary");
    b.post(&format!("/element/{toggle}/click"), json!({}));
    let newest = b.element("nav[aria-label=DMs] > a[href^='/dm/']");
    assert_eq!(
        b.get(&format!("/element/{newest}/text")),
        "Live recent thread",
        "Returning through Tools, Runs and Chat must not promote an older background playtest"
    );
    b.post("/url", json!({"url":format!("{server}/dm/{chat}")}));
    let input = b.element("textarea[data-enter-sends=true]");
    b.post(&format!("/element/{input}/value"), json!({"text":"/op"}));
    let option = b.element("#command-open");
    assert_eq!(b.get(&format!("/element/{option}/displayed")), true);
    b.post(&format!("/element/{option}/click"), json!({}));
    assert_eq!(b.get(&format!("/element/{input}/property/value")), "/open");
    assert_eq!(b.text(".composer-highlight .chat-command"), "/open");
    b.post(&format!("/element/{input}/clear"), json!({}));
    b.post(
        &format!("/element/{input}/value"),
        json!({"text":"Keep this unfinished draft"}),
    );
    // A descendant's new request opens the stable panel without navigating
    // the parent or touching the draft. Once closed it stays closed while
    // the same requests are still pending.
    let page_body = b.element("body");
    let request_toggle = b.element(".chat-requests > summary");
    let toggle = b.element(".chat-requests");
    if b.get(&format!("/element/{toggle}/property/open")) == true {
        b.post(&format!("/element/{request_toggle}/click"), json!({}));
    }
    {
        let mut state = state.lock().unwrap();
        let child = crate::chat::create(&mut state);
        state.chats.last_mut().unwrap().parent_chat = Some(chat.clone());
        let mut bots = serde_json::to_value(&state.bots).unwrap();
        bots["approvals"] = json!([{"id":"bot-browser-child","chat":child,"requester":child,"origin":"fixture","every_minutes":5,"room":true,"decision":"Pending","acknowledged":false}]);
        state.bots = serde_json::from_value(bots).unwrap();
    }
    let start = Instant::now();
    while b.get(&format!("/element/{toggle}/property/open")) != true {
        assert!(start.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(b.get(&format!("/element/{page_body}/name")), "body");
    assert_eq!(
        b.get(&format!("/element/{input}/property/value")),
        "Keep this unfinished draft"
    );
    b.post(&format!("/element/{request_toggle}/click"), json!({}));
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(b.get(&format!("/element/{toggle}/property/open")), false);
    b.post(&format!("/element/{request_toggle}/click"), json!({}));
    let frame = b.element("iframe.chat-transcript");
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
    let body = b.element("body");
    b.post("/frame", json!({"id":null}));
    let requests = b.element("iframe.chat-requests-frame");
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":requests}}));
    b.wait("Trust folder and open");
    b.wait("Bot request");
    let approval = b.element(".project-picker button[name=approve]");
    let approval_color = b.get(&format!("/element/{approval}/css/background-color"));
    assert!(
        approval_color == "rgb(181, 160, 255)" || approval_color == "rgba(181, 160, 255, 1)",
        "Approval must use the theme purple: {approval_color}"
    );
    assert_eq!(b.get(&format!("/element/{approval}/css/border-radius")), "0px");
    let previous_project = b.element(".project-picker > summary");
    b.post(&format!("/element/{previous_project}/click"), json!({}));
    {
        crate::projects::request(
            &mut state.lock().unwrap(),
            &chat,
            "/fixture/replacement",
            crate::projects::Requester::Agent { run: None },
        )
        .unwrap();
    }
    b.wait("/fixture/replacement");
    let replacement = b.element(".project-picker");
    assert_eq!(
        b.get(&format!("/element/{replacement}/property/open")),
        true,
        "A new request must not inherit the old collapsed state"
    );
    b.post("/frame", json!({"id":null}));
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
    assert_eq!(b.get(&format!("/element/{body}/name")), "body");
    {
        let mut state = state.lock().unwrap();
        state.runtime.runs[0].attempts[0].activity.push(crate::usage::Activity {
            id: "command-1".into(),
            kind: "command_execution".into(),
            text: "cargo test".into(),
            output: "Streaming output alpha".into(),
            status: "running".into(),
        });
    }
    b.wait("Streaming output alpha");
    {
        let mut state = state.lock().unwrap();
        state.runtime.runs[0].attempts[0].activity.push(crate::usage::Activity {
            id: "visible-message-1".into(),
            kind: "message".into(),
            text: "Cedar is checking inventory".into(),
            output: String::new(),
            status: "running".into(),
        });
    }
    b.wait("Cedar is checking inventory");
    assert!(
        !b.text(".run-activity").contains("Cedar is checking inventory"),
        "Chat messages must be inline, outside the collapsed tool log"
    );
    b.wait("Service streaming alpha");
    b.wait("printf service-output");
    {
        let mut state = state.lock().unwrap();
        let mut service = serde_json::to_value(&state.services[0]).unwrap();
        service["executions"][0]["output"] = json!("Service streaming alpha\nService streaming beta");
        state.services[0] = serde_json::from_value(service).unwrap();
    }
    b.wait("Service streaming beta");
    assert_eq!(
        b.get(&format!("/element/{body}/name")),
        "body",
        "Service output must update without replacing the transcript document"
    );
    let details = b.element(".run-activity > summary");
    b.post(&format!("/element/{details}/click"), json!({}));
    b.post("/frame", json!({"id":null}));
    b.post(&format!("/element/{input}/click"), json!({}));
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
    failed.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(1600));
    {
        let mut state = state.lock().unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = RunStatus::Completed;
        run.result = "Finished without another user message".into();
        run.attempts[0].activity.push(crate::usage::Activity {
            id: "workspace-diff".into(),
            kind: "file_change".into(),
            text: "1 file · +1 −1".into(),
            output: "--- a/demo.rs\n+++ b/demo.rs\n@@ -1 +1 @@\n-old\n+new".into(),
            status: "completed".into(),
        });
    }
    failed.store(false, Ordering::Relaxed);
    b.wait("Finished without another user message");
    let diff = b.element(".edit-diff > summary");
    b.post(&format!("/element/{diff}/click"), json!({}));
    assert_eq!(b.text(".diff-add"), "+new");
    assert_eq!(b.text(".diff-remove"), "-old");
    b.post("/frame", json!({"id":null}));
    b.post(&format!("/element/{input}/click"), json!({}));
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
    let details = b.element(".run-activity");
    assert_eq!(b.get(&format!("/element/{details}/property/open")), false);
    assert_eq!(
        b.get(&format!("/element/{body}/name")),
        "body",
        "The iframe must never navigate"
    );
    let article = b.element(".assistant-message");
    std::thread::sleep(Duration::from_millis(2200));
    assert_eq!(
        b.get(&format!("/element/{article}/name")),
        "article",
        "Unchanged polls must retain DOM nodes"
    );
    {
        let mut state = state.lock().unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = RunStatus::Waiting;
        run.question = Some(question);
    }
    b.wait("Decision requested");
    let activity = b.element(".run-activity");
    assert_eq!(
        b.get(&format!("/element/{activity}/property/open")),
        false,
        "Collapsed activity must survive a changed transcript in both browser realms"
    );
    b.post("/frame", json!({"id":null}));
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":requests}}));
    b.wait("Which format?");
    assert!(b.text("body").contains("Plain text"));
    let answer = b.element("form");
    assert_eq!(b.get(&format!("/element/{answer}/attribute/target")), "_top");
    b.post("/frame", json!({"id":null}));
    assert_eq!(
        b.get(&format!("/element/{input}/property/value")),
        "Keep this unfinished draft"
    );
    assert_eq!(b.get("/element/active")["element-6066-11e4-a52e-4f735466cecf"], input);
    // Fifteen real form submissions through the WASM client and HTTP command;
    // deterministic worker results isolate browser costs from model latency.
    b.post("/url", json!({"url":format!("{server}/dm/{port_chat}")}));
    let port_page = b.element("body");
    let draft = b.element("textarea[data-enter-sends=true]");
    b.post(
        &format!("/element/{draft}/value"),
        json!({"text":"Keep this draft while denying a port"}),
    );
    let port_frame = b.element("iframe.chat-requests-frame");
    b.post(
        "/frame",
        json!({"id":{"element-6066-11e4-a52e-4f735466cecf":port_frame}}),
    );
    b.wait("Port access");
    let request = b.element(".port-request");
    let deny = b.element(".port-request button[name=revoke]");
    // Deny must work without the loopback checkbox. FormData alone omits
    // submitter values and would accidentally send an approval instead.
    reject_port.store(true, Ordering::Relaxed);
    b.post(&format!("/element/{deny}/click"), json!({}));
    assert_eq!(
        b.get(&format!("/element/{deny}/property/disabled")),
        true,
        "A port decision needs immediate disabled feedback"
    );
    assert_eq!(b.get(&format!("/element/{deny}/text")), "Denying…");
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        b.get(&format!("/element/{deny}/property/disabled")),
        true,
        "A transcript poll must retain the pending form"
    );
    b.post(&format!("/element/{deny}/click"), json!({}));
    assert_eq!(
        port_posts.load(Ordering::Relaxed),
        1,
        "A repeated click must not send another decision"
    );
    b.wait("fixture port save failed");
    assert_eq!(
        b.get(&format!("/element/{deny}/property/disabled")),
        false,
        "A failed decision must be retryable"
    );
    assert_eq!(b.get(&format!("/element/{deny}/text")), "Deny");
    b.post(&format!("/element/{deny}/click"), json!({}));
    let start = Instant::now();
    loop {
        let requests = b.post(
            "/elements",
            json!({"using":"css selector","value":".port-request:not([hidden])"}),
        );
        if requests.as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "Denied port did not leave the chat prompt"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(port_posts.load(Ordering::Relaxed), 2);
    let _ = request;
    b.post("/frame", json!({"id":null}));
    assert_eq!(
        b.get(&format!("/element/{port_page}/name")),
        "body",
        "Port decisions must not navigate the page"
    );
    assert_eq!(
        b.get(&format!("/element/{draft}/property/value")),
        "Keep this draft while denying a port"
    );
    b.post("/url", json!({"url":format!("{server}/dm/{long_chat}")}));
    let page = b.element("body");
    let input = b.element("textarea[data-enter-sends=true]");
    let frame = b.element("iframe.chat-transcript");
    rejected.store(true, Ordering::Relaxed);
    b.post(
        &format!("/element/{input}/value"),
        json!({"text":"Prompt rejected\u{E007}"}),
    );
    b.wait("Could not save the chat");
    assert_eq!(b.get(&format!("/element/{input}/property/value")), "Prompt rejected");
    assert!(
        state
            .lock()
            .unwrap()
            .chats
            .iter()
            .find(|chat| chat.id == long_chat)
            .unwrap()
            .messages
            .is_empty()
    );
    rejected.store(false, Ordering::Relaxed);
    b.post(&format!("/element/{input}/clear"), json!({}));
    let mut first = None;
    let mut first_html = Value::Null;
    let mut timings = Vec::new();
    for turn in 1..=15 {
        let start = Instant::now();
        b.post(
            &format!("/element/{input}/value"),
            json!({"text":format!("Prompt {turn}\u{E007}")}),
        );
        b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
        b.wait(&format!("Response {turn}"));
        let scroller = b.element("html");
        let height = b
            .get(&format!("/element/{scroller}/property/scrollHeight"))
            .as_f64()
            .unwrap();
        let viewport = b
            .get(&format!("/element/{scroller}/property/clientHeight"))
            .as_f64()
            .unwrap();
        let top = b
            .get(&format!("/element/{scroller}/property/scrollTop"))
            .as_f64()
            .unwrap();
        assert!(
            height - viewport - top < 80.0,
            "Turn {turn} should follow the latest reply"
        );
        let current = b.element("article");
        if let Some(first) = &first {
            assert_eq!(
                &current,
                first,
                "Earlier messages changed at turn {turn}; before {first_html}, after {}",
                b.get(&format!("/element/{current}/property/outerHTML"))
            );
        } else {
            first_html = b.get(&format!("/element/{current}/property/outerHTML"));
            first = Some(current);
        }
        b.post("/frame", json!({"id":null}));
        assert_eq!(
            b.get(&format!("/element/{page}/name")),
            "body",
            "Sending must not navigate the page"
        );
        assert_eq!(b.get(&format!("/element/{input}/property/value")), "");
        timings.push(start.elapsed());
    }
    assert_eq!(
        state
            .lock()
            .unwrap()
            .chats
            .iter()
            .find(|chat| chat.id == long_chat)
            .unwrap()
            .messages
            .len(),
        15
    );
    b.post("/frame", json!({"id":{"element-6066-11e4-a52e-4f735466cecf":frame}}));
    let scroller = b.element("html");
    let bottom_before = b
        .get(&format!("/element/{scroller}/property/scrollTop"))
        .as_f64()
        .unwrap();
    b.post("/actions",json!({"actions":[{"type":"wheel","id":"history-scroll","actions":[{"type":"scroll","x":0,"y":0,"deltaX":0,"deltaY":-1200,"duration":100,"origin":{"element-6066-11e4-a52e-4f735466cecf":scroller}}]}]}));
    std::thread::sleep(Duration::from_millis(500));
    let top = b
        .get(&format!("/element/{scroller}/property/scrollTop"))
        .as_f64()
        .unwrap();
    assert!(
        top < bottom_before - 400.0,
        "The wheel must actually scroll the transcript: {bottom_before} → {top}"
    );
    {
        let mut state = state.lock().unwrap();
        let run = state
            .runtime
            .runs
            .iter_mut()
            .rev()
            .find(|run| run.chat_id.as_deref() == Some(&long_chat))
            .unwrap();
        run.result.push_str("\n\nTail arrived while reading history.");
    }
    b.wait("Tail arrived while reading history.");
    let after = b
        .get(&format!("/element/{scroller}/property/scrollTop"))
        .as_f64()
        .unwrap();
    assert!(
        (after - top).abs() < 20.0,
        "Reading history moved from {top} to {after}"
    );
    b.post("/actions",json!({"actions":[{"type":"wheel","id":"history-scroll","actions":[{"type":"scroll","x":0,"y":0,"deltaX":0,"deltaY":1000000,"duration":100,"origin":{"element-6066-11e4-a52e-4f735466cecf":scroller}}]}]}));
    std::thread::sleep(Duration::from_millis(500));
    {
        let mut state = state.lock().unwrap();
        let run = state
            .runtime
            .runs
            .iter_mut()
            .rev()
            .find(|run| run.chat_id.as_deref() == Some(&long_chat))
            .unwrap();
        run.result.push_str(&format!(
            "\n\n{}\nFollowing resumed.",
            "An additional streamed paragraph.\n\n".repeat(20)
        ));
    }
    b.wait("Following resumed.");
    let height = b
        .get(&format!("/element/{scroller}/property/scrollHeight"))
        .as_f64()
        .unwrap();
    let viewport = b
        .get(&format!("/element/{scroller}/property/clientHeight"))
        .as_f64()
        .unwrap();
    let top = b
        .get(&format!("/element/{scroller}/property/scrollTop"))
        .as_f64()
        .unwrap();
    assert!(
        height - viewport - top < 80.0,
        "Scrolling back to the end must re-enable following"
    );
    b.post("/frame", json!({"id":null}));
    timings.sort();
    println!(
        "{}: 15 turns, median {:?}, maximum {:?}",
        if chrome { "Chrome" } else { "Firefox" },
        timings[7],
        timings[14]
    );
    assert!(
        timings[14] < Duration::from_secs(5),
        "Local fixture responses should stay responsive"
    );
}

#[test]
fn asynchronous_send_commits_before_ack_and_preserves_native_navigation() {
    let mut state = State::default();
    let id = chat::create(&mut state);
    let fields = crate::fields("message=hello&composition=1&lens=research");
    let before = serde_json::to_vec(&state).unwrap();
    let mut response = Vec::new();
    send_message(
        &mut response,
        &mut state,
        &id,
        "engineering",
        "Accept: application/json",
        &fields,
        |_| Err(std::io::Error::other("disk full")),
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    assert!(
        String::from_utf8(response)
            .unwrap()
            .contains("\"error\":\"Could not save the chat")
    );
    let mut response = Vec::new();
    send_message(
        &mut response,
        &mut state,
        &id,
        "engineering",
        "Accept: application/json",
        &fields,
        |_| Ok(()),
    );
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(!response.contains("Location:"));
    assert!(response.contains("application/json"));
    assert_eq!(state.chats[0].messages.len(), 1);
    let mut response = Vec::new();
    send_message(
        &mut response,
        &mut state,
        &id,
        "engineering",
        "",
        &crate::fields("message=%2Fopen"),
        |_| panic!("Picker navigation must not save"),
    );
    assert!(
        String::from_utf8(response)
            .unwrap()
            .to_ascii_lowercase()
            .contains(&format!("location: /dm/{id}/project"))
    );
    let mut response = Vec::new();
    send_message(
        &mut response,
        &mut state,
        &id,
        "engineering",
        "",
        &crate::fields("message=%2Fcancel"),
        |_| Ok(()),
    );
    assert!(
        String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 303 See Other")
    );
}
#[test]
fn long_form_messages_checkpoint_completely_or_restore_the_draft() {
    let text = format!("{} END", "🦊".repeat(control_context::limits::CHAT_MESSAGE_CHARS - 4));
    let encoded = format!(
        "message={}%20END",
        "%F0%9F%A6%8A".repeat(control_context::limits::CHAT_MESSAGE_CHARS - 4)
    );
    let fields = crate::fields(&encoded);
    for header in ["", "Accept: application/json\r\n"] {
        let mut state = State::default();
        let id = chat::create(&mut state);
        let mut response = Vec::new();
        send_message(&mut response, &mut state, &id, "none", header, &fields, |saved| {
            let restored: State = serde_json::from_slice(&serde_json::to_vec(saved)?).unwrap();
            assert_eq!(restored.chats[0].messages[0].text, text);
            assert_eq!(restored.runtime.runs[0].objective.as_str(), text);
            Ok(())
        });
        assert!(String::from_utf8(response).unwrap().starts_with(if header.is_empty() {
            "HTTP/1.1 303"
        } else {
            "HTTP/1.1 200"
        }));
        let before = serde_json::to_vec(&state).unwrap();
        let mut response = Vec::new();
        send_message(&mut response, &mut state, &id, "none", header, &fields, |_| {
            Err(std::io::Error::other("fixture disk full"))
        });
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"));
        if header.is_empty() {
            assert!(response.contains(&text));
        }
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }
}

#[test]
fn failed_design_answer_checkpoint_restores_question_and_does_not_resume() {
    let (mut state, id) = crate::design::tests::paused();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        change_with_save(&mut state, "control.chat.send", &json!({"id":id,"message":"2"}), |_| {
            Err(std::io::Error::other("disk full"))
        })
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    assert!(
        change_with_save(
            &mut state,
            "control.chat.send",
            &json!({"id":id,"message":"2"}),
            |_| Ok(())
        )
        .is_ok()
    );
    assert_eq!(state.runtime.runs[0].status, RunStatus::Continued);
    assert_eq!(state.runtime.runs.len(), 2);
}
#[test]
fn rejected_first_message_creates_no_chat_and_keeps_the_escaped_draft() {
    let mut state = State::default();
    let mut response = Vec::new();
    assert!(handle(
        &mut response,
        &mut state,
        "POST",
        "/dms",
        "none",
        "",
        b"message=%2Fanswer+%3Cb%3Edraft"
    ));
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
        state.runtime.runs[0].status = RunStatus::named(status);
        state.runtime.runs[0].result = "Existing progress".into();
        let before = serde_json::to_vec(&state).unwrap();
        assert!(
            change_with_save(
                &mut state,
                "control.chat.send",
                &json!({"id":id,"message":"/cancel"}),
                |_| Err(std::io::Error::other("disk full"))
            )
            .is_err()
        );
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
    assert!(
        change_with_save(
            &mut state,
            "control.chat.send",
            &json!({"id":id,"message":"Test request"}),
            |_| Ok(())
        )
        .is_ok()
    );
    assert_eq!(state.runtime.runs.len(), 1);
}
#[test]
fn failed_mode_change_does_not_persist_mode_or_new_source_work() {
    let mut state = State::default();
    let id = chat::create(&mut state);
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        change_with_save(
            &mut state,
            "control.chat.send",
            &json!({"id":id,"mode":"improve","message":"Discuss a simpler UI"}),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
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
    let catalogue = rpc(&mut state, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = catalogue["result"]["tools"].as_array().unwrap();
    let names: std::collections::BTreeSet<_> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names.len(), tools.len(), "MCP tools must have unique names");
    for expected in [
        "control.chat.create",
        "control.room.read",
        "control.project.request",
        "control_github_open_pr",
    ] {
        assert!(names.contains(expected));
    }
    assert_eq!(
        rpc(&mut state, &json!({"jsonrpc":"2.0","id":3,"method":"unknown"}))["error"]["code"],
        -32601
    );
}
#[test]
fn room_checkpoint_failure_rolls_back_and_invalid_membership_creates_nothing() {
    let mut state = State::default();
    assert!(
        change_with_save(
            &mut state,
            "control.room.post",
            &json!({"room":"research","message":"A finding"}),
            |_| Err(std::io::Error::other("disk full"))
        )
        .is_err()
    );
    assert!(state.collective.posts.is_empty());
    assert!(
        change_with_save(
            &mut state,
            "control.chat.create",
            &json!({"room":"../../private"}),
            |_| Ok(())
        )
        .is_err()
    );
    assert!(state.chats.is_empty());
    let chat = change_with_save(&mut state, "control.chat.create", &json!({"room":"research"}), |_| {
        Ok(())
    })
    .unwrap();
    assert_eq!(chat["room"], format!("research/{}", chat["id"].as_str().unwrap()));
    assert!(
        crate::rooms::read(&state, chat["room"].as_str().unwrap())["chats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == chat["id"])
    );
}
/// Pins the operator endpoint's envelopes, including malformed input, so the
/// shared protocol module keeps each reply byte for byte.
#[test]
fn operator_mcp_envelopes_are_pinned() {
    let mut state = State::default();
    let version = env!("CARGO_PKG_VERSION");
    for (request, expected) in [
        (
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"control","version":version}}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
            json!({"jsonrpc":"2.0","id":"a","result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control","version":version}}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":2,"result":{}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":null,"result":{}}),
        ),
        (
            json!({"jsonrpc":"1.0","id":3,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":3,"error":{"code":-32600,"message":"Invalid request."}}),
        ),
        (
            json!({"id":4,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":4,"error":{"code":-32600,"message":"Invalid request."}}),
        ),
        (
            json!([{"jsonrpc":"2.0","id":5,"method":"ping"}]),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid request."}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":6,"method":"resources/list"}),
            json!({"jsonrpc":"2.0","id":6,"error":{"code":-32601,"message":"Method not found."}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":7}),
            json!({"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found."}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"control.packages.search","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":8,"result":{"content":[{"type":"text","text":"Package tools are served by the HTTP MCP endpoint."}],"isError":true}}),
        ),
        (
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"control.chats.list","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"[]"}],"structuredContent":{"data":[]},"isError":false}}),
        ),
    ] {
        assert_eq!(rpc(&mut state, &request), expected, "{request}");
    }
    let listed = rpc(&mut state, &json!({"jsonrpc":"2.0","id":10,"method":"tools/list"}));
    assert_eq!(
        listed,
        json!({"jsonrpc":"2.0","id":10,"result":{"tools":crate::tool_registry::http_definitions()}})
    );
}

/// Every header of the chat routes' three response kinds (a redirect, a plain
/// refusal and a page that loads the client), so a change to how they respond
/// cannot move a byte. Bodies pass through unchanged; the two short ones are
/// pinned too.
#[test]
fn chat_route_response_bytes_are_pinned() {
    const SECURITY: &str = "Cache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\n";
    const EMBEDDED: &str = "Content-Security-Policy: default-src 'self'; script-src 'none'; connect-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'\r\n";
    const CHAT: &str = "Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'\r\n";
    let mut state = State::default();
    let mut get = |target: &str| {
        let mut response = Vec::new();
        assert!(
            handle(&mut response, &mut state, "GET", target, "none", "", &[]),
            "{target}"
        );
        String::from_utf8(response).unwrap()
    };
    assert_eq!(
        get("/?view=evolution"),
        format!(
            "HTTP/1.1 303 See Other\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n{SECURITY}{EMBEDDED}location: /dms?improve=1\r\n\r\n"
        )
    );
    assert_eq!(
        get("/dm/..."),
        format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 15\r\n{SECURITY}{EMBEDDED}\r\nChat not found."
        )
    );
    let page = get("/dms?new=1");
    let (head, body) = page.split_once("\r\n\r\n").unwrap();
    assert!(body.contains("/tool-window/chat-bootstrap.js"));
    assert_eq!(
        head,
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{SECURITY}{}",
            body.len(),
            CHAT.trim_end()
        )
    );
}
