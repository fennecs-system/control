//! The channel core driven by an in-memory transport: no service types.
use super::*;
use crate::client::PortDecision;
use crate::client::operator::{Command, reply};
use std::cell::RefCell;

/// A fake messaging service: records posts, numbers messages, and can be told
/// to fail transiently or permanently.
#[derive(Default)]
struct Fake {
    posts: RefCell<Vec<(String, String, String)>>,
    keys: RefCell<std::collections::HashSet<String>>,
    failing: RefCell<Option<Posted>>,
}
impl Transport for Fake {
    fn capabilities(&self) -> Capabilities {
        Capabilities { replies: true, max_text: 400, ..Default::default() }
    }
    fn post(&self, to: &Recipient, text: &str, _reply_to: Option<&str>, key: &str) -> Posted {
        if let Some(failure) = self.failing.borrow().clone() {
            return failure;
        }
        // Like a nonce: a repeated key is not posted twice.
        if !self.keys.borrow_mut().insert(key.into()) {
            return Posted::Sent(None);
        }
        let mut posts = self.posts.borrow_mut();
        posts.push((to.conversation.clone(), text.into(), key.into()));
        Posted::Sent(Some(format!("m{}", posts.len())))
    }
}
fn me() -> Recipient {
    Recipient { person: "alice".into(), conversation: "dm-alice".into(), home: Some("home-chat".into()) }
}
fn other() -> Recipient {
    Recipient { person: "bob".into(), conversation: "dm-bob".into(), home: None }
}
fn on() -> Settings {
    Settings { enabled: true, ..Default::default() }
}
fn channel() -> Channel {
    Channel { name: "fake".into(), hive: None, refreshed: None, revisions: Value::Null, ledger: Ledger::default() }
}
fn groups() -> Value {
    json!([{"chat_id":"c-lead","title":"Swarm lead",
        "pending_questions":[{"question":{"id":"q-1"},"summary":"Choose\n1. A\n2. B"}],
        "pending_bots":[{"request_id":"bot-1","chat_id":"c-child"}],
        "pending_ports":[{"port_id":"port-1","name":"relay","target":{"host":"127.0.0.1","port":1},"loopback_required":true}]}])
}
fn inbound<'a>(replied: Option<&'a str>, text: &'a str) -> Inbound<'a> {
    Inbound { replied, text }
}

#[test]
fn requests_are_posted_once_per_recipient_and_retried_until_routable() {
    let (fake, mut channel) = (Fake::default(), channel());
    let recipients = [me(), other()];
    assert!(!channel.notify(&fake, &on(), &recipients, &groups()));
    assert_eq!(fake.posts.borrow().len(), 6, "three requests for each of two recipients");
    assert!(fake.posts.borrow()[0].1.starts_with("Control · Swarm lead\nDecision requested\nChoose"));
    // A later snapshot with the same requests posts nothing new.
    assert!(!channel.notify(&fake, &on(), &recipients, &groups()));
    assert_eq!(fake.posts.borrow().len(), 6);
    // Transient failures are retried; nothing is recorded until a message ID exists.
    let (fake, mut channel) = (Fake::default(), self::channel());
    *fake.failing.borrow_mut() = Some(Posted::Retry);
    assert!(channel.notify(&fake, &on(), &recipients, &groups()));
    assert!(channel.ledger.notices.is_empty());
    *fake.failing.borrow_mut() = None;
    assert!(!channel.notify(&fake, &on(), &recipients, &groups()));
    assert_eq!(channel.ledger.notices.len(), 6);
    // Settings filter kinds and chats; disabled forwards nothing.
    let (fake, mut channel) = (Fake::default(), self::channel());
    channel.notify(&fake, &Settings { kinds: vec![Kind::Bot], ..on() }, &recipients, &groups());
    channel.notify(&fake, &Settings { chats: vec!["c-other".into()], ..on() }, &recipients, &groups());
    channel.notify(&fake, &Settings::default(), &recipients, &groups());
    assert_eq!(fake.posts.borrow().len(), 2);
}

#[test]
fn replies_route_to_the_notified_request_as_the_tui_command() {
    let (fake, mut channel) = (Fake::default(), channel());
    channel.notify(&fake, &on(), &[me()], &groups());
    let decide = |replied, text| match channel.route(&on(), &me(), inbound(Some(replied), text)) {
        Some(Routed::Decide(notice)) => reply(&notice.pending(), text).map(|command| (notice.chat.clone(), command)),
        other => panic!("{other:?}"),
    };
    assert_eq!(decide("m1", "2"), Ok(("c-lead".into(), Command::Send("/answer q-1 2".into()))));
    assert!(decide("m2", "allow").unwrap_err().contains("--loopback"));
    assert_eq!(decide("m2", "allow --loopback").unwrap().1, Command::Port { id: "port-1".into(), decision: PortDecision::Allow, loopback: true });
    assert_eq!(decide("m3", "yes").unwrap().1, Command::Send("/bot allow bot-1".into()));
    // A typed command naming a notified request routes without a reply.
    assert!(matches!(channel.route(&on(), &me(), inbound(None, "/bot deny bot-1")), Some(Routed::Decide(n)) if n.message == "m3"));
    // Other people, other conversations and a disabled channel do not match.
    assert_eq!(channel.route(&on(), &other(), inbound(Some("m1"), "2")), None);
    assert_eq!(channel.route(&on(), &Recipient { conversation: "elsewhere".into(), ..me() }, inbound(Some("m1"), "2")), None);
    assert_eq!(channel.route(&Settings::default(), &me(), inbound(Some("m1"), "2")), None);
    // Ordinary messages without a selection stay with the adapter.
    assert_eq!(channel.route(&on(), &me(), inbound(None, "hello")), None);
}

#[test]
fn navigation_selection_and_relayed_replies_steer_chats() {
    let (fake, mut channel) = (Fake::default(), channel());
    assert_eq!(channel.route(&on(), &me(), inbound(None, "/chats")), Some(Routed::Navigate(Navigation::List)));
    channel.ledger.select(&me(), Some(("c-lead".into(), 1)));
    assert_eq!(channel.route(&on(), &me(), inbound(None, "/steer use the cache")), Some(Routed::Chat("c-lead".into())));
    assert_eq!(channel.route(&on(), &other(), inbound(None, "/steer")), None, "selection is per conversation");
    // Finished replies after the selection point are relayed once and in order.
    let detail = json!({"messages":[
        {"status":"completed","reply":"before"},
        {"status":"completed","reply":"first"},
        {"status":"waiting","reply":"question"},
        {"status":"completed","reply":"later"}]});
    assert!(!channel.relay(&fake, &me(), 0, "Swarm lead", &json!(7), &detail));
    assert_eq!(fake.posts.borrow().len(), 1);
    assert!(fake.posts.borrow()[0].1.ends_with("\n\nfirst"));
    assert_eq!(channel.ledger.active[0].delivered, 2, "waits at the waiting question");
    // Replying to a relayed reply continues that chat even after switching.
    channel.ledger.select(&me(), Some(("c-other".into(), 0)));
    assert_eq!(channel.route(&on(), &me(), inbound(Some("m1"), "more")), Some(Routed::Chat("c-lead".into())));
    // A transient failure keeps the reply for the next pass.
    let (fake, mut channel) = (Fake::default(), self::channel());
    channel.ledger.select(&me(), Some(("c-lead".into(), 1)));
    *fake.failing.borrow_mut() = Some(Posted::Retry);
    assert!(channel.relay(&fake, &me(), 0, "Swarm lead", &json!(7), &detail));
    assert_eq!((channel.ledger.active[0].delivered, &channel.ledger.active[0].revision), (1, &Value::Null));
    channel.ledger.select(&me(), None);
    assert!(channel.ledger.active.is_empty());
}

#[test]
fn routing_state_is_bounded_and_platform_neutral() {
    let mut ledger = Ledger::default();
    for n in 0..LIMIT + 5 {
        ledger.record(Notice { person: "alice".into(), conversation: "dm".into(), message: format!("m{n}"), chat: "c".into(), kind: Kind::Question, id: format!("q-{n}"), loopback: false });
    }
    assert_eq!(ledger.notices.len(), LIMIT);
    assert_eq!(ledger.notices[0].message, "m5");
    let text = serde_json::to_string(&ledger).unwrap();
    assert_eq!(serde_json::from_str::<Ledger>(&text).unwrap(), ledger);
    assert!(serde_json::from_str::<Ledger>(r#"{"notices":[]}"#).is_ok());
    let long = reply_text("Lead", "completed", "http://x/chat/c", &"x".repeat(1000), 300);
    assert!(long.chars().count() <= 300 && long.ends_with("Full reply in chat."));
}

#[test]
fn settings_are_opt_in_with_a_bang_prefix_and_filter_requests() {
    let settings = Settings::default();
    assert!(!settings.enabled && settings.prefix == "!");
    assert!(!settings.forwards("c-1", Kind::Port), "opt-in");
    assert!(on().forwards("c-1", Kind::Port));
    let only = Settings { kinds: vec![Kind::Question], chats: vec!["c-1".into()], ..on() };
    assert!(only.forwards("c-1", Kind::Question) && !only.forwards("c-1", Kind::Bot) && !only.forwards("c-2", Kind::Question));
    let parsed: Settings = serde_json::from_str(r#"{"enabled":true,"prefix":"/"}"#).unwrap();
    assert_eq!(parsed, Settings { prefix: "/".into(), ..on() });
    assert!(serde_json::from_str::<Settings>(r#"{"chat":"c-1"}"#).is_err());
    assert!(Settings::valid_prefix("!") && Settings::valid_prefix("/") && Settings::valid_prefix("??"));
    assert!(!Settings::valid_prefix("") && !Settings::valid_prefix("a") && !Settings::valid_prefix("!!!!") && !Settings::valid_prefix(" "));
}

#[test]
fn the_prefix_maps_to_control_commands_both_ways() {
    let bang = on();
    assert_eq!(bang.command(" !chats "), "/chats");
    assert_eq!(bang.command("!steer go left"), "/steer go left");
    assert_eq!(bang.command("!!"), "!!", "only a command word follows the prefix");
    assert_eq!(bang.command("/chats"), "/chats", "Control's own spelling still works");
    assert_eq!(bang.present("Reply /answer q-1 YOUR_ANSWER or /bot allow b"), "Reply !answer q-1 YOUR_ANSWER or !bot allow b");
    assert_eq!(bang.present("see /usr/bin and http://x/chat/c and a/answer"), "see /usr/bin and http://x/chat/c and a/answer");
    let slash = Settings { prefix: "/".into(), ..on() };
    assert_eq!(slash.present("/answer q"), "/answer q");
    // Routing understands the prefix, including help and decisions.
    let (fake, mut channel) = (Fake::default(), channel());
    channel.notify(&fake, &bang, &[me()], &groups());
    assert!(fake.posts.borrow()[0].1.contains("!answer q-1 YOUR_ANSWER") || fake.posts.borrow()[1].1.contains("!port allow"));
    assert_eq!(channel.route(&bang, &me(), inbound(None, "!help")), Some(Routed::Help));
    assert!(matches!(channel.route(&bang, &me(), inbound(None, "!bot deny bot-1")), Some(Routed::Decide(n)) if n.id == "bot-1"));
    assert_eq!(channel.route(&bang, &me(), inbound(None, "!use swarm, review")), Some(Routed::Navigate(Navigation::Use(vec!["swarm".into(), "review".into()]))));
}

#[test]
fn help_is_generated_from_the_shared_command_tables() {
    let help = on().help();
    for (usage, _) in chats::COMMANDS.iter().chain(operator::COMMANDS) {
        assert!(help.contains(&on().present(usage)), "{usage}");
    }
    for definition in control_context::commands::DEFINITIONS.iter().filter(|d| d.id != "answer") {
        assert!(help.contains(&on().present(definition.text)), "{}", definition.text);
    }
    assert!(help.contains("!help") && !help.contains(" /chats"));
}
