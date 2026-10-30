use std::{net::TcpStream, path::Path};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub fn ready() -> bool {
    ASSETS.iter().any(|(name, _)| name.ends_with(".wasm")) && asset("index.html").is_some()
}

fn asset(name: &str) -> Option<&'static [u8]> {
    ASSETS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, bytes)| *bytes)
}

pub fn serve(stream: &mut TcpStream, path: &str) {
    if path == "/tool-window/chat-bootstrap.js" {
        let bootstrap = asset("index.html")
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(generated_bootstrap);
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
    let Some(body) = asset(name) else {
        respond(
            stream,
            "404 Not Found",
            "text/plain",
            b"Asset not found.",
            false,
        );
        return;
    };
    let mime = match Path::new(name).extension().and_then(|s| s.to_str()) {
        Some("wasm") => "application/wasm",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    };
    respond(stream, "200 OK", mime, body, true);
}
// Reuse Trunk's generated module verbatim, served externally so chat needs no inline scripts.
fn generated_bootstrap(html: &str) -> Option<&str> {
    let (_, rest) = html.split_once("<script type=\"module\">")?;
    rest.split_once("</script>").map(|(body, _)| body)
}
fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &[u8], scripts: bool) {
    let policy = if scripts {control_http::Policy::ToolWindow} else {control_http::Policy::Asset};
    let _ = control_http::respond(stream, status, mime, body, "", policy);
}
