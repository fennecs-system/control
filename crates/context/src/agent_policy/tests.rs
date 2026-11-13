
use super::*;

#[test]
fn run_kinds_keep_their_wire_strings() {
    for kind in [
        RunKind::Chat,
        RunKind::SelfImprovement,
        RunKind::DelegationResult,
        RunKind::BotWake,
        RunKind::Job,
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        assert_eq!(RunKind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(RunKind::parse("nonsense"), None);
}

#[test]
fn only_automatic_events_are_attributed_to_control() {
    for kind in [
        "delegation-result",
        "agent-completion",
        "room-notification",
        "port-decision",
        "agent-message",
        "bot-decision",
        "bot-wake",
    ] {
        assert_eq!(
            MessageSpeaker::for_run_kind(Some(kind)),
            MessageSpeaker::Control
        );
    }
    for kind in [
        None,
        Some("chat"),
        Some("design-answer"),
        Some("steering"),
        Some("spawn"),
    ] {
        assert_eq!(MessageSpeaker::for_run_kind(kind), MessageSpeaker::User);
    }
}
