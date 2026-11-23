use super::*;
use std::io::Read;

fn serve_bytes(path: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    serve(&mut server, path);
    drop(server);
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

/// The whole response of a refused asset name, and every header of a served
/// asset when this build has one, so a change to how assets are served cannot
/// move a byte.
#[test]
fn asset_response_bytes_are_pinned() {
    const SECURITY: &str = "Cache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\n";
    assert_eq!(
        serve_bytes("/tool-window/../x"),
        format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n{SECURITY}Content-Security-Policy: default-src 'none'; frame-ancestors 'self'\r\n\r\nNot found."
        )
    );
    if let Some((name, body)) = ASSETS.iter().find(|(name, _)| name.ends_with(".css")) {
        let response = serve_bytes(&format!("/tool-window/{name}"));
        let (head, _) = response.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            head,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\n{SECURITY}Content-Security-Policy: default-src 'none'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'; frame-ancestors 'self'; form-action 'none'; base-uri 'none'",
                body.len()
            )
        );
    }
}
