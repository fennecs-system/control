use super::*;

/// A pending egress request from the fixture's running turn to `files.example:443`.
fn requested() -> (State, String, String, String) {
    let (mut state, run, chat) = super::super::tests::fixture();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "n1",
        Input {
            service_id: "svc-a".to_owned().try_into().unwrap(),
            name: "download".to_owned().try_into().unwrap(),
            direction: Direction::Egress,
            service_port: NonZeroU16::new(3128).unwrap(),
            target: Some(Endpoint {
                host: "files.example".into(),
                port: NonZeroU16::new(443).unwrap(),
            }),
            peer_port_id: None,
        },
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    let id = state.ports[0].id.as_str().to_owned();
    (state, run, chat, id)
}
const DNS: &str = "curl: (6) Could not resolve host: files.example\nexit 6";

#[test]
fn a_pending_request_says_how_to_wait_and_how_it_will_be_used() {
    let (state, _, _, id) = requested();
    let summary = state.ports[0].summary();
    let next = summary["next"].as_str().unwrap();
    assert!(
        next.contains(&format!(
            r#"control_turn_handoff {{"disposition":{{"kind":"wait","topics":["port:{id}"]}}}}"#
        )),
        "{next}"
    );
    assert!(next.contains("Do not poll or retry"), "{next}");
    let usage = summary["usage"].as_str().unwrap();
    assert!(
        usage.contains("curl -fsSLO https://files.example/…")
            && usage.contains("HTTPS_PROXY")
            && usage.contains("127.0.0.1:3128")
            && !usage.contains("--connect-to"),
        "{usage}"
    );
    assert_eq!(summary["ready"], json!(false));
}

#[test]
fn a_dns_failure_for_a_pending_host_says_to_wait_and_only_for_that_host() {
    let (state, run, _, id) = requested();
    let hint = egress_hint(
        &state,
        &run,
        r#"{"command":"curl -fsSLO https://files.example/a.jar"}"#,
        DNS,
    )
    .unwrap();
    assert!(
        hint.starts_with(&format!(
            "Egress to files.example:443 is pending operator approval (port request {id}): hand off"
        )),
        "{hint}"
    );
    assert!(hint.contains(&format!(r#""topics":["port:{id}"]"#)), "{hint}");
    // Another host, a success, or another chat's run get no hint.
    assert!(
        egress_hint(
            &state,
            &run,
            "curl https://other.example/",
            "curl: (6) Could not resolve host: other.example"
        )
        .is_none()
    );
    assert!(egress_hint(&state, &run, "curl https://files.example/", "downloaded 3 MB").is_none());
    let mut state = state;
    let (owner, origin) = (state.ports[0].chat.clone(), state.ports[0].origin_run.clone());
    state.ports[0].chat = "someone-else".into();
    state.ports[0].origin_run = "their-run".into();
    assert!(egress_hint(&state, &run, "curl https://files.example/", DNS).is_none());
    (state.ports[0].chat, state.ports[0].origin_run) = (owner, origin);
    // The host may appear only in the command (connection refused, no name).
    assert!(
        egress_hint(
            &state,
            &run,
            "curl https://files.example/",
            "curl: (7) Connection refused"
        )
        .is_some()
    );
}

#[test]
fn a_failure_for_an_approved_host_names_the_relay_line() {
    let (mut state, run, chat, id) = requested();
    state.ports[0].status = Status::Approved;
    state.ports[0].decision = Some(decisions::Receipt::pending(true));
    let hint = egress_hint(&state, &run, "curl -fsSLO https://files.example/a.jar", DNS).unwrap();
    assert!(
        hint.contains(&format!(
            "approved as port {id}, but this process did not use its proxy"
        )),
        "{hint}"
    );
    assert!(
        hint.contains("HTTPS_PROXY") && hint.contains("Restart a watch"),
        "{hint}"
    );
    // Already using the relay: point at its readiness instead.
    let relayed = egress_hint(
        &state,
        &run,
        "curl --connect-to files.example:443:127.0.0.1:3128 https://files.example/a.jar",
        "curl: (7) Failed to connect to 127.0.0.1 port 3128",
    )
    .unwrap();
    assert!(
        relayed.contains(&format!(r#"control_port_status {{"port_id":"{id}"}}"#)),
        "{relayed}"
    );
    assert!(
        state.ports[0].summary().get("next").is_none(),
        "only a pending request is waited on"
    );
    assert!(
        matches!(wait_topic(&state, &chat, &id), Err((true, message)) if message.contains("already approved") && message.contains("HTTPS_PROXY"))
    );
}

#[test]
fn a_port_wait_resolves_only_for_the_requesting_chat() {
    let (mut state, _, chat, id) = requested();
    assert_eq!(wait_topic(&state, &chat, &id), Ok(()));
    assert!(matches!(wait_topic(&state, &chat, "missing"), Err((false, _))));
    let stranger = crate::chat::create(&mut state);
    assert!(matches!(wait_topic(&state, &stranger, &id), Err((false, _))));
}
