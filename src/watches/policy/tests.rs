
use super::*;
use serde_json::json;

#[test]
fn bounded_literal_selection_does_not_interpret_patterns() {
    let policy: WakePolicy =
        serde_json::from_value(json!({"kind":"prefixes","prefixes":["event:","[error]"]})).unwrap();
    assert!(policy.admits(b"event: done"));
    assert!(policy.admits(b"[error] disconnected"));
    assert!(!policy.admits(b"status: event: done"));
    assert!(!policy.admits(b"error disconnected"));
    assert!(!(WakePolicy::ExitOnly {}).admits(b"event: done"));
    for value in [
        json!({"kind":"prefixes","prefixes":[]}),
        json!({"kind":"prefixes","prefixes":[""]}),
        json!({"kind":"prefixes","prefixes":["x".repeat(129)]}),
        json!({"kind":"prefixes","prefixes":vec!["x"; 9]}),
        json!({"kind":"prefixes","prefixes":["x\ny"]}),
        json!({"kind":"exit_only","prefixes":["x"]}),
        json!({"kind":"unknown"}),
    ] {
        assert!(
            serde_json::from_value::<WakePolicy>(value.clone()).is_err(),
            "{value}"
        );
    }
}
