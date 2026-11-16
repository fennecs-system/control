use control_ai::{Decoder, Event, MAX_RECORD_BYTES, Tokens, Usage, claude::Claude, codex::Codex, json_lines, json_lines_usage};
use serde_json::json;
use std::io::{self, Read};

const ALLOWED: &[&str] = &["control_workspace_run"];
const INIT: &str = "{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"mcp__control__control_workspace_run\"]}\n";
const FINAL: &str = "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":5,\"output_tokens\":2}}\n";

#[test]
fn visible_messages_are_emitted_with_stable_identity_and_without_reasoning_blocks() {
    let records=[
        json!({"type":"item.started","item":{"type":"agent_message","id":"message-1","text":"Checking"}}),
        json!({"type":"item.updated","item":{"type":"agent_message","id":"message-1","text":"Checking the inventory."}}),
        json!({"type":"item.completed","item":{"type":"reasoning","id":"private","text":"not conversation output"}}),
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    let mut events = Vec::new();
    json_lines(records.as_bytes(), Codex::default(), |event| {
        events.push(event)
    })
    .unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0],Event::Activity(a) if a.id=="message-1" && a.kind=="message" && a.text=="Checking")
    );
    assert!(
        matches!(&events[1],Event::Activity(a) if a.id=="message-1" && a.text=="Checking the inventory.")
    );
    let first = json!({"type":"assistant","message":{"id":"claude-1","content":[{"type":"thinking","text":"not conversation output"},{"type":"text","text":"Checking\u{001b} the inventory."}]}});
    let second = json!({"type":"assistant","message":{"id":"claude-2","content":[{"type":"text","text":"Connected."}]}});
    let records = format!("{INIT}{first}\n{second}\n{FINAL}");
    let mut events = Vec::new();
    json_lines(records.as_bytes(), Claude::new(ALLOWED), |event| {
        events.push(event)
    })
    .unwrap();
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0],Event::Activity(a) if a.id=="claude-1" && a.text=="Checking the inventory.")
    );
    assert!(matches!(&events[1],Event::Activity(a) if a.id=="claude-2" && a.text=="Connected."));
}

#[test]
fn claude_tool_calls_become_bounded_activity() {
    let large = "z".repeat(5000);
    let records = [
        json!({"type":"assistant","message":{"id":"claude-1","content":[{"type":"text","text":"Checking."},{"type":"tool_use","id":"toolu_1","name":"mcp__control__control_port_status","input":{}},{"type":"tool_use","id":"toolu_2","name":"mcp__control__control_port_request","input":{}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"{\"ready\":true}\u{001b}"}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","is_error":true,"content":[{"type":"text","text":"Peer ingress"},{"type":"tool_reference","tool_name":"x"},{"type":"text","text":"is not owned"}]}]}}),
        json!({"type":"assistant","message":{"id":"claude-2","content":[{"type":"tool_use","id":"toolu_3","name":"mcp__control__control_service_exec_status","input":{}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_3","content":large}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"unknown","content":"ignored"}]}}),
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    let mut activity = Vec::new();
    json_lines(format!("{INIT}{records}\n{FINAL}").as_bytes(), Claude::new(ALLOWED), |event| {
        if let Event::Activity(a) = event { activity.push(a) }
    })
    .unwrap();
    let summary: Vec<_> = activity.iter().map(|a| (a.id.as_str(), a.kind.as_str(), a.text.as_str(), a.status.as_str())).collect();
    assert_eq!(summary, [
        ("claude-1", "message", "Checking.", "running"),
        ("toolu_1", "mcp_tool_call", "control_port_status", "in_progress"),
        ("toolu_2", "mcp_tool_call", "control_port_request", "in_progress"),
        ("toolu_1", "mcp_tool_call", "control_port_status", "completed"),
        ("toolu_2", "mcp_tool_call", "control_port_request", "failed"),
        ("toolu_3", "mcp_tool_call", "control_service_exec_status", "in_progress"),
        ("toolu_3", "mcp_tool_call", "control_service_exec_status", "completed"),
    ]);
    assert_eq!(activity[3].output, "{\"ready\":true}");
    assert_eq!(activity[4].output, "Peer ingress\nis not owned");
    assert_eq!(activity[6].output.chars().count(), 1000);
}

/// A deferred tool called through `control_call` is recorded under its own name.
#[test]
fn control_call_activity_names_the_called_tool() {
    let records = [
        json!({"type":"assistant","message":{"id":"claude-1","content":[{"type":"tool_use","id":"toolu_1","name":"mcp__control__control_call","input":{"tool":"control_watch_stop","arguments":{"watch_id":"w"}}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"stopped"}]}}),
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    let mut activity = Vec::new();
    json_lines(format!("{INIT}{records}\n{FINAL}").as_bytes(), Claude::new(ALLOWED), |event| {
        if let Event::Activity(a) = event { activity.push((a.text, a.status)) }
    })
    .unwrap();
    assert_eq!(activity, [("control_watch_stop".to_owned(), "in_progress".to_owned()), ("control_watch_stop".to_owned(), "completed".to_owned())]);
    let codex = json!({"type":"item.completed","item":{"type":"mcp_tool_call","id":"item-1","tool":"control_call","arguments":{"tool":"control_watch_stop"},"status":"completed"}});
    let plain = json!({"type":"item.completed","item":{"type":"mcp_tool_call","id":"item-2","tool":"control_call","status":"completed"}});
    assert_eq!(control_ai::codex::activity(&codex).unwrap().text, "control_watch_stop");
    assert_eq!(control_ai::codex::activity(&plain).unwrap().text, "control_call");
}

#[test]
fn adapters_share_events_and_usage_without_application_state() {
    let output = json_lines(
        format!("{INIT}{FINAL}").as_bytes(),
        Claude::new(ALLOWED),
        |_| {},
    )
    .unwrap();
    assert_eq!(output.text.as_deref(), Some("done"));
    let usage = output.tokens.unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.cached_input_tokens,
            usage.output_tokens
        ),
        (45, 30, 2)
    );
    let mut events = Vec::new();
    let records = concat!(
        "{\"type\":\"item.started\",\"item\":{\"id\":\"1\",\"type\":\"command_execution\",\"command\":\"cargo test\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":45,\"cached_input_tokens\":30,\"output_tokens\":2}}"
    );
    let output = json_lines(records.as_bytes(), Codex::default(), |e| events.push(e)).unwrap();
    assert_eq!(output.tokens, Some(usage));
    assert!(matches!(&events[0], Event::Activity(a) if a.text == "cargo test"));
}

fn tokens(input: u64, cached: u64, output: u64) -> Option<Tokens> {
    Some(Tokens { input_tokens: input, cached_input_tokens: cached, output_tokens: output })
}

/// Recorded provider streams for the ways a turn ends. Usage is read from
/// whatever the provider reported, also when the turn failed or was stopped.
#[test]
fn recorded_streams_report_usage_for_every_ending() {
    let codex = |data: &str| json_lines_usage(data.as_bytes(), Codex::default(), |_| {});
    // Stopped by the host right after an accepted handoff: Codex reports
    // usage only in turn.completed, so nothing is known.
    let (output, usage) = codex(include_str!("fixtures/codex-handoff-stopped.jsonl"));
    assert!(output.is_ok());
    assert_eq!(usage, Usage::default(), "unknown, not zero");
    // The same handoff when the turn may end by itself.
    let (_, usage) = codex(include_str!("fixtures/codex-handoff-completed.jsonl"));
    assert_eq!(usage, Usage { tokens: tokens(184_233, 160_128, 2_210), complete: true, model: None });
    let (_, usage) = codex(include_str!("fixtures/codex-turn-failed.jsonl"));
    assert_eq!(usage.tokens, None);

    let claude = |data: &str| json_lines_usage(data.as_bytes(), Claude::new(ALLOWED), |_| {});
    // Stopped mid-turn: the per-call usage seen so far, each call counted once
    // with its latest counts, flagged as a lower bound.
    let (output, usage) = claude(include_str!("fixtures/claude-stopped-mid-turn.jsonl"));
    assert!(output.is_err(), "no final result");
    assert_eq!(usage, Usage {
        tokens: tokens(16_103 + 16_225, 12_000 + 16_100, 61 + 40),
        complete: false,
        model: Some("claude-sonnet-4-5-20250929".into()),
    });
    // An error result still carries the turn's totals.
    let (output, usage) = claude(include_str!("fixtures/claude-error-result.jsonl"));
    assert!(output.unwrap_err().to_string().contains("error_max_turns"));
    assert_eq!(usage, Usage {
        tokens: tokens(500_561, 480_100, 7_300),
        complete: true,
        model: Some("claude-opus-4-1-20250805".into()),
    });
    // A successful result's totals win over the per-call sum.
    let (output, usage) = claude(&format!("{INIT}{FINAL}"));
    assert_eq!((output.unwrap().tokens, usage.tokens, usage.complete), (tokens(45, 30, 2), tokens(45, 30, 2), true));
}

/// A running turn sees its usage grow call by call, once per change, and the
/// last report matches the final usage.
#[test]
fn live_usage_reports_each_change_while_the_stream_runs() {
    let mut seen = Vec::new();
    let (_, usage) = control_ai::json_lines_live(include_str!("fixtures/claude-stopped-mid-turn.jsonl").as_bytes(), Claude::new(ALLOWED), |_| {}, |u| seen.push(u.clone()));
    assert!(seen.len() >= 2, "{seen:?}");
    assert!(seen.windows(2).all(|w| w[0].tokens != w[1].tokens && !w[1].complete));
    assert!(seen[0].tokens.unwrap().input_tokens < seen.last().unwrap().tokens.unwrap().input_tokens);
    assert_eq!(seen.last(), Some(&usage));
    let mut codex = Vec::new();
    let _ = control_ai::json_lines_live(include_str!("fixtures/codex-handoff-completed.jsonl").as_bytes(), Codex::default(), |_| {}, |u| codex.push(u.clone()));
    assert_eq!(codex.len(), 1, "Codex reports only its end-of-turn total");
}

#[test]
fn strict_final_validation_and_tool_rejection_survive_extraction() {
    for data in [
        FINAL.to_owned(),
        format!("{INIT}{FINAL}{FINAL}"),
        format!("{INIT}{}", FINAL.trim_end()),
        format!("{INIT}garbage\n{FINAL}"),
    ] {
        assert!(json_lines(data.as_bytes(), Claude::new(ALLOWED), |_| {}).is_err());
    }
    let mut rejected = false;
    let data = format!(
        "{}{FINAL}",
        INIT.replace("mcp__control__control_workspace_run", "Bash")
    );
    assert!(
        json_lines(data.as_bytes(), Claude::new(ALLOWED), |event| {
            if matches!(event, Event::RejectedTools) {
                rejected = true;
            }
        })
        .is_err()
    );
    assert!(rejected);
    let mut decoder = Claude::new(ALLOWED);
    decoder.decode(&json!({"type":"unknown_future_event"}), &mut |_| {
        panic!("unknown event emitted")
    });
    assert!(decoder.finish().is_err());
}

#[test]
fn oversized_records_are_bounded_and_do_not_desynchronize_next_record() {
    let records = format!("{}\n{INIT}{FINAL}", "x".repeat(MAX_RECORD_BYTES + 1));
    assert!(json_lines(records.as_bytes(), Claude::new(ALLOWED), |_| {}).is_err());
    // Codex intentionally ignores non-JSON diagnostics; framing resumes at newline.
    let output = json_lines(records.as_bytes(), Codex::default(), |_| {}).unwrap();
    assert!(output.tokens.is_none());
}

struct Bytes<'a>(&'a [u8]);
impl Read for Bytes<'_> {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            return Ok(0);
        }
        target[0] = self.0[0];
        self.0 = &self.0[1..];
        Ok(1)
    }
}
#[test]
fn fragmented_utf8_and_bounded_assistant_messages() {
    let text = "🐾".repeat(6000);
    let record = json!({"type":"assistant","message":{"id":"message-1","content":[{"type":"text","text":text},{"type":"text","text":"tail"}]}});
    let records = format!("{INIT}{record}\n{FINAL}");
    let mut events = Vec::new();
    json_lines(Bytes(records.as_bytes()), Claude::new(ALLOWED), |event| {
        events.push(event)
    })
    .unwrap();
    assert!(matches!(&events[0], Event::Activity(a) if a.text.chars().count() == 4000));
}

#[test]
fn io_failure_does_not_become_a_successful_final_result() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::ConnectionReset.into())
        }
    }
    assert_eq!(
        json_lines(Broken, Claude::new(ALLOWED), |_| {})
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionReset
    );
}
