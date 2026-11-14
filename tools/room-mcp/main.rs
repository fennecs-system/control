//! Credential-free stdio MCP transport. The host authorizes every request.
//!
//! The transport answers every request it cannot deliver with a JSON-RPC
//! error and keeps serving: exiting would close the provider's MCP session and
//! fail every later tool call in the turn. Only a closed stdin or stdout ends it.
#[path = "../platform-flags.rs"]
mod platform_flags;
#[path = "../mailbox-limits.rs"]
mod mailbox_limits;
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
/// The latest exchange step, overwritten each time, so a stalled session shows
/// whether the provider or the host stopped answering.
const TRACE: &str = "/workspace/.control-room-mcp.last";
/// Versioned envelope with a maximal u128 timestamp and u64 sequence number.
const ENVELOPE_OVERHEAD: usize = 95;
const MAX_REQUEST: usize = mailbox_limits::REQUEST_ENVELOPE_BYTES - ENVELOPE_OVERHEAD;
const MAX_RESPONSE: usize = 512 * 1024;
/// How long a request may wait for the host before it is answered with an error.
const HOST_TIMEOUT: Duration = Duration::from_secs(35);
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
    file.take(MAX_RESPONSE as u64 + 1).read_to_string(&mut text)?;
    if text.len() > MAX_RESPONSE {
        return Err(io::Error::other("Response too large"));
    }
    Ok(text)
}
/// Raw token of the top-level "id" member, without a JSON dependency. None for notifications.
fn request_id(text: &str) -> Option<String> {
    member(text, "\"id\"")
}
/// Raw token of a top-level member, without a JSON dependency.
fn member(text: &str, key: &str) -> Option<String> {
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
                if key_start == Some(key) {
                    let rest = text[i + 1..].trim_start();
                    let end = if rest.starts_with(['{', '[']) {
                        let (mut level, mut quoted, mut escaped) = (0usize, false, false);
                        let mut end = rest.len();
                        for (j, b) in rest.bytes().enumerate() {
                            if quoted {
                                if escaped { escaped = false; }
                                else if b == b'\\' { escaped = true; }
                                else if b == b'"' { quoted = false; }
                            } else {
                                match b {
                                    b'"' => quoted = true,
                                    b'{' | b'[' => level += 1,
                                    b'}' | b']' => {
                                        level = level.saturating_sub(1);
                                        if level == 0 { end = j + 1; break; }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        end
                    } else if rest.starts_with('"') {
                        let mut j = 1;
                        while j < rest.len() && rest.as_bytes()[j] != b'"' {
                            j += if rest.as_bytes()[j] == b'\\' { 2 } else { 1 };
                        }
                        (j + 1).min(rest.len())
                    } else {
                        rest.find([',', '}']).unwrap_or(rest.len())
                    };
                    let id = rest[..end].trim();
                    return (!id.is_empty()).then(|| id.to_owned());
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
/// Host replies carry the bridge nonce independently of JSON-RPC IDs, which
/// also covers notifications and batches. Late replies never trigger a replay.
fn correlated_response(text: &str, nonce: &str) -> Option<String> {
    (member(text, "\"nonce\"").as_deref() == Some(nonce))
        .then(|| member(text, "\"response\""))
        .flatten()
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
/// A failed batch must resolve every request ID, rather than silently leaving
/// all of its calls pending. Notifications still receive no wire response.
fn delivery_error(request: &str, message: &str) -> Option<String> {
    if !request.starts_with('[') {
        return request_id(request).map(|id| error_reply(&id, message));
    }
    let (mut depth, mut quoted, mut escaped, mut start) = (0usize, false, false, 0usize);
    let mut replies = Vec::new();
    for (i, byte) in request.bytes().enumerate() {
        if quoted {
            if escaped { escaped = false; }
            else if byte == b'\\' { escaped = true; }
            else if byte == b'"' { quoted = false; }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                if depth == 1 { start = i; }
                depth += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 1 && let Some(id) = request_id(&request[start..=i]) {
                    replies.push(error_reply(&id, message));
                }
            }
            _ => {}
        }
    }
    (!replies.is_empty()).then(|| format!("[{}]", replies.join(",")))
}
fn note(message: &str) {
    eprintln!("control-room-mcp: {message}");
    journal(message);
}
fn journal(message: &str) {
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
fn trace(step: &str) {
    journal(step);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    if let Ok(mut file) = fs::OpenOptions::new().write(true).create(true).truncate(true).custom_flags(platform_flags::SAFE_READ).open(TRACE) {
        let _ = writeln!(file, "{stamp} {step}");
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
                if let Some(error) = delivery_error(line.trim(), "MCP request too large for the Control tool bridge; send smaller arguments or write large content to a file first") {
                    reply(&mut stdout, &error)?;
                }
                continue;
            }
            let _ = fs::remove_file(RESPONSE);
            let id = request_id(line.trim());
            let method = member(line.trim(), "\"method\"").unwrap_or_else(|| if line.trim_start().starts_with('[') { "batch".to_owned() } else { String::new() });
            let tool = member(line.trim(), "\"params\"").and_then(|params| member(&params, "\"name\"")).unwrap_or_default();
            trace(&format!("received {count} {method} {tool} id={}", id.as_deref().unwrap_or("-")));
            let nonce = format!("\"{stamp}-{count}\"");
            let envelope = format!("{{\"version\":1,\"nonce\":{nonce},\"request\":{}}}", line.trim());
            if let Err(error) = send(&envelope, stamp, count, &mut tries) {
                if let Some(error) = delivery_error(line.trim(), &format!("Control tool bridge could not deliver this request ({error}); it was not sent, so retrying is safe")) {
                    reply(&mut stdout, &error)?;
                }
                continue;
            }
            let start = Instant::now();
            loop {
                if let Ok(response) = response(Path::new(RESPONSE)) {
                    let Some(response) = correlated_response(response.trim(), &nonce) else {
                        let _ = fs::remove_file(RESPONSE);
                        continue;
                    };
                    if response.trim() != "null" {
                        reply(&mut stdout, response.trim())?;
                        trace(&format!("replied {count} {method} id={}", id.as_deref().unwrap_or("-")));
                    } else {
                        trace(&format!("acknowledged {count} {method}"));
                    }
                    break;
                }
                if start.elapsed() > HOST_TIMEOUT {
                    // Keep serving: answer this request with an error rather than exiting the transport.
                    note(&format!("request {count} had no host reply within {} s", HOST_TIMEOUT.as_secs()));
                    if let Some(error) = delivery_error(line.trim(), &format!("Control tool host did not respond within {} s; its effect may have happened, so inspect status before retrying", HOST_TIMEOUT.as_secs())) {
                        reply(&mut stdout, &error)?;
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
