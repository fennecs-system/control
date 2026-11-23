use super::{local_server, rejection};

#[test]
fn project_confirmation_requires_the_same_request_redirect_and_grant() {
    use super::*;
    let ready = json!({"project":{"request":"request-a","grant":"grant-a","pending":false}});
    assert!(
        ProjectApproval::from_response(ready.clone(), "request-a", true)
            .result
            .is_ok()
    );
    assert!(
        ProjectApproval::from_response(ready.clone(), "request-a", false)
            .result
            .is_err()
    );
    let changed = ProjectApproval::from_response(ready.clone(), "request-b", true);
    assert!(changed.result.unwrap_err().contains("request changed"));
    assert_eq!(changed.detail, ready);
    for project in [
        Value::Null,
        json!({"request":"request-a"}),
        json!({"request":"request-a","grant":42}),
        json!({"request":"request-a","grant":"grant-a","pending":true,"error":"Still copying"}),
    ] {
        let detail = json!({"project":project});
        let outcome = ProjectApproval::from_response(detail.clone(), "request-a", true);
        assert!(outcome.result.is_err());
        assert_eq!(outcome.detail, detail, "retain current evidence on failed confirmation");
    }
}
#[test]
fn remote_urls_and_embedded_credentials_are_rejected() {
    assert!(local_server("http://localhost:8080").is_ok());
    for url in [
        "https://example.com",
        "http://localhost.evil.test",
        "http://secret@localhost",
        "http://localhost/path",
        "http://localhost/?token=secret",
    ] {
        assert!(local_server(url).is_err());
    }
}

#[test]
fn refused_requests_say_why_without_echoing_html() {
    assert!(rejection(401, "<!doctype html>…").contains("CONTROL_STATE_DIR"));
    assert!(rejection(413, "").contains("too large"));
    assert_eq!(
        rejection(400, "Unsupported MCP protocol version.\n"),
        "Control rejected the request (HTTP 400): Unsupported MCP protocol version."
    );
    assert_eq!(
        rejection(500, "<html>boom</html>"),
        "Control rejected the request (HTTP 500)."
    );
    assert_eq!(
        rejection(400, &"x".repeat(500)).len(),
        "Control rejected the request (HTTP 400): ".len() + 200
    );
}
