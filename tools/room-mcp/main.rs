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
fn main() -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut input = io::stdin().lock();
    let mut count = 0u64;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.len() > 32700 {
            return Err(io::Error::other("MCP request too large"));
        }
        count += 1;
        let _ = fs::remove_file("/workspace/.control-room-response");
        let temp = format!("/workspace/.control-room-request-{stamp}-{count}");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(
            format!(
                "{{\"nonce\":\"{stamp}-{count}\",\"request\":{}}}",
                line.trim()
            )
            .as_bytes(),
        )?;
        fs::rename(&temp, "/workspace/.control-room-request")?;
        let start = Instant::now();
        loop {
            if let Ok(response) = response(Path::new("/workspace/.control-room-response")) {
                if response.trim() != "null" {
                    println!("{response}");
                    io::stdout().flush()?;
                }
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Control room host did not respond",
                ));
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
}
