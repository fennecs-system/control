
use super::*;
#[test]
fn completion_and_highlighting_keep_arguments_and_regular_chat_distinct() {
    assert_eq!(
        completions("/"),
        (0..control_context::commands::DEFINITIONS.len()).collect::<Vec<_>>()
    );
    assert_eq!(completions("/lis"), vec![8, 9]);
    assert_eq!(completions("/op"), vec![7]);
    assert_eq!(command_length("/open /a/project"), 5);
    assert_eq!(command_length("/opened"), 0);
    assert_eq!(completions("/imp"), vec![0, 1]);
    assert_eq!(completions("/improve o"), vec![1]);
    assert!(completions("/improve change the UI").is_empty());
    assert!(completions("ordinary chat").is_empty());
    assert!(completions("/improve\n").is_empty());
    assert_eq!(command_length("/improve change the UI"), 8);
    assert_eq!(command_length("/improve off"), 12);
    assert_eq!(command_length("/improve on"), 11);
    assert_eq!(command_length("/improvements"), 0);
    assert_eq!(command_length("ordinary chat"), 0);
    assert_eq!(completions("/bt"), vec![3]);
    assert_eq!(command_length("/btw <script>"), 4);
    assert_eq!(command_length("/steer change approach"), 6);
    assert_eq!(command_length("/steering"), 0);
    assert_eq!(completions("/ans"), vec![6]);
    assert_eq!(command_length("/answer q-123 2"), 7);
}
