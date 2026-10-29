use std::{
    fs,
    io::{self, Read, Write},
    net::TcpStream,
};

fn valid_name(name: &str) -> bool {
    name.strip_prefix("capture-")
        .and_then(|s| s.strip_suffix(".png"))
        .is_some_and(|s| !s.is_empty() && s.len() <= 30 && s.bytes().all(|c| c.is_ascii_digit()))
}
fn artifact(name: &str) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_name(name) {
        return Err(io::Error::other("Invalid capture"));
    }
    let dir = crate::config::state("browser").map_err(io::Error::other)?;
    if fs::symlink_metadata(&dir)?.file_type().is_symlink() {
        return Err(io::Error::other("Invalid directory"));
    }
    #[cfg(target_os = "macos")]
    let flags = 0x100 | 0x4;
    #[cfg(not(target_os = "macos"))]
    let flags = 0x20000 | 0x800;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(dir.join(name))?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("Invalid artifact"));
    }
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(io::Error::other("Invalid PNG"));
    }
    Ok(bytes)
}
fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &[u8], extra: &str) {
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{extra}Cache-Control: no-store\r\nReferrer-Policy: same-origin\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(body);
}
pub fn handle(stream: &mut TcpStream, method: &str, target: &str, body: &[u8]) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if !path.starts_with("/tools/browser") {
        return false;
    }
    if method == "GET" {
        if let Some(name) = path.strip_prefix("/tools/browser/artifacts/") {
            match artifact(name) {
                Ok(bytes) => respond(stream, "200 OK", "image/png", &bytes, ""),
                Err(_) => respond(
                    stream,
                    "404 Not Found",
                    "text/plain",
                    b"Capture not found.",
                    "",
                ),
            }
            return true;
        }
        if path == "/tools/browser" {
            let query = crate::fields(query);
            let preview = query.get("capture").filter(|s| valid_name(s)).map(|s| format!("<a href='/tools/browser/artifacts/{s}'><img class='browser-capture' src='/tools/browser/artifacts/{s}' alt='Browser capture'></a>")).unwrap_or_default();
            let content = format!("<h2>Browser</h2><p class='muted'>Offline HTML preview · Firefox</p><form method='post' action='/tools/browser/capture'><label>HTML file<input name='input' required placeholder='path/to/preview.html' maxlength='1000'></label><div class='browser-viewport'><label>Width<input name='width' type='number' min='240' max='2560' value='1280' required></label><label>Height<input name='height' type='number' min='240' max='2560' value='800' required></label></div><button class='primary'>Capture</button></form>{preview}");
            let page = crate::render(&crate::State::default(), "browser", "none", &content);
            respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                page.as_bytes(),
                "",
            );
            return true;
        }
    }
    if path == "/tools/browser/capture" && method == "POST" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let result = (|| -> io::Result<_> {
            let root = fs::canonicalize(".")?;
            let input = crate::mcp_stdio::source(
                &root,
                fields.get("input").map(String::as_str).unwrap_or(""),
            )?;
            let dimension = |key: &str| {
                fields
                    .get(key)
                    .and_then(|s| s.parse::<u16>().ok())
                    .ok_or_else(|| io::Error::other("Expected viewport dimensions"))
            };
            crate::mcp_stdio::render_preview(&input, dimension("width")?, dimension("height")?)
        })();
        match result {
            Ok(capture) => {
                let name = capture.path.file_name().unwrap().to_string_lossy();
                respond(stream, "303 See Other", "text/plain", b"", &format!("Location: /tools/browser?capture={name}\r\n"));
            }
            Err(_) => respond(stream, "400 Bad Request", "text/plain", b"Capture failed. Select an HTML file inside the Control workspace; check the pinned image, container engine, viewport, and artifact quota.", ""),
        }
    } else {
        respond(stream, "404 Not Found", "text/plain", b"Not found.", "");
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn capture_names_cannot_escape_artifact_directory() {
        assert!(super::valid_name("capture-1793199264262426000.png"));
        for name in [
            "../state.json",
            "capture-1/2.png",
            "capture-.png",
            "capture-1.png?secret",
            "capture-ab.png",
        ] {
            assert!(!super::valid_name(name));
        }
    }
}
