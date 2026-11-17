
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
#[test]
fn folder_approval_uses_the_in_chat_decision_path_but_folder_changes_navigate() {
    assert!(decision_action("/tools/ports", None));
    assert!(decision_action("/tools/bots", Some("deny")));
    assert!(decision_action("/chat/c-18e8804178ab23f0-135/project", Some("approve")));
    assert!(!decision_action("/chat/c-18e8804178ab23f0-135/project", None), "Change folder stays a page submission");
    assert!(!decision_action("/chat//project", Some("approve")));
    assert!(!decision_action("/chat/a/b/project", Some("approve")));
    assert!(!decision_action("/chat/c-1/preview/stop", Some("approve")));
}
#[test]
fn paged_lists_load_the_next_page_within_a_screen_of_view() {
    assert!(near_viewport(0.0, 800.0));
    assert!(near_viewport(1599.0, 800.0));
    assert!(!near_viewport(1600.0, 800.0));
    assert!(near_viewport(-200.0, 800.0), "a slot scrolled past still loads");
}
