use super::*;
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
    }
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
}
#[test]
fn a_local_server_receives_one_bounded_text_request() {
    use std::io::{BufRead, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
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
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["model"], "qwen-compactor");
        assert!(body.get("tools").is_none());
        let response = r#"{"choices":[{"finish_reason":"stop","message":{"content":"keep the original constraint"}}],"usage":{"prompt_tokens":20,"completion_tokens":6}}"#;
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
    });
    let mut e = endpoint();
    e.url = format!("http://{address}/v1");
    let output = call(&e, "Summarize evidence", "Original constraint").unwrap();
    assert_eq!(output.tokens.unwrap().output_tokens, 6);
    server.join().unwrap();
}
