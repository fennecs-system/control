
use super::*;
#[test]
fn metadata_is_unique_and_does_not_infer_authority_from_readonly_hints() {
    let mut names = std::collections::HashSet::new();
    for policy in POLICIES {
        assert!(names.insert(policy.name));
        assert!(!(policy.checkpoint && policy.bridge == Bridge::Read));
    }
    assert!(checkpoint("control_github_open_pr"));
    assert!(!checkpoint("control.rust.run")); // Executes locally, not a state transaction.
    assert_eq!(lookup("control.rust.run").unwrap().bridge, Bridge::Local);
    assert!(lookup("unknown").is_none());
}

#[test]
fn initialize_keeps_a_supported_client_version_and_otherwise_answers_the_latest() {
    assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
    assert_eq!(negotiate(Some("2025-11-25")), MCP_LATEST);
    assert_eq!(negotiate(Some("2024-01-01")), MCP_LATEST);
    assert_eq!(negotiate(None), MCP_LATEST);
    assert!(supported("2025-03-26") && !supported("2024-01-01"));
}
