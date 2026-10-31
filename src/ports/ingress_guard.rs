//! Raw TCP ingress is deliberately not a browser HTTP/TLS endpoint. A browser
//! cookie is scoped to a host, not a port; previews use the guarded HTTP surface.
use std::{
    io::{self, Read},
    net::TcpStream,
    time::{Duration, Instant},
};
const LIMIT: usize = 8192;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    More,
    Raw,
    Reject,
}
fn classify(bytes: &[u8], eof: bool) -> Decision {
    // A TLS ClientHello (including a fragmented record header) is never raw ingress.
    if bytes.first() == Some(&0x16) {
        if bytes.len() == 1 {
            return if eof {
                Decision::Reject
            } else {
                Decision::More
            };
        }
        if bytes[1] == 3 {
            return Decision::Reject;
        }
    }
    let mut first = bytes;
    while let Some(end) = first.iter().position(|b| *b == b'\n') {
        if first[..end].iter().all(u8::is_ascii_whitespace) {
            first = &first[end + 1..];
        } else {
            break;
        }
    }
    let end = first.iter().position(|b| *b == b'\n');
    let line = &first[..end.unwrap_or(first.len())];
    // Only the first request line classifies the protocol. Binary POST bodies
    // and non-ASCII header values must never override an HTTP classification.
    if line.iter().any(|b| !matches!(*b, b'\t' | b'\r' | 32..=126)) {
        return Decision::Raw;
    }
    let complete = end.is_some();
    if !complete && !eof {
        return if bytes.len() >= LIMIT {
            Decision::Reject
        } else {
            Decision::More
        };
    }
    let words = line
        .split(u8::is_ascii_whitespace)
        .filter(|s| !s.is_empty())
        .take(3)
        .collect::<Vec<_>>();
    let http = words
        .get(2)
        .is_some_and(|w| w.len() >= 5 && w[..5].eq_ignore_ascii_case(b"HTTP/"));
    let incomplete_http = eof
        && words.get(1).is_some_and(|w| {
            w.starts_with(b"/") || w.starts_with(b"http:") || w.starts_with(b"https:") || *w == b"*"
        });
    if http || incomplete_http {
        Decision::Reject
    } else if words.is_empty() && !eof {
        Decision::More
    } else {
        Decision::Raw
    }
}
pub(super) fn first_chunk(socket: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut pending = Vec::new();
    let mut deadline: Option<Instant> = None;
    loop {
        let mut bytes = [0; LIMIT];
        let length = LIMIT.saturating_sub(pending.len());
        if length == 0 {
            return Err(io::Error::other("Ambiguous raw ingress prefix"));
        }
        if let Some(end) = deadline {
            let remaining: Duration = end
                .checked_duration_since(Instant::now())
                .ok_or_else(|| io::Error::other("Raw ingress classification timed out"))?;
            socket.set_read_timeout(Some(remaining))?;
        }
        let n = socket.read(&mut bytes[..length])?;
        if n > 0 && deadline.is_none() {
            deadline = Some(Instant::now() + Duration::from_secs(2));
        }
        pending.extend_from_slice(&bytes[..n]);
        match classify(&pending, n == 0) {
            Decision::Raw => {
                socket.set_read_timeout(None)?;
                return Ok(pending);
            }
            Decision::Reject => {
                return Err(io::Error::other(
                    "HTTP/TLS ingress uses the guarded preview tool",
                ));
            }
            Decision::More => {}
        }
    }
}
#[cfg(test)]
mod tests {
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
}
#[cfg(test)]
mod binary_http_tests {
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
}
