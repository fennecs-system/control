
use super::local_server;

#[test]
fn project_confirmation_requires_the_same_request_redirect_and_grant() {
    use super::*;
    let ready = json!({"project":{"request":"request-a","grant":"grant-a","pending":false}});
    assert!(ProjectApproval::from_response(ready.clone(), "request-a", true).result.is_ok());
    assert!(ProjectApproval::from_response(ready.clone(), "request-a", false).result.is_err());
    let changed = ProjectApproval::from_response(ready.clone(), "request-b", true);
    assert!(changed.result.unwrap_err().contains("request changed"));
    assert_eq!(changed.detail, ready);
    for project in [Value::Null, json!({"request":"request-a"}), json!({"request":"request-a","grant":42}), json!({"request":"request-a","grant":"grant-a","pending":true,"error":"Still copying"})] {
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
