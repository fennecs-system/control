
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
    assert_eq!(request_id(r#"{"id":null}"#).as_deref(), Some("null"));
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

#[test]
fn replies_match_delivery_nonce_including_batches_and_notifications() {
    let nonce = r#""123-4""#;
    assert_eq!(correlated_response(r#"{"nonce":"123-4","response":null}"#, nonce).as_deref(), Some("null"));
    let batch = r#"[{"id":1,"result":{"text":"brackets: } ] and quote: \""}},{"id":"b","result":{}}]"#;
    assert_eq!(correlated_response(&format!("{{\"nonce\":{nonce},\"response\":{batch}}}"), nonce).as_deref(), Some(batch));
    assert!(correlated_response(r#"{"nonce":"123-3","response":{"id":1}}"#, nonce).is_none());
    assert!(correlated_response(r#"{"id":1,"result":{}}"#, nonce).is_none());
    assert_eq!(member(r#"{"result":{"nonce":"nested"},"nonce":"outer"}"#, "\"nonce\"").as_deref(), Some(r#""outer""#));
}

#[test]
fn delivery_errors_answer_each_batch_call_but_not_notifications() {
    let request = r#"[{"id":7,"params":{"text":"} ] \""}},{"method":"notify"},{"id":"b"}]"#;
    assert_eq!(delivery_error(request, "uncertain").unwrap(), format!("[{},{}]", error_reply("7", "uncertain"), error_reply(r#""b""#, "uncertain")));
    assert!(delivery_error(r#"[{"method":"notify"}]"#, "lost").is_none());
    assert_eq!(delivery_error(r#"{"id":null}"#, "lost").unwrap(), error_reply("null", "lost"));
}

#[test]
fn the_largest_versioned_envelope_still_fits_the_host_reader() {
    let request = " ".repeat(MAX_REQUEST);
    let envelope = format!("{{\"version\":1,\"nonce\":\"{}-{}\",\"request\":{request}}}", u128::MAX, u64::MAX);
    assert_eq!(envelope.len(), mailbox_limits::REQUEST_ENVELOPE_BYTES);
}
