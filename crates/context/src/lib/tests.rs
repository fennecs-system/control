
use super::*;
#[test]
fn available_tools_have_unique_discovery_metadata() {
    let mut ids = std::collections::BTreeSet::new();
    let mut protocol_names = std::collections::BTreeSet::new();
    for tool in TOOLS {
        assert!(ids.insert(tool.name));
        if tool.available {
            assert!(!tool.title.is_empty() && !tool.description.is_empty());
            if let Some(href) = tool.href {
                assert!(href.starts_with('/') && !href.starts_with("//"));
            }
        }
        for entry in tool.mcp_names {
            let name = entry.name;
            assert!(protocol_names.insert(name), "Duplicate MCP name: {name}");
            assert_eq!(tool_for_mcp(name), Some(&tool));
            // Every worker tool can be found from the deferred index.
            let worker = name.starts_with("control_");
            assert_eq!(worker, !entry.phrase.is_empty(), "{name}: index phrase");
            assert!(entry.phrase.split_whitespace().count() <= 8, "{name}: phrase over eight words");
            assert!(worker || (entry.core == 0 && entry.judge == Judge::Denied && entry.brief.is_empty()), "{name}");
        }
    }
    assert!(tool_for_mcp("control_agent_delegate").is_none());
}
#[test]
fn worker_spellings_derive_from_one_family_registration() {
    assert_eq!(worker_name("control.web.search"), Some("control_web_search"));
    assert_eq!(dotted_name("control_web_search"), Some("control.web.search"));
    assert_eq!(dotted_name("control_rust_bench"), Some("control.rust.bench"));
    assert_eq!(worker_name("control.artifacts.list"), None);
    assert_eq!(dotted_name("control_watch_stop"), None);
    assert_eq!(worker_name("control_web_search"), None);
}
#[test]
fn role_core_sets_are_catalog_metadata() {
    use Audience::*;
    for name in ["control_turn_handoff", "control_service_exec", "control_agent_spawn", "control_bot_request", "control_room_post", "control_tool_search"] {
        assert!(loaded(name, Member) && loaded(name, Lead), "{name}");
    }
    assert!(loaded("control_room_plan", Lead) && !loaded("control_room_plan", Member));
    assert!(!loaded("control_service_start", Advisor) && loaded("control_room_read", Advisor));
    assert!(!loaded("control_watch_stop", Member) && !loaded("unknown", Lead));
    assert!(judge_may_call("control_room_read", false) && !judge_may_call("control_room_claim", false));
    assert!(!judge_may_call("control_agent_message", false) && judge_may_call("control_agent_message", true));
    assert!(!judge_may_call("control_service_start", true) && !judge_may_call("unknown", true));
}
#[test]
fn selection_is_deterministic_and_does_not_expand_unknown_lenses() {
    assert_eq!(lens("engineering", &TOOLS), lens("engineering", &TOOLS));
    assert!(lens("missing", &TOOLS).is_empty());
    assert!(
        !lens("research", &TOOLS)
            .iter()
            .any(|t| t.name == "repo.patch")
    );
}
#[test]
fn selected_tools_still_need_authority() {
    assert!(inspect("engineering", &[]).contains("not authorized"));
    assert!(
        inspect("engineering", &["context.inspect"]).contains("permitted in this local example")
    );
    assert!(inspect("engineering", &["repo.patch"]).contains("not connected"));
}
#[test]
fn composition_is_deduplicated_and_order_independent() {
    let combined = compose(&["engineering", "research"], &TOOLS);
    assert_eq!(combined.len(), 5);
    assert_eq!(
        combined,
        compose(&["research", "engineering", "research"], &TOOLS)
    );
    assert!(compose(&[], &TOOLS).is_empty());
    assert!(inspect_composed(&["engineering", "research"], &[]).contains("not authorized"));
}
