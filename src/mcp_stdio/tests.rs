
use super::*;
#[test]
#[ignore = "requires the local pinned Nix browser image and generated HTML fixture; no model calls"]
fn live_browser_capture_returns_an_image_artifact() {
    let root = fs::canonicalize(".").unwrap();
    let result = dispatch(
        &mut None,
        &root,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.browser.capture","arguments":{"input":".control/browser-preview/chat.html","width":390,"height":844}}}),
    );
    assert_eq!(result["result"]["isError"], false, "Browser MCP failed");
    let data = result["result"]["content"][1]["data"].as_str().unwrap();
    let png = STANDARD.decode(data).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 390);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 844);
    let path = result["result"]["structuredContent"]["path"]
        .as_str()
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), png);
    assert_eq!(result["result"]["structuredContent"]["width"], 390);
    assert_eq!(result["result"]["structuredContent"]["height"], 844);
}
#[test]
fn bridge_rejects_state_changes_without_contacting_hive() {
    let result = dispatch(
        &mut None,
        Path::new("."),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.chat.send","arguments":{"id":"anything","message":"/improve change source"}}}),
    );
    assert_eq!(result["result"]["isError"], true);
    assert!(source(Path::new("/private/tmp"), "/etc/hosts").is_err());
}
/// Pins the read-only bridge's envelopes, including malformed input. Its
/// invalid-request reply keeps code -32601.
#[test]
fn bridge_envelopes_are_pinned() {
    let version = env!("CARGO_PKG_VERSION");
    for (request, expected) in [
        (json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"control-local-tools","version":version}}})),
        (json!({"jsonrpc":"2.0","id":2,"method":"initialize"}),
            json!({"jsonrpc":"2.0","id":2,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-local-tools","version":version}}})),
        (json!({"jsonrpc":"2.0","id":3,"method":"ping"}), json!({"jsonrpc":"2.0","id":3,"result":{}})),
        (json!({"jsonrpc":"1.0","id":4,"method":"ping"}), json!({"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"Invalid JSON-RPC request"}})),
        (json!({"jsonrpc":"2.0","id":5,"method":"resources/list"}), json!({"jsonrpc":"2.0","id":5,"error":{"code":-32601,"message":"Method not found"}})),
        (json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"control.nope"}}),
            json!({"jsonrpc":"2.0","id":6,"result":{"content":[{"type":"text","text":"Tool unavailable: this bridge exposes read-only Control access, web search, offline captures, Rust snippets and solvers"}],"isError":true}})),
        (json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"control.web.search","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":"Expected query and optional limit"}],"isError":true}})),
    ] {
        assert_eq!(dispatch(&mut None, Path::new("."), &request), expected, "{request}");
    }
}
