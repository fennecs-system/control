use super::*;

#[test]
fn local_and_shared_commands_share_completion_highlighting_and_help() {
    let help = help();
    let mut names = std::collections::HashSet::new();
    for entry in entries() {
        assert!(names.insert(entry.text), "duplicate command {}", entry.text);
        assert!(help.contains(entry.usage));
        assert!(help.contains(entry.description));
        assert!(matching(entry.text).any(|candidate| candidate.text == entry.text));
        assert_eq!(highlight(entry.text), Some(0..entry.text.len()));
    }
    assert_eq!(matching("/con").map(|entry| entry.text).collect::<Vec<_>>(), ["/context"]);
    assert_eq!(highlight("/con"), Some(0..4));
    assert_eq!(highlight("/context more"), Some(0..8));
    assert_eq!(highlight("/port allow port-a"), Some(0..11));
    assert_eq!(highlight("/improve off"), Some(0..12));
}

#[test]
fn ordinary_text_unknown_commands_and_arguments_do_not_offer_completions() {
    for text in ["", "hello", "/unknown", "/contextual", "/con\n", "/open /tmp/project", "/steer build it 🦊"] {
        assert_eq!(matching(text).count(), 0, "{text}");
    }
    for text in ["hello", "/unknown", "/contextual", " /con", "🦊 /con"] {
        assert_eq!(highlight(text), None, "{text}");
    }
}
