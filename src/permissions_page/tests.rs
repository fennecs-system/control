use super::*;

#[test]
fn form_fields_choose_each_kind_and_absent_boxes_are_off() {
    let fields = [("mcp_folder", "yes"), ("mcp_question", "yes"), ("mcp_port", "no"), ("agent_chat_tools", "yes")]
        .iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let saved = update(&fields);
    assert!(saved.mcp_approvals.folder && saved.mcp_approvals.question);
    assert!(!saved.mcp_approvals.port && !saved.mcp_approvals.bot);
    assert!(saved.agent_chat_tools);
    assert_eq!(update(&Default::default()), permissions::Permissions::default());
}

#[test]
fn page_lists_every_kind_of_pending_request_with_its_dm() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    let html = render(&state);
    assert!(html.contains("No pending requests.") && html.contains("action='/manage/permissions'"));
    assert!(!html.contains(" checked"), "everything is off by default");
    state.chats[0].project = Some(serde_json::from_value(serde_json::json!({"request":"req-1","path":"/srv/<data>","grant":null,"head":null,"pending":true})).unwrap());
    let html = render(&state);
    assert!(html.contains("<span class='tag'>folder</span>") && html.contains("/srv/&lt;data&gt;"), "{html}");
    assert!(html.contains(&format!("href='/dm/{chat}'")));
}
