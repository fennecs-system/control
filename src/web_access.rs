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
        if url.path() != "/" || url.query().is_some() {
            return Err("An origin cannot contain a path or query");
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
    let mut url = Url::parse(value).map_err(|_| fail("Invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default().is_none()
    {
        return Err(fail("Only credential-free HTTP(S) URLs are allowed"));
    }
    url.set_fragment(None);
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
struct FetchSlot<'a>(&'a AtomicUsize);
impl<'a> FetchSlot<'a> {
    fn acquire(counter: &'a AtomicUsize) -> io::Result<Self> {
        counter
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
                (n < 4).then_some(n + 1)
            })
            .map(|_| Self(counter))
            .map_err(|_| fail("Web fetch is busy; retry later"))
    }
}
impl Drop for FetchSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
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
    truncated: bool,
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
        let mut body = Vec::new();
        response
            .by_ref()
            .take(BODY_LIMIT as u64 + 1)
            .read_to_end(&mut body)?;
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
            if !matches!(
                mime.as_str(),
                "text/html" | "text/plain" | "application/xhtml+xml"
            ) {
                return Err(fail("Expected HTML/plain text"));
            }
            let truncated = body.len() > BODY_LIMIT;
            let mut body = body;
            body.truncate(BODY_LIMIT);
            let body = match String::from_utf8(body) {
                Ok(body) => body,
                Err(error) if truncated && error.utf8_error().error_len().is_none() => {
                    let valid = error.utf8_error().valid_up_to();
                    let mut bytes = error.into_bytes();
                    bytes.truncate(valid);
                    String::from_utf8(bytes).map_err(|_| fail("Page must be UTF-8"))?
                }
                Err(_) => return Err(fail("Page must be UTF-8")),
            };
            return Ok(Page {
                url,
                status,
                mime,
                body,
                truncated,
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
    fetch_evidence(&mut Http, &input, &FETCH_ACTIVE, authorize, evidence)
}
fn fetch_evidence(
    transport: &mut impl Transport,
    input: &Input,
    counter: &AtomicUsize,
    mut authorize: impl FnMut(&Url) -> io::Result<()>,
    render: impl FnOnce(Page, bool) -> io::Result<Value>,
) -> io::Result<Value> {
    let slot = FetchSlot::acquire(counter)?;
    let page = fetch(transport, &input.url, &mut authorize)?;
    let final_url = page.url.clone();
    // HTTP/DNS capacity protects network work only. Offline Firefox owns its
    // existing separate capture lock/quota and must not reserve a fetch slot.
    drop(slot);
    let result = render(page, input.capture)?;
    authorize(&final_url)?;
    Ok(result)
}
fn evidence(page: Page, capture: bool) -> io::Result<Value> {
    let document = Html::parse_document(&page.body);
    let normalize = |text: String, limit| {
        text.split_whitespace()
            .enumerate()
            .flat_map(|(index, word)| (index > 0).then_some(' ').into_iter().chain(word.chars()))
            .take(limit)
            .collect::<String>()
    };
    let title = document
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|e| normalize(e.text().collect(), 300))
        .unwrap_or_default();
    // Exclude script/style content rather than presenting it as source evidence.
    let mut text = if page.mime == "text/plain" {
        normalize(page.body.clone(), TEXT_LIMIT + 1)
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
            TEXT_LIMIT + 1,
        )
    };
    let text_truncated = if let Some((boundary, _)) = text.char_indices().nth(TEXT_LIMIT) {
        text.truncate(boundary);
        true
    } else {
        false
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
    let mut result = json!({"url":page.url.as_str(),"status":page.status,"title":title,"text":text,"links":links,"untrusted":true,"body_bytes":page.body.len(),"body_limit":BODY_LIMIT,"body_truncated":page.truncated,"text_truncated":text_truncated,"truncated":page.truncated||text_truncated,"text_limit":TEXT_LIMIT,"remote_assets_loaded":false});
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
        let grants=state.web_grants.iter().filter(|g|g.expires>now()).map(|g|format!("<li>{} · {} · {}s <form class='decision-form' method='post' action='/tools/browser/access'><input type='hidden' name='revoke' value='{}'><button>Revoke</button></form></li>",crate::escape(&g.origin.0),crate::escape(&g.chat),g.expires.saturating_sub(now()),crate::escape(&g.id))).collect::<String>();
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
mod tests;

#[cfg(test)]
#[path = "web_access/tests/integration_tests.rs"]
mod integration_tests;
