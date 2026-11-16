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
    meters: Vec<Meter>,
    /// Target lengths requested from the compactor, and scripted failures.
    words: Vec<usize>,
    summary_errors: VecDeque<io::Error>,
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
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String> {
        self.summaries.push(evidence.into());
        self.words.push(words);
        if let Some(error) = self.summary_errors.pop_front() {
            return Err(error);
        }
        Ok("earlier: posted status updates; constraint: keep the room informed".into())
    }
    fn record(&mut self, kind: &str, _: &str, text: &str, _: &str, status: &str) {
        self.records.push((kind.into(), format!("{text}:{status}")));
    }
    fn metered(&mut self, meter: &Meter) {
        self.meters.push(*meter);
    }
}
fn mock(replies: Vec<Value>) -> Mock {
    Mock { replies: replies.into_iter().map(Ok).collect(), ..Default::default() }
}
fn compactor(input_bytes: usize) -> Compactor {
    Compactor { input_bytes, output_tokens: 512 }
}
fn go(host: &mut Mock) -> Result<Outcome, Failure> {
    run(host, "qwen-local", &limits(), compactor(20000), "Current request: post a status", &Policy::default())
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
    let failure = run(&mut host, "m", &limits(), compactor(20000), "p", &Policy { max_steps: 2, ..Policy::default() }).err().unwrap();
    assert!(failure.1.contains("after 2 model steps"));
}
#[test]
fn oversized_mandatory_input_is_refused_before_any_model_call() {
    let mut host = mock(vec![]);
    let small = Limits { context_tokens: 2048, output_tokens: 512, input_bytes: 1024, output_bytes: 2048 };
    let failure = run(&mut host, "m", &small, compactor(1000), &"x".repeat(6000), &Policy::default()).err().unwrap();
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
    let outcome = run(&mut host, "m", &small, compactor(6000), "Current request: post six updates", &Policy::default()).unwrap();
    assert_eq!(outcome.text, "All updates posted.");
    assert!(outcome.compactions >= 1);
    assert!(!host.summaries.is_empty());
    assert!(host.summaries.iter().all(|s| s.len() <= 6000), "compactor input respects its budget");
    for request in &host.requests {
        let used = tokens(size(request["messages"].as_array().unwrap())) + tokens(size(request["tools"].as_array().unwrap()));
        assert!(used <= small.context_tokens - small.output_tokens, "request exceeded the budget: {used}");
    }
    let last = serde_json::to_string(&host.requests.last().unwrap()["messages"]).unwrap();
    assert!(last.contains("Summary of steps"));
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
    // A single bulky newest result is shortened as a last resort, not fatal.
    let mut host = mock(vec![
        reply(None, call("control_room_post", r#"{"message":"x"}"#), "tool_calls"),
        reply(Some("Done."), json!(null), "stop"),
    ]);
    host.tool_text = "y".repeat(11000);
    assert_eq!(run(&mut host, "m", &small, compactor(6000), "p", &Policy::default()).unwrap().text, "Done.");
    let last = serde_json::to_string(&host.requests[1]["messages"]).unwrap();
    assert!(last.contains("more bytes of this earlier result"), "{last}");
    // A request that leaves no room even then fails visibly.
    let mut host = mock(vec![reply(None, call("control_room_post", r#"{"message":"x"}"#), "tool_calls")]);
    host.tool_text = "y".repeat(11000);
    let failure = run(&mut host, "m", &small, compactor(6000), &"p".repeat(9300), &Policy::default()).err().unwrap();
    assert!(failure.1.contains("context is exhausted"), "{}", failure.1);
}

/// A long setup turn: every step runs a long script whose result echoes it,
/// followed by bulky output. Returns the input tokens of each request.
fn setup_turn(limits: &Limits, policy: &Policy, steps: usize) -> (Mock, Result<Outcome, Failure>, Vec<u32>) {
    let script = "apt-get install -y build-essential pkg-config libssl-dev\n".repeat(20);
    let arguments = json!({"message":script}).to_string();
    let mut replies: Vec<Value> = (0..steps).map(|_| reply(None, call("control_room_post", &arguments), "tool_calls")).collect();
    replies.push(reply(Some("Setup finished."), json!(null), "stop"));
    let mut host = mock(replies);
    host.tool_text = format!("$ {script}\n{}", "Setting up package 1.2.3 (amd64) ...\n".repeat(150));
    let outcome = run(&mut host, "m", limits, compactor(6000), "Current request: set up the build host", policy);
    let used = host.requests.iter()
        .map(|r| tokens(size(r["messages"].as_array().unwrap())) + tokens(size(r["tools"].as_array().unwrap())))
        .collect();
    (host, outcome, used)
}
#[test]
fn a_lean_transcript_grows_slowly_and_carries_nothing_twice() {
    // A large context, so neither run compacts: only the rolling window differs.
    let wide = Limits { context_tokens: 131072, output_tokens: 1024, input_bytes: 28000, output_bytes: 8192 };
    let (_, _, full) = setup_turn(&wide, &Policy { verbatim_steps: usize::MAX, ..Policy::default() }, 12);
    let (host, outcome, lean) = setup_turn(&wide, &Policy::default(), 12);
    assert_eq!(outcome.unwrap().compactions, 0);
    let growth = |used: &[u32]| used.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>();
    println!("input tokens per request, whole transcript: {full:?}\n  growth per step: {:?}", growth(&full));
    println!("input tokens per request, rolling window:   {lean:?}\n  growth per step: {:?}", growth(&lean));
    let (full_step, lean_step) = (growth(&full)[10], growth(&lean)[10]);
    assert!(lean_step * 5 < full_step, "a step past the window adds {lean_step} tokens, against {full_step} unwindowed");
    assert!(lean.last().unwrap() * 2 < *full.last().unwrap());
    // The newest results are whole; their echoed script is cited, not repeated.
    let last = host.requests.last().unwrap()["messages"].as_array().unwrap();
    let results = last.iter().filter(|m| m["role"] == "tool").collect::<Vec<_>>();
    assert!(results.iter().all(|m| !m["content"].as_str().unwrap().contains("apt-get install")));
    assert!(results.iter().rev().take(4).all(|m| m["content"].as_str().unwrap().contains("[the call's message argument")
        && !m["content"].as_str().unwrap().contains("more bytes of this earlier result")));
    assert!(results.iter().rev().skip(4).all(|m| m["content"].as_str().unwrap().contains("more bytes of this earlier result")));
    // Older calls keep a short head of their script.
    let calls = last.iter().filter(|m| m["tool_calls"].is_array()).collect::<Vec<_>>();
    assert!(calls[0]["tool_calls"][0]["function"]["arguments"].as_str().unwrap().contains("bytes elided"));
}
#[test]
fn a_long_turn_compacts_before_its_context_is_exhausted_and_is_told_once() {
    let small = Limits { context_tokens: 8192, output_tokens: 512, input_bytes: 28000, output_bytes: 2048 };
    let budget = small.context_tokens - small.output_tokens;
    let (host, outcome, used) = setup_turn(&small, &Policy::default(), 16);
    let outcome = outcome.unwrap();
    assert_eq!(outcome.text, "Setup finished.");
    assert!(outcome.compactions >= 1);
    println!("input tokens per request (budget {budget}): {used:?}");
    // Every request stays under the compaction threshold plus one step.
    let peak = *used.iter().max().unwrap();
    assert!(peak < budget, "peak {peak} of {budget}");
    // The summary was asked for under an explicit length.
    assert!(host.words.iter().all(|&w| w == crate::compaction::target_words(512)));
    // The context note was added once, to one tool result, naming the handoff.
    let notes = host.records.iter().filter(|(k, _)| k == "note").collect::<Vec<_>>();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].1.starts_with("Context ") && notes[0].1.contains("% full: publish your checkpoint (control_turn_handoff)"));
    let last = serde_json::to_string(&host.requests.last().unwrap()["messages"]).unwrap();
    assert!(last.matches("% full: publish your checkpoint").count() <= 1);
    assert!(host.requests.iter().any(|r| r.to_string().contains("% full: publish your checkpoint")));
}
#[test]
fn a_truncated_summary_is_retried_shorter_then_replaced_by_a_digest() {
    let small = Limits { context_tokens: 8192, output_tokens: 512, input_bytes: 28000, output_bytes: 2048 };
    let truncated = || io::Error::other("Output reached the declared token limit; partial text was discarded");
    let target = crate::compaction::target_words(512);
    // One truncated summary: retried with a tighter target, which succeeds.
    let mut host = mock(vec![]);
    host.summary_errors = VecDeque::from([truncated()]);
    let script = "x".repeat(3000);
    let mut replies: Vec<Value> = (0..12).map(|i| reply(None, call("control_room_post", &json!({"message":format!("{i}{script}")}).to_string()), "tool_calls")).collect();
    replies.push(reply(Some("Done."), json!(null), "stop"));
    host.replies = replies.into_iter().map(Ok).collect();
    host.tool_text = "output line\n".repeat(800);
    let outcome = run(&mut host, "m", &small, compactor(6000), "set up", &Policy::default()).unwrap();
    assert!(outcome.compactions >= 1);
    assert_eq!(&host.words[..2], &[target, target / 3]);
    assert!(host.records.iter().any(|(k, t)| k == "compaction" && t.contains("retried with a shorter summary target")));
    // Every summary fails: the turn continues on a labelled deterministic digest.
    let script = "y".repeat(3000);
    let mut replies: Vec<Value> = (0..12).map(|i| reply(None, call("control_room_post", &json!({"message":format!("{i}{script}")}).to_string()), "tool_calls")).collect();
    replies.push(reply(Some("Done."), json!(null), "stop"));
    let mut host = mock(replies);
    host.summary_errors = (0..64).map(|_| truncated()).collect();
    host.tool_text = "output line\n".repeat(800);
    let outcome = run(&mut host, "m", &small, compactor(6000), "set up", &Policy::default()).unwrap();
    assert_eq!(outcome.text, "Done.");
    assert!(outcome.compactions >= 1);
    let last = serde_json::to_string(&host.requests.last().unwrap()["messages"]).unwrap();
    assert!(last.contains("Digest of steps"), "{last}");
    assert!(last.contains("called control_room_post"));
    assert!(host.records.iter().any(|(k, t)| k == "compaction" && t.contains("deterministic digest") && t.contains("declared token limit")));
    // Cancellation during a summary is not papered over with a digest.
    let mut host = mock((0..12).map(|i| reply(None, call("control_room_post", &json!({"message":format!("{i}{script}")}).to_string()), "tool_calls")).collect());
    host.tool_text = "output line\n".repeat(800);
    host.summary_errors = VecDeque::from([io::Error::new(io::ErrorKind::Interrupted, "Model call cancelled")]);
    let _ = run(&mut host, "m", &small, compactor(6000), "set up", &Policy::default());
    assert!(!host.words.is_empty());
    assert!(!host.records.iter().any(|(k, t)| k == "compaction" && t.contains("Model call cancelled")));
}
#[test]
fn worker_tool_schemas_fit_the_declared_overhead_reserve() {
    use crate::worker_tools::deferred::{listed, offered, Facts};
    use control_context::Audience;
    for role in Audience::ALL {
        for improve in [false, true] {
            let facts = Facts { role, improve, managed: true, judge: role == Audience::Advisor, advisor: role == Audience::Advisor,
                shell: true, local: true, rooms: true };
            let definitions = listed(&facts);
            let tools = functions(&definitions);
            let estimate = tokens(size(&tools));
            let all = tokens(size(&functions(&offered(&facts))));
            println!("{role:?} improve={improve}: core {estimate} tokens, every callable tool {all} tokens");
            // The reserve covers the role core, schemas loaded during the turn and one exchange.
            assert!(estimate <= CORE_TOKENS, "core schemas need {estimate} tokens; raise CORE_TOKENS");
            // Names and parameter schemas are passed through unchanged.
            for (tool, definition) in tools.iter().zip(&definitions) {
                assert_eq!(tool["function"]["name"], definition["name"]);
                assert_eq!(tool["function"]["parameters"], definition["inputSchema"]);
            }
        }
    }
}
/// Deferred tools load as native functions for the rest of the turn: through
/// the search the model may call, or on first use of a name from the index.
#[test]
fn deferred_tools_load_natively_on_search_or_first_use() {
    struct Deferred(Mock);
    impl Host for Deferred {
        fn tools(&mut self) -> io::Result<Vec<Value>> {
            let mut tools = self.0.tools()?;
            tools.push(json!({"name":SEARCH,"description":"Search","inputSchema":{"type":"object"}}));
            Ok(tools)
        }
        fn call(&mut self, name: &str, arguments: &Value, nonce: &str) -> Value {
            if name != SEARCH {
                return self.0.call(name, arguments, nonce);
            }
            self.0.calls.push((name.into(), arguments.clone(), nonce.into()));
            let known = ["control_watch_stop", "control_port_status"];
            let tools = arguments["names"].as_array().unwrap().iter().filter_map(Value::as_str).filter(|n| known.contains(n))
                .map(|n| json!({"name":n,"description":format!("{n} schema"),"inputSchema":{"type":"object","properties":{"id":{"type":"string"}}}}))
                .collect::<Vec<_>>();
            crate::toolbox::result(Ok(json!({"tools":tools})))
        }
        fn poll(&mut self) -> Poll { self.0.poll() }
        fn model(&mut self, body: &Value) -> io::Result<Value> { self.0.model(body) }
        fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String> { self.0.summarize(evidence, words) }
        fn record(&mut self, kind: &str, id: &str, text: &str, output: &str, status: &str) { self.0.record(kind, id, text, output, status) }
        fn metered(&mut self, meter: &Meter) { self.0.metered(meter) }
    }
    let mut host = Deferred(mock(vec![
        reply(None, call("control_watch_stop", r#"{"id":"w1"}"#), "tool_calls"),
        reply(None, call(SEARCH, r#"{"names":["control_port_status"]}"#), "tool_calls"),
        reply(None, call("control_nope", "{}"), "tool_calls"),
        reply(Some("Stopped."), json!(null), "stop"),
    ]));
    let outcome = run(&mut host, "qwen-local", &limits(), compactor(20000), "Stop the watch", &Policy::default()).unwrap();
    assert_eq!(outcome.text, "Stopped.");
    let calls = host.0.calls.iter().map(|(n, a, nonce)| format!("{n} {a} {nonce}")).collect::<Vec<_>>();
    assert_eq!(calls, [
        r#"control_tool_search {"names":["control_watch_stop"]} 1-0-1"#,
        r#"control_watch_stop {"id":"w1"} 1-0"#,
        r#"control_tool_search {"names":["control_port_status"]} 2-0"#,
        r#"control_tool_search {"names":["control_nope"]} 3-0-1"#,
    ], "an unknown name loads nothing and never runs");
    let offered = |step: usize| host.0.requests[step]["tools"].as_array().unwrap().iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert!(!offered(0).contains(&"control_watch_stop".to_owned()));
    assert!(offered(1).contains(&"control_watch_stop".to_owned()) && !offered(1).contains(&"control_port_status".to_owned()));
    assert!(offered(2).contains(&"control_port_status".to_owned()));
    let last = serde_json::to_string(&host.0.requests[3]["messages"]).unwrap();
    assert!(last.contains("unknown tool"), "{last}");
}

fn used(mut value: Value, usage: Value) -> Value {
    value["usage"] = usage;
    value
}
/// What the attempt would record from the loop's final meter.
fn accounting(meter: Meter) -> (crate::usage::Accounting, Option<Tokens>) {
    let mut attempt = crate::usage::Attempt::default();
    attempt.record_usage(meter.usage(true, Some("qwen3-coder".into())));
    assert_eq!(attempt.reported_model.as_deref(), Some("qwen3-coder"));
    (attempt.accounting, attempt.tokens)
}
#[test]
fn usage_is_metered_after_every_model_call_with_cached_prompt_tokens() {
    use crate::usage::Accounting;
    let tokens = |i, c, o| Tokens { input_tokens: i, cached_input_tokens: c, output_tokens: o };
    // llama.cpp reports the reused prefix in timings.cache_n.
    let mut host = mock(vec![
        used(reply(None, call("control_room_post", r#"{"message":"a"}"#), "tool_calls"), json!({"prompt_tokens":1000,"completion_tokens":20})),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"Done."}}],
            "usage":{"prompt_tokens":1100,"completion_tokens":5},"timings":{"cache_n":990,"prompt_n":110,"predicted_n":5}}),
    ]);
    let outcome = go(&mut host).unwrap();
    assert_eq!(host.meters.len(), 2, "one live update per model call");
    assert_eq!(host.meters[0], Meter { tokens: Some(tokens(1000, 0, 20)), calls: 1, reported: 1 });
    assert_eq!(host.meters[1], Meter { tokens: Some(tokens(2100, 990, 25)), calls: 2, reported: 2 });
    assert_eq!(outcome.tokens, Some(tokens(2100, 990, 25)));
    // While running the totals are a lower bound; once over, all calls reported.
    let mut running = crate::usage::Attempt::default();
    running.record_usage(host.meters[0].usage(false, None));
    assert_eq!(running.accounting, Accounting::Partial);
    assert_eq!(accounting(host.meters[1]), (Accounting::Reported, Some(tokens(2100, 990, 25))));

    // A call without usage makes the total a lower bound.
    let mut host = mock(vec![
        reply(None, call("control_room_post", r#"{"message":"a"}"#), "tool_calls"),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"Done."}}]}),
    ]);
    go(&mut host).unwrap();
    assert_eq!(accounting(*host.meters.last().unwrap()), (Accounting::Partial, Some(tokens(100, 0, 10))));

    // No call reported anything: unknown, never zero.
    let mut host = mock(vec![json!({"choices":[{"finish_reason":"stop","message":{"content":"Done."}}]})]);
    go(&mut host).unwrap();
    assert_eq!(accounting(*host.meters.last().unwrap()), (Accounting::Unknown, None));
}
#[test]
fn stopped_turns_keep_the_usage_reported_before_they_stopped() {
    use crate::usage::Accounting;
    // Cancelled while the second call was in flight: the first call's usage is
    // kept and the unanswered call makes it a lower bound.
    let mut host = mock(vec![reply(None, call("control_room_post", r#"{"message":"a"}"#), "tool_calls")]);
    host.replies.push_back(Err(io::Error::new(io::ErrorKind::Interrupted, "Model call cancelled")));
    host.polls = VecDeque::from([Poll::Continue, Poll::Continue, Poll::Stop(("cancelled", "Cancelled.".into()))]);
    assert_eq!(go(&mut host).err().unwrap().0, "cancelled");
    let last = *host.meters.last().unwrap();
    assert_eq!((last.calls, last.reported), (2, 1));
    assert_eq!(accounting(last), (Accounting::Partial, Some(Tokens { input_tokens: 100, cached_input_tokens: 0, output_tokens: 10 })));
    // Cancelled between steps: every sent call reported, so the totals are exact.
    let mut host = mock(vec![reply(None, call("control_room_post", r#"{"message":"a"}"#), "tool_calls")]);
    host.polls = VecDeque::from([Poll::Continue, Poll::Stop(("cancelled", "Cancelled.".into()))]);
    assert_eq!(go(&mut host).err().unwrap().0, "cancelled");
    assert_eq!(accounting(*host.meters.last().unwrap()).0, Accounting::Reported);
    // A failed turn keeps its totals too.
    let mut host = mock((0..5).map(|_| reply(None, call("control_room_post", "[1]"), "tool_calls")).collect());
    assert_eq!(go(&mut host).err().unwrap().0, "failed");
    assert_eq!(host.meters.last().unwrap().tokens.unwrap().input_tokens, 400);
    // Refused before any call: nothing is metered (the worker records not launched).
    let mut host = mock(vec![]);
    let small = Limits { context_tokens: 2048, output_tokens: 512, input_bytes: 1024, output_bytes: 2048 };
    assert_eq!(run(&mut host, "m", &small, compactor(1000), &"x".repeat(6000), &Policy::default()).err().unwrap().0, "blocked");
    assert!(host.meters.is_empty());
}
