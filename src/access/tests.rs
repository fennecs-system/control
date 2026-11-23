use super::*;
#[test]
fn open_options_preserve_default_browser_and_reject_ambiguous_flags() {
    assert_eq!(open_mode(&[]).unwrap(), OpenMode::Browser);
    assert_eq!(open_mode(&["--link".into()]).unwrap(), OpenMode::Link);
    assert_eq!(open_mode(&["--help".into()]).unwrap(), OpenMode::Help);
    assert!(open_mode(&["--link".into(), "extra".into()]).is_err());
    assert!(open_mode(&["--unknown".into()]).is_err());
}
#[test]
fn operator_access_cannot_be_claimed_by_host_header_or_role() {
    let a = Access {
        session: "a".repeat(64),
    };
    assert!(!a.authorized("Host: 127.0.0.1:8080\r\nX-Role: architect\r\n"));
    assert!(!a.authorized("Cookie: control_operator=wrong"));
    assert!(a.authorized(&format!("Cookie: other=1; control_operator={}", "a".repeat(64))));
    assert!(!a.authorized(&format!("Cookie: other={}", "a".repeat(64))));
}

#[test]
fn public_build_probe_reports_the_compiled_binary_without_operator_state() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let access = Access {
        session: "not-an-operator-credential".into(),
    };
    assert!(access.gate(&mut server, "GET", "/health/build", ""));
    drop(server);
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    let (header, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(header.starts_with("HTTP/1.1 200") && header.contains("application/json"));
    assert!(!response.contains(&access.session));
    let build: control::build_info::BuildInfo = serde_json::from_str(body).unwrap();
    assert_eq!(build, control::build_info::current());
}

fn gate_response(access: &Access, target: &str, header: &str) -> (bool, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let handled = access.gate(&mut server, "GET", target, header);
    drop(server);
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    (handled, response)
}

#[test]
fn used_or_malformed_sign_in_links_never_fall_through_to_a_404() {
    let access = Access {
        session: "b".repeat(64),
    };
    let signed_in = format!("Cookie: control_operator={}", "b".repeat(64));
    for target in [
        format!("/unlock/{}", "0".repeat(64)),
        "/unlock/short".into(),
        "/unlock".into(),
    ] {
        let (handled, response) = gate_response(&access, &target, &signed_in);
        assert!(handled, "{target}");
        assert!(
            response.starts_with("HTTP/1.1 303") && response.to_ascii_lowercase().contains("location: /\r\n"),
            "{target}: {response}"
        );
        assert!(!response.to_ascii_lowercase().contains("set-cookie"), "{target}");
        let (handled, response) = gate_response(&access, &target, "");
        assert!(handled && response.starts_with("HTTP/1.1 401"), "{target}");
        assert!(response.contains("already used or has expired") && !response.contains(&access.session));
    }
    let (handled, response) = gate_response(&access, "/dms", "");
    assert!(handled && response.starts_with("HTTP/1.1 401") && !response.contains("already used"));
}

/// The whole response, so a change to how the gate responds cannot move a byte.
#[test]
fn gate_response_bytes_are_pinned() {
    let access = Access::for_tests(&"b".repeat(64));
    let head = "Cache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\n";
    assert_eq!(
        gate_response(&access, "/health", ""),
        (
            true,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 19\r\n{head}\r\nControl is running."
            )
        )
    );
    let signed_in = format!("Cookie: control_operator={}", "b".repeat(64));
    assert_eq!(
        gate_response(&access, "/unlock", &signed_in),
        (
            true,
            format!(
                "HTTP/1.1 303 See Other\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n{head}location: /\r\n\r\n"
            )
        )
    );
}
