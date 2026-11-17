use super::*;
#[test]
fn usage_includes_prefix_cache_hits_from_usage_or_llama_cpp_timings() {
    let openai = json!({"usage":{"prompt_tokens":900,"completion_tokens":40,"prompt_tokens_details":{"cached_tokens":800}},"timings":{"cache_n":1}});
    assert_eq!(usage(&openai), Some(Tokens { input_tokens: 900, cached_input_tokens: 800, output_tokens: 40 }));
    // llama.cpp: prompt_tokens counts the whole prompt; timings.cache_n is the reused prefix.
    let llama = json!({"usage":{"prompt_tokens":900,"completion_tokens":40},"timings":{"cache_n":850,"prompt_n":50,"predicted_n":40}});
    assert_eq!(usage(&llama), Some(Tokens { input_tokens: 900, cached_input_tokens: 850, output_tokens: 40 }));
    let timings_only = json!({"timings":{"cache_n":850,"prompt_n":50,"predicted_n":40}});
    assert_eq!(usage(&timings_only), Some(Tokens { input_tokens: 900, cached_input_tokens: 850, output_tokens: 40 }));
    // A cache count is never more than the input it is part of.
    let odd = json!({"usage":{"prompt_tokens":10,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":99}}});
    assert_eq!(usage(&odd).unwrap().cached_input_tokens, 10);
    assert_eq!(usage(&json!({"usage":{"prompt_tokens":10}})), None);
    assert_eq!(usage(&json!({})), None);
}
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
#[test]
fn tool_steps_decode_calls_and_keep_malformed_arguments_as_errors() {
    let value = json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[
        {"id":"a","type":"function","function":{"name":"control_room_post","arguments":"{\"message\":\"hi\"}"}},
        {"type":"function","function":{"name":"control_turn_handoff","arguments":""}},
        {"id":"c","type":"function","function":{"name":"x","arguments":"{oops"}},
        {"id":"d","type":"function","function":{"name":"y","arguments":"[1]"}}]}}],
        "usage":{"prompt_tokens":10,"completion_tokens":3}});
    let step = decode_step(&value, &limits()).unwrap();
    assert_eq!(step.text, None);
    assert!(!step.truncated);
    assert_eq!(step.calls[0].arguments, Ok(json!({"message":"hi"})));
    assert_eq!((step.calls[1].id.clone(), step.calls[1].arguments.clone()), (None, Ok(json!({}))));
    assert!(step.calls[2].arguments.as_ref().unwrap_err().contains("not valid JSON"));
    assert!(step.calls[3].arguments.as_ref().unwrap_err().contains("JSON object"));
    assert_eq!(step.tokens.unwrap().output_tokens, 3);
    let cut = json!({"choices":[{"finish_reason":"length","message":{"content":"partial"}}]});
    assert!(decode_step(&cut, &limits()).unwrap().truncated);
    for bad in [json!({}), json!({"choices":[{"message":7}]}), json!({"choices":[{"message":{"content":5}}]})] {
        assert!(decode_step(&bad, &limits()).is_err(), "{bad}");
    }
    let request = tool_request("qwen", &[json!({"role":"user","content":"hi"})], &[json!({"type":"function"})], &limits()).unwrap();
    assert_eq!((request["tool_choice"].clone(), request["max_tokens"].clone()), (json!("auto"), json!(512)));
}
#[test]
fn reasoning_is_kept_apart_from_the_answer_and_budgets_leave_room_to_answer() {
    // llama.cpp with reasoning_format=deepseek: a separate field.
    let value = json!({"choices":[{"finish_reason":"tool_calls","message":{"content":"Posting.","reasoning_content":"  The room needs an update.  ",
        "tool_calls":[{"id":"a","type":"function","function":{"name":"control_room_post","arguments":"{}"}}]}}],
        "usage":{"prompt_tokens":10,"completion_tokens":30,"completion_tokens_details":{"reasoning_tokens":21}}});
    let step = decode_step(&value, &limits()).unwrap();
    assert_eq!((step.text.as_deref(), step.reasoning.as_deref()), (Some("Posting."), Some("The room needs an update.")));
    assert_eq!(reasoning_tokens(&value), Some(21));
    // A server that leaves the think block inline: taken out of the text.
    let inline = json!({"choices":[{"finish_reason":"stop","message":{"content":"<think>\nplan it\n</think>\n\nDone."}}]});
    let step = decode_step(&inline, &limits()).unwrap();
    assert_eq!((step.text.as_deref(), step.reasoning.as_deref()), (Some("Done."), Some("plan it")));
    assert_eq!(decode(inline, &limits()).unwrap().text, "Done.");
    let thought_only = json!({"choices":[{"finish_reason":"stop","message":{"content":"<think>only thoughts</think>"}}]});
    assert_eq!(decode_step(&thought_only, &limits()).unwrap().text, None);
    // Budgets: per level, at most half the output allowance, off when tiny.
    assert_eq!(Thinking::default(), Thinking::Medium);
    assert_eq!([Thinking::Off, Thinking::Minimal, Thinking::Medium, Thinking::High].map(|t| t.budget(16384)), [0, 128, 1536, 4096]);
    assert_eq!(Thinking::High.budget(4096), 2048);
    assert_eq!(Thinking::Minimal.budget(100), 0);
    assert_eq!((Thinking::Medium.up(), Thinking::Medium.down(), Thinking::Minimal.down(), Thinking::Off.up()),
        (Thinking::High, Thinking::Low, Thinking::Minimal, Thinking::Off));
    assert_eq!(Thinking::ALL.map(|t| Thinking::parse(t.name())), Thinking::ALL.map(Some));
    assert_eq!(serde_json::to_value(Thinking::Low).unwrap(), json!("low"));
    let mut body = json!({"model":"qwen"});
    think(&mut body, 1536);
    assert_eq!(body, json!({"model":"qwen","chat_template_kwargs":{"enable_thinking":true},"reasoning_format":"deepseek","reasoning_budget_tokens":1536}));
    think(&mut body, 0);
    assert_eq!((body["chat_template_kwargs"]["enable_thinking"].clone(), body["reasoning_budget_tokens"].clone()), (json!(false), json!(0)));
}
