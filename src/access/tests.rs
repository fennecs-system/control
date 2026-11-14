
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
    assert!(a.authorized(&format!(
        "Cookie: other=1; control_operator={}",
        "a".repeat(64)
    )));
    assert!(!a.authorized(&format!("Cookie: other={}", "a".repeat(64))));
}

#[test]
fn public_build_probe_reports_the_compiled_binary_without_operator_state() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let access = Access { session: "not-an-operator-credential".into() };
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
