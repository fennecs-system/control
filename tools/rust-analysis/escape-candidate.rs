//! Rejected escaping candidate retained as a reproducible MCP benchmark fixture.
pub fn escape_html(text: &str) -> String {
    if text.len() >= 64
        && !text.contains('&')
        && !text.contains('<')
        && !text.contains('>')
        && !text.contains('"')
        && !text.contains('\'')
    {
        return text.to_owned();
    }
    let mut escaped = String::with_capacity(text.len());
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        let replacement = match byte {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\'' => "&#39;",
            _ => continue,
        };
        // An ASCII delimiter is always a UTF-8 boundary.
        escaped.push_str(&text[start..index]);
        escaped.push_str(replacement);
        start = index + 1;
    }
    escaped.push_str(&text[start..]);
    escaped
}
