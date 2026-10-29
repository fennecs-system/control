//! Credential-free stdio MCP transport. The host authorizes every request.
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn response(path: &Path) -> io::Result<String> {
    #[cfg(target_os = "macos")]
    let flags = 0x100 | 0x4;
    #[cfg(not(target_os = "macos"))]
    let flags = 0x20000 | 0x800;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
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
fn main() -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut input = io::stdin().lock();
    let mut count = 0u64;
    loop {
        let mut line = String::new();
        if input.by_ref().take(32701).read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.len() > 32700 {
            return Err(io::Error::other("MCP request too large"));
        }
        count += 1;
        let _ = fs::remove_file("/workspace/.control-room-response");
        let id = request_id(line.trim());
        let envelope = format!("{{\"nonce\":\"{stamp}-{count}\",\"request\":{}}}", line.trim());
        let send = |attempt: u32| -> io::Result<()> {
            let temp = format!("/workspace/.control-room-request-{stamp}-{count}-{attempt}");
            fs::OpenOptions::new().write(true).create_new(true).open(&temp)?.write_all(envelope.as_bytes())?;
            fs::rename(&temp, "/workspace/.control-room-request")
        };
        send(0)?;
        let (start, mut resent) = (Instant::now(), 0);
        loop {
            if let Ok(response) = response(Path::new("/workspace/.control-room-response")) {
                // A late reply to an earlier timed-out request is not this request's answer.
                if response_id(response.trim()) != id {
                    let _ = fs::remove_file("/workspace/.control-room-response");
                    if !Path::new("/workspace/.control-room-request").exists() {
                        resent += 1;
                        send(resent)?;
                    }
                    continue;
                }
                if response.trim() != "null" {
                    println!("{response}");
                    io::stdout().flush()?;
                }
                break;
            }
            if start.elapsed() > Duration::from_secs(35) {
                // Keep serving: answer this request with an error rather than exiting the transport.
                if let Some(id) = &id {
                    println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32000,\"message\":\"Control tool host did not respond within 35 s\"}}}}");
                    io::stdout().flush()?;
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transport_rejects_links_and_oversized_responses() {
        let dir =
            std::env::temp_dir().join(format!("control-room-transport-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("response"), "{}").unwrap();
        std::os::unix::fs::symlink(dir.join("response"), dir.join("link")).unwrap();
        assert!(response(&dir.join("link")).is_err());
        assert_eq!(response(&dir.join("response")).unwrap(), "{}");
        fs::write(dir.join("large"), vec![b'x'; 524289]).unwrap();
        assert!(response(&dir.join("large")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn timeouts_answer_the_exact_request_id() {
        assert_eq!(request_id(r#"{"jsonrpc":"2.0","params":{"id":9},"id": "a\"b"}"#).as_deref(), Some(r#""a\"b""#));
        assert_eq!(request_id(r#"{"id":12,"method":"x"}"#).as_deref(), Some("12"));
        assert!(request_id(r#"{"method":"notify","params":{"id":3}}"#).is_none());
        assert!(response_id("null").is_none());
    }
}
