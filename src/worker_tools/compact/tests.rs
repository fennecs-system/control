use super::*;
use serde_json::json;

#[test]
fn unset_fields_and_default_flags_are_omitted_but_outcomes_stay() {
    let mut data = json!({
        "diagnostic": "", "exit_code": null, "output": "", "output_truncated": false, "more": false,
        "events": [], "salience": {}, "nested": {"gap": false, "gave_up": false, "code": 0, "signal": null},
        "applied": false, "queued": false, "active": false, "continuation_enabled": false, "cursor": 0,
        "list": [{"truncated": false, "line": "x"}, null],
    });
    value(&mut data, None);
    assert_eq!(data, json!({
        "nested": {"code": 0}, "applied": false, "queued": false, "active": false,
        "continuation_enabled": false, "cursor": 0, "list": [{"line": "x"}, null],
    }));
}

#[test]
fn top_level_echoes_of_arguments_are_dropped() {
    let mut data = json!({"service_id": "svc-1", "watch_id": "w-1", "status": "completed", "nested": {"service_id": "svc-1"}});
    let arguments = json!({"service_id": "svc-1", "watch_id": "w-other"});
    value(&mut data, arguments.as_object());
    assert_eq!(data, json!({"watch_id": "w-1", "status": "completed", "nested": {"service_id": "svc-1"}}));
}

#[test]
fn reply_rewrites_structured_data_and_text_alike() {
    let data = json!({"execution_id": "abc", "exit_code": null, "output": "", "status": "running"});
    let mut response = json!({"jsonrpc": "2.0", "id": 1, "result": crate::toolbox::result(Ok(data))});
    response["result"]["content"].as_array_mut().unwrap().push(json!({"type": "text", "text": "note"}));
    reply("control_service_exec_status", &json!({"service_id": "svc"}), &mut response);
    let expected = json!({"execution_id": "abc", "status": "running"});
    assert_eq!(response["result"]["structuredContent"]["data"], expected);
    assert_eq!(serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap(), expected);
    assert_eq!(response["result"]["content"][1]["text"], "note", "appended notes survive");
}

#[test]
fn plain_json_text_results_are_compacted_too() {
    let mut response = json!({"result": {"content": [{"type": "text", "text": "{\"posted\":true,\"room\":\"\"}"}], "isError": false}});
    reply("control_room_post", &Value::Null, &mut response);
    assert_eq!(response["result"]["content"][0]["text"], "{\"posted\":true}");
    let mut prose = json!({"result": {"content": [{"type": "text", "text": "not json"}], "isError": false}});
    reply("control_room_post", &Value::Null, &mut prose);
    assert_eq!(prose["result"]["content"][0]["text"], "not json");
}

#[test]
fn errors_and_schema_results_are_never_rewritten() {
    let failure = crate::toolbox::tool_error(&json!(1), "Refused: retry control_x {\"a\":\"\"}");
    let mut response = failure.clone();
    reply("control_service_exec", &Value::Null, &mut response);
    assert_eq!(response, failure);
    let schemas = json!({"tools": [{"inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}]});
    let mut response = json!({"result": crate::toolbox::result(Ok(schemas.clone()))});
    reply(super::super::deferred::SEARCH, &Value::Null, &mut response);
    assert_eq!(response["result"]["structuredContent"]["data"], schemas);
}
