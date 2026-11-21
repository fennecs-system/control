use super::*;
use crate::backend::Backend;
/// An attempt as admission captures it: explicit, bot and local limits.
fn attempt(backend: Backend, explicit: Option<u64>, bot: Option<u64>, local: Option<u64>) -> Attempt {
    Attempt { backend, explicit_timeout_seconds: explicit, bot_timeout_seconds: bot, local_timeout_seconds: local, ..Default::default() }
}
#[test]
fn persistent_bots_use_their_own_turn_limit_and_explicit_limits_win() {
    // The bot turn limit captured at admission (default 1200 s) applies only
    // to approved persistent bots; ordinary attempts keep 600 s.
    let bot = attempt(Backend::Codex, None, Some(1200), None);
    assert_eq!(decide(limit(&bot, true), Duration::from_secs(1199), None), Decision::Continue);
    assert_eq!(decide(limit(&bot, true), Duration::from_secs(1200), None), Decision::TimedOut(1200));
    assert_eq!(decide(limit(&bot, false), Duration::from_secs(600), None), Decision::TimedOut(600));
    // A bot limit configured as unlimited (0/none), or an attempt captured
    // before the setting existed, has no wall-clock cliff.
    let unlimited = attempt(Backend::Codex, None, None, None);
    assert_eq!(decide(limit(&unlimited, true), Duration::from_secs(7 * 86400), None), Decision::Continue);
    // An explicit operator limit takes priority for every attempt.
    let explicit = attempt(Backend::Codex, Some(3600), Some(1200), None);
    assert_eq!(decide(limit(&explicit, true), Duration::from_secs(1200), None), Decision::Continue);
    let explicit = attempt(Backend::Codex, Some(900), None, None);
    assert_eq!(decide(limit(&explicit, false), Duration::from_secs(900), None), Decision::TimedOut(900));
}
#[test]
fn turn_limit_matrix_by_backend_and_bot_status() {
    // (attempt, approved persistent bot, expected limit)
    let cases = [
        // Local chat: the captured local limit (default 3600 s), not 600 s.
        (attempt(Backend::Local, None, Some(1200), Some(3600)), false, Some(3600)),
        // Local bot: the local limit applies to bots too, not the bot limit.
        (attempt(Backend::Local, None, Some(1200), Some(3600)), true, Some(3600)),
        // Local configured unlimited (0/none).
        (attempt(Backend::Local, None, Some(1200), None), false, None),
        (attempt(Backend::Local, None, Some(1200), None), true, None),
        // Hosted chats and bots keep their limits; a local limit is ignored.
        (attempt(Backend::Codex, None, Some(1200), Some(3600)), false, Some(600)),
        (attempt(Backend::Claude, None, Some(1200), None), false, Some(600)),
        (attempt(Backend::Codex, None, Some(1200), None), true, Some(1200)),
        // An explicit worker-timeout-secs wins everywhere.
        (attempt(Backend::Local, Some(900), Some(1200), Some(3600)), false, Some(900)),
        (attempt(Backend::Local, Some(900), Some(1200), None), true, Some(900)),
        (attempt(Backend::Codex, Some(900), Some(1200), None), true, Some(900)),
    ];
    for (at, (attempt, persistent, expected)) in cases.iter().enumerate() {
        assert_eq!(limit(attempt, *persistent), *expected, "case {at}");
    }
    // A local chat survives the 600 s cliff that failed a lead's first turn.
    let local = attempt(Backend::Local, None, None, Some(3600));
    assert_eq!(decide(limit(&local, false), Duration::from_secs(601), None), Decision::Continue);
    assert_eq!(decide(limit(&local, false), Duration::from_secs(3600), None), Decision::TimedOut(3600));
    // Without a state bridge the same selection applies.
    assert_eq!(poll(None, &local, Duration::from_secs(601)), Decision::Continue);
    assert_eq!(poll(None, &local, Duration::from_secs(3600)), Decision::TimedOut(3600));
}
#[test]
fn accepted_handoff_finishes_even_at_explicit_deadline_without_model_reply() {
    assert_eq!(
        decide(Some(600), Duration::from_secs(600), Some("saved".into())),
        Decision::Finish("saved".into())
    );
}
#[test]
fn requested_handoff_replies_with_the_last_message_and_wakes_keep_progress() {
    let said = |id: &str, kind: &str, text: &str| crate::usage::Activity { id: id.into(), kind: kind.into(), text: text.into(), output: String::new(), status: "completed".into() };
    let activity = [said("1", "message", "Report: the fix and its diff."), said("2", "mcp_tool_call", "control_turn_handoff"), said("3", "message", "  ")];
    // An operator-prompted turn shows what the agent said, not the placeholder or its progress line.
    assert_eq!(reply(true, &activity, None), "Report: the fix and its diff.");
    assert_eq!(reply(true, &activity, Some("staged")), "Report: the fix and its diff.");
    // A wake, delegation or agent message keeps the staged progress or placeholder.
    assert_eq!(reply(false, &activity, Some("staged")), "staged");
    assert_eq!(reply(false, &activity, None), "Agent yielded with a committed handoff.");
    // A requested turn that said nothing falls back the same way.
    assert_eq!(reply(true, &activity[1..], None), "Agent yielded with a committed handoff.");
}
