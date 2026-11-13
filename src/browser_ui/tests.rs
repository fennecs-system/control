
#[test]
fn capture_names_cannot_escape_artifact_directory() {
    assert!(super::valid_name("capture-1793199264262426000.png"));
    for name in [
        "../state.json",
        "capture-1/2.png",
        "capture-.png",
        "capture-1.png?secret",
        "capture-ab.png",
    ] {
        assert!(!super::valid_name(name));
    }
}
