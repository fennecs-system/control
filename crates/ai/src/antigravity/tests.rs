use super::*;
use crate::json_lines_usage;

fn run(decoder: Antigravity, lines: &[&str]) -> (io::Result<Output>, Usage, Vec<Activity>, bool) {
    let mut seen = Vec::new();
    let mut rejected = false;
    let stream = lines.join("\n") + "\n";
    let (output, usage) = json_lines_usage(stream.as_bytes(), decoder, |event| match event {
        Event::Activity(activity) => seen.push(activity),
        Event::RejectedTools => rejected = true,
    });
    (output, usage, seen, rejected)
}

// Verbatim from https://antigravity.google/docs/cli/headless#streaming-json
// (the `tools` array is abbreviated there too).
const INIT: &str = r#"{"event":"init","conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","init":{"cwd":"/home/user/project","tools":["ask_permission","run_command","write_to_file","..."],"permission_mode":"request-review"}}"#;
const USER: &str = r#"{"event":"step_update","step_update":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","step_index":0,"state":"DONE","step_type":"user_input"}}"#;
const RESPONSE: &str = r#"{"event":"step_update","step_update":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"Git rebase destructively rewrites a branch's commit history by systematically detaching its unique commits and sequentially reapplying them onto a new base commit.\n","duration_seconds":6.28,"usage":{"input_tokens":10302,"output_tokens":582,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":10884}}}"#;
const CHECKPOINT: &str = r#"{"event":"step_update","step_update":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","step_index":4,"state":"DONE","step_type":"checkpoint","duration_seconds":0.53,"usage":{"input_tokens":116,"output_tokens":7,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":123}}}"#;
const RESULT: &str = r#"{"event":"result","result":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","status":"SUCCESS","response":"Git rebase destructively rewrites a branch's commit history by systematically detaching its unique commits and sequentially reapplying them onto a new base commit.\n","duration_seconds":6.88,"num_turns":1,"usage":{"input_tokens":10418,"output_tokens":589,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":11007}}}"#;
// Verbatim from https://antigravity.google/docs/cli/headless#tool-calls-in-the-stream
const SHELL: &str = r#"{"event":"step_update","step_update":{"conversation_id":"edb1c8c1-50ba-4f3f-87eb-412d0e9d47c3","step_index":4,"state":"DONE","step_type":"tool","tool_name":"run_command","duration_seconds":0.07,"tool_info":{"name":"run_command","parameters":{"CommandLine":"echo hello_headless_demo"},"output":"hello_headless_demo\r\n"}}}"#;
// Verbatim from https://antigravity.google/docs/cli/headless#handle-exit-codes-and-errors
const UNKNOWN_MODEL: &str = r#"{"conversation_id":"","status":"ERROR","response":"","error":"invalid model selection (--model \"does-not-exist-model\" --effort \"\"): model does-not-exist-model is not recognized as a known model or custom model in settings\nAvailable models:\n  Gemini 3.6 Flash (High)\n  ...","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0}}"#;

#[test]
fn the_documented_stream_gives_the_reply_the_message_and_the_result_totals() {
    let (output, usage, seen, rejected) = run(Antigravity::agent(), &[INIT, USER, RESPONSE, CHECKPOINT, RESULT]);
    let output = output.unwrap();
    assert!(output.text.unwrap().starts_with("Git rebase destructively"));
    assert!(!rejected);
    // Uncached input plus cache reads; reads also shown as cached.
    let total = Tokens { input_tokens: 18531, cached_input_tokens: 8113, output_tokens: 589 };
    assert_eq!(output.tokens, Some(total));
    assert_eq!(usage, Usage { tokens: Some(total), complete: true, model: None });
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].kind.as_str(), seen[0].status.as_str()), ("message", "running"));
}

#[test]
fn streamed_deltas_join_into_one_message_and_steps_sum_until_the_result() {
    // ACTIVE fragments then a DONE, as in the stdin streaming example on
    // https://antigravity.google/docs/cli/headless#read-the-results
    let active = r#"{"event":"step_update","step_update":{"conversation_id":"9ec5","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"apple"}}"#;
    let done = r#"{"event":"step_update","step_update":{"conversation_id":"9ec5","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"\n","duration_seconds":1.169607627,"usage":{"input_tokens":30384,"output_tokens":4,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":30388}}}"#;
    let (output, usage, seen, _) = run(Antigravity::agent(), &[USER, active, done]);
    assert!(output.unwrap_err().to_string().starts_with("Antigravity did not return a final result"));
    // No result: the step sum is a lower bound.
    assert_eq!(usage, Usage { tokens: Some(Tokens { input_tokens: 30384, cached_input_tokens: 0, output_tokens: 4 }), complete: false, model: None });
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].text, "apple\n");
}

#[test]
fn control_tools_are_recorded_by_their_control_name() {
    // Eager MCP tools are declared as mcp_<server>_<tool>, lazy ones go through
    // call_mcp_tool (https://antigravity.google/docs/mcp#tool-loading-lazy-vs-eager);
    // the step shape is the documented tool step's.
    let init = r#"{"event":"init","conversation_id":"c","init":{"cwd":"/workspace","tools":["run_command","call_mcp_tool","mcp_control_control_room_read"],"permission_mode":"always-proceed","model":"gemini-4-neon-medium"}}"#;
    let start = r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"mcp_control_control_call","tool_info":{"name":"mcp_control_control_call","parameters":{"tool":"control_room_read","arguments":{}}}}}"#;
    let end = r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":2,"state":"DONE","step_type":"tool","tool_name":"mcp_control_control_call","duration_seconds":0.4,"tool_info":{"name":"mcp_control_control_call","parameters":{"tool":"control_room_read","arguments":{}},"output":"2 posts"}}}"#;
    let lazy = r#"{"event":"step_update","step_update":{"conversation_id":"c","step_index":3,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"name":"call_mcp_tool","parameters":{"ServerName":"control","ToolName":"control_turn_handoff","Arguments":{}},"error":{"type":"TOOL_ERROR","message":"handoff needs a summary"}}}}"#;
    let (output, usage, seen, rejected) = run(Antigravity::agent(), &[init, start, end, lazy, RESULT]);
    assert!(output.is_ok() && !rejected);
    assert_eq!(usage.model.as_deref(), Some("gemini-4-neon-medium"));
    let shown: Vec<_> = seen.iter().map(|a| (a.id.as_str(), a.text.as_str(), a.output.as_str(), a.status.as_str())).collect();
    assert_eq!(shown, [
        ("antigravity-step-2", "control_room_read", "", "in_progress"),
        ("antigravity-step-2", "control_room_read", "2 posts", "completed"),
        ("antigravity-step-3", "control_turn_handoff", "handoff needs a summary", "failed"),
    ]);
}

#[test]
fn a_tool_outside_controls_stops_the_turn() {
    let (output, _, seen, rejected) = run(Antigravity::agent(), &[INIT, USER, SHELL, RESULT]);
    assert!(rejected && seen.is_empty());
    assert_eq!(output.unwrap_err().to_string(), "Antigravity started with an unexpected tool (run_command) and was stopped before doing work.");
    // Another server's tools, and any tool at all in a text call.
    let other = r#"{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"name":"call_mcp_tool","parameters":{"ServerName":"github","ToolName":"create_issue"}}}}"#;
    let (_, _, _, rejected) = run(Antigravity::agent(), &[other, RESULT]);
    assert!(rejected);
    let eager = r#"{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"tool","tool_name":"mcp_control_control_room_read","tool_info":{"name":"mcp_control_control_room_read","parameters":{}}}}"#;
    let (output, _, _, rejected) = run(Antigravity::text(), &[eager, RESULT]);
    assert!(rejected && output.is_err());
    // A subagent step runs tools Control cannot see.
    let subagent = r#"{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"tool","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"type_name":"browser","role":"r","conversation_id":"x","log_uri":"","workspace_uris":[]}]}}}"#;
    let (output, _, _, rejected) = run(Antigravity::agent(), &[subagent, RESULT]);
    assert!(rejected);
    assert_eq!(output.unwrap_err().to_string(), "Antigravity started with an unexpected tool (subagent) and was stopped before doing work.");
}

#[test]
fn a_refused_control_tool_fails_a_run_that_reports_success() {
    // Approval-needing actions are soft-denied: the run exits 0
    // (https://antigravity.google/docs/cli/headless#permissions-in-headless-mode).
    let refused = r#"{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"tool","tool_name":"mcp_control_control_room_post","tool_info":{"name":"mcp_control_control_room_post","parameters":{},"error":{"type":"PERMISSION_DENIED","message":"tool requires approval"}}}}"#;
    let (output, _, seen, rejected) = run(Antigravity::agent(), &[refused, RESULT]);
    assert!(!rejected);
    assert_eq!(seen[0].status, "failed");
    assert_eq!(output.unwrap_err().to_string(), "Antigravity ended with a refused tool call (control_room_post): its permission rules did not allow a Control tool.");
}

#[test]
fn an_error_result_fails_with_its_first_line_and_the_reported_usage() {
    let line = format!(r#"{{"event":"result","result":{UNKNOWN_MODEL}}}"#);
    let (output, usage, _, _) = run(Antigravity::agent(), &[&line]);
    assert_eq!(output.unwrap_err().to_string(), "Antigravity ended with ERROR: invalid model selection (--model \"does-not-exist-model\" --effort \"\"): model does-not-exist-model is not recognized as a known model or custom model in settings.");
    assert_eq!(usage, Usage { tokens: Some(Tokens::default()), complete: true, model: None });
    // Documented terminal statuses other than SUCCESS are failures too.
    let waiting = r#"{"event":"result","result":{"status":"WAITING","response":"","num_turns":1}}"#;
    let (output, _, _, _) = run(Antigravity::agent(), &[waiting]);
    assert_eq!(output.unwrap_err().to_string(), "Antigravity ended with WAITING.");
}

#[test]
fn malformed_or_unknown_shapes_fail_closed() {
    for bad in [
        "not json",
        r#"{"type":"text"}"#,
        r#"{"event":"step_update","step_update":{"state":"RUNNING","step_type":"agent_response"}}"#,
        r#"{"event":"step_update","step_update":{"state":"DONE"}}"#,
        r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_info":{}}}"#,
    ] {
        let (output, _, _, _) = run(Antigravity::agent(), &[bad, RESULT]);
        assert_eq!(output.unwrap_err().to_string(), "Antigravity returned an invalid event stream", "{bad}");
    }
    // Two results for one prompt are not one turn.
    let (output, _, _, _) = run(Antigravity::agent(), &[RESULT, RESULT]);
    assert!(output.is_err());
    // An unrecognized event is skipped, as the CLI itself skips unknown input events.
    let (output, _, _, _) = run(Antigravity::agent(), &[r#"{"event":"future_thing"}"#, RESULT]);
    assert!(output.is_ok());
}

#[test]
fn endings_the_pinned_cli_printed_in_its_container_fail_with_their_cause() {
    // Recorded from agy 1.5.14 in the Nix image: no login, then an invalid API key.
    let unauthenticated = r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"authentication failed or timed out","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"finished_reason":"error"}}"#;
    let (output, _, _, _) = run(Antigravity::text(), &[unauthenticated]);
    assert_eq!(output.unwrap_err().to_string(), "Antigravity ended with ERROR: authentication failed or timed out.");
    let init = r#"{"event":"init","conversation_id":"fc5f","init":{"model":"gemini-4-neon-medium","cwd":"/workspace","tools":["ask_permission","call_mcp_tool","run_command"],"permission_mode":"always-proceed"}}"#;
    let error_step = r#"{"event":"step_update","step_update":{"conversation_id":"fc5f","step_index":1,"state":"DONE","step_type":"error_message"}}"#;
    let rejected_key = r#"{"event":"result","result":{"conversation_id":"fc5f","status":"ERROR","response":"","error":"agent executor error: generating and executing: Error 400, Message: API key not valid. Please pass a valid API key., Status: INVALID_ARGUMENT","duration_seconds":0,"num_turns":1,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"finished_reason":"error"}}"#;
    let (output, usage, _, rejected) = run(Antigravity::text(), &[init, USER, error_step, rejected_key]);
    assert!(!rejected);
    assert!(output.unwrap_err().to_string().contains("API key not valid"));
    assert_eq!(usage.model.as_deref(), Some("gemini-4-neon-medium"));
}
