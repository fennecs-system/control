use super::*;

/// Network check (no credential): the TLS WebSocket reaches Discord's
/// gateway and receives Hello with a heartbeat interval.
#[test]
#[ignore = "connects to Discord"]
fn the_gateway_says_hello() {
    let (mut socket, _) = tungstenite::connect(GATEWAY).expect("gateway connection");
    let Message::Text(text) = socket.read().expect("hello") else {
        panic!("expected text")
    };
    let hello: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(hello["op"], 10);
    assert!(hello["d"]["heartbeat_interval"].as_u64().is_some());
}
