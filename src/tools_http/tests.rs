use super::*;
use std::io::Read;

/// The whole response of a refused method, so a change to how the tools
/// pages respond cannot move a byte.
#[test]
fn refused_method_response_bytes_are_pinned() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    assert!(handle(&mut server, "PUT", "/tools/search", b""));
    drop(server);
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert_eq!(response, "HTTP/1.1 405 Method Not Allowed\r\nContent-Type: text/plain\r\nContent-Length: 16\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; connect-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'\r\n\r\nUse GET or POST.");
}
