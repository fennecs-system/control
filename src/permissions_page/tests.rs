use super::*;

fn fields(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

#[test]
fn form_fields_choose_each_kind_and_absent_boxes_are_off() {
    let saved = update(&fields(&[("mcp_folder", "yes"), ("mcp_question", "yes"), ("mcp_port", "no"), ("agent_chat_tools", "yes")]));
    assert!(saved.mcp_approvals.folder && saved.mcp_approvals.question);
    assert!(!saved.mcp_approvals.port && !saved.mcp_approvals.bot);
    assert!(saved.agent_chat_tools);
    assert_eq!(saved.auto_approval, AutoApproval::default(), "strict unless chosen");
    assert_eq!(update(&Default::default()), permissions::Permissions::default());
}

#[test]
fn the_auto_approval_form_sets_the_mode_kinds_targets_and_strict_principals() {
    let saved = update(&fields(&[("auto_mode", "auto"), ("auto_port", "auto"), ("auto_bot", "ask"), ("standing_targets", "127.0.0.1:25565\r\n\r\n mirror.example.com:8080 "),
        ("strict_discord:42", "yes"), ("auto_folder", "auto")]));
    let policy = saved.auto_approval;
    assert_eq!((policy.mode, policy.port, policy.bot), (Mode::Auto, KindMode::Auto, KindMode::Ask));
    assert_eq!(policy.standing_targets, ["127.0.0.1:25565", "mirror.example.com:8080"]);
    assert_eq!(policy.strict, ["discord:42"]);
    assert!(!policy.notices, "an unticked box is off");
    assert!(policy.validate().is_ok(), "there is no folder setting to pick up");
    let strict = update(&fields(&[("auto_mode", "strict"), ("auto_port", "auto")])).auto_approval;
    assert_eq!(strict.mode, Mode::Strict);
    assert!(update(&fields(&[("auto_mode", "auto"), ("standing_targets", "not a target")])).auto_approval.validate().is_err(), "saving refuses it");
}

#[test]
fn page_lists_every_kind_of_pending_request_with_its_dm() {
    let mut state = crate::State::default();
    let chat = crate::chat::create(&mut state);
    let html = render(&state);
    assert!(html.contains("No pending requests.") && html.contains("action='/manage/permissions'"));
    assert!(html.contains("value='strict' checked") && !html.contains("value='auto' checked"), "strict by default");
    assert!(!html.contains("mcp_folder' value='yes' checked") && !html.contains("agent_chat_tools' value='yes' checked"), "everything else is off");
    assert!(html.contains("Folder access (host files) and anything involving credentials always ask"));
    assert!(html.contains("Running now") && html.contains("No automatic decisions yet."));
    state.chats[0].project = Some(serde_json::from_value(serde_json::json!({"request":"req-1","path":"/srv/<data>","grant":null,"head":null,"pending":true})).unwrap());
    let html = render(&state);
    assert!(html.contains("<span class='tag'>folder</span>") && html.contains("/srv/&lt;data&gt;"), "{html}");
    assert!(html.contains(&format!("href='/dm/{chat}'")));
}
