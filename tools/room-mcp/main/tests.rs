
use super::*;
#[test]
fn transport_rejects_links_and_oversized_responses() {
    let dir = std::env::temp_dir().join(format!("control-room-transport-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("response"), "{}").unwrap();
    std::os::unix::fs::symlink(dir.join("response"), dir.join("link")).unwrap();
    assert!(response(&dir.join("link")).is_err());
    assert_eq!(response(&dir.join("response")).unwrap(), "{}");
    fs::write(dir.join("large"), vec![b'x'; 524289]).unwrap();
    assert!(response(&dir.join("large")).is_err());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn timeouts_answer_the_exact_request_id() {
    assert_eq!(
        request_id(r#"{"jsonrpc":"2.0","params":{"id":9},"id": "a\"b"}"#).as_deref(),
        Some(r#""a\"b""#)
    );
    assert_eq!(
        request_id(r#"{"id":12,"method":"x"}"#).as_deref(),
        Some("12")
    );
    assert!(request_id(r#"{"method":"notify","params":{"id":3}}"#).is_none());
    assert!(response_id("null").is_none());
}
#[test]
fn undeliverable_requests_get_escaped_errors_and_keep_framing() {
    let reply = error_reply("7", "lost \"x\"\n\\");
    assert_eq!(
        reply,
        r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"lost \"x\"\u000a\\"}}"#
    );
    let mut input = io::Cursor::new(b"tail of a large line\n{\"id\":2}\n".to_vec());
    skip_line(&mut input).unwrap();
    let mut next = String::new();
    input.read_line(&mut next).unwrap();
    assert_eq!(request_id(next.trim()).as_deref(), Some("2"));
}
#[test]
fn traces_name_the_top_level_method_not_nested_ones() {
    let line = r#"{"jsonrpc":"2.0","params":{"method":"inner"},"method":"tools/call","id":3}"#;
    assert_eq!(member(line, "\"method\"").as_deref(), Some("\"tools/call\""));
    assert!(member(r#"{"id":4}"#, "\"method\"").is_none());
}
