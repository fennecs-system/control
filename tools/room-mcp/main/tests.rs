
use super::*;
#[test]
fn transport_rejects_links_and_oversized_responses() {
    let dir = std::env::temp_dir().join(format!("control-room-transport-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("response"), "{}").unwrap();
    std::os::unix::fs::symlink(dir.join("response"), dir.join("link")).unwrap();
    assert!(response(&dir.join("link")).is_err());
    assert_eq!(response(&dir.join("response")).unwrap(), "{}");
    fs::write(dir.join("large"), vec![b'x'; 524289]).unwrap();
    assert!(response(&dir.join("large")).is_err());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn timeouts_answer_the_exact_request_id() {
    assert_eq!(
        request_id(r#"{"jsonrpc":"2.0","params":{"id":9},"id": "a\"b"}"#).as_deref(),
        Some(r#""a\"b""#)
    );
    assert_eq!(
        request_id(r#"{"id":12,"method":"x"}"#).as_deref(),
        Some("12")
    );
    assert!(request_id(r#"{"method":"notify","params":{"id":3}}"#).is_none());
    assert!(response_id("null").is_none());
}
