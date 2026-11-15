use super::*;

fn view() -> Value {
    json!({
        "pending_questions":[{"run_id":"r-1","chat_id":"c-child","question":{"id":"q-1"},"summary":"Choose\n\n1. A\n2. B\n\nReply with a number or your own direction. For this question: /answer q-1 YOUR_ANSWER"}],
        "pending_ports":[{"port_id":"port-1","name":"relay","target":{"host":"127.0.0.1","port":25565},"loopback_required":true}],
        "pending_bots":[{"request_id":"bot-1","chat_id":"c-child"}]
    })
}
fn item(view: &Value, kind: Kind) -> Pending<'_> {
    pending(view).into_iter().find(|p| p.kind == kind).unwrap()
}

#[test]
fn views_from_chat_read_and_requests_list_render_alike() {
    let view = view();
    let items = pending(&view);
    assert_eq!(items.iter().map(|p| (p.kind, p.id)).collect::<Vec<_>>(), [(Kind::Question, "q-1"), (Kind::Port, "port-1"), (Kind::Bot, "bot-1")]);
    assert_eq!(items[1].usage.as_deref(), Some("/port allow port-1 --loopback · /port deny port-1"));
    assert!(items[1].loopback && items[1].heading.starts_with("Port request · relay · "));
    assert_eq!(items[2].usage.as_deref(), Some("/bot allow bot-1 · /bot deny bot-1"));
    assert!(pending(&json!({})).is_empty());
}

#[test]
fn replies_become_the_commands_the_web_and_tui_send() {
    let view = view();
    let question = item(&view, Kind::Question);
    assert_eq!(reply(&question, " 2 "), Ok(Command::Send("/answer q-1 2".into())));
    assert_eq!(reply(&question, "use JSON"), Ok(Command::Send("/answer q-1 use JSON".into())));
    assert_eq!(reply(&question, "/answer q-1 1"), Ok(Command::Send("/answer q-1 1".into())));
    // A command naming another request, or none, is never redirected here.
    assert!(reply(&question, "/answer q-2 1").is_err());
    assert!(reply(&question, "/cancel all").is_err());
    let bot = item(&view, Kind::Bot);
    assert_eq!(reply(&bot, "Yes"), Ok(Command::Send("/bot allow bot-1".into())));
    assert_eq!(reply(&bot, "deny"), Ok(Command::Send("/bot deny bot-1".into())));
    assert_eq!(reply(&bot, "/bot deny bot-1"), Ok(Command::Send("/bot deny bot-1".into())));
    assert!(reply(&bot, "maybe").is_err() && reply(&bot, "yes please").is_err());
    let port = item(&view, Kind::Port);
    assert!(reply(&port, "allow").unwrap_err().contains("--loopback"));
    assert_eq!(reply(&port, "allow --loopback"), Ok(Command::Port { id: "port-1".into(), decision: PortDecision::Allow, loopback: true }));
    assert_eq!(reply(&port, "no"), Ok(Command::Port { id: "port-1".into(), decision: PortDecision::Deny, loopback: false }));
    assert!(reply(&port, "/port allow port-2 --loopback").is_err());
}

#[test]
fn typed_port_commands_are_checked_against_the_displayed_request() {
    let shown = |id: &str| (id == "port-1").then_some(true);
    assert!(port("/port allow port-1", shown).unwrap_err().contains("--loopback"));
    assert!(port("/port allow port-9 --loopback", shown).unwrap_err().contains("changed"));
    assert!(port("/port open port-1", shown).unwrap_err().starts_with("Use /port"));
    assert_eq!(port("/port deny port-1", shown), Ok(Command::Port { id: "port-1".into(), decision: PortDecision::Deny, loopback: false }));
    assert_eq!(named("/port allow port-1 --loopback"), Some((Kind::Port, "port-1")));
    assert_eq!(named("/answer q-1"), None);
    assert_eq!(named("/bot on"), None);
}

#[test]
fn forwarding_defaults_to_everything_and_filters_by_kind_and_chat() {
    assert!(Forwarding::default().forwards("c-1", Kind::Port));
    let only = Forwarding { kinds: vec![Kind::Question], chats: vec!["c-1".into()], ..Default::default() };
    assert!(only.forwards("c-1", Kind::Question));
    assert!(!only.forwards("c-1", Kind::Bot) && !only.forwards("c-2", Kind::Question));
    assert!(!Forwarding { enabled: false, ..Default::default() }.forwards("c-1", Kind::Port));
    let parsed: Forwarding = serde_json::from_str(r#"{"kinds":["bot"]}"#).unwrap();
    assert_eq!(parsed, Forwarding { kinds: vec![Kind::Bot], ..Default::default() });
    assert!(serde_json::from_str::<Forwarding>(r#"{"chat":"c-1"}"#).is_err());
}

#[test]
fn rendered_server_text_carries_no_control_characters() {
    let view = json!({"pending_bots":[{"request_id":"bot-\u{1b}[2J1","chat_id":"c-\u{7}x"}],"pending_ports":[{"port_id":"p\u{0}1","name":"re\u{1b}lay","target":{"host":"h\u{1b}","port":1}}]});
    for item in pending(&view) {
        let rendered = format!("{}{}", item.heading, item.usage.unwrap());
        assert!(!rendered.chars().any(char::is_control), "{rendered:?}");
    }
}
