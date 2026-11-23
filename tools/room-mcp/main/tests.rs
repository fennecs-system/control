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
    assert_eq!(request_id(r#"{"id":12,"method":"x"}"#).as_deref(), Some("12"));
    assert!(request_id(r#"{"method":"notify","params":{"id":3}}"#).is_none());
    assert_eq!(request_id(r#"{"id":null}"#).as_deref(), Some("null"));
}
#[test]
fn undeliverable_requests_get_escaped_errors_and_keep_framing() {
    let reply = error_reply(r#"{"id":7,"method":"initialize"}"#, "7", "lost \"x\"\n\\");
    assert_eq!(
        reply,
        r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32000,"message":"lost \"x\"\u000a\\"}}"#
    );
    // Codex leaves a tools/call pending on a JSON-RPC error; it reads isError results.
    let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control_room_post"}}"#;
    assert_eq!(
        error_reply(call, "2", "lost \"x\""),
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"lost \"x\""}],"isError":true}}"#
    );
    // An oversized call arrives truncated; its method still selects the result form.
    assert!(
        delivery_error(&call[..60], "too large")
            .unwrap()
            .contains(r#""id":2,"result""#)
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
    assert_eq!(
        correlated_response(r#"{"nonce":"123-4","response":null}"#, nonce).as_deref(),
        Some("null")
    );
    let batch = r#"[{"id":1,"result":{"text":"brackets: } ] and quote: \""}},{"id":"b","result":{}}]"#;
    assert_eq!(
        correlated_response(&format!("{{\"nonce\":{nonce},\"response\":{batch}}}"), nonce).as_deref(),
        Some(batch)
    );
    assert!(correlated_response(r#"{"nonce":"123-3","response":{"id":1}}"#, nonce).is_none());
    assert!(correlated_response(r#"{"id":1,"result":{}}"#, nonce).is_none());
    assert_eq!(
        member(r#"{"result":{"nonce":"nested"},"nonce":"outer"}"#, "\"nonce\"").as_deref(),
        Some(r#""outer""#)
    );
}

#[test]
fn delivery_errors_answer_each_batch_call_but_not_notifications() {
    let request = r#"[{"id":7,"params":{"text":"} ] \""}},{"method":"notify"},{"id":"b"}]"#;
    assert_eq!(
        delivery_error(request, "uncertain").unwrap(),
        format!(
            "[{},{}]",
            error_reply("", "7", "uncertain"),
            error_reply("", r#""b""#, "uncertain")
        )
    );
    assert!(delivery_error(r#"[{"method":"notify"}]"#, "lost").is_none());
    assert_eq!(
        delivery_error(r#"{"id":null}"#, "lost").unwrap(),
        error_reply("", "null", "lost")
    );
    let mixed = r#"[{"id":1,"method":"tools/call","params":{"name":"x"}},{"id":2,"method":"tools/list"}]"#;
    let replies = delivery_error(mixed, "lost").unwrap();
    assert!(
        replies.starts_with(r#"[{"jsonrpc":"2.0","id":1,"result":{"content""#),
        "{replies}"
    );
    assert!(
        replies.ends_with(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"lost"}}]"#),
        "{replies}"
    );
}

#[test]
fn the_largest_versioned_envelope_still_fits_the_host_reader() {
    let request = " ".repeat(MAX_REQUEST);
    let envelope = format!(
        "{{\"version\":1,\"nonce\":\"{}-{}\",\"request\":{request}}}",
        u128::MAX,
        u64::MAX
    );
    assert_eq!(envelope.len(), mailbox_limits::REQUEST_ENVELOPE_BYTES);
}

#[test]
fn diagnostics_distinguish_idless_errors_from_notifications_without_payloads() {
    let error =
        r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"private error text","data":{"secret":"not for logs"}}}"#;
    assert_eq!(
        wire_summary(error),
        format!("kind=error bytes={} error_code=-32600", error.len())
    );
    let notification =
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3,"reason":"private reason"}}"#;
    assert_eq!(
        wire_summary(notification),
        format!("kind=notification bytes={} error_code=-", notification.len())
    );
    for (text, kind) in [
        (
            r#"{"id":3,"method":"tools/call","params":{"error":{"code":-1}}}"#,
            "request",
        ),
        (
            r#"{"id":3,"result":{"content":[{"type":"text","text":"private"}]}}"#,
            "response",
        ),
        (r#"[{"id":3,"method":"ping"}]"#, "batch"),
        ("{}", "unknown"),
    ] {
        assert_eq!(
            wire_summary(text),
            format!("kind={kind} bytes={} error_code=-", text.len())
        );
    }
}

#[test]
fn escaped_room_text_remains_one_complete_response_frame() {
    let body = r#"{"id":3,"jsonrpc":"2.0","result":{"content":[{"type":"text","text":"{\"message\":\"first\\nsecond\\r\\nquote: \\\" slash: \\\\ } ] 🦊\"}"}],"isError":false}}"#;
    let envelope = format!("{{\"nonce\":\"7-3\",\"response\":{body}}}");
    let response = correlated_response(&envelope, r#""7-3""#).unwrap();
    assert_eq!(response, body);
    let mut output = Vec::new();
    reply(&mut output, &response).unwrap();
    assert_eq!(output.iter().filter(|&&byte| byte == b'\n').count(), 1);
    assert_eq!(&output[..output.len() - 1], body.as_bytes());
}
