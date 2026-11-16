use super::*;

fn line(tokens: Option<Tokens>, accounting: Accounting, running: bool, omitted: usize) -> String {
    status_text(&attempt_status(0, tokens, accounting, running, omitted))
}
const T: Tokens = Tokens { input_tokens: 5074, cached_input_tokens: 3235, output_tokens: 77 };

#[test]
fn numbers_group_thousands() {
    for (n, text) in [(0, "0"), (77, "77"), (999, "999"), (1000, "1,000"), (5074, "5,074"), (123456, "123,456"), (1234567, "1,234,567")] {
        assert_eq!(grouped(n), text);
    }
}

#[test]
fn finished_attempt_shows_compact_tokens_and_full_titles() {
    let parts = attempt_status(0, Some(T), Accounting::Reported, false, 0);
    assert_eq!(status_text(&parts), "attempt 1 · ↑ 5,074 in (3,235 cached) · ↓ 77 out");
    assert_eq!(parts[1].title, "5,074 input tokens (3,235 cached)");
    assert_eq!(parts[2].title, "77 output tokens");
}

#[test]
fn running_and_partial_attempts_are_marked() {
    let running = attempt_status(1, Some(T), Accounting::Partial, true, 0);
    assert_eq!(status_text(&running), "attempt 2 · ↑ 5,074 in (3,235 cached) · ↓ 77 out…");
    assert!(running[2].title.contains("so far"));
    let stopped = attempt_status(0, Some(T), Accounting::Partial, false, 0);
    assert_eq!(status_text(&stopped), "attempt 1 · ↑ ≥5,074 in (3,235 cached) · ↓ ≥77 out");
    assert!(stopped[1].title.contains("at least") && stopped[2].title.contains("at least"));
}

#[test]
fn missing_usage_is_unknown_never_zero() {
    assert_eq!(line(None, Accounting::Unknown, true, 0), "attempt 1 · tokens pending…");
    assert_eq!(line(None, Accounting::Unknown, false, 0), "attempt 1 · tokens unreported");
    // Nothing before a model call, and nothing for records without accounting.
    assert_eq!(attempt_status(0, None, Accounting::NotLaunched, false, 0).len(), 1);
    assert_eq!(attempt_status(0, None, Accounting::Unrecorded, true, 0).len(), 1);
}

#[test]
fn excluded_exchanges_are_pluralised() {
    let one = attempt_status(0, Some(T), Accounting::Reported, false, 1);
    assert_eq!(status_text(&one), "attempt 1 · ↑ 5,074 in (3,235 cached) · ↓ 77 out · 1 earlier exchange excluded");
    assert_eq!(one[3].title, "Context window · 1 earlier exchange excluded from attempt 1. Chat history retained.");
    assert_eq!(line(None, Accounting::Unrecorded, false, 3), "attempt 1 · 3 earlier exchanges excluded");
}
