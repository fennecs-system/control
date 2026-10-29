use std::{fs, io::Write, net::TcpStream, path::Path};

pub fn serve(stream: &mut TcpStream, path: &str) {
    if path == "/tool-window/chat-bootstrap.js" {
        let bootstrap = fs::read_to_string("web/dist/index.html")
            .ok()
            .and_then(|html| generated_bootstrap(&html).map(str::to_owned));
        match bootstrap {
            Some(body) => respond(stream, "200 OK", "text/javascript", body.as_bytes(), false),
            None => respond(
                stream,
                "503 Service Unavailable",
                "text/plain",
                b"Build the Rust/WASM UI with Trunk.",
                false,
            ),
        }
        return;
    }
    let name = path
        .strip_prefix("/tool-window/")
        .filter(|s| !s.is_empty())
        .unwrap_or("index.html");
    if name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        respond(stream, "404 Not Found", "text/plain", b"Not found.", false);
        return;
    }
    let file = Path::new("web/dist").join(name);
    let Ok(body) = fs::read(file) else {
        respond(stream,"503 Service Unavailable","text/html; charset=utf-8",b"<!doctype html><html lang='en'><head><title>Build pending</title></head><body><p>The Rust WASM tool window has not been built. Run Trunk in web/. The native Rust inspector remains available above.</p></body></html>",true);
        return;
    };
    let mime = match Path::new(name).extension().and_then(|s| s.to_str()) {
        Some("wasm") => "application/wasm",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    };
    respond(stream, "200 OK", mime, &body, true);
}
// Reuse Trunk's generated module verbatim, served externally so chat needs no inline scripts.
fn generated_bootstrap(html: &str) -> Option<&str> {
    let (_, rest) = html.split_once("<script type=\"module\">")?;
    rest.split_once("</script>").map(|(body, _)| body)
}
fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &[u8], scripts: bool) {
    // Only the isolated, compiler-generated tool window needs bootstrap scripts.
    let policy = if scripts {
        "default-src 'none'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'; frame-ancestors 'self'; form-action 'none'; base-uri 'none'"
    } else {
        "default-src 'none'; frame-ancestors 'self'"
    };
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nContent-Security-Policy: {policy}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",body.len());
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}
