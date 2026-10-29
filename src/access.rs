//! Operator access is separate from worker and Discord identities.
use std::{
    fs,
    io::{self, Read, Write},
    net::TcpStream,
    path::Path,
    process::Command,
};

pub struct Access {
    session: String,
}
fn random() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn private_file(path: &Path, text: &str) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}
fn equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
impl Access {
    pub fn load() -> io::Result<Self> {
        let path = crate::config::state("operator-session").map_err(io::Error::other)?;
        let session = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let s = random()?;
                private_file(&path, &s)?;
                s
            }
            Err(e) => return Err(e),
        };
        if session.len() != 64 || !session.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::other("Invalid operator session file"));
        }
        Ok(Self { session })
    }
    fn authorized(&self, header: &str) -> bool {
        header
            .lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
            .flat_map(|(_, v)| v.split(';'))
            .filter_map(|c| c.trim().split_once('='))
            .any(|(name, value)| name == "control_operator" && equal(value, &self.session))
    }
    pub fn gate(&self, stream: &mut TcpStream, method: &str, target: &str, header: &str) -> bool {
        if method == "GET" && target == "/health" {
            respond(stream, "200 OK", "text/plain", "Control is running.", "");
            return true;
        }
        if method == "GET" {
            if let Some(token) = target
                .strip_prefix("/unlock/")
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                let unlocked = crate::config::state("operator-unlock").is_ok_and(|path| {
                    fs::read_to_string(&path).is_ok_and(|expected| equal(token, &expected))
                        && fs::remove_file(path).is_ok()
                });
                if unlocked {
                    respond(stream,"303 See Other","text/plain","",&format!("Location: /\r\nSet-Cookie: control_operator={}; Path=/; HttpOnly; SameSite=Strict\r\n",self.session));
                    return true;
                }
            }
        }
        if self.authorized(header) {
            return false;
        }
        respond(stream,"401 Unauthorized","text/html; charset=utf-8","<!doctype html><html lang='en'><head><title>Control / operator access</title></head><body><h1>Operator access required</h1><p>On this laptop, run <code>cargo run -- open</code> from the Control directory to open an authenticated browser session.</p><p>Discord users and worker containers cannot obtain operator access from the website.</p></body></html>","");
        true
    }
}
fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &str, extra: &str) {
    let _=write!(stream,"HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}Connection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\n\r\n{body}",body.len());
}
pub fn open(addr: &str) -> io::Result<()> {
    crate::storage::initialize()?;
    let unlock = random()?;
    private_file(
        &crate::config::state("operator-unlock").map_err(io::Error::other)?,
        &unlock,
    )?;
    let port = addr
        .parse::<std::net::SocketAddr>()
        .map_err(io::Error::other)?
        .port();
    // Match Discord's localhost links so browser cookies use the same host.
    let url = format!("http://localhost:{port}/unlock/{unlock}");
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(&url).status()?;
    #[cfg(not(target_os = "macos"))]
    let status = Command::new("xdg-open").arg(&url).status()?;
    if !status.success() {
        return Err(io::Error::other("Could not open the operator browser"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_access_cannot_be_claimed_by_host_header_or_role() {
        let a = Access {
            session: "a".repeat(64),
        };
        assert!(!a.authorized("Host: 127.0.0.1:8080\r\nX-Role: architect\r\n"));
        assert!(!a.authorized("Cookie: control_operator=wrong"));
        assert!(a.authorized(&format!(
            "Cookie: other=1; control_operator={}",
            "a".repeat(64)
        )));
        assert!(!a.authorized(&format!("Cookie: other={}", "a".repeat(64))));
    }
}
