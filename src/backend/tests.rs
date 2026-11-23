use super::*;

/// Each backend is selected by its prefix and persisted under its id.
#[test]
fn each_backend_is_selected_by_its_prefix_and_persisted_by_its_id() {
    for (id, backend) in [("grok", Backend::Grok), ("antigravity", Backend::Antigravity)] {
        assert_eq!(Backend::named(id), Some(backend));
        assert!(Backend::hosted().any(|b| b == backend));
        assert_eq!(serde_json::to_string(&backend).unwrap(), format!("\"{id}\""));
    }
}
