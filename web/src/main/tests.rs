
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
    assert!(decision_action("/dm/c-18e8804178ab23f0-135/project", Some("approve")));
    assert!(!decision_action("/dm/c-18e8804178ab23f0-135/project", None), "Change folder stays a page submission");
    assert!(!decision_action("/dm//project", Some("approve")));
    assert!(!decision_action("/dm/a/b/project", Some("approve")));
    assert!(!decision_action("/dm/c-1/preview/stop", Some("approve")));
}
/// After `launchctl kickstart`, the browser's own EventSource retry can meet
/// `503 Control is stopping`, which closes the stream for good; the page then
/// sat on a 10 s fallback with no live connection until reloaded. The client
/// reopens it with bounded backoff for the page's life.
#[test]
fn closed_live_streams_reopen_with_bounded_backoff() {
    assert_eq!(reconnect_delay(0), 1000, "the first reopen is quick");
    assert_eq!((1..=4).map(reconnect_delay).collect::<Vec<_>>(), [2000, 4000, 8000, 16_000]);
    assert_eq!(reconnect_delay(5), 30_000);
    assert_eq!(reconnect_delay(u32::MAX), 30_000, "never overflows, never gives up");
}
#[test]
fn a_redeployed_client_reloads_only_a_quiet_page() {
    let client = "import init from '/tool-window/control-tool-window-5b9e1c7c2f52f8e9.js';";
    assert!(!client_changed(None, client), "an unknown first reading is the page's own client");
    assert!(!client_changed(Some(client), client), "a plain restart keeps the page");
    assert!(client_changed(Some(client), &client.replace("5b9e", "77aa")));
    assert!(reload_is_quiet(false, ""));
    assert!(reload_is_quiet(false, " \n"));
    assert!(!reload_is_quiet(false, "half a message"), "a draft is never lost");
    assert!(!reload_is_quiet(true, ""), "nor a field being edited");
}
#[test]
fn refreshed_regions_read_fragments_only_for_new_slots() {
    let shown = ["svc-a".to_owned(), "svc-b".to_owned()];
    assert!(!slots_added(&shown, &shown[..1]), "a service that stopped needs no read");
    assert!(!slots_added(&shown, &[]), "nor does the last one stopping");
    assert!(slots_added(&shown, &["svc-c".to_owned()]));
    assert!(slots_added(&[], &["svc-a".to_owned()]));
}
#[test]
fn paged_lists_load_the_next_page_within_a_screen_of_view() {
    assert!(near_viewport(0.0, 800.0));
    assert!(near_viewport(1599.0, 800.0));
    assert!(!near_viewport(1600.0, 800.0));
    assert!(near_viewport(-200.0, 800.0), "a slot scrolled past still loads");
}
