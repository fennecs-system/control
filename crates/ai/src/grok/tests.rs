use super::*;
use crate::json_lines_usage;

/// Lines in the shapes Grok Build's `AcpLine` reducer serializes.
fn run(decoder: Grok, lines: &[&str]) -> (io::Result<Output>, Usage, Vec<Activity>, bool) {
    let mut seen = Vec::new();
    let mut rejected = false;
    let stream = lines.join("\n") + "\n";
    let (output, usage) = json_lines_usage(stream.as_bytes(), decoder, |event| match event {
        Event::Activity(activity) => seen.push(activity),
        Event::RejectedTools => rejected = true,
    });
    (output, usage, seen, rejected)
}
const END: &str = r#"{"type":"end","stopReason":"end_turn","sessionId":"s","requestId":"r","usage":{"input_tokens":812,"cache_read_input_tokens":41000,"cache_creation_input_tokens":100,"output_tokens":45,"reasoning_tokens":7,"total_tokens":41957},"num_turns":2,"modelUsage":{"grok-4.7":{"inputTokens":812}}}"#;

#[test]
fn a_turn_reports_tools_the_last_response_and_the_end_totals() {
    let (output, usage, seen, rejected) = run(Grok::agent(), &[
        r#"{"type":"available_commands","tools":["search_tool","use_tool"],"commands":[]}"#,
        r#"{"type":"thought","data":"plan"}"#,
        r#"{"type":"text","data":"Reading "}"#,
        r#"{"type":"text","data":"the room."}"#,
        r#"{"type":"tool_call","toolCallId":"call_1","title":"Use tool","kind":"other","status":"in_progress","toolName":"use_tool","rawInput":{"tool_name":"control__control_call","tool_input":{"tool":"control_room_read"}},"content":[],"locations":[]}"#,
        r#"{"type":"usage","messageId":"resp_1","stopReason":"tool_use","usage":{"input_tokens":400,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"reasoning_tokens":0}}"#,
        r#"{"type":"tool_call_update","toolCallId":"call_1","status":"completed","content":[],"rawOutput":{"posts":2},"locations":[]}"#,
        r#"{"type":"text","data":"Two posts."}"#,
        END,
    ]);
    let output = output.unwrap();
    assert_eq!(output.text.as_deref(), Some("Two posts."));
    assert!(!rejected);
    // Uncached input plus cache reads and writes; reads also shown as cached.
    let total = Tokens { input_tokens: 41912, cached_input_tokens: 41000, output_tokens: 45 };
    assert_eq!(output.tokens, Some(total));
    assert_eq!(usage, Usage { tokens: Some(total), complete: true, model: Some("grok-4.7".into()) });
    let shown: Vec<_> = seen.iter().map(|a| (a.kind.as_str(), a.text.as_str(), a.output.as_str(), a.status.as_str())).collect();
    assert_eq!(shown, [
        ("message", "Reading the room.", "", "running"),
        ("mcp_tool_call", "control_room_read", "", "in_progress"),
        ("mcp_tool_call", "control_room_read", "{\"posts\":2}", "completed"),
        ("message", "Two posts.", "", "running"),
    ]);
}

#[test]
fn a_tool_outside_the_mcp_meta_tools_stops_the_turn() {
    for line in [
        r#"{"type":"available_commands","tools":["use_tool","run_terminal_cmd"],"commands":[]}"#,
        r#"{"type":"tool_call","toolCallId":"c","title":"Shell","kind":"execute","status":"in_progress","toolName":"run_terminal_cmd","rawInput":{},"content":[],"locations":[]}"#,
    ] {
        let (output, _, seen, rejected) = run(Grok::agent(), &[line, END]);
        assert!(rejected && seen.is_empty());
        assert_eq!(output.unwrap_err().to_string(), "Grok started with an unexpected tool (run_terminal_cmd) and was stopped before doing work.");
    }
    // A text call expects no tools at all, the meta-tools included.
    let (output, _, _, rejected) = run(Grok::text(), &[r#"{"type":"available_commands","tools":["use_tool"],"commands":[]}"#, END]);
    assert!(rejected && output.is_err());
    // Namespaced ids and Control's own catalog keys are the expected set.
    let (output, _, _, rejected) = run(Grok::agent(), &[r#"{"type":"available_commands","tools":["grok:search_tool","control__control_turn_handoff"],"commands":[]}"#, r#"{"type":"text","data":"ok"}"#, END]);
    assert!(!rejected && output.unwrap().text.as_deref() == Some("ok"));
}

#[test]
fn errors_and_other_stop_reasons_fail_with_the_usage_reported_so_far() {
    let response = r#"{"type":"usage","usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":5,"cache_creation_input_tokens":0,"reasoning_tokens":0}}"#;
    let (output, usage, _, _) = run(Grok::agent(), &[response, response, r#"{"type":"error","message":"Rate limit exceeded\u0007 for this plan"}"#]);
    assert_eq!(output.unwrap_err().to_string(), "Grok ended with an error: Rate limit exceeded for this plan.");
    // Two responses summed: a lower bound, not the provider's total.
    assert_eq!(usage, Usage { tokens: Some(Tokens { input_tokens: 30, cached_input_tokens: 10, output_tokens: 4 }), complete: false, model: None });

    let (output, _, _, _) = run(Grok::agent(), &[r#"{"type":"text","data":"partial"}"#, r#"{"type":"end","stopReason":"max_tokens","sessionId":"s","requestId":"r"}"#]);
    assert_eq!(output.unwrap_err().to_string(), "Grok ended with max_tokens.");
    let (output, _, _, _) = run(Grok::agent(), &[r#"{"type":"max_turns_reached"}"#]);
    assert_eq!(output.unwrap_err().to_string(), "Grok ended with max_turns_reached.");
}

#[test]
fn a_stream_without_an_end_or_with_malformed_lines_is_not_a_result() {
    let (output, usage, _, _) = run(Grok::agent(), &[r#"{"type":"text","data":"cut off"}"#]);
    assert!(output.unwrap_err().to_string().starts_with("Grok did not return a final result"));
    assert_eq!(usage, Usage::default());
    let (output, _, _, _) = run(Grok::agent(), &["not json", END]);
    assert_eq!(output.unwrap_err().to_string(), "Grok returned an invalid event stream");
    // An end with no text is an empty reply, not a failure: the turn may have handed off.
    let (output, _, seen, _) = run(Grok::agent(), &[END]);
    assert!(output.unwrap().text.as_deref() == Some("") && seen.is_empty());
}
