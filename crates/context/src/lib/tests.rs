
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
        for name in tool.mcp_names {
            assert!(protocol_names.insert(name), "Duplicate MCP name: {name}");
            assert_eq!(tool_for_mcp(name), Some(&tool));
        }
    }
    assert!(tool_for_mcp("control_agent_delegate").is_none());
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
