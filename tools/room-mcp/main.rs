//! Credential-free stdio MCP transport. The host authorizes every request.
//!
//! The transport answers every request it cannot deliver with a JSON-RPC
//! error and keeps serving: exiting would close the provider's MCP session and
//! fail every later tool call in the turn. Only a closed stdin or stdout ends it.
#[path = "../platform-flags.rs"]
mod platform_flags;
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const REQUEST: &str = "/workspace/.control-room-request";
const RESPONSE: &str = "/workspace/.control-room-response";
/// Bridge diagnostics for the host; `.control-` files are never published.
const LOG: &str = "/workspace/.control-room-mcp.log";
const LOG_LIMIT: u64 = 65536;
const MAX_REQUEST: usize = 32700;
/// Shared bind mounts can briefly report a fresh rename or create as missing.
const SEND_ATTEMPTS: u32 = 8;
fn response(path: &Path) -> io::Result<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(platform_flags::SAFE_READ)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Not a regular response"));
    }
    let mut text = String::new();
    file.take(524289).read_to_string(&mut text)?;
    if text.len() > 524288 {
        return Err(io::Error::other("Response too large"));
    }
    Ok(text)
}
/// Raw token of the top-level "id" member, without a JSON dependency. None for notifications.
fn request_id(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let (mut depth, mut i, mut key_start) = (0usize, 0usize, None);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                if depth == 1 && key_start.is_none() {
                    key_start = Some(&text[start..=i.min(bytes.len() - 1)]);
                }
            }
            b':' if depth == 1 => {
                if key_start == Some("\"id\"") {
                    let rest = text[i + 1..].trim_start();
                    let end = if rest.starts_with('"') {
                        let mut j = 1;
                        while j < rest.len() && rest.as_bytes()[j] != b'"' {
                            j += if rest.as_bytes()[j] == b'\\' { 2 } else { 1 };
                        }
                        (j + 1).min(rest.len())
                    } else {
                        rest.find([',', '}']).unwrap_or(rest.len())
                    };
                    let id = rest[..end].trim();
                    return (!id.is_empty() && id != "null").then(|| id.to_owned());
                }
            }
            b',' if depth == 1 => key_start = None,
            b'{' | b'[' => {
                depth += 1;
                key_start = None;
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    None
}
fn response_id(text: &str) -> Option<String> {
    if text == "null" { None } else { request_id(text) }
}
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
fn error_reply(id: &str, message: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32000,\"message\":\"{}\"}}}}",
        escape(message)
    )
}
fn note(message: &str) {
    eprintln!("control-room-mcp: {message}");
    if fs::symlink_metadata(LOG).is_ok_and(|m| !m.is_file() || m.len() >= LOG_LIMIT) {
        return;
    }
    // SAFE_READ's no-follow and non-blocking flags also guard this append.
    if let Ok(mut log) = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(platform_flags::SAFE_READ)
        .open(LOG)
    {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let _ = writeln!(log, "{stamp} {message}");
    }
}
/// Publish one request atomically. Every try uses a fresh temporary name, so a
/// failed rename never leaves a half-published request behind.
fn send(envelope: &str, stamp: u128, count: u64, tries: &mut u32) -> io::Result<()> {
    let mut last = None;
    for attempt in 0..SEND_ATTEMPTS {
        let temp = format!("/workspace/.control-room-request-{stamp}-{count}-{tries}");
        *tries += 1;
        let result = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .and_then(|mut file| file.write_all(envelope.as_bytes()))
            .map_err(|e| ("write", e))
            .and_then(|()| fs::rename(&temp, REQUEST).map_err(|e| ("publish", e)));
        match result {
            Ok(()) => return Ok(()),
            Err((step, error)) => {
                let _ = fs::remove_file(&temp);
                note(&format!("request {count} {step} try {attempt}: {error}"));
                last = Some(error);
                std::thread::sleep(Duration::from_millis(10 << attempt.min(5)));
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("request not sent")))
}
/// Discard the remainder of an oversized line so the next request stays framed.
fn skip_line(input: &mut impl BufRead) -> io::Result<()> {
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            input.consume(end + 1);
            return Ok(());
        }
        let length = buffer.len();
        input.consume(length);
    }
}
fn reply(stdout: &mut impl Write, line: &str) -> io::Result<()> {
    writeln!(stdout, "{line}")?;
    stdout.flush()
}
fn main() -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut input = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut count = 0u64;
    let mut tries = 0u32;
    let result = (|| -> io::Result<()> {
        loop {
            let mut raw = Vec::new();
            if input.by_ref().take(MAX_REQUEST as u64 + 1).read_until(b'\n', &mut raw)? == 0 {
                return Ok(());
            }
            let line = String::from_utf8_lossy(&raw).into_owned();
            count += 1;
            if raw.len() > MAX_REQUEST && raw.last() != Some(&b'\n') {
                skip_line(&mut input)?;
                note(&format!("request {count} exceeded {MAX_REQUEST} bytes"));
                if let Some(id) = request_id(line.trim()) {
                    reply(&mut stdout, &error_reply(&id, "MCP request too large for the Control tool bridge; send smaller arguments or write large content to a file first"))?;
                }
                continue;
            }
            let _ = fs::remove_file(RESPONSE);
            let id = request_id(line.trim());
            let envelope = format!("{{\"nonce\":\"{stamp}-{count}\",\"request\":{}}}", line.trim());
            if let Err(error) = send(&envelope, stamp, count, &mut tries) {
                if let Some(id) = &id {
                    reply(&mut stdout, &error_reply(id, &format!("Control tool bridge could not deliver this request ({error}); it was not sent, so retrying is safe")))?;
                }
                continue;
            }
            let start = Instant::now();
            loop {
                if let Ok(response) = response(Path::new(RESPONSE)) {
                    // A late reply to an earlier timed-out request is not this request's answer.
                    if response_id(response.trim()) != id {
                        let _ = fs::remove_file(RESPONSE);
                        if !Path::new(REQUEST).exists() {
                            if let Err(error) = send(&envelope, stamp, count, &mut tries) {
                                if let Some(id) = &id {
                                    reply(&mut stdout, &error_reply(id, &format!("Control tool bridge lost this request ({error}); check its effect before retrying")))?;
                                }
                                break;
                            }
                        }
                        continue;
                    }
                    if response.trim() != "null" {
                        reply(&mut stdout, response.trim())?;
                    }
                    break;
                }
                if start.elapsed() > Duration::from_secs(35) {
                    // Keep serving: answer this request with an error rather than exiting the transport.
                    note(&format!("request {count} had no host reply within 35 s"));
                    if let Some(id) = &id {
                        reply(&mut stdout, &error_reply(id, "Control tool host did not respond within 35 s"))?;
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    })();
    if let Err(error) = &result {
        note(&format!("transport closed after request {count}: {error}"));
    }
    result
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
