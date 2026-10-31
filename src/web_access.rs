//! Exact-origin HTTP capabilities. Workers never acquire a network interface.
use crate::{State, identity::Actor};
use reqwest::{Url, blocking::Client, redirect::Policy};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const BODY_LIMIT: usize = 1024 * 1024;
const TEXT_LIMIT: usize = 16_000;
const MAX_GRANTS: usize = 64;
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Origin(String);
impl TryFrom<String> for Origin {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let url = checked_url(&value)
            .map_err(|_| "Expected a public HTTP(S) origin, e.g. https://example.com")?;
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("An origin cannot contain a path, query or fragment");
        }
        Ok(Self(url.origin().ascii_serialization()))
    }
}
impl From<Origin> for String {
    fn from(value: Origin) -> Self {
        value.0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    id: String,
    chat: String,
    actor: Actor,
    origin: Origin,
    issued: u64,
    expires: u64,
}
impl Grant {
    fn permits(&self, chat: &str, actor: &Actor, url: &Url, time: u64) -> bool {
        self.chat == chat
            && &self.actor == actor
            && time >= self.issued
            && time < self.expires
            && self.expires.saturating_sub(self.issued) <= 3600
            && self.origin.0 == url.origin().ascii_serialization()
    }
}
fn checked_url(value: &str) -> io::Result<Url> {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(fail("Invalid URL"));
    }
    let url = Url::parse(value).map_err(|_| fail("Invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default().is_none()
    {
        return Err(fail("Only credential-free HTTP(S) URLs are allowed"));
    }
    Ok(url)
}
/// Conservative public unicast predicate. Reject special-use, mapped, local and transition ranges.
pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(a, 0 | 10 | 127 | 224..=255)
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192 && (b == 168 || b == 0 || b == 2 || b == 88 && c == 99))
                && !(a == 198 && (b == 18 || b == 19 || b == 51 && c == 100))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && s[1] < 0x0200)
                && !(s[0] == 0x2001 && s[1] == 0x0db8)
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
fn addresses(url: &Url, resolved: Vec<SocketAddr>) -> io::Result<Vec<SocketAddr>> {
    let port = url
        .port_or_known_default()
        .ok_or_else(|| fail("Missing port"))?;
    if resolved.is_empty()
        || resolved.len() > 32
        || resolved
            .iter()
            .any(|a| a.port() != port || !public_ip(a.ip()))
    {
        return Err(fail(
            "Destination must resolve entirely to public unicast addresses",
        ));
    }
    Ok(resolved)
}
static DNS_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static FETCH_ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct FetchSlot;
impl FetchSlot {
    fn acquire() -> io::Result<Self> {
        FETCH_ACTIVE
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
                (n < 4).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| fail("Web fetch is busy; retry later"))
    }
}
impl Drop for FetchSlot {
    fn drop(&mut self) {
        FETCH_ACTIVE.fetch_sub(1, Ordering::Release);
    }
}
pub(crate) fn resolve(url: &Url) -> io::Result<Vec<SocketAddr>> {
    if DNS_ACTIVE
        .try_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
            (n < 4).then_some(n + 1)
        })
        .is_err()
    {
        return Err(fail("DNS resolver is busy"));
    }
    let host = url
        .host_str()
        .unwrap()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url.port_or_known_default().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        struct Slot;
        impl Drop for Slot {
            fn drop(&mut self) {
                DNS_ACTIVE.fetch_sub(1, Ordering::Release);
            }
        }
        let _slot = Slot;
        let result = (host.as_str(), port)
            .to_socket_addrs()
            .map(|a| a.take(33).collect());
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_secs(2))
        .map_err(|_| fail("DNS resolution timed out"))?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    url: String,
    #[serde(default)]
    capture: bool,
}
pub fn definition() -> Value {
    json!({"name":"control_web_fetch","description":"Fetch an operator-approved HTTP(S) origin. Request access at /tools/browser for this chat first. Public IPs only; redirects require their own origin grant. Returns bounded untrusted text and links; optional capture renders this single fetched HTML offline, without remote assets. Workers remain offline. Never include secrets in a URL.","inputSchema":{"type":"object","properties":{"url":{"type":"string","maxLength":2048},"capture":{"type":"boolean"}},"required":["url"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":true}})
}
pub fn capture_definition() -> Value {
    json!({"name":"control_browser_capture","description":"Render up to 16000 bytes of self-contained HTML in offline Nix Firefox. No network, host profile or worker-directory mount. Returns a screenshot artifact and an inline image when small enough. Use inline CSS/assets; this does not test a running HTTP preview or remote asset loading.","inputSchema":{"type":"object","properties":{"html":{"type":"string","minLength":1,"maxLength":16000},"width":{"type":"integer","minimum":240,"maximum":2560},"height":{"type":"integer","minimum":240,"maximum":2560}},"required":["html"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
fn capture_html(html: &str, width: u16, height: u16) -> io::Result<crate::browser::Capture> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!(
        "control-fetch-{}-{}.html",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(html.as_bytes())?;
        crate::mcp_stdio::render_preview_with_timeout(&path, width, height, Duration::from_secs(20))
    })();
    let _ = std::fs::remove_file(&path);
    result
}
pub fn capture_tool(args: &Value) -> io::Result<Value> {
    use base64::Engine;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        html: String,
        width: Option<u16>,
        height: Option<u16>,
    }
    let input: Input = serde_json::from_value(args.clone())
        .map_err(|_| fail("Expected HTML and optional viewport"))?;
    if input.html.is_empty() || input.html.len() > 16000 {
        return Err(fail("Expected 1–16000 bytes of HTML"));
    }
    let width = input.width.unwrap_or(1024);
    let height = input.height.unwrap_or(768);
    if !(240..=2560).contains(&width) || !(240..=2560).contains(&height) {
        return Err(fail("Viewport must be 240–2560 pixels per side"));
    }
    let capture = capture_html(&input.html, width, height)?;
    let data = json!({"url":format!("/tools/browser/artifacts/{}",capture.path.file_name().unwrap().to_string_lossy()),"bytes":capture.bytes,"width":width,"height":height,"offline":true});
    let mut content = vec![json!({"type":"text","text":data.to_string()})];
    if capture.bytes <= 256 * 1024 {
        content.push(json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(std::fs::read(capture.path)?)}));
    }
    Ok(json!({"content":content,"structuredContent":data,"isError":false}))
}

#[derive(Debug)]
struct Page {
    url: Url,
    status: u16,
    mime: String,
    body: String,
}
trait Transport {
    fn resolve(&mut self, url: &Url) -> io::Result<Vec<SocketAddr>>;
    fn get(
        &mut self,
        url: &Url,
        addresses: &[SocketAddr],
        remaining: Duration,
    ) -> io::Result<(u16, String, Option<String>, Vec<u8>)>;
}
struct Http;
impl Transport for Http {
    fn resolve(&mut self, url: &Url) -> io::Result<Vec<SocketAddr>> {
        resolve(url)
    }
    fn get(
        &mut self,
        url: &Url,
        addresses: &[SocketAddr],
        remaining: Duration,
    ) -> io::Result<(u16, String, Option<String>, Vec<u8>)> {
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(remaining.min(Duration::from_secs(3)))
            .connect_timeout(Duration::from_secs(2))
            .resolve_to_addrs(url.host_str().unwrap(), addresses)
            .user_agent("Control/0.1 public-page-fetch")
            .build()
            .map_err(io::Error::other)?;
        let mut response = client
            .get(url.clone())
            .header("Accept", "text/html, text/plain;q=0.9")
            .send()
            .map_err(io::Error::other)?;
        let status = response.status().as_u16();
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let location = response
            .headers()
            .get("location")
            .map(|h| h.to_str().map(str::to_owned))
            .transpose()
            .map_err(io::Error::other)?;
        if (300..400).contains(&status) {
            return Ok((status, mime, location, Vec::new()));
        }
        if !matches!(
            mime.as_str(),
            "text/html" | "text/plain" | "application/xhtml+xml"
        ) {
            return Err(fail("Only HTML and plain text pages are supported"));
        }
        if response
            .content_length()
            .is_some_and(|n| n > BODY_LIMIT as u64)
        {
            return Err(fail("Page exceeds 1 MiB"));
        }
        let mut body = Vec::new();
        response
            .by_ref()
            .take(BODY_LIMIT as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > BODY_LIMIT {
            return Err(fail("Page exceeds 1 MiB"));
        }
        Ok((status, mime, location, body))
    }
}
fn fetch(
    transport: &mut impl Transport,
    initial: &str,
    mut allowed: impl FnMut(&Url) -> io::Result<()>,
) -> io::Result<Page> {
    let mut url = checked_url(initial)?;
    let deadline = Instant::now() + Duration::from_secs(8);
    for hop in 0..=4 {
        allowed(&url)?;
        let resolved = addresses(&url, transport.resolve(&url)?)?;
        allowed(&url)?; // DNS resolution may outlive a revocation or lease.
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| fail("Fetch timed out"))?;
        let (status, mime, location, body) = transport.get(&url, &resolved, remaining)?;
        allowed(&url)?; // Do not return evidence after revocation/expiry.
        if [301, 302, 303, 307, 308].contains(&status) {
            if hop == 4 {
                return Err(fail("Redirect limit reached"));
            }
            let next = url
                .join(
                    location
                        .as_deref()
                        .ok_or_else(|| fail("Redirect has no Location"))?,
                )
                .map_err(io::Error::other)?;
            url = checked_url(next.as_str())?;
        } else {
            if !(200..300).contains(&status) {
                return Err(fail("Remote page returned a non-success status"));
            }
            if body.len() > BODY_LIMIT
                || !matches!(
                    mime.as_str(),
                    "text/html" | "text/plain" | "application/xhtml+xml"
                )
            {
                return Err(fail("Expected HTML/plain text of at most 1 MiB"));
            }
            let body = String::from_utf8(body).map_err(|_| fail("Page must be UTF-8"))?;
            return Ok(Page {
                url,
                status,
                mime,
                body,
            });
        }
    }
    unreachable!()
}
fn permitted(state: &State, chat: &str, actor: &Actor, url: &Url, time: u64) -> io::Result<()> {
    crate::identity::authorize(actor, crate::identity::Action::Task).map_err(fail)?;
    if !state
        .chats
        .iter()
        .any(|c| c.id == chat && &c.actor == actor)
        || !state
            .web_grants
            .iter()
            .any(|g| g.permits(chat, actor, url, time))
    {
        return Err(fail(
            "Origin not approved for this chat. Ask the operator to grant access at /tools/browser.",
        ));
    }
    Ok(())
}
pub fn worker(shared: &Arc<Mutex<State>>, run_id: &str, args: &Value) -> io::Result<Value> {
    let _slot = FetchSlot::acquire()?;
    let input: Input = serde_json::from_value(args.clone())
        .map_err(|_| fail("Expected URL and optional capture"))?;
    let (chat, actor, generation) = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let run = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == run_id && r.status == "running")
            .ok_or_else(|| fail("Run is not active"))?;
        (
            run.chat_id
                .clone()
                .ok_or_else(|| fail("A chat is required"))?,
            run.actor.clone(),
            run.attempts.last().and_then(|a| a.workspace_id.clone()),
        )
    };
    let authorize = |url: &Url| {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        if !state.runtime.runs.iter().any(|r| {
            r.id == run_id
                && r.status == "running"
                && r.actor == actor
                && r.attempts.last().and_then(|a| a.workspace_id.as_ref()) == generation.as_ref()
        }) {
            return Err(fail("Attempt is no longer active"));
        }
        permitted(&state, &chat, &actor, url, now())
    };
    let page = fetch(&mut Http, &input.url, authorize)?;
    let final_url = page.url.clone();
    let result = evidence(page, input.capture)?;
    authorize(&final_url)?;
    Ok(result)
}
fn evidence(page: Page, capture: bool) -> io::Result<Value> {
    let document = Html::parse_document(&page.body);
    let normalize = |text: String, limit| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(limit)
            .collect::<String>()
    };
    let title = document
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|e| normalize(e.text().collect(), 300))
        .unwrap_or_default();
    // Exclude script/style content rather than presenting it as source evidence.
    let text = if page.mime == "text/plain" {
        normalize(page.body.clone(), TEXT_LIMIT)
    } else {
        normalize(
            document
                .tree
                .nodes()
                .filter_map(|node| {
                    let text = node.value().as_text()?;
                    if node
                        .ancestors()
                        .filter_map(|n| n.value().as_element())
                        .any(|e| matches!(e.name(), "script" | "style" | "noscript" | "template"))
                    {
                        None
                    } else {
                        Some(text.to_string())
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            TEXT_LIMIT,
        )
    };
    let links = document
        .select(&Selector::parse("a[href]").unwrap())
        .filter_map(|a| {
            let url = page.url.join(a.value().attr("href")?).ok()?;
            let url = checked_url(url.as_str()).ok()?;
            Some(json!({"url":url.as_str(),"text":normalize(a.text().collect(),160)}))
        })
        .take(20)
        .collect::<Vec<_>>();
    let mut result = json!({"url":page.url.as_str(),"status":page.status,"title":title,"text":text,"links":links,"untrusted":true,"body_bytes":page.body.len(),"text_limit":TEXT_LIMIT,"remote_assets_loaded":false});
    if capture {
        let rendering = if page.mime == "text/plain" {
            capture_html(
                &format!("<pre>{}</pre>", crate::escape(&page.body)),
                1280,
                800,
            )
        } else {
            capture_html(&page.body, 1280, 800)
        };
        match rendering {
            Ok(c) => {
                result["capture"] = json!(format!(
                    "/tools/browser/artifacts/{}",
                    c.path.file_name().unwrap().to_string_lossy()
                ))
            }
            Err(_) => {
                result["capture_error"] = json!(
                    "Offline capture unavailable; check the pinned browser image and artifact quota"
                )
            }
        }
    }
    Ok(result)
}

fn change_access(
    state: &mut State,
    fields: &std::collections::HashMap<String, String>,
    issued: u64,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<()> {
    let old = state.web_grants.clone();
    if let Some(id) = fields.get("revoke") {
        state.web_grants.retain(|g| &g.id != id);
    } else {
        let chat = fields.get("chat").ok_or_else(|| fail("Select a chat"))?;
        let actor = state
            .chats
            .iter()
            .find(|c| &c.id == chat)
            .ok_or_else(|| fail("Chat not found"))?
            .actor
            .clone();
        crate::identity::authorize(&actor, crate::identity::Action::Task).map_err(fail)?;
        let origin =
            Origin::try_from(fields.get("origin").cloned().unwrap_or_default()).map_err(fail)?;
        let seconds = fields
            .get("seconds")
            .and_then(|n| n.parse::<u64>().ok())
            .filter(|n| (1..=3600).contains(n))
            .ok_or_else(|| fail("Lease must be 1–3600 seconds"))?;
        let expires = issued
            .checked_add(seconds)
            .ok_or_else(|| fail("Invalid lease"))?;
        let mut retained = state
            .web_grants
            .iter()
            .filter(|g| g.expires > issued)
            .cloned()
            .collect::<Vec<_>>();
        if retained.len() >= MAX_GRANTS {
            return Err(fail("Access grant limit reached; revoke an existing grant"));
        }
        retained.push(Grant {
            id: format!(
                "{}-{}",
                issued,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            chat: chat.clone(),
            actor,
            origin,
            issued,
            expires,
        });
        state.web_grants = retained;
    }
    if let Err(error) = save(state) {
        state.web_grants = old;
        return Err(error);
    }
    Ok(())
}

/// This route runs only behind the authenticated local-operator HTTP gate.
pub fn http(
    stream: &mut std::net::TcpStream,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    body: &[u8],
) -> bool {
    if path != "/tools/browser/access" {
        return false;
    }
    let result = (|| -> io::Result<String> {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        if method == "POST" {
            let fields = crate::fields(&String::from_utf8_lossy(body));
            change_access(&mut state, &fields, now(), crate::storage::save)?;
        } else if method != "GET" {
            return Err(fail("Use GET or POST"));
        }
        let chats = state
            .chats
            .iter()
            .map(|c| {
                format!(
                    "<option value='{}'>{}</option>",
                    crate::escape(&c.id),
                    crate::escape(&c.title)
                )
            })
            .collect::<String>();
        let grants=state.web_grants.iter().filter(|g|g.expires>now()).map(|g|format!("<li>{} · {} · {}s <form method='post' action='/tools/browser/access'><input type='hidden' name='revoke' value='{}'><button>Revoke</button></form></li>",crate::escape(&g.origin.0),crate::escape(&g.chat),g.expires.saturating_sub(now()),crate::escape(&g.id))).collect::<String>();
        Ok(crate::render(
            &state,
            "browser",
            "none",
            &format!(
                "<h2>Web access</h2><form method='post' action='/tools/browser/access'><label>Chat<select name='chat'>{chats}</select></label><label>Origin<input name='origin' type='url' placeholder='https://example.com' required></label><label>Seconds<input name='seconds' type='number' value='900' min='1' max='3600' required></label><button class='primary'>Allow origin</button></form><ul>{grants}</ul><a href='/tools/browser'>Browser</a>"
            ),
        ))
    })();
    let (status, page) = match result {
        Ok(page) => ("200 OK", page),
        Err(error) => (
            "400 Bad Request",
            crate::error_page(&error.to_string(), "/tools/browser/access"),
        ),
    };
    let _ = control_http::respond(
        stream,
        status,
        "text/html; charset=utf-8",
        page.as_bytes(),
        "",
        control_http::Policy::Page,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    fn grant() -> Grant {
        Grant {
            id: "g1".into(),
            chat: "chat-1".into(),
            actor: Actor::LocalOperator,
            origin: Origin::try_from("https://example.com".to_owned()).unwrap(),
            issued: 100,
            expires: 200,
        }
    }
    #[test]
    fn capability_is_exact_origin_owner_and_lease_not_a_domain_suffix() {
        let g = grant();
        let actor = Actor::LocalOperator;
        let url = Url::parse("https://example.com/page").unwrap();
        assert!(g.permits("chat-1", &actor, &url, 100));
        for other in [
            "https://sub.example.com/",
            "http://example.com/",
            "https://example.com:444/",
        ] {
            assert!(!g.permits("chat-1", &actor, &Url::parse(other).unwrap(), 101));
        }
        assert!(!g.permits("chat-2", &actor, &url, 101));
        assert!(!g.permits("chat-1", &Actor::Unknown, &url, 101));
        assert!(!g.permits("chat-1", &actor, &url, 99));
        assert!(!g.permits("chat-1", &actor, &url, 200));
        let mut bad = g.clone();
        bad.expires = 4000;
        assert!(!bad.permits("chat-1", &actor, &url, 101));
        for origin in [
            "file:///etc/passwd",
            "https://user:password@example.com",
            "https://example.com/path",
            "https://example.com?token=x",
            "https://example.com#frag",
        ] {
            assert!(Origin::try_from(origin.to_owned()).is_err());
        }
        assert!(serde_json::from_str::<Origin>("\"file:///etc/passwd\"").is_err());
    }
    #[test]
    fn special_addresses_and_mixed_dns_answers_are_denied() {
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.31.1.1",
            "192.168.1.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2001:20::1",
            "2002:0808:0808::1",
            "3fff::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
        let url = Url::parse("https://example.com/").unwrap();
        assert!(
            addresses(
                &url,
                vec![
                    "8.8.8.8:443".parse().unwrap(),
                    "127.0.0.1:443".parse().unwrap()
                ]
            )
            .is_err()
        );
        assert!(addresses(&url, vec!["8.8.8.8:80".parse().unwrap()]).is_err());
        assert!(addresses(&url, Vec::new()).is_err());
    }
    type Reply = (u16, String, Option<String>, Vec<u8>);
    struct Fixture {
        dns: VecDeque<Vec<SocketAddr>>,
        replies: VecDeque<Reply>,
        requests: Vec<String>,
    }
    impl Fixture {
        fn new(replies: Vec<Reply>) -> Self {
            Self {
                dns: VecDeque::new(),
                replies: replies.into(),
                requests: Vec::new(),
            }
        }
    }
    impl Transport for Fixture {
        fn resolve(&mut self, _: &Url) -> io::Result<Vec<SocketAddr>> {
            Ok(self
                .dns
                .pop_front()
                .unwrap_or_else(|| vec!["8.8.8.8:443".parse().unwrap()]))
        }
        fn get(&mut self, url: &Url, _: &[SocketAddr], _: Duration) -> io::Result<Reply> {
            self.requests.push(url.to_string());
            self.replies
                .pop_front()
                .ok_or_else(|| fail("Unexpected request"))
        }
    }
    fn html(s: &str) -> Reply {
        (200, "text/html".into(), None, s.as_bytes().to_vec())
    }
    fn redirect(url: &str) -> Reply {
        (302, "".into(), Some(url.into()), Vec::new())
    }
    #[test]
    fn redirects_and_dns_rebinding_cannot_escape_the_grant() {
        let g = grant();
        let mut transport = Fixture::new(vec![redirect("https://evil.example/private")]);
        assert!(
            fetch(&mut transport, "https://example.com/", |u| {
                if g.permits("chat-1", &Actor::LocalOperator, u, 101) {
                    Ok(())
                } else {
                    Err(fail("denied"))
                }
            })
            .is_err()
        );
        assert_eq!(transport.requests.len(), 1);
        let mut transport = Fixture::new(vec![redirect("/second"), html("unexpected")]);
        transport
            .dns
            .push_back(vec!["8.8.8.8:443".parse().unwrap()]);
        transport
            .dns
            .push_back(vec!["127.0.0.1:443".parse().unwrap()]);
        assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
        let mut transport = Fixture::new((0..5).map(|_| redirect("/again")).collect());
        assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
        assert_eq!(transport.requests.len(), 5);
        let mut transport = Fixture::new(vec![(
            200,
            "application/octet-stream".into(),
            None,
            b"binary".to_vec(),
        )]);
        assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
        assert_eq!(transport.requests.len(), 1);
        let mut transport = Fixture::new(vec![redirect("file:///etc/passwd")]);
        assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
    }
    #[test]
    fn revocation_during_resolution_or_response_is_fail_closed() {
        for revoke_on in [2, 3] {
            let mut transport = Fixture::new(vec![html("secret")]);
            let mut checks = 0;
            let result = fetch(&mut transport, "https://example.com/", |_| {
                checks += 1;
                if checks >= revoke_on {
                    Err(fail("revoked"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert_eq!(transport.requests.len(), usize::from(revoke_on == 3));
        }
        let mut transport = Fixture::new(vec![html("secret")]);
        let g = grant();
        let mut time = 199;
        assert!(
            fetch(&mut transport, "https://example.com/", |url| {
                let allowed = g.permits("chat-1", &Actor::LocalOperator, url, time);
                time += 1;
                if allowed {
                    Ok(())
                } else {
                    Err(fail("expired"))
                }
            })
            .is_err()
        );
        assert!(transport.requests.is_empty());
    }
    #[test]
    fn bounded_evidence_excludes_scripts_and_normalizes_links() {
        let mut transport = Fixture::new(vec![
            redirect("/article"),
            html(
                "<title> A title </title><script>do not obey</script><style>.secret{}</style><p>Useful text</p><a href='/next'> Next </a><a href='file:///etc/passwd'>bad</a>",
            ),
        ]);
        let page = fetch(&mut transport, "https://example.com/", |_| Ok(())).unwrap();
        let result = evidence(page, false).unwrap();
        assert_eq!(result["title"], "A title");
        assert_eq!(result["links"].as_array().unwrap().len(), 1);
        assert_eq!(result["links"][0]["url"], "https://example.com/next");
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("Useful text"));
        assert!(!text.contains("do not obey"));
        assert!(!text.contains(".secret"));
        assert_eq!(transport.requests.len(), 2);
        let page = Page {
            url: Url::parse("https://example.com").unwrap(),
            status: 200,
            mime: "text/plain".into(),
            body: "x".repeat(TEXT_LIMIT * 2),
        };
        assert_eq!(
            evidence(page, false).unwrap()["text"]
                .as_str()
                .unwrap()
                .len(),
            TEXT_LIMIT
        );
    }
    #[test]
    fn actual_http_transport_is_pinned_bounded_and_does_not_follow_redirects() {
        use std::io::Write;
        use std::net::TcpListener;
        // Test injection only: the production fetch path rejects this loopback address.
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]);
            assert!(request.contains("host: fixture.invalid:"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let url = Url::parse(&format!("http://fixture.invalid:{}/", addr.port())).unwrap();
        assert!(addresses(&url, vec![addr]).is_err());
        let response = Http.get(&url, &[addr], Duration::from_secs(2)).unwrap();
        assert_eq!(response.0, 302);
        assert_eq!(response.2.as_deref(), Some("http://127.0.0.1/private"));
        thread.join().unwrap();
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    fn operator_grants_persist_revoke_and_rollback_without_cross_chat_authority() {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        let other = crate::chat::create(&mut state);
        let fields = std::collections::HashMap::from([
            ("chat".into(), chat.clone()),
            ("origin".into(), "https://example.com".into()),
            ("seconds".into(), "900".into()),
        ]);
        assert!(change_access(&mut state, &fields, 100, |_| Err(fail("disk full"))).is_err());
        assert!(state.web_grants.is_empty());
        let mut saved = Vec::new();
        change_access(&mut state, &fields, 100, |s| {
            saved = serde_json::to_vec(s).unwrap();
            Ok(())
        })
        .unwrap();
        let mut state: State = serde_json::from_slice(&saved).unwrap();
        let url = Url::parse("https://example.com/page").unwrap();
        assert!(permitted(&state, &chat, &Actor::LocalOperator, &url, 101).is_ok());
        assert!(permitted(&state, &other, &Actor::LocalOperator, &url, 101).is_err());
        assert!(permitted(&state, &chat, &Actor::LocalOperator, &url, 1000).is_err());
        let revoke =
            std::collections::HashMap::from([("revoke".into(), state.web_grants[0].id.clone())]);
        assert!(change_access(&mut state, &revoke, 101, |_| Err(fail("disk full"))).is_err());
        assert_eq!(state.web_grants.len(), 1);
        change_access(&mut state, &revoke, 101, |_| Ok(())).unwrap();
        assert!(permitted(&state, &chat, &Actor::LocalOperator, &url, 101).is_err());
    }
    #[test]
    fn worker_cannot_approve_destinations_and_bad_capture_fails_before_docker() {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Build a page", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        let id = run.id.clone();
        let shared = Arc::new(Mutex::new(state));
        let error = worker(&shared, &id, &json!({"url":"https://example.invalid/"})).unwrap_err();
        assert!(error.to_string().contains("Origin not approved"));
        assert!(shared.lock().unwrap().web_grants.is_empty());
        for args in [
            json!({"html":""}),
            json!({"html":"x".repeat(16001)}),
            json!({"html":"<p>x</p>","width":1}),
            json!({"html":"x","origin":"https://example.com"}),
        ] {
            assert!(capture_tool(&args).is_err());
        }
    }
    #[test]
    fn redirect_loops_oversize_and_unsupported_content_are_bounded() {
        struct TooLarge;
        impl Transport for TooLarge {
            fn resolve(&mut self, _: &Url) -> io::Result<Vec<SocketAddr>> {
                Ok(vec!["8.8.8.8:443".parse().unwrap()])
            }
            fn get(
                &mut self,
                _: &Url,
                _: &[SocketAddr],
                _: Duration,
            ) -> io::Result<(u16, String, Option<String>, Vec<u8>)> {
                Ok((200, "text/html".into(), None, vec![b'x'; BODY_LIMIT + 1]))
            }
        }
        assert!(
            fetch(&mut TooLarge, "https://example.com/", |_| Ok(()))
                .unwrap_err()
                .to_string()
                .contains("1 MiB")
        );
    }
    #[test]
    #[ignore = "requires a pinned Nix Firefox image and isolated CONTROL_CONFIG_DIR/CONTROL_STATE_DIR; no network or model calls"]
    fn live_worker_capture_returns_viewable_offline_artifact() {
        use base64::Engine;
        let started = Instant::now();
        let result=capture_tool(&json!({"html":"<!doctype html><html><style>body{background:#17131f;color:#cab4fa;font-family:monospace}h1{font-size:30px}</style><h1>Control webpage demo</h1><p>Offline worker capture</p></html>","width":640,"height":480})).unwrap();
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["offline"], true);
        let data = result["content"][1]["data"]
            .as_str()
            .expect("simple page should fit inline evidence budget");
        let png = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 640);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 480);
        println!(
            "Offline worker capture: {} PNG bytes in {:?}",
            png.len(),
            started.elapsed()
        );
    }
}
