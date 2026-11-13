//! Response serialization only. Routing, authorization and state remain in callers.
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Page,
    Embedded,
    Chat,
    /// A top-level page that runs the Rust/WASM client; never framed.
    Client,
    Restricted,
    RestrictedEmbedded,
    Access,
    Asset,
    ToolWindow,
}
impl Policy {
    pub fn csp(self) -> &'static str {
        match self {
            Self::Page => "default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
            Self::Embedded => "default-src 'self'; script-src 'none'; connect-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'",
            Self::Chat => "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'self'; base-uri 'none'",
            Self::Client => "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
            Self::Restricted => "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
            Self::RestrictedEmbedded => "default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'self'",
            Self::Access => "default-src 'none'; frame-ancestors 'none'",
            Self::Asset => "default-src 'none'; frame-ancestors 'self'",
            Self::ToolWindow => "default-src 'none'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'; frame-ancestors 'self'; form-action 'none'; base-uri 'none'",
        }
    }
}

/// Compatibility boundary for existing route handlers. Protocol types validate
/// all fields before the first byte is sent. Extra headers cannot override framing
/// or security headers. Duplicate Set-Cookie headers remain distinct.
pub fn respond(
    output: &mut impl Write,
    status: &str,
    mime: &str,
    body: &[u8],
    extra: &str,
    policy: Policy,
) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid response headers");
    HeaderValue::from_str(status).map_err(|_| invalid())?;
    let status = StatusCode::from_bytes(
        status
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .as_bytes(),
    )
    .map_err(|_| invalid())?;
    let mime = HeaderValue::from_str(mime).map_err(|_| invalid())?;
    let mut headers = HeaderMap::new();
    for line in extra.split("\r\n").filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(invalid)?;
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        if [
            "content-length",
            "content-type",
            "connection",
            "transfer-encoding",
            "cache-control",
            "content-security-policy",
            "referrer-policy",
            "x-content-type-options",
        ]
        .contains(&name.as_str())
        {
            return Err(invalid());
        }
        headers.append(
            name,
            HeaderValue::from_str(value.trim()).map_err(|_| invalid())?,
        );
    }
    if matches!(status.as_u16(), 204 | 304) && !body.is_empty() {
        return Err(invalid());
    }
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    if !matches!(status.as_u16(), 204 | 304) {
        head.push_str(&format!(
            "Content-Type: {}\r\nContent-Length: {}\r\n",
            mime.to_str().map_err(|_| invalid())?,
            body.len()
        ));
    }
    head.push_str(
        "Cache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n",
    );
    head.push_str(if policy == Policy::Access {
        "Referrer-Policy: no-referrer\r\n"
    } else {
        "Referrer-Policy: same-origin\r\n"
    });
    head.push_str(&format!("Content-Security-Policy: {}\r\n", policy.csp()));
    for (name, value) in &headers {
        head.push_str(&format!(
            "{}: {}\r\n",
            name.as_str(),
            value.to_str().map_err(|_| invalid())?
        ));
    }
    head.push_str("\r\n");
    output.write_all(head.as_bytes())?;
    output.write_all(body)
}

#[cfg(test)]
#[path = "lib/tests.rs"]
mod tests;
