use super::*;

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
            "Codex could not use or refresh the read-only account session. Refresh the host login and submit a new run. The credential mount was not made writable.",
        ),
        (
            b"Permission denied",
            "Codex encountered a filesystem permission denial. Check credential ownership, ephemeral state, and sandbox compatibility.",
        ),
        (
            b"401 Unauthorized",
            "Codex account authentication was rejected. Refresh the host login and submit a new run.",
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
        assert_eq!(cause.message("Codex"), message);
    }
    assert_eq!(classify(b"unexpected raw text fake-secret-for-test"), None);
    assert_eq!(
        Cause::SignIn.message("Antigravity"),
        "Antigravity's login is missing, expired or was rejected. Run control antigravity check, then sign in again with control antigravity login or replace the API key."
    );
}

#[test]
fn diagnostics_never_echo_their_text() {
    let failure = Failure::Provider {
        agent: "Codex",
        cause: classify(b"401 Unauthorized bearer fake-secret-for-test").unwrap(),
    };
    assert!(failure.message().contains("authentication was rejected"));
    assert!(!failure.message().contains("fake-secret-for-test"));
    assert_eq!(
        classify(b"Invalid API key fake-secret-for-test"),
        Some(Cause::AuthRejected)
    );
}

#[test]
fn each_failure_has_one_status_and_wording() {
    assert_eq!(Failure::Waiting.status().as_str(), "waiting");
    assert_eq!(Failure::Waiting.message(), "Waiting for a design decision.");
    assert_eq!(Failure::Cancelled(Cancelled::Running).message(), CANCELLED);
    assert_eq!(
        Failure::Cancelled(Cancelled::BeforeLaunch(Some("Grok"))).message(),
        "Cancelled before Grok launch."
    );
    assert_eq!(
        Failure::Cancelled(Cancelled::BeforeLaunch(None)).message(),
        "Cancelled before launch."
    );
    assert_eq!(Failure::UnfinishedTool.status(), Status::Interrupted);
    assert_eq!(Failure::TimedOut(Duration::from_secs(600)).status(), Status::Failed);
    assert!(
        Failure::TimedOut(Duration::from_secs(600))
            .message()
            .contains("600 s time limit")
    );
}
