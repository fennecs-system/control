use super::*;
use crate::client::operator::{Command, reply};
use crate::client::PortDecision;
use crate::client::chats::Navigation;

fn id(value: &str) -> DiscordId {
    value.parse().unwrap()
}
fn notice(message: &str, kind: Kind, request: &str) -> Notice {
    Notice { user: id("1"), channel: id("100"), message: id(message), chat: "c-lead".into(), kind, id: request.into(), loopback: false }
}
fn ledger() -> Ledger {
    let mut ledger = Ledger::default();
    ledger.record(notice("500", Kind::Question, "q-1"));
    ledger.record(notice("501", Kind::Bot, "bot-1"));
    ledger.record(Notice { loopback: true, ..notice("502", Kind::Port, "port-1") });
    ledger
}

#[test]
fn replies_route_to_the_notified_chat_and_request() {
    let ledger = ledger();
    let question = ledger.target(id("1"), id("100"), Some(id("500")), "2").unwrap();
    assert_eq!(question.chat, "c-lead");
    assert_eq!(reply(&question.pending(), "2"), Ok(Command::Send("/answer q-1 2".into())));
    let bot = ledger.target(id("1"), id("100"), Some(id("501")), "yes").unwrap();
    assert_eq!(reply(&bot.pending(), "yes"), Ok(Command::Send("/bot allow bot-1".into())));
    let port = ledger.target(id("1"), id("100"), Some(id("502")), "allow").unwrap();
    assert!(reply(&port.pending(), "allow").is_err(), "loopback must be explicit");
    assert_eq!(reply(&port.pending(), "allow --loopback"), Ok(Command::Port { id: "port-1".into(), decision: PortDecision::Allow, loopback: true }));
    // A typed command naming a notified request routes without a reply.
    assert_eq!(ledger.target(id("1"), id("100"), None, "/bot deny bot-1").unwrap().message, id("501"));
    // Ordinary messages and replies to anything else keep the DM conversation.
    assert!(ledger.target(id("1"), id("100"), None, "hello").is_none());
    assert!(ledger.target(id("1"), id("100"), Some(id("999")), "2").is_none());
}

#[test]
fn notices_are_private_to_their_recipient() {
    let ledger = ledger();
    assert!(ledger.target(id("2"), id("100"), Some(id("500")), "2").is_none());
    assert!(ledger.target(id("1"), id("200"), Some(id("500")), "2").is_none());
    assert!(ledger.target(id("2"), id("100"), None, "/answer q-1 2").is_none());
}

#[test]
fn each_request_is_sent_once_per_recipient_and_the_table_is_bounded() {
    let mut ledger = ledger();
    assert!(ledger.sent(id("1"), "c-lead", Kind::Question, "q-1"));
    assert!(!ledger.sent(id("2"), "c-lead", Kind::Question, "q-1"));
    assert!(!ledger.sent(id("1"), "c-lead", Kind::Bot, "q-1"));
    assert_ne!(Ledger::receipt(id("1"), "c-lead", Kind::Bot, "x"), Ledger::receipt(id("2"), "c-lead", Kind::Bot, "x"));
    for n in 0..LIMIT {
        ledger.record(notice(&(1000 + n).to_string(), Kind::Question, &format!("q-{n}x")));
    }
    assert_eq!(ledger.notices.len(), LIMIT);
    assert!(!ledger.sent(id("1"), "c-lead", Kind::Question, "q-1"));
    let round = serde_json::to_vec(&ledger).unwrap();
    assert_eq!(serde_json::from_slice::<Ledger>(&round).unwrap(), ledger);
}

#[test]
fn notices_name_the_chat_the_request_and_how_to_decide() {
    let view = serde_json::json!({"pending_bots":[{"request_id":"bot-1","chat_id":"c-child"}]});
    let pending = crate::client::operator::pending(&view).remove(0);
    let text = text("Swarm lead", "http://127.0.0.1:8080/chat/c-lead", &pending);
    assert!(text.starts_with("Control · Swarm lead\nBot request · c-child"));
    assert!(text.contains("/bot allow bot-1 · /bot deny bot-1"));
    assert!(text.ends_with("open http://127.0.0.1:8080/chat/c-lead"));
}

#[test]
fn operator_messages_route_to_decisions_navigation_replied_or_selected_chats() {
    let mut ledger = ledger();
    let me = (id("1"), id("100"));
    assert_eq!(ledger.destination(me.0, me.1, Some(id("500")), "2"), Destination::Decide(&ledger.notices[0]));
    assert_eq!(ledger.destination(me.0, me.1, None, "/chats"), Destination::Navigate(Navigation::List));
    assert_eq!(ledger.destination(me.0, me.1, None, "/resume c-lead"), Destination::Navigate(Navigation::Open("c-lead".into())));
    // Without a selection, ordinary messages keep the DM's own chat.
    assert_eq!(ledger.destination(me.0, me.1, None, "fix the build"), Destination::Dm);
    ledger.select(me.0, me.1, Some(("c-lead".into(), 4)));
    assert_eq!(ledger.active(me.0, me.1).unwrap().delivered, 4);
    assert_eq!(ledger.destination(me.0, me.1, None, "/steer use the cache"), Destination::Chat("c-lead"));
    // Replying to a relayed reply continues that chat, whatever is selected.
    ledger.relay(Relayed { user: me.0, channel: me.1, message: id("700"), chat: "c-helper".into() });
    assert_eq!(ledger.destination(me.0, me.1, Some(id("700")), "more detail"), Destination::Chat("c-helper"));
    // Another person's DM is unaffected by this selection or relay.
    assert_eq!(ledger.destination(id("2"), me.1, Some(id("700")), "hi"), Destination::Dm);
    assert_eq!(ledger.destination(me.0, id("200"), None, "hi"), Destination::Dm);
    ledger.select(me.0, me.1, Some(("c-other".into(), 0)));
    assert_eq!(ledger.active.len(), 1, "one selection per DM");
    ledger.select(me.0, me.1, None);
    assert_eq!(ledger.destination(me.0, me.1, None, "hi"), Destination::Dm);
    let round = serde_json::to_vec(&ledger).unwrap();
    assert_eq!(serde_json::from_slice::<Ledger>(&round).unwrap(), ledger);
    // Tables written before selections existed still load.
    assert!(serde_json::from_str::<Ledger>(r#"{"notices":[]}"#).is_ok());
}
