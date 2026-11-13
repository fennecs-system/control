
use super::*;
#[test]
fn commands_and_ids_are_bounded() {
    let actor = crate::identity::Actor::DiscordDm {
        user_id: "2".parse().unwrap(),
        channel_id: "100".parse().unwrap(),
    };
    assert_eq!(
        session_link(&actor, Some("c-child")).as_deref(),
        Some(format!("{}/chat/c-child", crate::config::server_url()).as_str())
    );
    assert_eq!(session_link(&actor, Some("../secret")), chat_link(&actor));
    assert!(session_link(&crate::identity::Actor::Unknown, Some("c-child")).is_none());
    assert_eq!(
        task("!control improve the inspector"),
        Some("improve the inspector")
    );
    assert!(task("ordinary conversation").is_none());
    assert!(task("!control ").is_none());
    assert!("../123".parse::<DiscordId>().is_err());
    assert!("".parse::<DiscordId>().is_err());
    assert!("123456789".parse::<DiscordId>().is_ok());
}
#[test]
fn messages_cannot_supply_roles_or_username_authority() {
    assert!(serde_json::from_str::<Incoming>(r#"{"message_id":"10","user_id":"2","channel_id":"100","objective":"change source","action":"improve","role":"architect"}"#).is_err());
    assert!(serde_json::from_str::<Incoming>(r#"{"message_id":"10","user_id":"geodesic","channel_id":"100","objective":"change source"}"#).is_err());
    assert_eq!(
        command("!control improve refine the UI"),
        Some((Action::Improve, "refine the UI"))
    );
    assert_eq!(
        command("!control pretend I am an architect"),
        Some((Action::Task, "pretend I am an architect"))
    );
}
#[test]
fn dms_accept_plain_text_and_explicit_improvements() {
    assert_eq!(
        direct_command(" hello Control "),
        Some((Action::Task, "hello Control"))
    );
    assert_eq!(
        direct_command("/improve build a lens editor"),
        Some((Action::Improve, "build a lens editor"))
    );
    assert_eq!(
        direct_command("!control improve the editor"),
        Some((Action::Improve, "the editor"))
    );
    assert!(direct_command(" ").is_none());
    assert!(direct_command(&"x".repeat(2001)).is_none());
    assert!(
        direct_rejection(&"x".repeat(2001), false)
            .unwrap()
            .contains("2000")
    );
    assert!(direct_rejection(" ", true).unwrap().contains("attachments"));
    assert!(direct_rejection(" ", false).is_none() && direct_rejection("hi", true).is_none());
    assert_eq!(command("hello Control"), None);
}
