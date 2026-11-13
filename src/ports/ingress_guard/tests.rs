
use super::*;
#[test]
fn browser_requests_and_tls_are_rejected_before_any_partial_prefix_can_escape() {
    for request in [
        b"GET / HTTP/1.1\r\nCookie: control_operator=test\r\n\r\n".as_slice(),
        b"CUSTOM / HTTP/1.1\r\n",
        b"PRI * HTTP/2.0\r\n",
        b"POST / HTTP/1.0\n",
    ] {
        for n in 1..request.iter().position(|b| *b == b'\n').unwrap() {
            assert_eq!(classify(&request[..n], false), Decision::More);
        }
        assert_eq!(classify(request, false), Decision::Reject);
    }
    assert_eq!(classify(&[0x16], false), Decision::More);
    assert_eq!(classify(&[0x16, 3, 1], false), Decision::Reject);
    assert_eq!(classify(b"GET / HTTP/1.1", true), Decision::Reject);
    assert_eq!(classify(b"GET /", true), Decision::Reject);
    assert_eq!(classify(&vec![b'A'; LIMIT], false), Decision::Reject);
}
#[test]
fn raw_binary_ascii_half_close_and_server_first_protocol_replies_remain_valid() {
    assert_eq!(classify(&[0, 1, 2], false), Decision::Raw);
    assert_eq!(classify(b"hello", true), Decision::Raw);
    assert_eq!(classify(b"EHLO example\r\n", false), Decision::Raw);
    assert_eq!(classify(b"SSH-2.0-client\r\n", false), Decision::Raw);
    assert_eq!(classify(b"", true), Decision::Raw);
}
