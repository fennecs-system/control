
use super::*;
#[test]
fn response_policy_and_binary_lengths_are_independent_of_routes() {
    for policy in [
        Policy::Page,
        Policy::Embedded,
        Policy::Chat,
        Policy::Client,
        Policy::Restricted,
        Policy::RestrictedEmbedded,
        Policy::Access,
        Policy::Asset,
        Policy::ToolWindow,
    ] {
        let mut bytes = Vec::new();
        respond(
            &mut bytes,
            "200 OK",
            "application/octet-stream",
            &[0, 255, 1],
            "Set-Cookie: first=1\r\nSet-Cookie: second=2\r\n",
            policy,
        )
        .unwrap();
        let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let header = std::str::from_utf8(&bytes[..split]).unwrap();
        assert_eq!(&bytes[split..], &[0, 255, 1]);
        assert!(
            header.contains("Content-Length: 3\r\n") && header.contains("Cache-Control: no-store")
        );
        assert!(header.contains(policy.csp()));
        assert_eq!(header.matches("set-cookie:").count(), 2);
        assert_eq!(
            header.contains("'unsafe-inline'"),
            policy == Policy::ToolWindow
        );
        assert_eq!(
            header.contains("'wasm-unsafe-eval'"),
            matches!(policy, Policy::ToolWindow | Policy::Chat | Policy::Client)
        );
        assert_eq!(
            header.contains("frame-ancestors 'none'"),
            matches!(
                policy,
                Policy::Page | Policy::Client | Policy::Restricted | Policy::Access
            )
        );
    }
}
#[test]
fn malformed_or_conflicting_headers_are_rejected_before_writing() {
    for extra in [
        "Location: /safe\nInjected: yes",
        "Content-Length: 99\r\n",
        "Bad name: x\r\n",
        "Transfer-Encoding: chunked\r\n",
        "Content-Security-Policy: unsafe\r\n",
    ] {
        let mut bytes = Vec::new();
        assert!(
            respond(
                &mut bytes,
                "200 OK",
                "text/plain",
                b"body",
                extra,
                Policy::Page
            )
            .is_err()
        );
        assert!(bytes.is_empty());
    }
    let mut bytes = Vec::new();
    respond(
        &mut bytes,
        "304 Not Modified",
        "text/plain",
        b"",
        "ETag: \"revision\"\r\n",
        Policy::Embedded,
    )
    .unwrap();
    let header = String::from_utf8(bytes).unwrap();
    assert!(!header.contains("Content-Length:"));
}
