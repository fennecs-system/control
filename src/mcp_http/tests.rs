use super::*;
use serde_json::json;

/// Sends one request through the `/mcp` route: (handled, status line, body).
fn send(shared: &Arc<Mutex<State>>, method: &str, header: &str, body: &[u8]) -> (bool, String, String) {
    let mut response = Vec::new();
    let handled = handle(&mut response, shared, method, "/mcp", header, body);
    let response = String::from_utf8(response).unwrap();
    let status = response.lines().next().unwrap_or("").to_owned();
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body.to_owned()).unwrap_or_default();
    (handled, status, body)
}

/// The operator route's own framing answers before any tool runs.
#[test]
fn operator_mcp_http_framing_is_pinned() {
    let shared = Arc::new(Mutex::new(State::default()));
    let cases: [(&str, &str, &[u8], &str, &str); 7] = [
        ("GET", "", b"", "HTTP/1.1 405 Method Not Allowed", "Use MCP POST requests."),
        ("POST", "MCP-Protocol-Version: 1999-01-01\r\n", b"not json", "HTTP/1.1 400 Bad Request", "Unsupported MCP protocol version."),
        ("POST", "", b"not json", "HTTP/1.1 400 Bad Request", r#"{"error":{"code":-32700,"message":"Invalid JSON."},"id":null,"jsonrpc":"2.0"}"#),
        ("POST", "", br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "HTTP/1.1 202 Accepted", ""),
        ("POST", "", br#"{"jsonrpc":"2.0","method":"tools/call"}"#, "HTTP/1.1 400 Bad Request", "A request ID is required."),
        ("POST", "", br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, "HTTP/1.1 400 Bad Request", "A request ID is required."),
        ("POST", "mcp-protocol-version: 2025-03-26\r\n", br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, "HTTP/1.1 200 OK", r#"{"id":1,"jsonrpc":"2.0","result":{}}"#),
    ];
    for (method, header, body, status, expected) in cases {
        assert_eq!(send(&shared, method, header, body), (true, status.to_owned(), expected.to_owned()), "{method} {header}");
    }
    let mut other = Vec::new();
    assert!(!handle(&mut other, &shared, "POST", "/mcp/x", "", b"{}") && other.is_empty());
}

/// Toolbox calls are refused, not passed on, when they are unacknowledged
/// or name an unsupported version; packages pass such calls to the operator
/// MCP, which declines package tools.
#[test]
fn unlocked_tools_keep_their_own_checks() {
    let shared = Arc::new(Mutex::new(State::default()));
    let invalid = (true, "HTTP/1.1 400 Bad Request".to_owned(), "Invalid MCP request.".to_owned());
    for (header, body) in [
        ("", json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"control.web.search"}})),
        ("", json!({"jsonrpc":"1.0","id":1,"method":"tools/call","params":{"name":"control.web.search"}})),
        ("Mcp-Protocol-Version: 1999-01-01\r\n", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.web.search","arguments":{}}})),
    ] {
        assert_eq!(send(&shared, "POST", header, body.to_string().as_bytes()), invalid, "{body}");
    }
    let (handled, status, body) = send(&shared, "POST", "", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.web.search","arguments":{}}}).to_string().as_bytes());
    assert_eq!((handled, status.as_str()), (true, "HTTP/1.1 200 OK"));
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap(),
        json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"Expected query and optional limit"}],"isError":true}}));
    let (_, status, body) = send(&shared, "POST", "", json!({"jsonrpc":"1.0","id":3,"method":"tools/call","params":{"name":"control.packages.search","arguments":{}}}).to_string().as_bytes());
    assert_eq!((status.as_str(), serde_json::from_str::<Value>(&body).unwrap()),
        ("HTTP/1.1 200 OK", json!({"jsonrpc":"2.0","id":3,"error":{"code":-32600,"message":"Invalid request."}})));
    // A GitHub call by GET is not a tool call; the operator MCP refuses the method.
    let (_, status, _) = send(&shared, "GET", "", json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"control_github_open_pr"}}).to_string().as_bytes());
    assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
}
