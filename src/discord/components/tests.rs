use super::*;
use crate::client::channel::{Settings, request};
use crate::client::operator;

fn rendered(n: usize) -> Request {
    let groups = json!([{"chat_id":"c-lead","title":"Swarm lead",
        "pending_questions":[{"question":{"id":"q-1","input":{"question":"Which cache?","options":["A","B","C","D","E","F"],"recommended":0}},"summary":"Which cache?"}],
        "pending_ports":[{"port_id":"port-1","name":"relay","target":{"host":"127.0.0.1","port":8081},"loopback_required":true}],
        "pending_bots":[{"request_id":"bot-1","chat_id":"c-child"}]}]);
    let settings = Settings { enabled: true, ..Default::default() };
    let pending = operator::pending(&groups[0]);
    request::request(&settings, "Swarm lead", "http://localhost:8080/dm/c-lead", &pending[n], &groups[0])
}
fn buttons(message: &Value) -> Vec<Value> {
    message["components"].as_array().unwrap().iter().flat_map(|row| row["components"].as_array().unwrap().clone()).collect()
}

#[test]
fn requests_render_as_coloured_embeds_with_tagged_buttons() {
    let question = message(&rendered(0), "abcd1234", true);
    let embed = &question["embeds"][0];
    assert_eq!(embed["title"], "Decision requested");
    assert_eq!(embed["color"], color(Severity::Attention));
    assert!(embed["footer"]["text"].as_str().unwrap().contains("!answer q-1"));
    assert_eq!(embed["fields"][0], json!({"name":"DM","value":"Swarm lead","inline":true}));
    assert_eq!(question["content"], "Control · Decision requested · Swarm lead");
    assert_eq!(question["allowed_mentions"], json!({"parse":[]}));
    // Six options take two rows; the answer and link buttons a third.
    let rows = question["components"].as_array().unwrap();
    assert_eq!(rows.iter().map(|row| row["components"].as_array().unwrap().len()).collect::<Vec<_>>(), [5, 1, 2]);
    let all = buttons(&question);
    assert_eq!(all[0]["custom_id"], "ctl:abcd1234:o1");
    assert_eq!(all[0]["style"], 1, "recommended");
    assert_eq!(all[1]["style"], 2);
    assert_eq!(all[6]["custom_id"], "ctl:abcd1234:answer");
    assert_eq!(all[7], json!({"type":2,"style":5,"label":"Open DM","url":"http://localhost:8080/dm/c-lead"}));
    for button in &all {
        assert!(button["label"].as_str().unwrap().chars().count() <= 80);
        assert!(button["custom_id"].as_str().is_none_or(|id| id.len() <= 100));
    }
    let port = message(&rendered(1), "abcd1234", true);
    assert_eq!(port["embeds"][0]["color"], color(Severity::Danger));
    assert_eq!(buttons(&port).iter().map(|b| b["label"].as_str().unwrap()).collect::<Vec<_>>(), ["Allow loopback", "Deny", "Open DM"]);
    assert_eq!(buttons(&port)[0]["style"], 4);
    // Without links the URL moves into the text.
    let plain = message(&rendered(2), "abcd1234", false);
    assert!(buttons(&plain).iter().all(|b| b["style"] != 5));
    assert!(plain["embeds"][0]["description"].as_str().unwrap().ends_with("http://localhost:8080/dm/c-lead"));
}

#[test]
fn settling_recolours_and_keeps_only_link_buttons() {
    let mut posted = message(&rendered(2), "abcd1234", true);
    posted["embeds"][0]["type"] = json!("rich");
    let edit = settled(&posted, &request::decided("allow")).unwrap();
    assert_eq!(edit["embeds"][0]["color"], color(Severity::Success));
    assert_eq!(edit["embeds"][0]["footer"]["text"], "Allowed by you");
    assert_eq!(edit["embeds"][0]["title"], "Bot request");
    assert!(edit["embeds"][0]["type"].is_null(), "only fields Discord accepts are sent back");
    assert_eq!(buttons(&edit), [json!({"type":2,"style":5,"label":"Open DM","url":"http://localhost:8080/dm/c-lead"})]);
    let unlinked = settled(&message(&rendered(2), "t", false), &request::closed()).unwrap();
    assert_eq!(unlinked["components"], json!([]));
    assert!(settled(&json!({"content":"plain text fallback","embeds":[]}), &request::closed()).is_none());
}

fn interaction(kind: u64, custom_id: &str) -> Value {
    json!({"id":"111","token":"tok-en.1","type":kind,"channel_id":"222","user":{"id":"333"},"message":{"id":"444"},
        "data":{"custom_id":custom_id,"component_type":2}})
}

#[test]
fn interactions_are_parsed_only_for_this_nodes_dm_buttons() {
    let pressed = press(&interaction(3, "ctl:abcd1234:allow"), "abcd1234").unwrap();
    assert_eq!(pressed, Press { id: "111".into(), token: "tok-en.1".into(), user: "333".parse().unwrap(), channel: "222".parse().unwrap(), message: "444".into(), action: "allow".into(), text: None });
    assert_eq!(pressed.response("abcd1234"), json!({"type":6}));
    // Another node's tag, another application's ID, malformed actions: not ours.
    for id in ["ctl:ffff0000:allow", "other:allow", "ctl:abcd1234:", "ctl:abcd1234:Allow", "ctl:abcd1234:allow:x", "ctl:abcd1234:averyveryverylongaction"] {
        assert!(press(&interaction(3, id), "abcd1234").is_none(), "{id}");
    }
    // Server interactions, wrong component types and missing fields are ignored.
    let mut guild = interaction(3, "ctl:abcd1234:allow");
    guild["guild_id"] = json!("555");
    assert!(press(&guild, "abcd1234").is_none());
    let mut select = interaction(3, "ctl:abcd1234:allow");
    select["data"]["component_type"] = json!(3);
    assert!(press(&select, "abcd1234").is_none());
    for field in ["message", "user", "token"] {
        let mut missing = interaction(3, "ctl:abcd1234:allow");
        missing[field] = Value::Null;
        assert!(press(&missing, "abcd1234").is_none(), "{field}");
    }
    let mut bad = interaction(3, "ctl:abcd1234:allow");
    bad["message"]["id"] = json!("../x");
    assert!(press(&bad, "abcd1234").is_none());
    // The answer button opens a modal; its submission carries the text.
    let answer = press(&interaction(3, "ctl:abcd1234:answer"), "abcd1234").unwrap();
    assert!(answer.prompts());
    let modal = answer.response("abcd1234");
    assert_eq!((modal["type"].as_u64(), modal["data"]["custom_id"].as_str()), (Some(9), Some("ctl:abcd1234:answer")));
    let mut submit = interaction(5, "ctl:abcd1234:answer");
    submit["data"] = json!({"custom_id":"ctl:abcd1234:answer","components":[{"type":1,"components":[{"type":4,"custom_id":"text","value":"  use files  "}]}]});
    let submitted = press(&submit, "abcd1234").unwrap();
    assert_eq!((submitted.text.as_deref(), submitted.prompts()), (Some("use files"), false));
    assert_eq!(submitted.response("abcd1234"), json!({"type":6}));
    // The newer label layout, an empty answer and a modal for another action.
    submit["data"]["components"] = json!([{"type":18,"component":{"type":4,"custom_id":"text","value":"2"}}]);
    assert_eq!(press(&submit, "abcd1234").unwrap().text.as_deref(), Some("2"));
    submit["data"]["components"] = json!([{"type":18,"component":{"type":4,"custom_id":"text","value":"  "}}]);
    assert!(press(&submit, "abcd1234").is_none());
    submit["data"]["custom_id"] = json!("ctl:abcd1234:allow");
    assert!(press(&submit, "abcd1234").is_none());
}

#[test]
fn the_node_tag_is_short_and_stable() {
    let tag = tag();
    assert_eq!(tag.len(), 8);
    assert!(tag.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(tag, super::tag());
}
