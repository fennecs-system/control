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
#[test]
fn a_length_cut_is_reported_distinctly_and_never_returned_as_partial_text() {
    let cut = json!({"choices":[{"finish_reason":"length","message":{"content":"half an ans"}}]});
    let error = decode(cut, &limits()).unwrap_err().to_string();
    assert!(error.contains("token limit"), "{error}");
    for malformed in [
        json!({}),
        json!({"choices":[]}),
        json!({"choices":[{"finish_reason":"stop","message":{}}]}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"  "}}]}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":7}}]}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"a"}},{"finish_reason":"stop","message":{"content":"b"}}]}),
    ] {
        assert!(decode(malformed.clone(), &limits()).is_err(), "{malformed}");
    }
}
#[test]
fn narrowed_limits_only_lower_output_and_widen_input_budget_accordingly() {
    let declared = limits();
    assert_eq!(declared.narrowed(None).unwrap(), declared);
    let smaller = declared.narrowed(Some(128)).unwrap();
    assert_eq!(smaller.output_tokens, 128);
    assert!(smaller.input_budget() > declared.input_budget());
    assert_eq!(declared.narrowed(Some(100_000)).unwrap().output_tokens, 512);
    assert!(declared.narrowed(Some(0)).is_err());
}
#[test]
fn served_identity_is_bounded_untrusted_metadata() {
    let value = json!({"model":"qwen-local","system_fingerprint":"b11669-620d222"});
    assert_eq!(
        served(&value),
        Served {
            model: Some("qwen-local".into()),
            fingerprint: Some("b11669-620d222".into())
        }
    );
    let hostile = json!({"model":"x".repeat(300),"system_fingerprint":"a\nb"});
    assert_eq!(served(&hostile), Served::default());
}
