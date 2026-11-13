//! Shared loopback MCP client. No terminal, application state or server routing.
use reqwest::{
    Url,
    blocking::Client,
    header::{ACCEPT, COOKIE, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{fs, io, time::Duration};
const VERSION: &str = "2025-11-25";
pub struct Hive {
    pub(crate) client: Client,
    pub(crate) endpoint: String,
    pub(crate) next: u64,
    #[cfg(test)]
    pub(crate) operator_cookie: HeaderValue,
}
fn failure(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
}
pub(crate) fn local_server(value: &str) -> io::Result<Url> {
    let url = Url::parse(value).map_err(|_| failure("Invalid server URL."))?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(failure(
            "The local harness requires a loopback HTTP origin without URL credentials.",
        ));
    }
    Ok(url)
}
impl Hive {
    pub fn connect(server: &str) -> io::Result<Self> {
        let url = local_server(server)?;
        let path = std::env::var_os("CONTROL_OPERATOR_FILE")
            .map(std::path::PathBuf::from)
            .map(Ok)
            .unwrap_or_else(|| crate::config::state("operator-session"))
            .map_err(failure)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| {
            failure("Start the server first: ./target/release/control serve, then ./target/release/control open.")
        })?;
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(failure(
                "Operator credential must be a private regular file.",
            ));
        }
        // Only this application consumes the local operator credential. It is never printed or placed in a URL.
        let token =
            fs::read_to_string(path).map_err(|_| failure("Cannot load operator credential."))?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("Invalid operator credential."));
        }
        let mut cookie = HeaderValue::from_str(&format!("control_operator={token}"))
            .map_err(|_| failure("Invalid operator credential."))?;
        cookie.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, cookie.clone());
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert("mcp-protocol-version", HeaderValue::from_static(VERSION));
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| failure("Cannot create the local MCP client."))?;
        let mut hive = Self {
            client,
            endpoint: url.join("mcp").unwrap().to_string(),
            next: 0,
            #[cfg(test)]
            operator_cookie: cookie,
        };
        let init = hive.rpc("initialize", json!({"protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"control-harness","version":env!("CARGO_PKG_VERSION")}}))?;
        if init["protocolVersion"] != VERSION {
            return Err(failure("Server negotiated an unsupported MCP version."));
        }
        let response = hive
            .client
            .post(&hive.endpoint)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .map_err(|_| failure("Could not initialize the MCP session."))?;
        if response.status().as_u16() != 202 {
            return Err(failure("Server rejected MCP initialization."));
        }
        Ok(hive)
    }
    pub fn rpc(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.next += 1;
        let response = self
            .client
            .post(&self.endpoint)
            .timeout(
                if method == "tools/call"
                    && control_context::tool_policy::lookup(params["name"].as_str().unwrap_or(""))
                        .is_some_and(|tool| {
                            tool.route == control_context::tool_policy::Route::Toolbox
                        })
                {
                    Duration::from_secs(25)
                } else if method == "tools/call"
                    && control_context::tool_policy::checkpoint(
                        params["name"].as_str().unwrap_or(""),
                    )
                {
                    // A slow checkpoint must not look like a failure after the server committed it.
                    Duration::from_secs(30)
                } else {
                    Duration::from_secs(3)
                },
            )
            .json(&json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params}))
            .send()
            .map_err(|_| failure("Control is unavailable. Reconnecting on the next refresh."))?;
        if !response.status().is_success() {
            return Err(failure(
                "Control rejected the request. Check local operator access.",
            ));
        }
        let data: Value = response
            .json()
            .map_err(|_| failure("Invalid MCP response."))?;
        if data["id"] != self.next || data.get("error").is_some() {
            return Err(failure("MCP request failed."));
        }
        Ok(data["result"].clone())
    }
    pub fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value> {
        let result = self.rpc("tools/call", json!({"name":name,"arguments":arguments}))?;
        if result["isError"] == true {
            return Err(failure(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap_or("Tool request failed."),
            ));
        }
        Ok(result["structuredContent"]["data"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::local_server;
    #[test]
    fn remote_urls_and_embedded_credentials_are_rejected() {
        assert!(local_server("http://localhost:8080").is_ok());
        for url in ["https://example.com", "http://localhost.evil.test", "http://secret@localhost", "http://localhost/path", "http://localhost/?token=secret"] {
            assert!(local_server(url).is_err());
        }
    }
}
