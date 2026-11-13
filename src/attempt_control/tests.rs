
use super::*;
#[test]
fn productive_persistent_attempts_have_no_default_wall_clock_cliff() {
    assert_eq!(
        decide(limit(None, true), Duration::from_secs(7 * 86400), None),
        Decision::Continue
    );
    assert_eq!(
        decide(limit(None, false), Duration::from_secs(600), None),
        Decision::TimedOut
    );
    assert_eq!(
        decide(limit(Some(1200), true), Duration::from_secs(1199), None),
        Decision::Continue
    );
    assert_eq!(
        decide(limit(Some(1200), true), Duration::from_secs(1200), None),
        Decision::TimedOut
    );
}
#[test]
fn accepted_handoff_finishes_even_at_explicit_deadline_without_model_reply() {
    assert_eq!(
        decide(Some(600), Duration::from_secs(600), Some("saved".into())),
        Decision::Finish("saved".into())
    );
}
