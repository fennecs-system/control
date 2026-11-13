
use super::*;
use control_core::resources::{ChatId, ImageId, Lease, Owner, Resource, Scope};
pub(crate) fn pending_fixture() -> (State, String, String) {
    let (mut state, run, chat) = fixture();
    request(&mut state, &run, Direction::Egress).unwrap();
    (state, run, chat)
}
#[test]
fn pending_approvals_are_visible_only_in_the_request_scope() {
    let (mut state, _, chat) = pending_fixture();
    let other = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(chat.clone());
    assert!(pending_json(&state, &other).is_empty());
    assert_eq!(pending_json(&state, &chat).len(), 1);
    let html = crate::chat::requests(&state, &chat).unwrap();
    assert!(html.contains("class='assistant-message port-request'"));
    assert!(html.contains("value='-1'>Service lifetime"));
    assert!(html.contains("target='_top'"));
}
#[test]
fn requests_from_spawned_descendants_surface_in_the_top_level_chat() {
    let (mut state, run, chat) = pending_fixture();
    let mut parent = chat.clone();
    for _ in 0..2 {
        let child = crate::chat::create(&mut state);
        state.chats.last_mut().unwrap().parent_chat = Some(parent.clone());
        parent = child;
    }
    let mut wake = state.runtime.runs[0].clone();
    (wake.id, wake.chat_id, wake.kind) = (
        "bot-wake".into(),
        Some(parent.clone()),
        crate::runtime::RunKind::BotWake,
    );
    state.runtime.runs.push(wake);
    let mut port = state.ports[0].clone();
    (port.id, port.chat, port.origin_run) = (
        "port-grandchild".to_owned().try_into().unwrap(),
        parent.clone(),
        "bot-wake".into(),
    );
    state.ports.push(port);
    let ids = |id: &str| {
        pending_json(&state, id)
            .iter()
            .map(|p| p["port_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert!(
        ids(&chat).contains(&"port-grandchild".to_owned()),
        "{:?}",
        ids(&chat)
    );
    assert_eq!(
        ids(&parent),
        ["port-grandchild"],
        "a descendant sees only its own requests"
    );
    let unrelated = crate::chat::create(&mut state);
    assert!(pending_json(&state, &unrelated).is_empty());
    let _ = run;
}
pub(crate) fn fixture() -> (State, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Run a tool", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        ..Default::default()
    });
    let run_id = run.id.clone();
    let resource = Resource {
        id: ResourceId::try_from("svc-a".to_owned()).unwrap(),
        owner: Owner {
            node: NodeId::try_from("node-a".to_owned()).unwrap(),
            chat: ChatId::try_from(chat.clone()).unwrap(),
        },
        generation: Generation::INITIAL,
        image: ImageId::try_from(format!("sha256:{}", "a".repeat(64))).unwrap(),
        scope: Scope::PRIVATE_OFFLINE,
        lease: Lease::new(now(), 300).unwrap(),
        phase: Phase::Running(ContainerId::try_from("b".repeat(64)).unwrap()),
    };
    state.services.push(serde_json::from_value(json!({"resource":resource,"origin":{"run":run_id,"attempt":"attempt-a"},"actor":Actor::LocalOperator,"command":"sleep 300","uid":1000,"gid":1000,"diagnostic":""})).unwrap());
    (state, run_id, chat)
}
fn input(direction: Direction) -> Input {
    Input {
        service_id: ResourceId::try_from("svc-a".to_owned()).unwrap(),
        name: Name::try_from("echo".to_owned()).unwrap(),
        direction,
        service_port: NonZeroU16::new(31000).unwrap(),
        target: if direction == Direction::Egress {
            Some(Endpoint {
                host: "127.0.0.1".into(),
                port: NonZeroU16::new(31001).unwrap(),
            })
        } else {
            None
        },
        peer_port_id: None,
    }
}
fn request(state: &mut State, run: &str, direction: Direction) -> io::Result<Value> {
    admit(
        state,
        run,
        "attempt-a",
        "nonce-a",
        input(direction),
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
}
#[test]
fn port_capacity_is_per_chat_with_a_node_wide_cap() {
    let (mut state, run, chat) = fixture();
    request(&mut state, &run, Direction::Ingress).unwrap();
    let template = state.ports[0].clone();
    let fill = |state: &mut State, owner: &str, count: usize| {
        for index in 0..count {
            let mut port = template.clone();
            port.id = ResourceId::try_from(format!("port-{owner}-{index}")).unwrap();
            port.chat = owner.into();
            port.name = Name::try_from(format!("held{index}")).unwrap();
            state.ports.push(port);
        }
    };
    let ask = |state: &mut State, nonce: &str, name: &str| {
        let mut input = input(Direction::Ingress);
        input.name = Name::try_from(name.to_owned()).unwrap();
        admit(
            state,
            &run,
            "attempt-a",
            nonce,
            input,
            "node-a".to_owned().try_into().unwrap(),
            |_| Ok(()),
        )
    };
    fill(&mut state, &chat, PORTS_PER_CHAT - 1);
    let error = ask(&mut state, "nonce-b", "extra").unwrap_err().to_string();
    assert!(
        error.contains("control_port_stop") && error.contains("This chat"),
        "{error}"
    );
    state.ports.retain(|port| port.id == template.id);
    fill(&mut state, "c-other", PORTS_PER_CHAT * 4);
    assert!(
        ask(&mut state, "nonce-c", "second").is_ok(),
        "another chat at its cap does not block this one"
    );
    fill(&mut state, "c-third", MAX_PORTS);
    let error = ask(&mut state, "nonce-d", "third").unwrap_err().to_string();
    assert!(error.contains("node-wide"), "{error}");
}
#[test]
fn stopping_many_ports_is_one_save_and_rolls_back_together() {
    let (mut state, run, _) = fixture();
    request(&mut state, &run, Direction::Ingress).unwrap();
    let mut second = state.ports[0].clone();
    second.id = ResourceId::try_from("port-second".to_owned()).unwrap();
    state.ports.push(second);
    let before: Vec<Status> = state.ports.iter().map(|p| p.status).collect();
    assert!(stop_all(&mut state, &[0, 1], |_| Err(io::Error::other("disk"))).is_err());
    assert_eq!(
        state.ports.iter().map(|p| p.status).collect::<Vec<_>>(),
        before
    );
    let saves = std::cell::Cell::new(0);
    let stopped = stop_all(&mut state, &[0, 1], |_| {
        saves.set(saves.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        (saves.get(), stopped["stopped"].as_array().unwrap().len()),
        (1, 2)
    );
    assert!(state.ports.iter().all(|p| p.status == Status::Stopping));
}
#[test]
#[ignore = "requires local Chrome/Firefox WebDriver; isolated fixture, no credentials/models"]
fn live_port_controls_have_equal_buttons_and_themed_checkbox() {
    use reqwest::blocking::Client;
    use std::io::{BufRead, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    let (mut state, run, _) = fixture();
    request(&mut state, &run, Direction::Egress).unwrap();
    let state = Arc::new(Mutex::new(state));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/");
            if !crate::serve_stylesheet(&mut stream, path) {
                if path == "/tools/ports" {
                    http(&mut stream, &state, "GET", path, &[]);
                } else {
                    let _ =
                        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                }
            }
        }
    });
    struct Server(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            self.1.take().unwrap().join().unwrap();
        }
    }
    let _server = Server(stop, Some(server));
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let driver = std::env::var("CONTROL_WEBDRIVER_URL").unwrap();
    let chrome = std::env::var("CONTROL_QA_BROWSER").is_ok_and(|b| b == "chrome");
    let caps = if chrome {
        json!({"browserName":"chrome","goog:chromeOptions":{"binary":"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome","args":["--headless=new","--no-first-run"]}})
    } else {
        json!({"browserName":"firefox","moz:firefoxOptions":{"binary":"/Applications/Firefox.app/Contents/MacOS/firefox","args":["-headless"]}})
    };
    let session: Value = client
        .post(format!("{driver}/session"))
        .json(&json!({"capabilities":{"alwaysMatch":caps}}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let id = session["value"]["sessionId"]
        .as_str()
        .expect("Browser session");
    let base = format!("{driver}/session/{id}");
    struct Browser(Client, String);
    impl Drop for Browser {
        fn drop(&mut self) {
            let _ = self.0.delete(&self.1).send();
        }
    }
    let _browser = Browser(client.clone(), base.clone());
    let post = |path: &str, args: Value| -> Value {
        let response = client
            .post(format!("{base}{path}"))
            .json(&args)
            .send()
            .unwrap();
        assert!(response.status().is_success(), "WebDriver {path}");
        response.json::<Value>().unwrap()["value"].clone()
    };
    let get = |path: &str| -> Value {
        client
            .get(format!("{base}{path}"))
            .send()
            .unwrap()
            .json::<Value>()
            .unwrap()["value"]
            .clone()
    };
    post("/url", json!({"url":format!("{url}/tools/ports")}));
    let elements = post(
        "/elements",
        json!({"using":"css selector","value":"form[action='/tools/ports'] > button"}),
    );
    let buttons = elements.as_array().unwrap();
    assert_eq!(buttons.len(), 2);
    let element = |value: &Value| {
        value["element-6066-11e4-a52e-4f735466cecf"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let allow = element(&buttons[0]);
    let deny = element(&buttons[1]);
    let a = get(&format!("/element/{allow}/rect"));
    let b = get(&format!("/element/{deny}/rect"));
    assert_eq!(a["height"], b["height"]);
    assert_eq!(a["width"], b["width"]);
    assert_eq!(
        get(&format!("/element/{allow}/css/border-top-left-radius")),
        "3px"
    );
    let checkbox = element(&post(
        "/element",
        json!({"using":"css selector","value":"input[type=checkbox]"}),
    ));
    assert_eq!(get(&format!("/element/{checkbox}/css/appearance")), "none");
    let before = get(&format!("/element/{checkbox}/css/background-color"));
    post(&format!("/element/{checkbox}/click"), json!({}));
    assert_eq!(get(&format!("/element/{checkbox}/property/checked")), true);
    assert_ne!(
        get(&format!("/element/{checkbox}/css/background-color")),
        before
    );
}
#[test]
fn service_commands_collapse_once_their_run_finishes() {
    let (mut state, run, chat) = fixture();
    let mut second = serde_json::to_value(&state.services[0]).unwrap();
    second["resource"]["id"] = json!("svc-b");
    state.services.push(serde_json::from_value(second).unwrap());
    for service in ["svc-a", "svc-b"] {
        crate::services::enqueue_test_command(&mut state, &run, service, service, "echo probe", 60)
            .unwrap();
    }
    let live = crate::chat::transcript(&state, &chat).unwrap();
    assert!(live.contains("Commands · 2</summary>"));
    assert_eq!(
        live.matches("class='command-activity' data-transcript-key='service-command-")
            .count(),
        2
    );
    assert_eq!(
        live.matches("' open><summary><span>").count(),
        1,
        "Only the newest live command is expanded"
    );
    state.runtime.runs[0].status = "completed".into();
    let done = crate::chat::transcript(&state, &chat).unwrap();
    assert!(!done.contains("-commands' open"));
    assert!(!done.contains("' open><summary><span>"));
}
#[test]
fn captured_resource_handoff_is_exact_and_does_not_reown_ports() {
    let (mut state, parent, parent_chat) = fixture();
    let node: NodeId = "node-a".to_owned().try_into().unwrap();
    let grants = crate::services::grants(&state, &state.runtime.runs[0]);
    let child_chat = crate::chat::create(&mut state);
    let mut child = state.runtime.runs[0].clone();
    child.id = "child-run".into();
    child.chat_id = Some(child_chat);
    state.runtime.runs.push(child);
    assert!(!crate::services::accessible(
        &state.services[0],
        &node,
        &state.runtime.runs[1]
    ));
    state.runtime.runs[1].resource_grants = grants;
    assert!(crate::services::accessible(
        &state.services[0],
        &node,
        &state.runtime.runs[1]
    ));
    let result = admit(
        &mut state,
        "child-run",
        "attempt-a",
        "child-request",
        input(Direction::Ingress),
        node.clone(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.ports[0].chat, parent_chat);
    assert_eq!(state.ports[0].origin_run, "child-run");
    assert!(accessible(
        &state,
        &state.ports[0],
        &state.runtime.runs[0],
        &node
    ));
    assert!(
        crate::services::enqueue_test_command(
            &mut state,
            "child-run",
            "exec",
            "svc-a",
            "echo probe",
            60
        )
        .is_ok()
    );
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(accessible(
        &restored,
        &restored.ports[0],
        &restored.runtime.runs[1],
        &node
    ));
    let mut denied = restored.runtime.runs[1].clone();
    denied.actor = Actor::Unknown;
    assert!(!crate::services::accessible(
        &restored.services[0],
        &node,
        &denied
    ));
    denied = restored.runtime.runs[1].clone();
    let mut persona = crate::delegation::lineage(&denied);
    persona.role = crate::delegation::Role::Judge;
    denied.persona = Some(persona);
    assert!(!crate::services::accessible(
        &restored.services[0],
        &node,
        &denied
    ));
    assert!(!crate::services::accessible(
        &restored.services[0],
        &"other-node".to_owned().try_into().unwrap(),
        &restored.runtime.runs[1]
    ));
    state.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
    assert!(!accessible(
        &state,
        &state.ports[0],
        &state.runtime.runs[1],
        &node
    ));
    assert!(!crate::services::accessible(
        &state.services[0],
        &node,
        &state.runtime.runs[1]
    ));
    assert!(result["port_id"].is_string());
    assert_eq!(state.runtime.runs[0].id, parent);
}
#[test]
fn pending_requests_are_idempotent_owned_and_rollback_on_save_failure() {
    let (mut s, run, _) = fixture();
    let mut saves = 0;
    assert!(
        admit(
            &mut s,
            &run,
            "attempt-a",
            "nonce-a",
            input(Direction::Ingress),
            "node-a".to_owned().try_into().unwrap(),
            |_| Err(fail("disk full"))
        )
        .is_err()
    );
    assert!(s.ports.is_empty());
    let first = request(&mut s, &run, Direction::Ingress).unwrap();
    let second = admit(
        &mut s,
        &run,
        "attempt-a",
        "nonce-a",
        input(Direction::Ingress),
        "node-a".to_owned().try_into().unwrap(),
        |_| {
            saves += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(saves, 0);
    assert!(request(&mut s, &run, Direction::Egress).is_err());
    assert!(
        admit(
            &mut s,
            &run,
            "old-attempt",
            "other",
            input(Direction::Ingress),
            "node-a".to_owned().try_into().unwrap(),
            |_| Ok(())
        )
        .is_err()
    );
    s.services[0].resource.owner.chat = "another-chat".to_owned().try_into().unwrap();
    assert!(
        admit(
            &mut s,
            &run,
            "attempt-a",
            "another",
            input(Direction::Ingress),
            "node-a".to_owned().try_into().unwrap(),
            |_| Ok(())
        )
        .is_err()
    );
}
#[test]
fn an_operator_service_stop_ends_its_approved_ports() {
    let (mut state, run, _) = fixture();
    let time = now();
    request(&mut state, &run, Direction::Ingress).unwrap();
    let id = state.ports[0].id.as_str().to_owned();
    approve(&mut state, &id, 60, false, None, time, |_| Ok(())).unwrap();
    assert!(authorized(&state, &state.ports[0], time).is_ok());
    let service = state.services[0].resource.id.as_str().to_owned();
    let before = serde_json::to_vec(&state).unwrap();
    assert!(crate::services::stop(&mut state, &service, time, |_| Err(fail("disk full"))).is_err());
    assert_eq!(
        serde_json::to_vec(&state).unwrap(),
        before,
        "a failed save keeps the service"
    );
    crate::services::stop(&mut state, &service, time, |_| Ok(())).unwrap();
    assert!(matches!(
        state.services[0].resource.phase,
        Phase::Stopping { .. }
    ));
    assert!(authorized(&state, &state.ports[0], time).is_err());
    crate::services::stop(&mut state, &service, time, |_| panic!("already stopping")).unwrap();
    assert!(crate::services::stop(&mut state, "svc-missing", time, |_| Ok(())).is_err());
}
#[test]
fn service_lifetime_approval_is_explicit_revocable_and_survives_reload() {
    let (mut state, run, _) = fixture();
    let time = now();
    state.services[0].resource.lease = Lease::until_stopped(time).unwrap();
    request(&mut state, &run, Direction::Ingress).unwrap();
    let id = state.ports[0].id.as_str().to_owned();
    approve(&mut state, &id, -1, false, None, time, |_| Ok(())).unwrap();
    assert!(state.ports[0].summary()["expires_unix"].is_null());
    assert_eq!(state.ports[0].summary()["until_stopped"], true);
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(authorized(&restored, &restored.ports[0], time + 86400).is_ok());
    state.ports[0].status = Status::Stopping;
    assert!(authorized(&state, &state.ports[0], time + 86400).is_err());
    state.ports[0].status = Status::Approved;
    state.services[0].resource.phase = Phase::Stopping {
        container: None,
        reason: control_core::resources::StopReason::Requested,
    };
    assert!(authorized(&state, &state.ports[0], time + 86400).is_err());
}
#[test]
fn approval_clamps_to_service_deadline_after_operator_review() {
    let (mut state, run, _) = fixture();
    request(&mut state, &run, Direction::Ingress).unwrap();
    let id = state.ports[0].id.as_str().to_owned();
    let deadline = state.services[0].resource.lease.expires();
    // The form was opened with 312 seconds left; 13 seconds pass before approval.
    approve(
        &mut state,
        &id,
        312,
        false,
        None,
        deadline - 299,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.ports[0].approval.as_ref().unwrap().expires, deadline);
    state.ports[0].status = Status::Pending;
    assert!(approve(&mut state, &id, 0, false, None, deadline - 1, |_| Ok(())).is_err());
    assert!(approve(&mut state, &id, 3601, false, None, deadline - 1, |_| Ok(())).is_err());
    assert!(approve(&mut state, &id, 300, false, None, deadline, |_| Ok(())).is_err());
    assert_eq!(state.ports[0].status, Status::Pending);
}
#[test]
fn approval_is_exact_endpoint_and_durable_service_generation_not_a_network_grant() {
    let (mut s, run, _) = fixture();
    request(&mut s, &run, Direction::Egress).unwrap();
    let id = s.ports[0].id.as_str().to_owned();
    let time = now();
    let target = "127.0.0.1:31001".parse().unwrap();
    assert!(approve(&mut s, &id, 60, false, Some(target), time, |_| Ok(())).is_err());
    assert!(
        approve(&mut s, &id, 60, true, Some(target), time, |_| Err(fail(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(s.ports[0].status, Status::Pending);
    assert!(s.ports[0].approval.is_none());
    approve(&mut s, &id, 60, true, Some(target), time, |_| Ok(())).unwrap();
    let port = s.ports[0].clone();
    assert!(authorized(&s, &port, time).is_ok());
    assert!(authorized(&s, &port, time - 1).is_err());
    assert!(authorized(&s, &port, time + 60).is_err());
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert!(!restored.ports[0].ready);
    assert!(authorized(&restored, &port, time).is_ok());
    s.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
    assert!(authorized(&s, &port, time).is_err());
    s.services[0].resource.generation = Generation::INITIAL;
    s.services[0].resource.phase = Phase::Running("c".repeat(64).try_into().unwrap());
    assert!(authorized(&s, &port, time).is_err());
    s.services[0].resource.phase = Phase::Running("b".repeat(64).try_into().unwrap());
    s.ports[0].status = Status::Stopping;
    assert!(authorized(&s, &port, time).is_err());
}
#[test]
fn local_targets_require_exact_literal_operator_approval_and_no_dns_bypass() {
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port: NonZeroU16::new(8080).unwrap(),
    };
    let local = "127.0.0.1:8080".parse().unwrap();
    assert!(validate_destination(&endpoint, &[local], false).is_err());
    assert_eq!(
        validate_destination(&endpoint, &[local], true).unwrap(),
        local
    );
    for host in ["localhost", "2130706433", "example.com"] {
        let e = Endpoint {
            host: host.into(),
            ..endpoint.clone()
        };
        assert!(validate_destination(&e, &[local], true).is_err());
    }
    for target in [
        "169.254.169.254:8080",
        "192.168.1.1:8080",
        "224.0.0.1:8080",
        "127.0.0.2:8080",
        "127.0.0.1:8081",
    ] {
        assert!(validate_destination(&endpoint, &[target.parse().unwrap()], true).is_err());
    }
    let public = Endpoint {
        host: "example.com".into(),
        port: NonZeroU16::new(443).unwrap(),
    };
    assert!(validate_destination(&public, &["8.8.8.8:443".parse().unwrap()], false).is_ok());
    assert!(
        validate_destination(
            &public,
            &[
                "8.8.8.8:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap()
            ],
            false
        )
        .is_err()
    );
    for host in ["example.com/path", "user@host", "host?x", "[::1]"] {
        assert!(
            Endpoint {
                host: host.into(),
                ..endpoint.clone()
            }
            .url()
            .is_err()
        );
    }
}
fn peer_fixture() -> (State, String, NodeId, String) {
    let (mut state, run, _) = fixture();
    let mut client = serde_json::to_value(&state.services[0]).unwrap();
    client["resource"]["id"] = json!("svc-b");
    client["resource"]["phase"] = json!(Phase::Running(
        ContainerId::try_from("c".repeat(64)).unwrap()
    ));
    state.services.push(serde_json::from_value(client).unwrap());
    let node: NodeId = "node-a".to_owned().try_into().unwrap();
    let ingress = admit(
        &mut state,
        &run,
        "attempt-a",
        "server",
        input(Direction::Ingress),
        node.clone(),
        |_| Ok(()),
    )
    .unwrap();
    let ingress = ingress["port_id"].as_str().unwrap().to_owned();
    (state, run, node, ingress)
}
fn peer_input(peer: &str) -> Input {
    Input {
        service_id: "svc-b".to_owned().try_into().unwrap(),
        name: "game-route".to_owned().try_into().unwrap(),
        direction: Direction::Egress,
        service_port: NonZeroU16::new(25565).unwrap(),
        target: None,
        peer_port_id: Some(peer.to_owned().try_into().unwrap()),
    }
}
#[test]
fn peer_routes_admit_only_owned_ingress_of_another_service() {
    let (mut state, run, node, ingress) = peer_fixture();
    let egress = admit(
        &mut state,
        &run,
        "attempt-a",
        "client",
        Input {
            name: "download".to_owned().try_into().unwrap(),
            ..input(Direction::Egress)
        },
        node.clone(),
        |_| Ok(()),
    )
    .unwrap();
    let egress = egress["port_id"].as_str().unwrap().to_owned();
    let both = Input {
        target: input(Direction::Egress).target,
        ..peer_input(&ingress)
    };
    let same_service = Input {
        service_id: "svc-a".to_owned().try_into().unwrap(),
        service_port: NonZeroU16::new(31002).unwrap(),
        ..peer_input(&ingress)
    };
    let ingress_with_peer = Input {
        direction: Direction::Ingress,
        ..peer_input(&ingress)
    };
    for (nonce, request) in [
        ("both", both),
        ("self", same_service),
        ("ingress", ingress_with_peer),
        ("egress-peer", peer_input(&egress)),
        ("missing", peer_input("port-missing")),
    ] {
        assert!(
            admit(
                &mut state,
                &run,
                "attempt-a",
                nonce,
                request,
                node.clone(),
                |_| Ok(())
            )
            .is_err(),
            "{nonce}"
        );
    }
    let foreign = crate::chat::create(&mut state);
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .chat = foreign.clone();
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "foreign",
            peer_input(&ingress),
            node.clone(),
            |_| Ok(())
        )
        .is_err()
    );
    let owner = state.services[0].resource.owner.chat.as_str().to_owned();
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .chat = owner;
    let value = admit(
        &mut state,
        &run,
        "attempt-a",
        "route",
        peer_input(&ingress),
        node.clone(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(value["peer_port_id"], ingress.as_str());
    assert!(value["target"].is_null());
    let changed = Input {
        peer_port_id: Some(egress.clone().try_into().unwrap()),
        ..peer_input(&ingress)
    };
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "route",
            changed,
            node.clone(),
            |_| Ok(())
        )
        .is_err()
    );
    let route = route(&state, state.ports.last().unwrap());
    assert!(
        route.contains("peer ingress echo (service svc-a :31000)"),
        "{route}"
    );
    let peer = state
        .ports
        .iter()
        .position(|p| p.id.as_str() == ingress)
        .unwrap();
    state.ports[peer].status = Status::Stopped;
    assert_eq!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "route",
            peer_input(&ingress),
            node.clone(),
            |_| Ok(())
        )
        .unwrap()["port_id"],
        value["port_id"]
    );
    state.ports[peer].status = Status::Pending;
    state.ports[peer].actor = Actor::Unknown;
    let other = Input {
        name: "other-route".to_owned().try_into().unwrap(),
        service_port: NonZeroU16::new(25566).unwrap(),
        ..peer_input(&ingress)
    };
    assert!(
        admit(&mut state, &run, "attempt-a", "actor", other, node, |_| Ok(
            ()
        ))
        .is_err()
    );
}
#[test]
fn peer_route_connects_only_while_the_ingress_listens() {
    let (mut state, run, node, ingress) = peer_fixture();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "route",
        peer_input(&ingress),
        node,
        |_| Ok(()),
    )
    .unwrap();
    let egress = state.ports.last().unwrap().clone();
    assert!(!peer_listening(&state, &egress));
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .ready = true;
    assert!(peer_listening(&state, &egress));
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(!peer_listening(&restored, &egress));
    assert!(peer_listening(&state, &state.ports[0]));
}
#[test]
fn delegated_peer_routes_capture_exact_authority_across_owner_chats() {
    let (mut state, run, node, ingress) = peer_fixture();
    let child = crate::chat::create(&mut state);
    let parent_grant = state.services[0].grant();
    state.services[1].resource.owner.chat = child.clone().try_into().unwrap();
    state.runtime.runs[0].chat_id = Some(child);
    // A shared actor, guessed peer ID or UI parent is insufficient.
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "route",
            peer_input(&ingress),
            node.clone(),
            |_| Ok(())
        )
        .is_err()
    );
    state.runtime.runs[0].resource_grants = vec![parent_grant];
    let value = admit(
        &mut state,
        &run,
        "attempt-a",
        "route",
        peer_input(&ingress),
        node,
        |_| Ok(()),
    )
    .unwrap();
    let route_id = value["port_id"].as_str().unwrap();
    let time = now();
    approve(&mut state, &ingress, 60, false, None, time, |_| Ok(())).unwrap();
    state.ports[0].approval.as_mut().unwrap().host_port = 40001;
    approve(
        &mut state,
        route_id,
        60,
        false,
        Some("127.0.0.1:40001".parse().unwrap()),
        time,
        |_| Ok(()),
    )
    .unwrap();
    let route = state.ports.last().unwrap().clone();
    assert_ne!(route.chat, state.ports[0].chat);
    assert!(authorized(&state, &route, time).is_ok());
    // The captured route survives model-turn history removal and restart.
    state.runtime.runs.clear();
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(authorized(&restored, &route, time).is_ok());
    state.ports.last_mut().unwrap().peer_grant = None;
    assert!(authorized(&state, &route, time).is_err());
    *state.ports.last_mut().unwrap() = route.clone();
    state.ports[0].actor = Actor::Unknown;
    assert!(authorized(&state, &route, time).is_err());
    state.ports[0].actor = route.actor.clone();
    state.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
    assert!(authorized(&state, &route, time).is_err());
}
#[test]
fn peer_route_follows_the_approved_ingress_and_closes_with_it() {
    let (mut state, run, node, ingress) = peer_fixture();
    let egress = admit(
        &mut state,
        &run,
        "attempt-a",
        "route",
        peer_input(&ingress),
        node,
        |_| Ok(()),
    )
    .unwrap();
    let egress = egress["port_id"].as_str().unwrap().to_owned();
    let time = now();
    let listener: SocketAddr = "127.0.0.1:40001".parse().unwrap();
    assert!(
        approve(
            &mut state,
            &egress,
            60,
            false,
            Some(listener),
            time,
            |_| Ok(())
        )
        .is_err()
    );
    approve(&mut state, &ingress, 60, false, None, time, |_| Ok(())).unwrap();
    assert!(
        approve(
            &mut state,
            &egress,
            60,
            false,
            Some(listener),
            time,
            |_| Ok(())
        )
        .is_err()
    );
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .approval
        .as_mut()
        .unwrap()
        .host_port = 40001;
    for wrong in [
        None,
        Some("127.0.0.1:40002".parse().unwrap()),
        Some("8.8.8.8:40001".parse().unwrap()),
    ] {
        assert!(approve(&mut state, &egress, 60, true, wrong, time, |_| Ok(())).is_err());
    }
    approve(&mut state, &egress, 60, true, Some(listener), time, |_| {
        Ok(())
    })
    .unwrap();
    let port = state
        .ports
        .iter()
        .find(|p| p.id.as_str() == egress)
        .unwrap()
        .clone();
    assert!(!port.approval.as_ref().unwrap().allow_loopback);
    assert!(authorized(&state, &port, time).is_ok());
    assert_eq!(intent(&state, &port, &port.node, time), Intent::Serve);
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(authorized(&restored, &port, time).is_ok());
    let mut legacy = serde_json::to_value(&state).unwrap();
    for port in legacy["ports"].as_array_mut().unwrap() {
        port.as_object_mut().unwrap().remove("peer_grant");
    }
    let legacy: State = serde_json::from_value(legacy).unwrap();
    assert!(
        authorized(&legacy, &port, time).is_ok(),
        "Existing same-chat peer approvals remain valid"
    );
    let peer = |state: &mut State| {
        state
            .ports
            .iter_mut()
            .find(|p| p.id.as_str() == ingress)
            .unwrap()
            .clone()
    };
    let original = peer(&mut state);
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .approval
        .as_mut()
        .unwrap()
        .host_port = 40002;
    assert!(authorized(&state, &port, time).is_err());
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .clone_from(&original);
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .status = Status::Stopping;
    assert!(authorized(&state, &port, time).is_err());
    assert_eq!(intent(&state, &port, &port.node, time), Intent::Stop);
    state
        .ports
        .iter_mut()
        .find(|p| p.id.as_str() == ingress)
        .unwrap()
        .clone_from(&original);
    state.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
    assert!(authorized(&state, &port, time).is_err());
}
#[test]
fn legacy_port_records_without_peer_still_load() {
    let (mut state, run, _) = fixture();
    request(&mut state, &run, Direction::Egress).unwrap();
    let mut value = serde_json::to_value(&state.ports[0]).unwrap();
    assert!(value.get("peer").is_none());
    value.as_object_mut().unwrap().remove("peer");
    let port: Port = serde_json::from_value(value).unwrap();
    assert!(port.peer.is_none());
}
