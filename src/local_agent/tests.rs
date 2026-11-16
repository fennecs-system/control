use super::*;
use std::collections::VecDeque;

fn limits() -> Limits {
    Limits { context_tokens: 32768, output_tokens: 1024, input_bytes: 28000, output_bytes: 8192 }
}
fn reply(content: Option<&str>, calls: Value, finish: &str) -> Value {
    json!({"choices":[{"finish_reason":finish,"message":{"role":"assistant","content":content,"tool_calls":calls}}],
        "usage":{"prompt_tokens":100,"completion_tokens":10}})
}
fn call(name: &str, arguments: &str) -> Value {
    json!([{"id":format!("id-{name}"),"type":"function","function":{"name":name,"arguments":arguments}}])
}
/// A scripted endpoint plus a recording dispatcher standing in for worker tools.
#[derive(Default)]
struct Mock {
    replies: VecDeque<io::Result<Value>>,
    requests: Vec<Value>,
    calls: Vec<(String, Value, String)>,
    polls: VecDeque<Poll>,
    summaries: Vec<String>,
    records: Vec<(String, String)>,
    tool_text: String,
}
impl Host for Mock {
    fn tools(&mut self) -> io::Result<Vec<Value>> {
        Ok(vec![
            json!({"name":"control_room_post","description":"Post","inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}}),
            json!({"name":"control_turn_handoff","description":"Hand off","inputSchema":{"type":"object"}}),
        ])
    }
    fn call(&mut self, name: &str, arguments: &Value, nonce: &str) -> Value {
        self.calls.push((name.into(), arguments.clone(), nonce.into()));
        if name == "control_turn_handoff" {
            self.polls.push_back(Poll::Finish("handed off".into()));
        }
        json!({"content":[{"type":"text","text":if self.tool_text.is_empty() {"ok".to_owned()} else {self.tool_text.clone()}}],"isError":false})
    }
    fn poll(&mut self) -> Poll {
        self.polls.pop_front().unwrap_or(Poll::Continue)
    }
    fn model(&mut self, body: &Value) -> io::Result<Value> {
        self.requests.push(body.clone());
        self.replies.pop_front().unwrap_or_else(|| Err(io::Error::other("script exhausted")))
    }
    fn summarize(&mut self, evidence: &str) -> io::Result<String> {
        self.summaries.push(evidence.into());
        Ok("earlier: posted status updates; constraint: keep the room informed".into())
    }
    fn record(&mut self, kind: &str, _: &str, text: &str, _: &str, status: &str) {
        self.records.push((kind.into(), format!("{text}:{status}")));
    }
}
fn mock(replies: Vec<Value>) -> Mock {
    Mock { replies: replies.into_iter().map(Ok).collect(), ..Default::default() }
}
fn go(host: &mut Mock) -> Result<Outcome, Failure> {
    run(host, "qwen-local", &limits(), 20000, "Current request: post a status", &Policy::default())
}

#[test]
fn tool_calls_run_through_the_dispatcher_and_text_ends_the_turn() {
    let mut host = mock(vec![
        reply(Some("Posting."), call("control_room_post", r#"{"message":"hello"}"#), "tool_calls"),
        reply(Some("Posted the status."), json!(null), "stop"),
    ]);
    let outcome = go(&mut host).unwrap();
    assert_eq!(outcome.text, "Posted the status.");
    assert_eq!(outcome.steps, 2);
    assert_eq!(outcome.tokens.unwrap().output_tokens, 20);
    assert_eq!(host.calls, vec![("control_room_post".into(), json!({"message":"hello"}), "1-0".into())]);
    // The request carries Control's tools as functions and the tool result.
    let first = &host.requests[0];
    assert_eq!(first["tools"][0]["function"]["name"], "control_room_post");
    assert_eq!(first["tool_choice"], "auto");
    assert_eq!(first["max_tokens"], 1024);
    let second = host.requests[1]["messages"].as_array().unwrap();
    assert_eq!(second[2]["tool_calls"][0]["function"]["name"], "control_room_post");
    assert_eq!(second[3]["role"], "tool");
    assert_eq!(second[3]["tool_call_id"], "id-control_room_post");
    assert!(host.records.iter().any(|(k, t)| k == "mcp_tool_call" && t.ends_with(":completed")));
}
#[test]
fn a_committed_handoff_finishes_the_attempt_without_another_model_call() {
    let mut host = mock(vec![reply(None, call("control_turn_handoff", "{}"), "tool_calls")]);
    let outcome = go(&mut host).unwrap();
    assert_eq!(outcome.text, "handed off");
    assert_eq!(host.requests.len(), 1);
}
#[test]
fn malformed_steps_get_bounded_corrective_feedback_and_never_execute() {
    let mut host = mock(vec![
        reply(None, call("control_room_post", "{not json"), "tool_calls"),
        reply(None, call("control_unknown", "{}"), "tool_calls"),
        reply(Some("cut"), json!(null), "length"),
        reply(Some("Recovered."), json!(null), "stop"),
    ]);
    assert_eq!(go(&mut host).unwrap().text, "Recovered.");
    assert!(host.calls.is_empty(), "malformed calls must not run");
    let last = host.requests[3]["messages"].as_array().unwrap();
    let text = serde_json::to_string(last).unwrap();
    assert!(text.contains("not valid JSON") && text.contains("unknown tool") && text.contains("output limit"), "{text}");

    let mut host = mock((0..5).map(|_| reply(None, call("control_room_post", "[1]"), "tool_calls")).collect());
    let (status, message) = go(&mut host).err().unwrap();
    assert_eq!(status, "failed");
    assert!(message.contains("4 malformed steps"), "{message}");
    assert!(host.calls.is_empty());
    assert_eq!(host.requests.len(), 4);
    // A valid step resets the repair count.
    let mut host = mock(vec![
        reply(None, call("control_room_post", "["), "tool_calls"),
        reply(None, call("control_room_post", r#"{"message":"a"}"#), "tool_calls"),
        reply(None, call("control_room_post", "["), "tool_calls"),
        reply(None, call("control_room_post", "["), "tool_calls"),
        reply(None, call("control_room_post", "["), "tool_calls"),
        reply(Some("done"), json!(null), "stop"),
    ]);
    assert_eq!(go(&mut host).unwrap().text, "done");
}
#[test]
fn empty_and_unusable_replies_fail_visibly_without_fallback() {
    let mut host = mock(vec![json!({"choices":[]})]);
    assert!(go(&mut host).err().unwrap().1.contains("unusable response"));
    let mut host = Mock { replies: VecDeque::from([Err(io::Error::other("Model endpoint unavailable; no fallback was used"))]), ..Default::default() };
    let message = go(&mut host).err().unwrap().1;
    assert!(message.contains("No hosted fallback"), "{message}");
}
#[test]
fn cancellation_and_deadlines_stop_between_steps_and_during_calls() {
    // Cancelled while a model request is in flight: the transport returns, the
    // next poll reports the cancellation and no tool runs.
    let mut host = Mock {
        replies: VecDeque::from([Err(io::Error::new(io::ErrorKind::Interrupted, "Model call cancelled"))]),
        polls: VecDeque::from([Poll::Continue, Poll::Stop(("cancelled", "Cancelled.".into()))]),
        ..Default::default()
    };
    assert_eq!(go(&mut host).err().unwrap().0, "cancelled");
    assert!(host.calls.is_empty());
    // Cancelled after a tool step: no further model request.
    let mut host = mock(vec![reply(None, call("control_room_post", r#"{"message":"x"}"#), "tool_calls"), reply(Some("late"), json!(null), "stop")]);
    host.polls = VecDeque::from([Poll::Continue, Poll::Stop(("cancelled", "Cancelled.".into()))]);
    assert_eq!(go(&mut host).err().unwrap().0, "cancelled");
    assert_eq!(host.requests.len(), 1);
    // The step budget is a hard bound.
    let mut host = mock((0..3).map(|_| reply(None, call("control_room_post", r#"{"message":"x"}"#), "tool_calls")).collect());
    let failure = run(&mut host, "m", &limits(), 20000, "p", &Policy { max_steps: 2, ..Policy::default() }).err().unwrap();
    assert!(failure.1.contains("after 2 model steps"));
}
#[test]
fn oversized_mandatory_input_is_refused_before_any_model_call() {
    let mut host = mock(vec![]);
    let small = Limits { context_tokens: 2048, output_tokens: 512, input_bytes: 1024, output_bytes: 2048 };
    let failure = run(&mut host, "m", &small, 1000, &"x".repeat(6000), &Policy::default()).err().unwrap();
    assert_eq!(failure.0, "blocked");
    assert!(failure.1.contains("does not fit"), "{}", failure.1);
    assert!(host.requests.is_empty());
}
#[test]
fn a_turn_that_outgrows_the_context_is_compacted_on_the_local_compactor_and_then_fits() {
    // Each tool result is large; after a few steps the conversation exceeds the
    // 8k context, so earlier steps are summarized and the next request fits.
    let small = Limits { context_tokens: 8192, output_tokens: 512, input_bytes: 28000, output_bytes: 2048 };
    let mut replies: Vec<Value> = (0..6)
        .map(|i| reply(None, call("control_room_post", &format!(r#"{{"message":"update {i}"}}"#)), "tool_calls"))
        .collect();
    replies.push(reply(Some("All updates posted."), json!(null), "stop"));
    let mut host = mock(replies);
    host.tool_text = "room transcript ".repeat(300);
    let outcome = run(&mut host, "m", &small, 6000, "Current request: post six updates", &Policy::default()).unwrap();
    assert_eq!(outcome.text, "All updates posted.");
    assert!(outcome.compactions >= 1);
    assert!(!host.summaries.is_empty());
    assert!(host.summaries.iter().all(|s| s.len() <= 6000), "compactor input respects its budget");
    for request in &host.requests {
        let used = tokens(size(request["messages"].as_array().unwrap())) + tokens(size(request["tools"].as_array().unwrap()));
        assert!(used <= small.context_tokens - small.output_tokens, "request exceeded the budget: {used}");
    }
    let last = serde_json::to_string(&host.requests.last().unwrap()["messages"]).unwrap();
    assert!(last.contains("Summary of earlier steps"));
    assert!(last.contains("Current request: post six updates"), "the request itself is never compacted");
    // Every kept tool result still follows its call.
    for request in &host.requests {
        let messages = request["messages"].as_array().unwrap();
        for (i, m) in messages.iter().enumerate() {
            if m["role"] == "tool" {
                assert!(messages[..i].iter().rev().find(|p| p["role"] != "tool").is_some_and(|p| p["tool_calls"].is_array()));
            }
        }
    }
    assert!(host.records.iter().any(|(k, _)| k == "compaction"));
}
#[test]
fn context_that_cannot_be_compacted_further_fails_visibly() {
    let small = Limits { context_tokens: 4096, output_tokens: 512, input_bytes: 28000, output_bytes: 2048 };
    let mut host = mock(vec![reply(None, call("control_room_post", r#"{"message":"x"}"#), "tool_calls")]);
    host.tool_text = "y".repeat(11000);
    let failure = run(&mut host, "m", &small, 6000, "p", &Policy::default()).err().unwrap();
    assert!(failure.1.contains("context is exhausted"), "{}", failure.1);
}
#[test]
fn worker_tool_schemas_fit_the_declared_overhead_reserve() {
    for improve in [false, true] {
        let mut definitions = crate::tool_registry::worker_definitions(improve);
        definitions.push(crate::claude::definition());
        let tools = functions(&definitions);
        let estimate = tokens(size(&tools));
        assert!(estimate + 1024 < OVERHEAD_TOKENS, "tool schemas need {estimate} tokens; raise OVERHEAD_TOKENS");
        // Names and parameter schemas are passed through unchanged.
        for (tool, definition) in tools.iter().zip(&definitions) {
            assert_eq!(tool["function"]["name"], definition["name"]);
            assert_eq!(tool["function"]["parameters"], definition["inputSchema"]);
        }
    }
}
