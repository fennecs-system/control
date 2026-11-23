use super::*;

/// The default Codex account's recovery, which the host words.
fn again() -> SignIn {
    SignIn::from("codex login (on the host)".to_owned())
}

/// The sentences Codex's failures have always read, now worded from the cause.
#[test]
fn each_cause_keeps_its_fixed_wording() {
    let cases = [
        (
            &b"model_not_found"[..],
            "The signed-in account does not support the requested Codex model or runtime feature.",
        ),
        (
            b"Read-only file system",
            "Codex could not use or refresh its account session. Sign in again with codex login (on the host) and submit a new run.",
        ),
        (
            b"Permission denied",
            "Codex encountered a filesystem permission denial. Check credential ownership, ephemeral state, and sandbox compatibility.",
        ),
        (
            b"401 Unauthorized",
            "Codex account authentication was rejected. Sign in again with codex login (on the host) and submit a new run.",
        ),
        (
            b"SSL error",
            "Codex could not validate the provider TLS connection. Check the Nix worker certificate configuration.",
        ),
        (
            b"error parsing config",
            "The pinned Codex worker rejected its runtime configuration. Check CLI and permission-profile compatibility.",
        ),
        (
            b"usage limit",
            "The signed-in Codex account reached a usage or rate limit.",
        ),
        (
            b"stream disconnected",
            "Codex could not complete its provider connection. Check container network connectivity and account availability.",
        ),
    ];
    for (bytes, message) in cases {
        let cause = classify(bytes).unwrap();
        assert_eq!(cause.message("Codex", &again()), message);
    }
    assert_eq!(classify(b"unexpected raw text fake-secret-for-test"), None);
    assert_eq!(
        Cause::SignIn.message(
            "Antigravity",
            &SignIn::from("control antigravity login --account work".to_owned())
        ),
        "Antigravity's login is missing, expired or was rejected. Sign in again with control antigravity login --account work and submit a new run."
    );
}

#[test]
fn diagnostics_never_echo_their_text() {
    let failure = Failure::Provider {
        agent: "Codex",
        cause: classify(b"401 Unauthorized bearer fake-secret-for-test").unwrap(),
    };
    assert!(failure.message(&again()).contains("authentication was rejected"));
    assert!(!failure.message(&again()).contains("fake-secret-for-test"));
    assert_eq!(
        classify(b"Invalid API key fake-secret-for-test"),
        Some(Cause::AuthRejected)
    );
}

#[test]
fn each_failure_has_one_status_and_wording() {
    assert_eq!(Failure::Waiting.status().as_str(), "waiting");
    assert_eq!(Failure::Waiting.message(&again()), "Waiting for a design decision.");
    assert_eq!(Failure::Cancelled(Cancelled::Running).message(&again()), CANCELLED);
    assert_eq!(
        Failure::Cancelled(Cancelled::BeforeLaunch(Some("Grok"))).message(&again()),
        "Cancelled before Grok launch."
    );
    assert_eq!(
        Failure::Cancelled(Cancelled::BeforeLaunch(None)).message(&again()),
        "Cancelled before launch."
    );
    assert_eq!(Failure::UnfinishedTool.status(), Status::Interrupted);
    assert_eq!(Failure::TimedOut(Duration::from_secs(600)).status(), Status::Failed);
    assert!(
        Failure::TimedOut(Duration::from_secs(600))
            .message(&again())
            .contains("600 s time limit")
    );
}

/// The retry time Codex 0.164.0 writes into its limit messages
/// (codex-rs/protocol/src/error.rs and error_tests.rs), in the two forms it has;
/// anything else names no time and the host backs off instead.
#[test]
fn a_usage_limit_carries_the_retry_time_codex_states() {
    // 2026-11-26 12:24 UTC.
    const NOV_26: u64 = 1_795_695_840;
    let cases: [(&str, Option<StatedRetry>); 8] = [
        (
            "You\u{2019}ve hit your usage limit. Visit https://chatgpt.com/settings/usage to purchase more credits or try again at Nov 26th, 2026 12:24 PM.",
            Some(StatedRetry::On(NOV_26)),
        ),
        (
            "You\u{2019}ve hit your usage limit. Try again at Nov 26th, 2026 12:24 PM.",
            Some(StatedRetry::On(NOV_26)),
        ),
        (
            "You\u{2019}ve hit your usage limit. To get more access now, send a request to your admin or try again at 1:05 AM.",
            Some(StatedRetry::Today(65)),
        ),
        (
            "You\u{2019}ve hit your usage limit. Try again at 12:24 PM.",
            Some(StatedRetry::Today(12 * 60 + 24)),
        ),
        (
            "You\u{2019}ve hit your usage limit. Try again at 12:05 AM.",
            Some(StatedRetry::Today(5)),
        ),
        ("You\u{2019}ve hit your usage limit. Try again later.", None),
        // The worker fixtures' shortened text: a date without a time is no time.
        (
            "You\u{2019}ve hit your usage limit. Visit https://example.invalid/usage or try again at Nov 26th.",
            None,
        ),
        (
            "You\u{2019}ve hit your usage limit. Try again at Feb 30th, 2026 1:00 PM.",
            None,
        ),
    ];
    for (text, retry_at) in cases {
        assert_eq!(
            classify(text.as_bytes()),
            Some(Cause::UsageLimit { retry_at }),
            "{text}"
        );
    }
    assert_eq!(
        stated_retry("try again at Jan 1st, 1970 12:00 AM"),
        Some(StatedRetry::On(0))
    );
    assert_eq!(
        stated_retry("try again at Feb 29th, 2028 11:59 PM."),
        Some(StatedRetry::On(1_835_481_540))
    );
    assert_eq!(stated_retry("try again at 13:00 PM"), None);
    // Codex's event lines as Control captures them: the time ends the JSON string.
    assert_eq!(
        stated_retry(
            r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again at 12:24 PM."}}"#
        ),
        Some(StatedRetry::Today(12 * 60 + 24))
    );
    assert_eq!(
        stated_retry(r#"{"type":"error","message":"... or try again at Nov 26th, 2026 12:24 PM."}"#),
        Some(StatedRetry::On(NOV_26))
    );
    // The wording never changes with the time.
    assert_eq!(
        classify(cases[0].0.as_bytes()).unwrap().message("Codex", &again()),
        "The signed-in Codex account reached a usage or rate limit."
    );
}
