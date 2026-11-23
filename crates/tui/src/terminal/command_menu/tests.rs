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
    assert_eq!(highlight("/memory on local-qwen"), Some(0..10));
    assert!(help.contains("/memory on [MODEL]") && help.contains("/memory off"));
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

/// The indexed lookup returns exactly what a full scan in menu order would.
#[test]
fn indexed_lookup_matches_a_full_scan() {
    let all = build().collect::<Vec<_>>();
    let scan = |input: &str| all.iter().filter(|e| input.starts_with('/') && !input.contains('\n') && e.text.starts_with(input)).map(|e| e.text).collect::<Vec<_>>();
    let longest = |input: &str| all.iter().filter(|e| input.strip_prefix(e.text).is_some_and(|t| t.is_empty() || t.starts_with(char::is_whitespace))).map(|e| e.text.len()).max();
    let mut inputs = vec!["".to_owned(), "/".into(), "ordinary text".into(), "/zzz".into(), "/port\nallow".into(), "/port  allow 7".into()];
    for entry in &all {
        for end in 1..=entry.text.len() {
            inputs.push(entry.text[..end].to_owned());
        }
        inputs.push(format!("{} arg", entry.text));
        inputs.push(format!("{}x", entry.text));
    }
    for input in &inputs {
        assert_eq!(matching(input).map(|e| e.text).collect::<Vec<_>>(), scan(input), "{input:?}");
        if input.starts_with('/') && let Some(end) = longest(input) {
            assert_eq!(highlight(input), Some(0..end), "{input:?}");
        }
    }
}
