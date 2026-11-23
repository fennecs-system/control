use super::*;
use std::{
    io::{BufRead, Write},
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub(crate) fn endpoint() -> Endpoint {
    Endpoint {
        id: "local-qwen".into(),
        model: "qwen-compactor".into(),
        url: "http://127.0.0.1:8081/v1".into(),
        revision: "weights-sha-and-runtime".into(),
        limits: Limits {
            context_tokens: 32768,
            output_tokens: 1024,
            input_bytes: 28000,
            output_bytes: 8192,
        },
        timeout_seconds: 30,
        cli: None,
        tools: false,
        thinking: Default::default(),
    }
}
/// A one-request mock endpoint. The handler receives the parsed body.
fn serve(
    handler: impl FnOnce(TcpStream, serde_json::Value) + Send + 'static,
) -> (Endpoint, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("POST /v1/chat/completions "));
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                assert!(!key.eq_ignore_ascii_case("authorization"));
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        handler(stream, serde_json::from_slice(&body).unwrap());
    });
    let mut e = endpoint();
    e.url = format!("http://{address}/v1");
    (e, server)
}
fn reply(mut stream: TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}
#[test]
fn endpoints_cannot_smuggle_credentials_or_redirect_to_cloud() {
    let mut e = endpoint();
    assert!(e.validate().is_ok());
    for url in [
        "https://example.com/v1",
        "http://user:secret@localhost/v1",
        "http://localhost/v1?key=secret",
        "http://localhost/v1#x",
        "file:///v1",
        "http://localhost/other",
    ] {
        e.url = url.into();
        assert!(e.validate().is_err(), "{url}");
    }
    let mut local_cli = endpoint();
    local_cli.url.clear();
    local_cli.cli = Some(crate::backend::Backend::Local);
    assert!(local_cli.validate().is_err());
}
#[test]
fn a_local_server_receives_one_bounded_text_request() {
    let (e, server) = serve(|stream, body| {
        assert_eq!(body["model"], "qwen-compactor");
        assert_eq!(body["max_tokens"], 1024);
        assert!(body.get("tools").is_none());
        // Summaries never think, whatever the server's default.
        assert_eq!(
            (
                body["chat_template_kwargs"]["enable_thinking"].clone(),
                body["reasoning_budget_tokens"].clone()
            ),
            (serde_json::json!(false), serde_json::json!(0))
        );
        reply(
            stream,
            "200 OK",
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"keep the original constraint"}}],"usage":{"prompt_tokens":20,"completion_tokens":6}}"#,
        );
    });
    let output = call(&e, "Summarize evidence", "Original constraint").unwrap();
    assert_eq!(output.tokens.unwrap().output_tokens, 6);
    server.join().unwrap();
}
#[test]
fn generation_reports_usage_provenance_and_the_narrowed_output_limit() {
    let (e, server) = serve(|stream, body| {
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["messages"][0]["content"], tool::SYSTEM);
        // Chunked framing, as some servers send it.
        let json = r#"{"model":"qwen3.8-27b-q4_k_m","system_fingerprint":"b11669-620d222","choices":[{"finish_reason":"stop","message":{"content":"ready"}}],"usage":{"prompt_tokens":12,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}"#;
        let (a, b) = json.split_at(40);
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
            a.len(),
            b.len()
        )
        .unwrap();
    });
    let generation = generate(&e, tool::SYSTEM, "Say ready", Some(64), &|| false).unwrap();
    server.join().unwrap();
    let report = tool::report(&e, &generation);
    assert_eq!(report["text"], "ready");
    assert_eq!(report["usage"]["cached_input_tokens"], 4);
    assert_eq!(report["usage_reported"], true);
    assert_eq!(report["limits"]["output_tokens"], 64);
    let provenance = &report["provenance"];
    assert_eq!(provenance["id"], "local-qwen");
    assert_eq!(provenance["revision"], "weights-sha-and-runtime");
    assert_eq!(provenance["adapter"], "private-endpoint");
    assert_eq!(provenance["served_model"], "qwen3.8-27b-q4_k_m");
    assert_eq!(provenance["runtime_fingerprint"], "b11669-620d222");
}
#[test]
fn missing_usage_stays_unknown_rather_than_zero() {
    let (e, server) = serve(|stream, _| {
        reply(
            stream,
            "200 OK",
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"ok"}}]}"#,
        )
    });
    let generation = generate(&e, "s", "i", None, &|| false).unwrap();
    server.join().unwrap();
    let report = tool::report(&e, &generation);
    assert!(report["usage"].is_null());
    assert_eq!(report["usage_reported"], false);
    assert!(report["output_tokens_per_second"].is_null());
}
#[test]
fn oversized_input_is_refused_before_any_connection() {
    let mut e = endpoint();
    // Nothing listens here; a connection attempt would report unavailability instead.
    e.url = "http://127.0.0.1:9/v1".into();
    let error = generate(&e, "s", &"x".repeat(28001), None, &|| false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("context budget"), "{error}");
    assert!(generate(&e, "s", "i", Some(0), &|| false).is_err());
}
#[test]
fn malformed_truncated_and_tool_bearing_replies_fail_visibly() {
    for (status, body, expected) in [
        ("200 OK", "not json", "Malformed model response"),
        (
            "200 OK",
            r#"{"choices":[{"finish_reason":"length","message":{"content":"half"}}]}"#,
            "token limit",
        ),
        (
            "200 OK",
            r#"{"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"1"}]}}]}"#,
            "Tool-bearing",
        ),
        (
            "200 OK",
            r#"{"choices":[{"finish_reason":"stop","message":{"content":""}}]}"#,
            "Missing or oversized",
        ),
        (
            "400 Bad Request",
            r#"{"error":{"code":400,"message":"the request exceeds the available context size\n(32768 tokens)"}}"#,
            "HTTP 400): the request exceeds the available context size (32768 tokens)",
        ),
        ("503 Service Unavailable", "", "HTTP 503"),
    ] {
        let (e, server) = serve(move |stream, _| reply(stream, status, body));
        let error = generate(&e, "s", "i", None, &|| false).unwrap_err().to_string();
        server.join().unwrap();
        assert!(error.contains(expected), "{status} {body}: {error}");
    }
}
#[test]
fn oversized_response_bodies_are_rejected() {
    let (e, server) = serve(|mut stream, _| {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            transport::BODY_BYTES + 1
        );
    });
    let error = generate(&e, "s", "i", None, &|| false).unwrap_err().to_string();
    server.join().unwrap();
    assert!(error.contains("size limit"), "{error}");
}
#[test]
fn cancellation_closes_the_connection_so_the_server_can_stop() {
    let closed = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&closed);
    let (e, server) = serve(move |mut stream, _| {
        // Never reply; wait until the client disconnects.
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut byte = [0u8; 1];
        if matches!(stream.read(&mut byte), Ok(0)) {
            observed.store(true, Ordering::SeqCst);
        }
    });
    let started = Instant::now();
    let error = generate(&e, "s", "i", None, &|| started.elapsed() > Duration::from_millis(300)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(5));
    server.join().unwrap();
    assert!(closed.load(Ordering::SeqCst), "server did not observe the close");
    // A call cancelled before it starts makes no request at all.
    let mut idle = endpoint();
    idle.url = "http://127.0.0.1:9/v1".into();
    assert_eq!(
        generate(&idle, "s", "i", None, &|| true).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}
#[test]
fn a_silent_endpoint_hits_the_declared_timeout() {
    let (mut e, server) = serve(|stream, _| {
        std::thread::sleep(Duration::from_millis(1500));
        drop(stream);
    });
    e.timeout_seconds = 1;
    let error = generate(&e, "s", "i", None, &|| false).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    server.join().unwrap();
}
#[test]
fn the_tool_selects_only_private_endpoints_and_serializes_calls() {
    let mut cli = endpoint();
    cli.id = "hosted-cli".into();
    cli.url.clear();
    cli.cli = Some(crate::backend::Backend::Claude);
    let models = vec![endpoint(), cli.clone()];
    assert_eq!(tool::select(&models, None).unwrap().id, "local-qwen");
    assert!(tool::select(&models, Some("hosted-cli")).is_err());
    let mut second = endpoint();
    second.id = "other".into();
    let error = tool::select(&[endpoint(), second], None).unwrap_err().to_string();
    assert!(error.contains("local-qwen") && error.contains("other"), "{error}");
    assert!(tool::select(&[cli], None).is_err());
    assert!(tool::call(&serde_json::json!({"prompt":"x","tools":[]}), &|| false).is_err());
    assert!(tool::call(&serde_json::json!({"prompt":"  "}), &|| false).is_err());
    // Only this test uses the guarded entry point, so holding the guard is deterministic.
    let _busy = tool::BUSY.lock().unwrap();
    let mut idle = endpoint();
    idle.url = "http://127.0.0.1:9/v1".into();
    let error = tool::run(&idle, "s", "i", None, &|| false).unwrap_err().to_string();
    assert!(error.contains("already running"), "{error}");
}
