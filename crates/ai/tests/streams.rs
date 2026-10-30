use control_ai::{Decoder, Event, MAX_RECORD_BYTES, claude::Claude, codex::Codex, json_lines};
use serde_json::json;
use std::io::{self, Read};

const ALLOWED: &[&str] = &["control_workspace_run"];
const INIT: &str = "{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"mcp__control__control_workspace_run\"]}\n";
const FINAL: &str = "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":5,\"output_tokens\":2}}\n";

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
    let record = json!({"type":"assistant","message":{"content":[{"text":text},{"text":"tail"}]}});
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
