
use super::local_server;
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
