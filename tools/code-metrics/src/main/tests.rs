
use super::*;

#[test]
fn groups_preserve_presentation_boundary() {
    assert_eq!(group("./src/terminal.rs"), "terminal");
    assert_eq!(group("web/src/main.rs"), "web");
    assert_eq!(group("crates/bot/src/lib.rs"), "crates/bot");
}

#[test]
fn functions_use_test_attribute_spans_and_skip_anonymous_closures() {
    let source =
        syn::parse_file("#[cfg(test)] mod checks {\n fn scenario() {}\n}\nfn live() {}\n").unwrap();
    let mut tests = TestScopes::default();
    tests.visit_file(&source);
    let tree = json!({"spaces": [{"name": "scenario", "kind": "function", "start_line": 2, "spaces": [{"name": "<anonymous>", "kind": "function"}]}, {"name":"live", "kind":"function", "start_line":4}]});
    let mut output = vec![];
    functions(&tree, "src/example.rs", &tests.0, &mut output);
    assert_eq!(output.len(), 2);
    assert_eq!(output[0]["test_scope"], true);
    assert_eq!(output[1]["test_scope"], false);
}

#[test]
fn external_test_modules_include_unannotated_helpers() {
    let helper = json!({"name": "fixture", "kind": "function", "start_line": 1});
    for (file, expected) in [
        ("src/application/notifications/tests.rs", true),
        ("src/room_wakes/tests/helpers.rs", true),
        ("crates/bot/tests.rs", true),
        ("src/application/notifications.rs", false),
        ("src/latest_tests.rs", false),
    ] {
        let mut output = vec![];
        functions(&helper, file, &[], &mut output);
        assert_eq!(output[0]["test_scope"], expected, "{file}");
    }
}
