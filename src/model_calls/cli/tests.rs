use super::*;
#[test]
fn claude_text_calls_reject_tool_events_even_after_an_empty_advertised_list() {
    let transcript=[
        serde_json::json!({"type":"system","subtype":"init","tools":[]}),
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"unexpected","name":"Bash","input":{"command":"true"}}]}}),
        serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":"not accepted"})
    ].iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")+"\n";
    let flag=Arc::new(AtomicBool::new(false));
    assert!(decode(transcript.as_bytes(),crate::backend::Backend::Claude,Arc::clone(&flag),1024).is_err());
    assert!(flag.load(Ordering::Acquire));
}
#[test]
fn codex_text_decoding_rejects_tools_unknown_items_failures_and_oversize() {
    let good = concat!(
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"retained constraint\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":20,\"cached_input_tokens\":0,\"output_tokens\":5}}\n"
    );
    let flag = Arc::new(AtomicBool::new(false));
    let result = decode(good.as_bytes(), crate::backend::Backend::Codex, flag, 1024).unwrap();
    assert_eq!(result.text, "retained constraint");
    assert_eq!(result.tokens.unwrap().output_tokens, 5);
    for kind in [
        "command_execution",
        "file_change",
        "mcp_tool_call",
        "web_search",
        "unexpected_future_tool",
    ] {
        let flag = Arc::new(AtomicBool::new(false));
        let record =
            serde_json::json!({"type":"item.started","item":{"type":kind}}).to_string() + "\n";
        assert!(
            decode(
                record.as_bytes(),
                crate::backend::Backend::Codex,
                Arc::clone(&flag),
                1024
            )
            .is_err()
        );
        assert!(flag.load(Ordering::Acquire));
    }
    assert!(
        decode(
            good.as_bytes(),
            crate::backend::Backend::Codex,
            Arc::default(),
            2
        )
        .is_err()
    );
}
#[test]
fn claude_requires_empty_tools_and_a_successful_final_text_result() {
    let init = serde_json::json!({"type":"system","subtype":"init","tools":[]}).to_string() + "\n";
    let result=serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":"retained finding"}).to_string()+"\n";
    assert!(
        decode(
            (init + &result).as_bytes(),
            crate::backend::Backend::Claude,
            Arc::default(),
            1024
        )
        .is_ok()
    );
    let init =
        serde_json::json!({"type":"system","subtype":"init","tools":["Bash"]}).to_string() + "\n";
    let flag = Arc::new(AtomicBool::new(false));
    assert!(
        decode(
            (init + &result).as_bytes(),
            crate::backend::Backend::Claude,
            Arc::clone(&flag),
            1024
        )
        .is_err()
    );
    assert!(flag.load(Ordering::Acquire));
}
