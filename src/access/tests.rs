
use super::*;
#[test]
fn open_options_preserve_default_browser_and_reject_ambiguous_flags() {
    assert_eq!(open_mode(&[]).unwrap(), OpenMode::Browser);
    assert_eq!(open_mode(&["--link".into()]).unwrap(), OpenMode::Link);
    assert_eq!(open_mode(&["--help".into()]).unwrap(), OpenMode::Help);
    assert!(open_mode(&["--link".into(), "extra".into()]).is_err());
    assert!(open_mode(&["--unknown".into()]).is_err());
}
#[test]
fn operator_access_cannot_be_claimed_by_host_header_or_role() {
    let a = Access {
        session: "a".repeat(64),
    };
    assert!(!a.authorized("Host: 127.0.0.1:8080\r\nX-Role: architect\r\n"));
    assert!(!a.authorized("Cookie: control_operator=wrong"));
    assert!(a.authorized(&format!(
        "Cookie: other=1; control_operator={}",
        "a".repeat(64)
    )));
    assert!(!a.authorized(&format!("Cookie: other={}", "a".repeat(64))));
}
