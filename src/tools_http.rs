use crate::{escape, toolbox};
use serde_json::{json, Value};
use std::net::TcpStream;

fn respond(stream: &mut TcpStream, status: &str, mime: &str, body: &str) {
    let _ = control_http::respond(stream, status, mime, body.as_bytes(), "", control_http::Policy::Embedded);
}
pub fn handle(
    stream: &mut TcpStream,
    method: &str,
    target: &str,
    header: &str,
    body: &[u8],
) -> bool {
    let (path, _) = target.split_once('?').unwrap_or((target, ""));
    if path == "/mcp" && method == "POST" {
        let Ok(request) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        let name = request["params"]["name"].as_str().unwrap_or("");
        if request["method"] != "tools/call" || !toolbox::known(name) {
            return false;
        }
        let version = header
            .lines()
            .filter_map(|s| s.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case("mcp-protocol-version"))
            .map(|(_, v)| v.trim());
        if request["jsonrpc"] != "2.0"
            || request.get("id").is_none()
            || version.is_some_and(|v| !control_context::tool_policy::supported(v))
        {
            respond(
                stream,
                "400 Bad Request",
                "text/plain",
                "Invalid MCP request.",
            );
        } else {
            let value = toolbox::result(toolbox::call(name, &request["params"]["arguments"]));
            respond(
                stream,
                "200 OK",
                "application/json",
                &json!({"jsonrpc":"2.0","id":request["id"],"result":value}).to_string(),
            );
        }
        return true;
    }
    if !["/tools/search", "/tools/rust"].contains(&path) {
        return false;
    }
    if !["GET", "POST"].contains(&method) {
        respond(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            "Use GET or POST.",
        );
        return true;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let is_rust = path == "/tools/rust";
    let input = fields
        .get(if is_rust { "code" } else { "query" })
        .map(String::as_str)
        .unwrap_or(if is_rust {
            "fn main() {\n    println!(\"Hello, Control\");\n}"
        } else {
            ""
        });
    let mut content = if is_rust {
        format!("<h2>Rust playground</h2><p class='muted'>Rust 2024 · standard library · offline</p><form method='post' action='/tools/rust'><textarea class='rust-source' name='code' required maxlength='16384' rows='12' aria-label='Rust source'>{}</textarea><button class='primary'>Run</button></form>", escape(input))
    } else {
        format!("<h2>Search</h2><form method='post' action='/tools/search'><label>Search the web<input name='query' required maxlength='500' value='{}'></label><button class='primary'>Search</button></form>", escape(input))
    };
    if method == "POST" {
        let args = if is_rust {
            json!({"code":input})
        } else {
            json!({"query":input})
        };
        match toolbox::call(
            if is_rust {
                "control.rust.run"
            } else {
                "control.web.search"
            },
            &args,
        ) {
            Ok(value) if is_rust => content.push_str(&format!(
                "<p role='status'>{}</p><pre class='tool-output'>{}</pre>",
                escape(value["status"].as_str().unwrap_or("")),
                escape(value["output"].as_str().unwrap_or(""))
            )),
            Ok(value) => {
                let results = value["results"].as_array().unwrap();
                content.push_str(&format!(
                    "<p class='muted' role='status'>{} · {} results{}</p>",
                    escape(value["provider"].as_str().unwrap()),
                    results.len(),
                    if value["fallback"] == true {
                        " · fallback"
                    } else {
                        ""
                    }
                ));
                for result in results {
                    content.push_str(&format!("<article class='record'><h3><a href='{}' rel='noreferrer'>{}</a></h3><p>{}</p></article>", escape(result["url"].as_str().unwrap()), escape(result["title"].as_str().unwrap()), escape(result["snippet"].as_str().unwrap())));
                }
            }
            Err(error) => content.push_str(&format!(
                "<p role='alert'>{}</p>",
                escape(&error.to_string())
            )),
        }
    }
    let page = crate::render(&crate::State::default(), "tool-page", "none", &content);
    respond(stream, "200 OK", "text/html; charset=utf-8", &page);
    true
}
