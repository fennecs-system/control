//! One bounded JSON POST to a validated private endpoint. Plain HTTP polls for
//! cancellation between reads and closes the connection when cancelled, so the
//! inference server can stop work; it never forwards credentials or proxies.
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

/// Encoded response body ceiling, independent of declared output limits.
pub const BODY_BYTES: usize = 131072;
const HEAD_BYTES: usize = 16384;
const POLL: Duration = Duration::from_millis(100);

fn unavailable() -> io::Error {
    io::Error::other("Model endpoint unavailable; no fallback was used")
}
fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "Model call exceeded its declared timeout; no fallback was used",
    )
}

pub fn post(url: &reqwest::Url, body: &[u8], timeout: Duration, cancel: &dyn Fn() -> bool) -> io::Result<Vec<u8>> {
    exchange(url, Some(body), timeout, cancel)
}
/// One bounded read-only GET (server statistics), with the same framing,
/// size ceiling and timeout as `post`.
pub fn get(url: &reqwest::Url, timeout: Duration) -> io::Result<Vec<u8>> {
    exchange(url, None, timeout, &|| false)
}
fn exchange(
    url: &reqwest::Url,
    body: Option<&[u8]>,
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
) -> io::Result<Vec<u8>> {
    if url.scheme() == "https" {
        return tls(url, body, timeout);
    }
    let deadline = Instant::now() + timeout;
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let host = url.host_str().ok_or_else(unavailable)?;
    let port = url.port_or_known_default().ok_or_else(unavailable)?;
    let mut stream = None;
    for address in (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .map_err(|_| unavailable())?
    {
        let wait = remaining().min(Duration::from_secs(5));
        if wait.is_zero() {
            return Err(timed_out());
        }
        if let Ok(connected) = TcpStream::connect_timeout(&address, wait) {
            stream = Some(connected);
            break;
        }
    }
    let mut stream = stream.ok_or_else(unavailable)?;
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let head = match body {
        Some(body) => format!(
            "POST {} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            url.path(),
            body.len()
        ),
        None => format!(
            "GET {} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n",
            url.path()
        ),
    };
    stream.set_write_timeout(Some(remaining().max(Duration::from_millis(1))))?;
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.unwrap_or_default()))
        .map_err(|_| unavailable())?;
    stream.set_read_timeout(Some(POLL))?;
    let mut received = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        if let Some(body) = complete(&received)? {
            return body;
        }
        if cancel() {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(cancelled());
        }
        if remaining().is_zero() {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(timed_out());
        }
        match stream.read(&mut chunk) {
            Ok(0) => return finish(&received),
            Ok(n) => {
                received.extend_from_slice(&chunk[..n]);
                if received.len() > HEAD_BYTES + BODY_BYTES * 2 {
                    let _ = stream.shutdown(Shutdown::Both);
                    return Err(io::Error::other("Model response exceeds size limit"));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err(unavailable()),
        }
    }
}

struct Head {
    status: u16,
    length: Option<usize>,
    chunked: bool,
    body: usize,
}
fn head(bytes: &[u8]) -> io::Result<Option<Head>> {
    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if bytes.len() > HEAD_BYTES {
            Err(io::Error::other("Malformed model response headers"))
        } else {
            Ok(None)
        };
    };
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| io::Error::other("Malformed model response headers"))?;
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1."))
        .and_then(|line| line.get(2..5))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other("Malformed model response status"))?;
    let (mut length, mut chunked) = (None, false);
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case("content-length") {
            length = Some(
                value
                    .trim()
                    .parse()
                    .map_err(|_| io::Error::other("Malformed model response length"))?,
            );
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
    }
    Ok(Some(Head {
        status,
        length,
        chunked,
        body: end + 4,
    }))
}
/// A complete response before EOF, when its framing says so.
fn complete(bytes: &[u8]) -> io::Result<Option<io::Result<Vec<u8>>>> {
    let Some(head) = head(bytes)? else { return Ok(None) };
    if head.length.is_some_and(|length| length > BODY_BYTES) {
        return Err(io::Error::other("Model response exceeds size limit"));
    }
    let body = &bytes[head.body..];
    if head.chunked {
        return Ok(dechunk(body)?.map(|body| accept(head.status, body)));
    }
    Ok(head
        .length
        .filter(|length| body.len() >= *length)
        .map(|length| accept(head.status, body[..length].to_vec())))
}
fn finish(bytes: &[u8]) -> io::Result<Vec<u8>> {
    if let Some(done) = complete(bytes)? {
        return done;
    }
    let head = head(bytes)?.ok_or_else(unavailable)?;
    if head.chunked || head.length.is_some() {
        return Err(io::Error::other("Model response ended early"));
    }
    accept(head.status, bytes[head.body..].to_vec())
}
fn dechunk(mut body: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let Some(end) = body.windows(2).position(|w| w == b"\r\n") else {
            return Ok(None);
        };
        let size = std::str::from_utf8(&body[..end])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next()?.trim(), 16).ok())
            .ok_or_else(|| io::Error::other("Malformed chunked model response"))?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(Some(out));
        }
        if body.len() < size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&body[..size]);
        if out.len() > BODY_BYTES {
            return Err(io::Error::other("Model response exceeds size limit"));
        }
        body = &body[size + 2..];
    }
}
fn accept(status: u16, body: Vec<u8>) -> io::Result<Vec<u8>> {
    if body.len() > BODY_BYTES {
        return Err(io::Error::other("Model response exceeds size limit"));
    }
    if (200..300).contains(&status) {
        return Ok(body);
    }
    Err(io::Error::other(format!(
        "Model endpoint rejected the request (HTTP {status}){}",
        reason(&body).map(|r| format!(": {r}")).unwrap_or_default()
    )))
}
/// The server's own short error message, bounded and stripped of control text.
fn reason(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let message = value["error"]["message"].as_str().or(value["error"].as_str())?;
    let message: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(240)
        .collect();
    Some(message.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty())
}

/// TLS endpoints are bounded by the timeout only; cancellation is checked before.
fn tls(url: &reqwest::Url, body: Option<&[u8]>, timeout: Duration) -> io::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| io::Error::other("Cannot create model transport"))?;
    let request = match body {
        Some(body) => client
            .post(url.clone())
            .header("content-type", "application/json")
            .body(body.to_vec()),
        None => client.get(url.clone()),
    };
    let response = request.send().map_err(|_| unavailable())?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response.take(BODY_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    accept(status, bytes)
}
/// A call stopped by its `cancel` before or while it waited.
pub fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Model call cancelled; no fallback was used")
}
