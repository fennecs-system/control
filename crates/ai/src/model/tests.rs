use super::*;
fn limits() -> Limits {
    Limits {
        context_tokens: 4096,
        output_tokens: 512,
        input_bytes: 65536,
        output_bytes: 2048,
    }
}
#[test]
fn requests_are_bounded_text_only_and_leave_output_capacity() {
    let limits = limits();
    let payload = Request {
        model: "qwen",
        system: "Evidence only",
        input: "hello",
        limits: &limits,
    }
    .chat_completions()
    .unwrap();
    assert!(payload.get("tools").is_none());
    assert_eq!(payload["stream"], false);
    assert!(
        Request {
            model: "qwen",
            system: "system",
            input: &"x".repeat(4096),
            limits: &limits
        }
        .chat_completions()
        .is_err()
    );
    assert_eq!(limits.input_budget(), 3072);
}
#[test]
fn truncated_tool_bearing_and_oversized_responses_are_rejected_usage_stays_unknown() {
    let good =
        json!({"choices":[{"finish_reason":"stop","message":{"content":"a useful summary"}}]});
    assert!(decode(good.clone(), &limits()).unwrap().tokens.is_none());
    for reason in ["length", "tool_calls", "error"] {
        let mut v = good.clone();
        v["choices"][0]["finish_reason"] = json!(reason);
        assert!(decode(v, &limits()).is_err());
    }
    let mut v = good.clone();
    v["choices"][0]["message"]["tool_calls"] = json!([{"name":"execute"}]);
    assert!(decode(v, &limits()).is_err());
    let mut v = good;
    v["choices"][0]["message"]["content"] = json!("x".repeat(2049));
    assert!(decode(v, &limits()).is_err());
}
