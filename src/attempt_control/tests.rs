use super::*;
#[test]
fn persistent_bots_use_their_own_turn_limit_and_explicit_limits_win() {
    // The bot turn limit captured at admission (default 1200 s) applies only
    // to approved persistent bots; ordinary attempts keep 600 s.
    assert_eq!(decide(limit(None, true, Some(1200)), Duration::from_secs(1199), None), Decision::Continue);
    assert_eq!(decide(limit(None, true, Some(1200)), Duration::from_secs(1200), None), Decision::TimedOut(1200));
    assert_eq!(decide(limit(None, false, Some(1200)), Duration::from_secs(600), None), Decision::TimedOut(600));
    // A bot limit configured as unlimited (0/none), or an attempt captured
    // before the setting existed, has no wall-clock cliff.
    assert_eq!(decide(limit(None, true, None), Duration::from_secs(7 * 86400), None), Decision::Continue);
    // An explicit operator limit takes priority for every attempt.
    assert_eq!(decide(limit(Some(3600), true, Some(1200)), Duration::from_secs(1200), None), Decision::Continue);
    assert_eq!(decide(limit(Some(900), false, None), Duration::from_secs(900), None), Decision::TimedOut(900));
}
#[test]
fn accepted_handoff_finishes_even_at_explicit_deadline_without_model_reply() {
    assert_eq!(
        decide(Some(600), Duration::from_secs(600), Some("saved".into())),
        Decision::Finish("saved".into())
    );
}
