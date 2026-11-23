//! The exact envelopes Control's transports answered with before they shared
//! this module, including their replies to malformed input.
use super::*;

#[test]
fn versions_negotiate_to_the_clients_own_or_the_latest() {
    for (requested, answered) in [
        (Some("2025-11-25"), "2025-11-25"),
        (Some("2025-03-26"), "2025-03-26"),
        (Some("1999-01-01"), LATEST),
        (None, LATEST),
    ] {
        assert_eq!(negotiate(requested), answered);
    }
    assert!(supported("2025-03-26") && !supported("") && !supported("2024-11-05"));
}

#[test]
fn requests_are_read_without_validation() {
    for (request, expected, notification, expected_id) in [
        (json!({"jsonrpc":"2.0","id":1,"method":"initialize"}), Method::Initialize, false, json!(1)),
        (json!({"jsonrpc":"2.0","id":"a","method":"ping"}), Method::Ping, false, json!("a")),
        (json!({"jsonrpc":"2.0","id":null,"method":"tools/list"}), Method::ToolsList, false, Value::Null),
        (json!({"jsonrpc":"2.0","method":"tools/call"}), Method::ToolsCall, true, Value::Null),
        (json!({"jsonrpc":"2.0","method":"notifications/initialized"}), Method::Other("notifications/initialized"), true, Value::Null),
        (json!({"jsonrpc":"2.0","id":2,"method":7}), Method::Other(""), false, json!(2)),
        (json!({"jsonrpc":"2.0","id":3}), Method::Other(""), false, json!(3)),
        (json!([{"jsonrpc":"2.0","id":4,"method":"ping"}]), Method::Other(""), true, Value::Null),
        (json!("ping"), Method::Other(""), true, Value::Null),
    ] {
        assert_eq!((method(&request), is_notification(&request), id(&request)), (expected, notification, &expected_id), "{request}");
    }
    assert_eq!(parse(br#"{"id":1}"#), Some(json!({"id":1})));
    assert_eq!(parse(b"{"), None);
    assert!(is_v2(&json!({"jsonrpc":"2.0"})) && !is_v2(&json!({"jsonrpc":"1.0"})) && !is_v2(&json!({"jsonrpc":2.0})) && !is_v2(&json!([])));
}

#[test]
fn calls_name_their_tool_or_the_empty_name() {
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.chats.list","arguments":{"limit":2}}});
    let call = call(&request).unwrap();
    assert_eq!((call.name, call.arguments), ("control.chats.list", &json!({"limit":2})));
    let unnamed = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":7}});
    let unnamed = super::call(&unnamed).unwrap();
    assert_eq!((unnamed.name, unnamed.arguments), ("", &Value::Null));
    assert!(super::call(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"name":"x"}})).is_none());
}

#[test]
fn replies_and_results_keep_their_shapes() {
    let id = json!(9);
    for (actual, expected) in [
        (reply(&id, json!({})), json!({"jsonrpc":"2.0","id":9,"result":{}})),
        (reply(&Value::Null, json!({})), json!({"jsonrpc":"2.0","id":null,"result":{}})),
        (error(&id, PARSE_ERROR, "Invalid JSON."), json!({"jsonrpc":"2.0","id":9,"error":{"code":-32700,"message":"Invalid JSON."}})),
        (error(&id, INVALID_REQUEST, "Invalid request."), json!({"jsonrpc":"2.0","id":9,"error":{"code":-32600,"message":"Invalid request."}})),
        (initialize(LATEST, "control-room", "0.1"), json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-room","version":"0.1"}})),
        (tools(vec![json!({"name":"a"})]), json!({"tools":[{"name":"a"}]})),
        (tools(Vec::new()), json!({"tools":[]})),
        (text("done"), json!({"content":[{"type":"text","text":"done"}],"isError":false})),
        (data(&json!({"n":1})), json!({"content":[{"type":"text","text":"{\"n\":1}"}],"structuredContent":{"data":{"n":1}},"isError":false})),
        (data(&json!("s")), json!({"content":[{"type":"text","text":"\"s\""}],"structuredContent":{"data":"s"},"isError":false})),
        (failure("No."), json!({"content":[{"type":"text","text":"No."}],"isError":true})),
        (tool_error(&id, "No."), json!({"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"No."}],"isError":true}})),
    ] {
        assert_eq!(actual, expected);
    }
    // The wire text keeps serde_json's key order.
    assert_eq!(error(&Value::Null, PARSE_ERROR, "Invalid JSON.").to_string(), r#"{"error":{"code":-32700,"message":"Invalid JSON."},"id":null,"jsonrpc":"2.0"}"#);
}

#[test]
fn worker_faults_and_refusals() {
    for (request, fault) in [
        (json!({"jsonrpc":"1.0","id":1,"method":"ping"}), Some(json!({"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}}))),
        (json!({"id":2,"method":"ping"}), Some(json!({"jsonrpc":"2.0","id":2,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}}))),
        (json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{}}), Some(json!({"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"tools/call requires params.name"}}))),
        (json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":4}}), Some(json!({"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"tools/call requires params.name"}}))),
        (json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"x"}}), None),
        (json!({"jsonrpc":"2.0","id":6,"method":"resources/list"}), None),
    ] {
        assert_eq!(request_fault(&request), fault, "{request}");
    }
    assert_eq!(refusal(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"x"}}), "Run is not active"),
        json!({"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":"Run is not active"}],"isError":true}}));
    assert_eq!(refusal(&json!({"jsonrpc":"2.0","id":8,"method":"initialize"}), "Run is not active"),
        json!({"jsonrpc":"2.0","id":8,"error":{"code":-32602,"message":"Run is not active"}}));
}

#[test]
fn batches_answer_each_element_and_drop_notifications() {
    let requests = [json!({"jsonrpc":"2.0","id":1,"method":"ping"}), json!({"jsonrpc":"2.0","method":"notifications/progress"}), json!(42), json!({}),
        json!({"jsonrpc":"1.0","id":2,"method":"ping"}), json!({"jsonrpc":"2.0","id":3})];
    let mut seen = Vec::new();
    let replies = batch(&requests, |index, request| {
        seen.push(index);
        if is_notification(request) { Value::Null } else { reply(id(request), json!({})) }
    });
    let invalid = error(&Value::Null, INVALID_REQUEST, "Invalid JSON-RPC batch element");
    assert_eq!(seen, [0, 1]);
    assert_eq!(replies, json!([reply(&json!(1), json!({})), invalid, invalid, invalid, invalid]));
    assert_eq!(batch(&[], |_, _| Value::Null), json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Empty JSON-RPC batch"}}));
    assert_eq!(batch(&[json!({"jsonrpc":"2.0","method":"notifications/initialized"})], |_, _| Value::Null), Value::Null);
}

/// A fake audience that records what it was asked.
struct Fake(Vec<String>);
impl Tools for Fake {
    fn list(&mut self) -> Vec<Value> {
        self.0.push("list".into());
        vec![json!({"name":"echo"})]
    }
    fn call(&mut self, call: Call<'_>) -> Value {
        self.0.push(format!("call {}", call.name));
        if call.name == "echo" { data(call.arguments) } else { failure("Unknown tool.") }
    }
}

/// The three servers that share `serve`, with the wording each used before.
const OPERATOR: Server = Server { name: "control", version: "1", instructions: None, invalid: (INVALID_REQUEST, "Invalid request."), unknown: (METHOD_NOT_FOUND, "Method not found.") };
const CHAT: Server = Server { name: "control-chat", version: "1", instructions: Some("Drive chats."), invalid: (INVALID_REQUEST, "Invalid JSON-RPC request"), unknown: (METHOD_NOT_FOUND, "Method not found") };
const BRIDGE: Server = Server { name: "control-local-tools", version: "1", instructions: None, invalid: (METHOD_NOT_FOUND, "Invalid JSON-RPC request"), unknown: (METHOD_NOT_FOUND, "Method not found") };

#[test]
fn servers_answer_every_method_in_their_own_words() {
    let init = |name: &str, version: &str| json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":name,"version":"1"}});
    let mut chat_init = init("control-chat", "2025-03-26");
    chat_init["instructions"] = json!("Drive chats.");
    for (server, request, expected, asked) in [
        (&OPERATOR, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}), reply(&json!(1), init("control", "2025-03-26")), None),
        (&OPERATOR, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2000-01-01"}}), reply(&json!(1), init("control", LATEST)), None),
        (&CHAT, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}), reply(&json!(1), chat_init.clone()), None),
        (&BRIDGE, json!({"jsonrpc":"2.0","id":1,"method":"initialize"}), reply(&json!(1), init("control-local-tools", LATEST)), None),
        (&OPERATOR, json!({"jsonrpc":"2.0","id":2,"method":"ping"}), reply(&json!(2), json!({})), None),
        (&CHAT, json!({"jsonrpc":"2.0","method":"ping"}), reply(&Value::Null, json!({})), None),
        (&OPERATOR, json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}), reply(&json!(3), json!({"tools":[{"name":"echo"}]})), Some("list")),
        (&OPERATOR, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"echo","arguments":{"a":1}}}), reply(&json!(4), data(&json!({"a":1}))), Some("call echo")),
        (&CHAT, json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{}}), reply(&json!(5), failure("Unknown tool.")), Some("call ")),
        (&OPERATOR, json!({"jsonrpc":"1.0","id":6,"method":"ping"}), error(&json!(6), -32600, "Invalid request."), None),
        (&OPERATOR, json!([{"jsonrpc":"2.0","id":7,"method":"ping"}]), error(&Value::Null, -32600, "Invalid request."), None),
        (&CHAT, json!({"id":8,"method":"tools/list"}), error(&json!(8), -32600, "Invalid JSON-RPC request"), None),
        (&BRIDGE, json!({"jsonrpc":"1.0","id":9,"method":"tools/call","params":{"name":"echo"}}), error(&json!(9), -32601, "Invalid JSON-RPC request"), None),
        (&OPERATOR, json!({"jsonrpc":"2.0","id":10,"method":"resources/list"}), error(&json!(10), -32601, "Method not found."), None),
        (&CHAT, json!({"jsonrpc":"2.0","id":11}), error(&json!(11), -32601, "Method not found"), None),
        (&BRIDGE, json!({"jsonrpc":"2.0","id":12,"method":"notifications/initialized"}), error(&json!(12), -32601, "Method not found"), None),
    ] {
        let mut fake = Fake(Vec::new());
        assert_eq!(serve(server, &request, &mut fake), expected, "{} {request}", server.name);
        assert_eq!(fake.0, asked.into_iter().map(str::to_owned).collect::<Vec<_>>(), "{request}");
    }
}

#[test]
fn initialize_keeps_a_supported_client_version_and_otherwise_answers_the_latest() {
    assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
    assert_eq!(negotiate(Some("2025-11-25")), LATEST);
    assert_eq!(negotiate(Some("2024-01-01")), LATEST);
    assert_eq!(negotiate(None), LATEST);
    assert!(supported("2025-03-26") && !supported("2024-01-01"));
}
