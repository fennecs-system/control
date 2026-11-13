
use super::*;
#[test]
fn binary_bodies_and_observed_header_bytes_cannot_override_http_request_line() {
    for request in [
        b"POST / HTTP/1.1\r\nCookie: control_operator=test\r\nContent-Length: 1\r\n\r\n\0"
            .as_slice(),
        b"GET / HTTP/1.1\r\nX-Value: \xff\r\nCookie: control_operator=test\r\n\r\n",
        b"\r\nGET / HTTP/1.1\r\n\0",
    ] {
        assert_eq!(classify(request, false), Decision::Reject);
    }
}
