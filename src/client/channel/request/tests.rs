//! Structured requests, action-to-reply mapping and press validation, with an
//! in-memory transport. Successful presses reach the operator interface, so
//! they are covered through the shared reply path rather than executed here.
use super::*;
use crate::client::PortDecision;
use crate::client::channel::{Capabilities, Channel, Posted, Pressed, Transport};
use crate::client::operator::{self, Command};
use serde_json::json;
use std::cell::RefCell;

/// A service with buttons: records structured posts, plain posts and settles.
#[derive(Default)]
struct Rich {
    buttons: bool,
    requests: RefCell<Vec<Request>>,
    texts: RefCell<Vec<String>>,
    settled: RefCell<Vec<(String, Settled)>>,
}
impl Transport for Rich {
    fn capabilities(&self) -> Capabilities {
        Capabilities { replies: true, buttons: self.buttons, edits: self.buttons, max_text: 400, ..Default::default() }
    }
    fn post(&self, _to: &Recipient, text: &str, _reply_to: Option<&str>, _key: &str) -> Posted {
        self.texts.borrow_mut().push(text.into());
        Posted::Sent(Some(format!("m{}", self.texts.borrow().len() + self.requests.borrow().len())))
    }
    fn post_request(&self, _to: &Recipient, request: &Request, _text: &str, _key: &str) -> Posted {
        self.requests.borrow_mut().push(request.clone());
        Posted::Sent(Some(format!("m{}", self.texts.borrow().len() + self.requests.borrow().len())))
    }
    fn settle(&self, _to: &Recipient, message: &str, settled: &Settled) -> Posted {
        self.settled.borrow_mut().push((message.into(), settled.clone()));
        Posted::Sent(None)
    }
}
fn me() -> Recipient {
    Recipient { person: "alice".into(), conversation: "dm-alice".into(), home: None }
}
fn on() -> Settings {
    Settings { enabled: true, ..Default::default() }
}
fn channel() -> Channel {
    Channel { name: "fake".into(), hive: None, refreshed: None, revisions: Value::Null, ledger: Ledger::default() }
}
fn groups() -> Value {
    json!([{"chat_id":"c-lead","title":"Swarm lead",
        "pending_questions":[{"question":{"id":"q-1","input":{"question":"Which cache?","options":["Redis","Files"],"recommended":1}},"summary":"Which cache?\n\n1. Redis\n2. Files (recommended)\n\nReply with a number or your own direction. For this question: /answer q-1 YOUR_ANSWER"}],
        "pending_bots":[{"request_id":"bot-1","chat_id":"c-child","every_minutes":15}],
        "pending_ports":[{"port_id":"port-1","name":"relay","target":{"host":"127.0.0.1","port":8081},"direction":"egress","service_port":80,"loopback_required":true}]}])
}

#[test]
fn rich_requests_carry_fields_severity_and_actions_in_the_channel_spelling() {
    let groups = groups();
    let pending = operator::pending(&groups[0]);
    let render = |n: usize| request(&on(), "Swarm lead", "http://x/chat/c-lead", &pending[n], &groups[0]);
    let question = render(0);
    assert_eq!((question.title.as_str(), question.severity), ("Decision requested", Severity::Attention));
    assert!(question.body.starts_with("Which cache?") && question.body.contains("**2.** Files (recommended)"));
    assert!(question.hint.contains("!answer q-1"), "commands use the channel prefix: {}", question.hint);
    assert!(question.fields.contains(&("DM", "Swarm lead".into())) && question.fields.contains(&("Recommended", "Option 2".into())));
    let ids = question.actions.iter().map(|a| a.id).collect::<Vec<_>>();
    assert_eq!(ids, ["o1", "o2", "answer", "open"]);
    assert_eq!(question.actions[1].style, Style::Primary, "the recommended option stands out");
    assert_eq!(question.actions[3].act, Act::Open("http://x/chat/c-lead".into()));
    // Ports in this order: the loopback request, then a plain one.
    let port = render(1);
    assert_eq!((port.title.as_str(), port.severity), ("Port request", Severity::Danger));
    assert!(port.fields.contains(&("Target", "127.0.0.1:8081".into())) && port.fields.contains(&("Service port", "80".into())));
    assert_eq!(port.actions.iter().map(|a| a.id).collect::<Vec<_>>(), ["loopback", "deny", "open"], "loopback is never one plain allow");
    let mut plain = groups.clone();
    plain[0]["pending_ports"][0]["loopback_required"] = json!(false);
    let pending_plain = operator::pending(&plain[0]);
    let port = request(&on(), "Swarm lead", "l", &pending_plain[1], &plain[0]);
    assert_eq!((port.severity, port.actions[0].id), (Severity::Warning, "allow"));
    let bot = render(2);
    assert_eq!((bot.title.as_str(), bot.severity), ("Bot request", Severity::Warning));
    assert!(bot.fields.contains(&("Wakes", "every 15 min".into())));
    assert_eq!(bot.actions.iter().map(|a| (a.id, a.label.as_str())).collect::<Vec<_>>(), [("allow", "Approve"), ("deny", "Deny"), ("open", "Open DM")]);
    // A view without the details still renders from the pending summary.
    let bare = json!({"chat_id":"c","pending_questions":[{"question":{"id":"q-2"},"summary":"Pick one\u{7}"}]});
    let pending = operator::pending(&bare);
    let rendered = request(&on(), "T", "l", &pending[0], &bare);
    assert_eq!(rendered.body, "Pick one", "control characters are dropped");
    assert_eq!(rendered.actions.iter().map(|a| a.id).collect::<Vec<_>>(), ["answer", "open"]);
}

#[test]
fn each_action_maps_to_the_command_its_typed_reply_would_send() {
    let groups = groups();
    let pending = operator::pending(&groups[0]);
    let command = |n: usize, action: &str| operator::reply(&pending[n], reply(pending[n].kind, action).unwrap());
    assert_eq!(command(0, "o2"), Ok(Command::Send("/answer q-1 2".into())));
    assert_eq!(command(1, "loopback"), Ok(Command::Port { id: "port-1".into(), decision: PortDecision::Allow, loopback: true }));
    assert_eq!(command(1, "deny"), Ok(Command::Port { id: "port-1".into(), decision: PortDecision::Deny, loopback: false }));
    assert!(command(1, "allow").is_err(), "a loopback port still requires the explicit loopback action");
    assert_eq!(command(2, "allow"), Ok(Command::Send("/bot allow bot-1".into())));
    assert_eq!(command(2, "deny"), Ok(Command::Send("/bot deny bot-1".into())));
    // Every rendered reply action maps for its own kind, and only for it.
    for (n, item) in pending.iter().enumerate() {
        let rendered = request(&on(), "T", "l", item, &groups[0]);
        for action in rendered.actions.iter().filter(|a| a.act == Act::Reply) {
            assert!(operator::reply(item, reply(item.kind, action.id).unwrap()).is_ok(), "{n} {}", action.id);
        }
    }
    assert_eq!(reply(Kind::Question, "allow"), None);
    assert_eq!(reply(Kind::Bot, "loopback"), None);
    assert_eq!(reply(Kind::Bot, "o1"), None);
    assert_eq!(reply(Kind::Question, "o10"), None);
    assert_eq!(reply(Kind::Question, "answer"), None, "answer needs the prompted text");
}

#[test]
fn presses_are_validated_against_the_recipients_own_unsettled_notices() {
    let (rich, mut channel) = (Rich { buttons: true, ..Default::default() }, channel());
    assert!(!channel.notify(&rich, &on(), &[me()], &groups()));
    assert_eq!((rich.requests.borrow().len(), rich.texts.borrow().len()), (3, 0));
    assert!(channel.ledger.notices.iter().all(|n| n.actions && !n.settled));
    let ledger = &channel.ledger;
    // m1 is the question, m2 the port, m3 the bot.
    assert_eq!(ledger.press(&me(), "m1", "o1", None).map(|(n, r)| (n.id, r)), Ok(("q-1".into(), "1".into())));
    assert_eq!(ledger.press(&me(), "m1", "answer", Some("  use files  ")).map(|(_, r)| r), Ok("use files".into()));
    assert_eq!(ledger.press(&me(), "m1", "answer", Some("  ")), Err(Refusal::Invalid));
    assert_eq!(ledger.press(&me(), "m1", "answer", None), Err(Refusal::Invalid));
    assert_eq!(ledger.press(&me(), "m3", "answer", Some("yes")), Err(Refusal::Invalid), "text only answers questions");
    assert_eq!(ledger.press(&me(), "m3", "o1", Some("x")), Err(Refusal::Invalid));
    assert_eq!(ledger.press(&me(), "m2", "loopback", None).map(|(_, r)| r), Ok("allow --loopback".into()));
    assert_eq!(ledger.press(&me(), "m3", "loopback", None), Err(Refusal::Invalid));
    assert_eq!(ledger.press(&me(), "m3", "open", None), Err(Refusal::Invalid));
    // Someone else's conversation, another person and unknown messages are ignored.
    let bob = Recipient { person: "bob".into(), ..me() };
    assert_eq!(ledger.press(&bob, "m3", "allow", None), Err(Refusal::Unknown));
    assert_eq!(ledger.press(&Recipient { conversation: "dm-other".into(), ..me() }, "m3", "allow", None), Err(Refusal::Unknown));
    assert_eq!(ledger.press(&me(), "m99", "allow", None), Err(Refusal::Unknown));
    assert_eq!(channel.press(&rich, &on(), &bob, "m3", "allow", None), Pressed::Ignored);
    // Forwarding off refuses before anything is sent to Control.
    assert!(matches!(channel.press(&rich, &Settings::default(), &me(), "m3", "allow", None), Pressed::Reply(text) if text.contains("forwarding is off")));
    assert!(matches!(channel.press(&rich, &on(), &me(), "m3", "loopback", None), Pressed::Reply(text) if text.contains("does not apply")));
    // A request no longer pending settles its message once; later presses are refused.
    let remaining = json!([{"chat_id":"c-lead","title":"Swarm lead","pending_questions":groups()[0]["pending_questions"].clone()}]);
    assert!(!channel.notify(&rich, &on(), &[me()], &remaining));
    assert_eq!(rich.settled.borrow().iter().map(|(m, s)| (m.as_str(), s.text.as_str())).collect::<Vec<_>>(), [("m2", "No longer pending"), ("m3", "No longer pending")]);
    assert_eq!(channel.ledger.press(&me(), "m3", "allow", None), Err(Refusal::Settled));
    assert!(matches!(channel.press(&rich, &on(), &me(), "m3", "allow", None), Pressed::Reply(text) if text.contains("already decided")));
    assert!(!channel.notify(&rich, &on(), &[me()], &remaining));
    assert_eq!(rich.settled.borrow().len(), 2, "settled once");
    assert_eq!(rich.requests.borrow().len(), 3, "nothing is re-posted");
}

#[test]
fn services_without_buttons_keep_the_plain_text_with_typed_commands() {
    let (plain, mut channel) = (Rich::default(), channel());
    assert!(!channel.notify(&plain, &on(), &[me()], &groups()));
    assert!(plain.requests.borrow().is_empty());
    let texts = plain.texts.borrow();
    assert_eq!(texts.len(), 3);
    assert!(texts[0].starts_with("Control · Swarm lead\nDecision requested\nWhich cache?") && texts[0].contains("!answer q-1 YOUR_ANSWER"));
    assert!(texts[1].contains("!port allow port-1 --loopback · !port deny port-1"));
    assert!(texts[2].contains("!bot allow bot-1 · !bot deny bot-1"));
    assert!(channel.ledger.notices.iter().all(|n| !n.actions));
    // Closing a plain notice marks it without trying to edit it.
    drop(texts);
    channel.notify(&plain, &on(), &[me()], &json!([]));
    assert!(channel.ledger.notices.iter().all(|n| n.settled) && plain.settled.borrow().is_empty());
    // The text of a settled ledger round-trips; older ledgers without the fields load.
    let text = serde_json::to_string(&channel.ledger).unwrap();
    assert_eq!(serde_json::from_str::<Ledger>(&text).unwrap(), channel.ledger);
    let old = r#"{"notices":[{"person":"a","conversation":"d","message":"m","chat":"c","kind":"bot","id":"b"}]}"#;
    let old = serde_json::from_str::<Ledger>(old).unwrap();
    assert!(!old.notices[0].settled && !old.notices[0].actions);
}

#[test]
fn settled_outcomes_name_the_decision() {
    assert_eq!(decided("allow"), Settled { text: "Allowed by you".into(), severity: Severity::Success });
    assert_eq!(decided("deny").severity, Severity::Muted);
    assert_eq!(decided("o3").text, "Answered by you: option 3");
    assert_eq!(decided("answer").text, "Answered by you");
    assert_eq!(closed().severity, Severity::Muted);
}
