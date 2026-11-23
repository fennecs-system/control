//! Shared loopback MCP client. No terminal, application state or server routing.
use reqwest::{
    Url,
    blocking::Client,
    header::{ACCEPT, COOKIE, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{fs, io, time::Duration};
const VERSION: &str = control_mcp::LATEST;
pub mod channel;
pub mod chats;
pub mod operator;
pub mod updates;
use control_context::github;
pub struct Hive {
    client: Client,
    endpoint: String,
    next: u64,
    operator_cookie: HeaderValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortDecision {
    Allow,
    Deny,
}

/// The refreshed projection is retained even when approval is not confirmed.
/// A transport failure is an outer error and must not be blindly retried.
pub struct ProjectApproval {
    pub detail: Value,
    pub result: Result<(), String>,
}
impl ProjectApproval {
    fn from_response(detail: Value, request: &str, redirected: bool) -> Self {
        let project = &detail["project"];
        let result = if project["request"] != request {
            Err("Project request changed. Use /open to review it.".into())
        } else if !redirected || project["pending"] == true || !project["grant"].is_string() {
            Err(project["error"]
                .as_str()
                .unwrap_or("Folder was not opened. Use /open to review it or /open /absolute/path to change it.")
                .into())
        } else {
            Ok(())
        };
        Self { detail, result }
    }
}
fn failure(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
}
/// Explains a refused request; the server's plain-text reason is short and never holds credentials.
fn rejection(status: u16, body: &str) -> String {
    match status {
        401 => "Control refused this machine's operator credential (HTTP 401). This command and `control serve` must use the same CONTROL_STATE_DIR; if the server's state was reset or moved, restart `control serve`.".into(),
        413 => "Control rejected the request as too large (HTTP 413).".into(),
        _ => {
            let reason: String = body.trim().chars().take(200).collect();
            if reason.is_empty() || reason.starts_with('<') {
                format!("Control rejected the request (HTTP {status}).")
            } else {
                format!("Control rejected the request (HTTP {status}): {reason}")
            }
        }
    }
}
pub fn local_server(value: &str) -> io::Result<Url> {
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
    pub fn github_write(&mut self, chat: &str, operation: github::Operation) -> io::Result<Value> {
        operation.validate().map_err(failure)?;
        self.tool("control_github_write", json!({"chat_id":chat,"operation":operation}))
    }
    pub fn github_read(&mut self, chat: &str, query: github::Query) -> io::Result<Value> {
        query.validate().map_err(failure)?;
        self.tool("control_github_read", json!({"chat_id":chat,"query":query}))
    }
    #[cfg(feature = "fixture")]
    pub fn fixture(client: Client, endpoint: String) -> Self {
        Self {
            client,
            endpoint,
            next: 0,
            operator_cookie: HeaderValue::from_static("fixture"),
        }
    }
    #[cfg(feature = "fixture")]
    pub fn http(&self) -> &Client {
        &self.client
    }
    #[cfg(feature = "fixture")]
    pub fn operator_cookie(&self) -> &HeaderValue {
        &self.operator_cookie
    }
    /// Submit an operator decision through this connection's authenticated
    /// origin. The server rechecks ownership, request identity and permission.
    pub fn decide_port(&self, chat: &str, request: &str, decision: PortDecision, loopback: bool) -> io::Result<()> {
        let endpoint = Url::parse(&self.endpoint).map_err(failure)?;
        let mut fields = vec![
            ("id", request),
            ("return_chat", chat),
            ("response", "json"),
            ("seconds", "-1"),
        ];
        if matches!(decision, PortDecision::Deny) {
            fields.push(("revoke", "yes"));
        }
        if loopback {
            fields.push(("loopback", "yes"));
        }
        let response = self
            .client
            .post(endpoint.join("/tools/ports").map_err(failure)?)
            .timeout(Duration::from_secs(15))
            .header("Origin", endpoint.origin().ascii_serialization())
            .form(&fields)
            .send()
            .map_err(failure)?;
        let status = response.status();
        let value: Value = response.json().map_err(failure)?;
        if !status.is_success() {
            return Err(failure(value["error"].as_str().unwrap_or("Port decision failed")));
        }
        Ok(())
    }

    /// Accept or reject proposal `claim:N` through the Knowledge → Review form,
    /// which refuses one that is no longer pending.
    pub fn review_knowledge(&self, id: &str, accept: bool) -> io::Result<()> {
        let endpoint = Url::parse(&self.endpoint).map_err(failure)?;
        let index = id
            .strip_prefix("claim:")
            .ok_or_else(|| failure("Knowledge proposal IDs look like claim:N"))?;
        let response = self
            .client
            .post(endpoint.join("/review").map_err(failure)?)
            .timeout(Duration::from_secs(15))
            .header("Origin", endpoint.origin().ascii_serialization())
            .form(&[
                ("id", index),
                ("decision", if accept { "accepted" } else { "rejected" }),
                ("response", "json"),
            ])
            .send()
            .map_err(failure)?;
        let status = response.status();
        let value: Value = response.json().map_err(failure)?;
        if !status.is_success() {
            return Err(failure(value["error"].as_str().unwrap_or("Knowledge review failed")));
        }
        Ok(())
    }

    /// Confirm the exact displayed request and read back its current result.
    /// A redirect by itself is not proof that access was granted.
    pub fn approve_project(&mut self, chat: &str, request: &str) -> io::Result<ProjectApproval> {
        let endpoint = Url::parse(&self.endpoint).map_err(failure)?;
        let mut target = endpoint.join("/").map_err(failure)?;
        target
            .path_segments_mut()
            .map_err(|_| failure("Invalid server URL"))?
            .pop_if_empty()
            .extend(["dm", chat, "project"]);
        let response = self
            .client
            .post(target)
            .timeout(Duration::from_secs(15))
            .header("Origin", endpoint.origin().ascii_serialization())
            .form(&[("approve", "yes"), ("request", request)])
            .send()
            .map_err(|_| failure("Project approval was not confirmed; use /open to refresh before retrying."))?;
        let status = response.status();
        // The server accepts at once and imports in the background; wait for
        // the recorded outcome rather than holding the request open.
        let started = std::time::Instant::now();
        loop {
            let detail = self.tool("control.chat.read", json!({"id":chat}))?;
            if detail["project_importing"] != true || detail["project"]["request"] != request {
                return Ok(ProjectApproval::from_response(detail, request, status.as_u16() == 303));
            }
            if started.elapsed() > Duration::from_secs(1800) {
                return Err(failure("Folder import is still running; use /open to check it later."));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// The accounts cooling down, as the providers page words them.
    pub fn account_notes(&self) -> io::Result<Value> {
        let endpoint = Url::parse(&self.endpoint).map_err(failure)?;
        let response = self
            .client
            .get(endpoint.join("/providers/accounts").map_err(failure)?)
            .send()
            .map_err(failure)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(failure(rejection(status.as_u16(), &body)));
        }
        response.json().map_err(failure)
    }
    /// Clear an account's cooldown through the providers page's form.
    pub fn clear_account(&self, backend: &str, account: &str) -> io::Result<()> {
        let endpoint = Url::parse(&self.endpoint).map_err(failure)?;
        let response = self
            .client
            .post(endpoint.join("/providers").map_err(failure)?)
            .timeout(Duration::from_secs(15))
            .header("Origin", endpoint.origin().ascii_serialization())
            .form(&[("action", "account-clear"), ("provider", backend), ("account", account)])
            .send()
            .map_err(failure)?;
        let status = response.status();
        // The form answers a cleared account by redirecting back to the page.
        if status.is_success() || status.is_redirection() {
            return Ok(());
        }
        let body = response.text().unwrap_or_default();
        Err(failure(rejection(status.as_u16(), &body)))
    }

    /// Dismiss the exact displayed folder request. Returns the server's message;
    /// a request that is no longer pending is a no-op, not an error.
    pub fn dismiss_project(&self, chat: &str, request: &str) -> io::Result<String> {
        let endpoint = Url::parse(&self.endpoint).map_err(failure)?;
        let mut target = endpoint.join("/").map_err(failure)?;
        target
            .path_segments_mut()
            .map_err(|_| failure("Invalid server URL"))?
            .pop_if_empty()
            .extend(["dm", chat, "project"]);
        let response = self
            .client
            .post(target)
            .timeout(Duration::from_secs(15))
            .header("Origin", endpoint.origin().ascii_serialization())
            .form(&[("dismiss", "yes"), ("request", request), ("response", "json")])
            .send()
            .map_err(|_| failure("Dismissal was not confirmed; use /open to check the request."))?;
        let status = response.status();
        let value: Value = response.json().map_err(failure)?;
        if !status.is_success() {
            return Err(failure(
                value["error"].as_str().unwrap_or("Folder request was not dismissed"),
            ));
        }
        Ok(value["message"]
            .as_str()
            .unwrap_or("Folder request dismissed.")
            .to_owned())
    }

    pub fn connect(server: &str) -> io::Result<Self> {
        let url = local_server(server)?;
        let path = std::env::var_os("CONTROL_OPERATOR_FILE")
            .map(std::path::PathBuf::from)
            .map(Ok)
            .unwrap_or_else(|| control_config::state("operator-session"))
            .map_err(failure)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| failure("Start the server first: control serve, then control open."))?;
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(failure("Operator credential must be a private regular file."));
        }
        // Only this application consumes the local operator credential. It is never printed or placed in a URL.
        let token = fs::read_to_string(path).map_err(|_| failure("Cannot load operator credential."))?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(failure("Invalid operator credential."));
        }
        let mut cookie = HeaderValue::from_str(&format!("control_operator={token}"))
            .map_err(|_| failure("Invalid operator credential."))?;
        cookie.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, cookie.clone());
        headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/event-stream"));
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
                        .is_some_and(|tool| tool.route == control_context::tool_policy::Route::Toolbox)
                {
                    Duration::from_secs(25)
                } else if method == "tools/call"
                    && matches!(
                        params["name"].as_str(),
                        Some("control_github_read" | "control_github_write")
                    )
                {
                    // One PR lookup plus one page/mutation, at most 20 s each.
                    Duration::from_secs(50)
                } else if method == "tools/call"
                    && control_context::tool_policy::checkpoint(params["name"].as_str().unwrap_or(""))
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
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            return Err(failure(rejection(status, &body)));
        }
        let data: Value = response.json().map_err(|_| failure("Invalid MCP response."))?;
        if data["id"] != self.next || data.get("error").is_some() {
            return Err(failure("MCP request failed."));
        }
        Ok(data["result"].clone())
    }
    pub fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value> {
        let result = self.rpc("tools/call", json!({"name":name,"arguments":arguments}))?;
        if result["isError"] == true {
            return Err(failure(
                result["content"][0]["text"].as_str().unwrap_or("Tool request failed."),
            ));
        }
        Ok(result["structuredContent"]["data"].clone())
    }
}

#[cfg(test)]
mod tests;
