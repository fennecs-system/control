//! Service-owned TCP ports over verified stdio relays. Containers stay offline.
mod ingress_guard;
#[path = "../tools/tcp-relay/protocol.rs"]
mod protocol;
use crate::{
    State,
    identity::{self, Action, Actor},
    services::{RelayMode, Service},
};
use control_core::resources::{ContainerId, Generation, NodeId, Phase, ResourceId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    num::NonZeroU16,
    process::{Child, ChildStdout},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const MAX_PORTS: usize = 8;
const MAX_RECORDS: usize = 128;
const MAX_CONNECTIONS: usize = 4;
fn fail(s: impl ToString) -> io::Error {
    io::Error::other(s.to_string())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct Name(String);
impl TryFrom<String> for Name {
    type Error = &'static str;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.is_empty()
            || s.len() > 48
            || !s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            Err("Port name must be 1–48 ASCII letters/digits/hyphens")
        } else {
            Ok(Self(s))
        }
    }
}
impl From<Name> for String {
    fn from(n: Name) -> Self {
        n.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Ingress,
    Egress,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    host: String,
    port: NonZeroU16,
}
impl Endpoint {
    fn url(&self) -> io::Result<reqwest::Url> {
        if self.host.is_empty() || self.host.len() > 253 {
            return Err(fail("Invalid target host"));
        }
        let host = if let Ok(ip) = self.host.parse::<IpAddr>() {
            match ip {
                IpAddr::V4(_) => ip.to_string(),
                IpAddr::V6(_) => format!("[{ip}]"),
            }
        } else {
            if !self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
            {
                return Err(fail("Target must be a hostname or literal IP"));
            }
            self.host.to_ascii_lowercase()
        };
        reqwest::Url::parse(&format!("http://{host}:{}/", self.port)).map_err(fail)
    }
    fn pin(&self, allow_loopback: bool) -> io::Result<SocketAddr> {
        let url = self.url()?;
        let answers = crate::web_access::resolve(&url)?;
        validate_destination(self, &answers, allow_loopback)
    }
}
fn validate_destination(
    endpoint: &Endpoint,
    answers: &[SocketAddr],
    allow_loopback: bool,
) -> io::Result<SocketAddr> {
    let explicit_loopback = allow_loopback
        && endpoint
            .host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if answers.is_empty()
        || answers.len() > 32
        || answers.iter().any(|a| {
            a.port() != endpoint.port.get()
                || !(crate::web_access::public_ip(a.ip())
                    || explicit_loopback
                        && a.ip().to_string() == endpoint.host
                        && a.ip().is_loopback())
        })
    {
        return Err(fail(
            "Target must resolve entirely to public addresses; a literal loopback endpoint needs explicit operator approval",
        ));
    }
    Ok(answers[0])
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    Approved,
    Stopping,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    issued: u64,
    expires: u64,
    host_port: u16,
    pinned: Option<SocketAddr>,
    allow_loopback: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    id: ResourceId,
    service: ResourceId,
    generation: Generation,
    container: ContainerId,
    node: NodeId,
    chat: String,
    actor: Actor,
    origin_run: String,
    origin_attempt: String,
    name: Name,
    direction: Direction,
    service_port: NonZeroU16,
    target: Option<Endpoint>,
    status: Status,
    approval: Option<Approval>,
    #[serde(default)]
    diagnostic: String,
    #[serde(skip)]
    ready: bool,
}
impl Port {
    fn summary(&self) -> Value {
        json!({"port_id":self.id,"service_id":self.service,"name":self.name,"direction":self.direction,"service_port":self.service_port,"target":self.target,"status":self.status,"ready":self.ready,"local_address":self.approval.as_ref().filter(|_|self.direction==Direction::Ingress).filter(|a|a.host_port!=0).map(|a|format!("127.0.0.1:{}",a.host_port)),"pinned_target":self.approval.as_ref().and_then(|a|a.pinned),"expires_unix":self.approval.as_ref().map(|a|a.expires),"diagnostic":self.diagnostic,"approval_url":"/tools/ports"})
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    service_id: ResourceId,
    name: Name,
    direction: Direction,
    service_port: NonZeroU16,
    target: Option<Endpoint>,
}
pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"control_port_request","description":"Request operator approval for a named TCP port owned by this chat's running persistent service. ingress exposes non-HTTP/non-TLS raw service loopback on host 127.0.0.1; HTTP artifacts use control_preview_start. egress exposes a service loopback relay to one explicitly approved pinned target, including TLS. Containers remain network=none. No traffic until approved at /tools/ports. A target is required only for egress. Port lifetime cannot exceed the service lease.","inputSchema":{"type":"object","properties":{"service_id":{"type":"string"},"name":{"type":"string","maxLength":48},"direction":{"type":"string","enum":["ingress","egress"]},"service_port":{"type":"integer","minimum":1,"maximum":65535},"target":{"type":"object","properties":{"host":{"type":"string"},"port":{"type":"integer","minimum":1,"maximum":65535}},"required":["host","port"],"additionalProperties":false}},"required":["service_id","name","direction","service_port"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
        json!({"name":"control_port_status","description":"Read owned port approvals, readiness, pinned endpoints, assigned loopback addresses and bounded diagnostics. Omit port_id for active and ten recent terminal records.","inputSchema":{"type":"object","properties":{"port_id":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
        json!({"name":"control_port_stop","description":"Persist revocation of an owned port and close its listener/connections. Stopping is intent; inspect until stopped. Never stops the owning service.","inputSchema":{"type":"object","properties":{"port_id":{"type":"string"}},"required":["port_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
    ]
}
pub fn known(name: &str) -> bool {
    matches!(
        name,
        "control_port_request" | "control_port_status" | "control_port_stop"
    )
}
fn service<'a>(state: &'a State, port: &Port, time: u64) -> io::Result<&'a Service> {
    identity::authorize(&port.actor, Action::Task).map_err(fail)?;
    if !state
        .chats
        .iter()
        .any(|c| c.id == port.chat && c.actor == port.actor)
    {
        return Err(fail("Port owner no longer has this chat"));
    }
    state
        .services
        .iter()
        .find(|s| {
            s.resource.id == port.service
                && s.resource.generation == port.generation
                && s.owned_by(&port.node, &port.chat, &port.actor)
                && matches!(&s.resource.phase,Phase::Running(id)if id==&port.container)
                && !s.resource.lease.expired(time)
        })
        .ok_or_else(|| fail("Port service ownership, generation or lifetime changed"))
}
fn authorized(state: &State, port: &Port, time: u64) -> io::Result<Service> {
    let current = state
        .ports
        .iter()
        .find(|p| p.id == port.id && p.status == Status::Approved && p.approval == port.approval)
        .ok_or_else(|| fail("Port was revoked"))?;
    let approval = current
        .approval
        .as_ref()
        .ok_or_else(|| fail("Port has no approval"))?;
    if time < approval.issued
        || time >= approval.expires
        || approval.expires.saturating_sub(approval.issued) > 3600
    {
        return Err(fail("Port approval expired"));
    }
    let service = service(state, current, time)?;
    if approval.expires > service.resource.lease.expires() {
        return Err(fail("Port lease exceeds service lease"));
    }
    match current.direction {
        Direction::Ingress if approval.pinned.is_none() => {}
        Direction::Egress => {
            let endpoint = current
                .target
                .as_ref()
                .ok_or_else(|| fail("Missing target"))?;
            validate_destination(
                endpoint,
                &[approval
                    .pinned
                    .ok_or_else(|| fail("Missing pinned target"))?],
                approval.allow_loopback,
            )?;
        }
        _ => return Err(fail("Invalid port approval")),
    }
    Ok(service.clone())
}
fn admit(
    state: &mut State,
    run_id: &str,
    generation: &str,
    nonce: &str,
    input: Input,
    node: NodeId,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<Value> {
    let run = state
        .runtime
        .runs
        .iter()
        .find(|r| r.id == run_id && r.status == "running")
        .ok_or_else(|| fail("Run is not active"))?;
    identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::is_judge(run)
        || run.attempts.last().and_then(|a| a.workspace_id.as_deref()) != Some(generation)
    {
        return Err(fail("Inactive or read-only attempt"));
    }
    let chat = run
        .chat_id
        .as_ref()
        .ok_or_else(|| fail("Port requires a chat"))?;
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(fail("Invalid request nonce"));
    }
    if (input.direction == Direction::Egress) != input.target.is_some() {
        return Err(fail("Only egress requests require a target"));
    }
    if let Some(target) = &input.target {
        target.url()?;
    }
    let service = state
        .services
        .iter()
        .find(|s| {
            s.resource.id == input.service_id
                && s.owned_by(&node, chat, &run.actor)
                && matches!(s.resource.phase, Phase::Running(_))
                && !s.resource.lease.expired(now())
        })
        .ok_or_else(|| fail("A running service owned by this chat is required"))?;
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{}\0{chat}\0{run_id}\0{generation}\0{nonce}", node.as_str()).as_bytes(),
    );
    let suffix = digest.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let id = ResourceId::try_from(format!("port-{suffix}")).map_err(fail)?;
    if let Some(old) = state.ports.iter().find(|p| p.id == id) {
        if old.service != input.service_id
            || old.name != input.name
            || old.direction != input.direction
            || old.service_port != input.service_port
            || old.target != input.target
        {
            return Err(fail("Nonce reused with a different port request"));
        }
        return Ok(old.summary());
    }
    if state
        .ports
        .iter()
        .filter(|p| p.status != Status::Stopped)
        .count()
        >= MAX_PORTS
    {
        return Err(fail("Active port limit reached"));
    }
    if state.ports.iter().any(|p| {
        p.service == input.service_id
            && p.status != Status::Stopped
            && (p.name == input.name
                || (p.direction == Direction::Egress
                    && input.direction == Direction::Egress
                    && p.service_port == input.service_port))
    }) {
        return Err(fail(
            "Service port name or egress listener is already reserved",
        ));
    }
    let port = Port {
        id,
        service: input.service_id,
        generation: service.resource.generation,
        container: service.resource.phase.container().unwrap().clone(),
        node,
        chat: chat.clone(),
        actor: run.actor.clone(),
        origin_run: run_id.into(),
        origin_attempt: generation.into(),
        name: input.name,
        direction: input.direction,
        service_port: input.service_port,
        target: input.target,
        status: Status::Pending,
        approval: None,
        diagnostic: String::new(),
        ready: false,
    };
    let retired = if state.ports.len() >= MAX_RECORDS {
        let index = state
            .ports
            .iter()
            .position(|p| {
                p.status == Status::Stopped
                    && !state.runtime.runs.iter().any(|r| {
                        r.id == p.origin_run
                            && r.status == "running"
                            && r.attempts.last().and_then(|a| a.workspace_id.as_deref())
                                == Some(&p.origin_attempt)
                    })
            })
            .ok_or_else(|| fail("Port history belongs to active attempts; begin a new turn"))?;
        Some((index, state.ports.remove(index)))
    } else {
        None
    };
    let value = port.summary();
    state.ports.push(port);
    if let Err(error) = save(state) {
        state.ports.pop();
        if let Some((i, p)) = retired {
            state.ports.insert(i, p);
        }
        return Err(error);
    }
    Ok(value)
}
pub fn call(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    nonce: &str,
    name: &str,
    args: &Value,
) -> io::Result<Value> {
    let node = NodeId::try_from(crate::config::node_id().map_err(fail)?).map_err(fail)?;
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    if name == "control_port_request" {
        return admit(
            &mut state,
            run,
            generation,
            nonce,
            serde_json::from_value(args.clone()).map_err(fail)?,
            node,
            crate::storage::save,
        );
    }
    let r = state
        .runtime
        .runs
        .iter()
        .find(|r| {
            r.id == run
                && r.status == "running"
                && r.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(generation)
        })
        .ok_or_else(|| fail("Attempt is not active"))?;
    identity::authorize(&r.actor, r.action).map_err(fail)?;
    if crate::delegation::is_judge(r) {
        return Err(fail("Judge cannot manage ports"));
    }
    let actor = r.actor.clone();
    let chat = r
        .chat_id
        .clone()
        .ok_or_else(|| fail("A chat is required"))?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Id {
        port_id: Option<ResourceId>,
    }
    let id: Id = serde_json::from_value(args.clone()).map_err(fail)?;
    let owned = |p: &&Port| p.node == node && p.chat == chat && p.actor == actor;
    if name == "control_port_status" && id.port_id.is_none() {
        return Ok(
            json!({"ports":state.ports.iter().filter(owned).filter(|p|p.status!=Status::Stopped).chain(state.ports.iter().rev().filter(owned).filter(|p|p.status==Status::Stopped).take(10)).map(Port::summary).collect::<Vec<_>>()}),
        );
    }
    let index = state
        .ports
        .iter()
        .position(|p| {
            p.node == node
                && p.chat == chat
                && p.actor == actor
                && Some(&p.id) == id.port_id.as_ref()
        })
        .ok_or_else(|| fail("Owned port not found"))?;
    if name == "control_port_stop" && state.ports[index].status != Status::Stopped {
        let old = state.ports[index].status;
        state.ports[index].status = Status::Stopping;
        if let Err(e) = crate::storage::save(&state) {
            state.ports[index].status = old;
            return Err(e);
        }
    }
    Ok(state.ports[index].summary())
}

fn approve(
    state: &mut State,
    id: &str,
    seconds: u64,
    allow_loopback: bool,
    pinned: Option<SocketAddr>,
    time: u64,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<()> {
    let index = state
        .ports
        .iter()
        .position(|p| p.id.as_str() == id && p.status == Status::Pending)
        .ok_or_else(|| fail("Pending request not found"))?;
    let port = &state.ports[index];
    let service = service(state, port, time)?;
    let expires = time
        .checked_add(seconds)
        .filter(|end| (1..=3600).contains(&seconds) && *end <= service.resource.lease.expires())
        .ok_or_else(|| {
            fail("Choose a lease within the remaining service lifetime (maximum one hour)")
        })?;
    match port.direction {
        Direction::Ingress if pinned.is_none() => {}
        Direction::Egress => {
            validate_destination(
                port.target.as_ref().ok_or_else(|| fail("Missing target"))?,
                &[pinned.ok_or_else(|| fail("Missing pinned address"))?],
                allow_loopback,
            )?;
        }
        _ => return Err(fail("Invalid approval")),
    }
    let old = state.ports[index].clone();
    state.ports[index].approval = Some(Approval {
        issued: time,
        expires,
        host_port: 0,
        pinned,
        allow_loopback,
    });
    state.ports[index].status = Status::Approved;
    state.ports[index].diagnostic.clear();
    if let Err(error) = save(state) {
        state.ports[index] = old;
        return Err(error);
    }
    Ok(())
}
pub fn http(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    body: &[u8],
) -> bool {
    if path != "/tools/ports" {
        return false;
    }
    let result = (|| -> io::Result<String> {
        if method == "POST" {
            let fields = crate::fields(&String::from_utf8_lossy(body));
            let id = fields.get("id").ok_or_else(|| fail("Missing port ID"))?;
            if fields.contains_key("revoke") {
                let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
                let p = state
                    .ports
                    .iter_mut()
                    .find(|p| p.id.as_str() == id)
                    .ok_or_else(|| fail("Port not found"))?;
                let old = p.status;
                if old != Status::Stopped {
                    p.status = Status::Stopping;
                }
                if let Err(e) = crate::storage::save(&state) {
                    state
                        .ports
                        .iter_mut()
                        .find(|p| p.id.as_str() == id)
                        .unwrap()
                        .status = old;
                    return Err(e);
                }
            } else {
                let (request, service) = {
                    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
                    let p = state
                        .ports
                        .iter()
                        .find(|p| p.id.as_str() == id && p.status == Status::Pending)
                        .ok_or_else(|| fail("Pending port not found"))?;
                    (p.clone(), service(&state, p, now())?.clone())
                };
                if request.node.as_str() != crate::config::node_id().map_err(fail)? {
                    return Err(fail("Port belongs to another node"));
                }
                crate::services::relay_capable(&service)?;
                crate::services::port_binding(&service, &request.container)?;
                let allow_loopback = fields.get("loopback").is_some_and(|v| v == "yes");
                let pinned = request
                    .target
                    .as_ref()
                    .map(|e| e.pin(allow_loopback))
                    .transpose()?;
                let seconds = fields
                    .get("seconds")
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| fail("Expected lease seconds"))?;
                let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
                approve(
                    &mut state,
                    id,
                    seconds,
                    allow_loopback,
                    pinned,
                    now(),
                    crate::storage::save,
                )?;
            }
        } else if method != "GET" {
            return Err(fail("Use GET or POST"));
        }
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let ports=state.ports.iter().rev().filter(|p|p.status!=Status::Stopped).take(MAX_PORTS).map(|p|{
            let service=state.services.iter().find(|s|s.resource.id==p.service);let ttl=service.map(|s|s.resource.lease.expires().saturating_sub(now()).min(3600)).unwrap_or(0);
            let detail=match p.direction{Direction::Ingress=>format!("host loopback → service :{}",p.service_port),Direction::Egress=>format!("service :{} → {}:{}",p.service_port,crate::escape(&p.target.as_ref().map(|e|e.host.clone()).unwrap_or_default()),p.target.as_ref().map(|e|e.port.get()).unwrap_or(0))};
            let action=if p.status==Status::Pending{format!("<label>Seconds<input name='seconds' type='number' min='1' max='{}' value='{}' required></label>{}<button class='primary'>Allow port</button><button name='revoke' value='yes' formnovalidate>Deny</button>",ttl,ttl.min(900),if p.direction==Direction::Egress{"<label><input type='checkbox' name='loopback' value='yes'> Allow this exact literal loopback target</label>"}else{""})}else{"<button name='revoke' value='yes'>Revoke</button>".into()};
            let address=p.approval.as_ref().filter(|a|a.host_port!=0).map(|a|format!(" · 127.0.0.1:{}",a.host_port)).unwrap_or_default();
            format!("<article><h3>{}</h3><p>{detail}{address}</p><p>{:?} · {} · {} · {}</p><p>{}</p><form method='post' action='/tools/ports'><input type='hidden' name='id' value='{}'>{action}</form></article>",crate::escape(&p.name.0),p.status,crate::escape(&p.chat),if p.ready{"ready"}else{"waiting"},crate::escape(p.service.as_str()),crate::escape(&p.diagnostic),crate::escape(p.id.as_str()))
        }).collect::<String>();
        Ok(crate::render(
            &state,
            "tool-page",
            "none",
            &format!(
                "<h2>TCP ports</h2>{ports}<p class='muted'>Request a named service port from chat. Access remains limited to this service and lease.</p>"
            ),
        ))
    })();
    let (status, page) = match result {
        Ok(p) => ("200 OK", p),
        Err(e) => (
            "400 Bad Request",
            crate::error_page(&e.to_string(), "/tools/ports"),
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
fn live_service(
    shared: &Arc<Mutex<State>>,
    port: &Port,
    cancel: &AtomicBool,
) -> io::Result<Service> {
    if cancel.load(Ordering::Acquire) || crate::shutdown::requested() {
        return Err(fail("Port is stopping"));
    }
    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
    authorized(&state, port, now())
}
fn receipt(shared: &Arc<Mutex<State>>, port: &Port, ready: bool, diagnostic: &str) {
    if let Ok(mut state) = shared.lock()
        && let Some(p) = state.ports.iter_mut().find(|p| {
            p.id == port.id && p.approval == port.approval && p.status == Status::Approved
        })
    {
        p.ready = ready;
        let message = diagnostic.chars().take(256).collect::<String>();
        if p.diagnostic != message {
            let old = std::mem::replace(&mut p.diagnostic, message);
            if crate::storage::save(&state).is_err()
                && let Some(p) = state.ports.iter_mut().find(|p| p.id == port.id)
            {
                p.diagnostic = old;
            }
        }
    }
}
fn handshake(
    child: &mut Child,
    expected: &'static [u8],
    wait: Duration,
    shared: &Arc<Mutex<State>>,
    port: &Port,
    cancel: &AtomicBool,
) -> io::Result<ChildStdout> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("Relay output unavailable"))?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = vec![0; expected.len()];
        let result = stdout.read_exact(&mut bytes).and_then(|()| {
            if bytes == expected {
                Ok(stdout)
            } else {
                Err(fail("Unexpected relay handshake"))
            }
        });
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + wait;
    let result = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(fail("Relay handshake closed"));
            }
            Err(_) => {}
        }
        if Instant::now() >= deadline || live_service(shared, port, cancel).is_err() {
            break Err(fail("Relay handshake expired or was revoked"));
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = reader.join();
    result
}
struct Relay {
    child: Child,
    input: Arc<Mutex<std::process::ChildStdin>>,
    alive: Arc<AtomicBool>,
    heartbeat: Option<JoinHandle<()>>,
    credit: Arc<protocol::Credit>,
    may_receive: Arc<AtomicBool>,
}
impl Relay {
    fn new(mut child: Child) -> io::Result<Self> {
        let input = Arc::new(Mutex::new(
            child
                .stdin
                .take()
                .ok_or_else(|| fail("Relay input unavailable"))?,
        ));
        let alive = Arc::new(AtomicBool::new(true));
        if let Err(error) = protocol::write(
            &mut *input.lock().map_err(|_| fail("Relay unavailable"))?,
            protocol::Kind::Credit,
            &[],
        ) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let writer = input.clone();
        let signal = alive.clone();
        let heartbeat = thread::spawn(move || {
            while signal.load(Ordering::Acquire) {
                if writer
                    .lock()
                    .map_err(|_| fail("Relay unavailable"))
                    .and_then(|mut w| protocol::write(&mut *w, protocol::Kind::Ping, &[]))
                    .is_err()
                {
                    signal.store(false, Ordering::Release);
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
        });
        Ok(Self {
            child,
            input,
            alive,
            heartbeat: Some(heartbeat),
            credit: Arc::new(protocol::Credit::default()),
            may_receive: Arc::new(AtomicBool::new(true)),
        })
    }
}
impl std::ops::Deref for Relay {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for Relay {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.heartbeat.take() {
            let _ = t.join();
        }
    }
}
fn bridge(
    mut socket: TcpStream,
    child: Relay,
    mut output: ChildStdout,
    shared: &Arc<Mutex<State>>,
    port: &Port,
    cancel: &AtomicBool,
) -> io::Result<()> {
    live_service(shared, port, cancel)?;
    // BSD can inherit nonblocking mode from the listening socket.
    socket.set_nonblocking(false)?;
    socket.set_nodelay(true)?;
    let mut reading = socket.try_clone()?;
    let closing = socket.try_clone()?;
    let input = child.input.clone();
    let credit = child.credit.clone();
    let alive = child.alive.clone();
    let done_write = Arc::new(AtomicBool::new(false));
    let done_read = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let a = done_write.clone();
    let port_direction = port.direction;
    let failure = failed.clone();
    let writer = thread::spawn(move || {
        let mut buffer = [0; protocol::LIMIT];
        if port_direction == Direction::Ingress {
            match ingress_guard::first_chunk(&mut reading) {
                Ok(bytes) if !bytes.is_empty() => {
                    if credit
                        .take(&alive)
                        .and_then(|()| {
                            input
                                .lock()
                                .map_err(|_| fail("Relay unavailable"))
                                .and_then(|mut w| {
                                    protocol::write(&mut *w, protocol::Kind::Data, &bytes)
                                })
                        })
                        .is_err()
                    {
                        failure.store(true, Ordering::Release);
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    failure.store(true, Ordering::Release);
                    return;
                }
            }
        }
        loop {
            if credit.take(&alive).is_err() {
                failure.store(true, Ordering::Release);
                break;
            }
            let (kind, size) = match reading.read(&mut buffer) {
                Ok(0) => (protocol::Kind::Fin, 0),
                Ok(n) => (protocol::Kind::Data, n),
                Err(_) => {
                    failure.store(true, Ordering::Release);
                    break;
                }
            };
            if input
                .lock()
                .map_err(|_| fail("Relay unavailable"))
                .and_then(|mut input| protocol::write(&mut *input, kind, &buffer[..size]))
                .is_err()
            {
                failure.store(true, Ordering::Release);
                break;
            }
            if kind == protocol::Kind::Fin {
                a.store(true, Ordering::Release);
                break;
            }
        }
    });
    let b = done_read.clone();
    let failure = failed.clone();
    let input = child.input.clone();
    let permission = child.may_receive.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Option<Vec<u8>>>(1);
    let delivery = thread::spawn(move || {
        for data in rx {
            match data {
                Some(data) => {
                    if socket.write_all(&data).is_err() {
                        failure.store(true, Ordering::Release);
                        break;
                    }
                    permission.store(true, Ordering::Release);
                    if input
                        .lock()
                        .map_err(|_| fail("Relay unavailable"))
                        .and_then(|mut w| protocol::write(&mut *w, protocol::Kind::Credit, &[]))
                        .is_err()
                    {
                        failure.store(true, Ordering::Release);
                        break;
                    }
                }
                None => {
                    let _ = socket.shutdown(Shutdown::Write);
                    b.store(true, Ordering::Release);
                    break;
                }
            }
        }
    });
    let failure = failed.clone();
    let credit = child.credit.clone();
    let permission = child.may_receive.clone();
    let output_closed = Arc::new(AtomicBool::new(false));
    let closed = output_closed.clone();
    let reader = thread::spawn(move || {
        let mut buffer = [0; protocol::LIMIT];
        let mut fin = false;
        loop {
            match protocol::read(&mut output, &mut buffer) {
                Ok((protocol::Kind::Credit, _)) => {
                    if credit.grant().is_err() {
                        failure.store(true, Ordering::Release);
                        break;
                    }
                }
                Ok((protocol::Kind::Data, n))
                    if !fin && permission.swap(false, Ordering::AcqRel) =>
                {
                    if tx.try_send(Some(buffer[..n].to_vec())).is_err() {
                        failure.store(true, Ordering::Release);
                        break;
                    }
                }
                Ok((protocol::Kind::Fin, _))
                    if !fin && permission.swap(false, Ordering::AcqRel) =>
                {
                    fin = true;
                    if tx.try_send(None).is_err() {
                        failure.store(true, Ordering::Release);
                        break;
                    }
                }
                Err(error) if fin && error.kind() == io::ErrorKind::UnexpectedEof => break,
                _ => {
                    failure.store(true, Ordering::Release);
                    break;
                }
            }
        }
        closed.store(true, Ordering::Release);
    });
    while !(done_read.load(Ordering::Acquire) && done_write.load(Ordering::Acquire))
        && !failed.load(Ordering::Acquire)
        && live_service(shared, port, cancel).is_ok()
    {
        if output_closed.load(Ordering::Acquire) && done_read.load(Ordering::Acquire) {
            let _ = closing.shutdown(Shutdown::Read);
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = closing.shutdown(Shutdown::Both);
    drop(child);
    let _ = writer.join();
    let _ = reader.join();
    let _ = delivery.join();
    Ok(())
}
fn connect_relay(
    shared: &Arc<Mutex<State>>,
    port: &Port,
    cancel: &AtomicBool,
) -> io::Result<(Relay, ChildStdout)> {
    let service = live_service(shared, port, cancel)?;
    let binding = crate::services::port_binding(&service, &port.container)?;
    live_service(shared, port, cancel)?;
    let mut child = Relay::new(binding.spawn(
        RelayMode::Connect,
        port.service_port,
        port.approval.as_ref().unwrap().expires,
    )?)?;
    let output = handshake(
        &mut child,
        b"CONTROL_RELAY_V2_READY\n",
        Duration::from_secs(3),
        shared,
        port,
        cancel,
    )?;
    Ok((child, output))
}
fn ingress(shared: Arc<Mutex<State>>, mut port: Port, cancel: Arc<AtomicBool>) -> io::Result<()> {
    let service = live_service(&shared, &port, &cancel)?;
    crate::services::port_binding(&service, &port.container)?;
    let listener = TcpListener::bind(("127.0.0.1", port.approval.as_ref().unwrap().host_port))?;
    listener.set_nonblocking(true)?;
    let assigned = listener.local_addr()?.port();
    if port.approval.as_ref().unwrap().host_port == 0 {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        authorized(&state, &port, now())?;
        let index = state.ports.iter().position(|p| p.id == port.id).unwrap();
        state.ports[index].approval.as_mut().unwrap().host_port = assigned;
        if let Err(error) = crate::storage::save(&state) {
            state.ports[index].approval.as_mut().unwrap().host_port = 0;
            return Err(error);
        }
        port.approval.as_mut().unwrap().host_port = assigned;
    }
    receipt(&shared, &port, true, "");
    let active = Arc::new(AtomicUsize::new(0));
    let mut sessions: Vec<JoinHandle<()>> = Vec::new();
    while live_service(&shared, &port, &cancel).is_ok() {
        sessions.retain(|s| !s.is_finished());
        match listener.accept() {
            Ok((socket, _)) => {
                if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                    drop(socket);
                    continue;
                }
                active.fetch_add(1, Ordering::AcqRel);
                let active = active.clone();
                let shared = shared.clone();
                let port = port.clone();
                let cancel = cancel.clone();
                sessions.push(thread::spawn(move || {
                    let result =
                        connect_relay(&shared, &port, &cancel).and_then(|(child, output)| {
                            bridge(socket, child, output, &shared, &port, &cancel)
                        });
                    if let Err(error) = result {
                        receipt(&shared, &port, true, &error.to_string());
                    }
                    active.fetch_sub(1, Ordering::Release);
                }));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        thread::sleep(Duration::from_millis(25));
    }
    drop(listener);
    cancel.store(true, Ordering::Release);
    for s in sessions {
        let _ = s.join();
    }
    receipt(&shared, &port, false, "");
    Ok(())
}
fn egress(shared: Arc<Mutex<State>>, port: Port, cancel: Arc<AtomicBool>) -> io::Result<()> {
    while let Ok(service) = live_service(&shared, &port, &cancel) {
        let result = (|| {
            let binding = crate::services::port_binding(&service, &port.container)?;
            live_service(&shared, &port, &cancel)?;
            let approval = port.approval.as_ref().unwrap();
            let mut child = Relay::new(binding.spawn(
                RelayMode::Listen,
                port.service_port,
                approval.expires,
            )?)?;
            let output = handshake(
                &mut child,
                b"CONTROL_RELAY_V2_READY\n",
                Duration::from_secs(3),
                &shared,
                &port,
                &cancel,
            )?;
            child.stdout = Some(output);
            receipt(&shared, &port, true, "");
            let output = handshake(
                &mut child,
                b"CONTROL_RELAY_V2_CONNECTED\n",
                Duration::from_secs(approval.expires.saturating_sub(now())),
                &shared,
                &port,
                &cancel,
            )?;
            live_service(&shared, &port, &cancel)?;
            let socket = match TcpStream::connect_timeout(
                &approval
                    .pinned
                    .ok_or_else(|| fail("Missing pinned target"))?,
                Duration::from_secs(2),
            ) {
                Ok(s) => s,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            };
            bridge(socket, child, output, &shared, &port, &cancel)
        })();
        if let Err(error) = result {
            receipt(&shared, &port, false, &error.to_string());
        }
        for _ in 0..10 {
            if live_service(&shared, &port, &cancel).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    receipt(&shared, &port, false, "");
    Ok(())
}
struct Active {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intent {
    Wait,
    Serve,
    Stop,
}
fn intent(state: &State, port: &Port, node: &NodeId, time: u64) -> Intent {
    if &port.node != node {
        return Intent::Stop;
    }
    match port.status {
        Status::Pending if service(state, port, time).is_ok() => Intent::Wait,
        Status::Approved if authorized(state, port, time).is_ok() => Intent::Serve,
        _ => Intent::Stop,
    }
}
fn stop_record(shared: &Arc<Mutex<State>>, id: &ResourceId) {
    if let Ok(mut state) = shared.lock()
        && let Some(p) = state.ports.iter_mut().find(|p| &p.id == id)
    {
        let old = p.status;
        p.status = Status::Stopped;
        p.ready = false;
        if crate::storage::save(&state).is_err() {
            state.ports.iter_mut().find(|p| &p.id == id).unwrap().status = old;
        }
    }
}
pub fn supervise(shared: Arc<Mutex<State>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let Ok(node) = crate::config::node_id()
            .and_then(|s| NodeId::try_from(s).map_err(|_| "Invalid node ID"))
        else {
            return;
        };
        let mut active: HashMap<ResourceId, Active> = HashMap::new();
        while !crate::shutdown::requested() {
            let ports = match shared.lock() {
                Ok(s) => s
                    .ports
                    .iter()
                    .filter(|p| p.status != Status::Stopped)
                    .cloned()
                    .collect::<Vec<_>>(),
                Err(_) => break,
            };
            for port in ports {
                let decision = shared
                    .lock()
                    .map(|s| intent(&s, &port, &node, now()))
                    .unwrap_or(Intent::Stop);
                if decision != Intent::Serve {
                    if let Some(a) = active.remove(&port.id) {
                        a.cancel.store(true, Ordering::Release);
                        let _ = a.handle.join();
                    }
                    if decision == Intent::Stop {
                        stop_record(&shared, &port.id);
                    }
                    continue;
                }
                if active.get(&port.id).is_some_and(|a| a.handle.is_finished())
                    && let Some(a) = active.remove(&port.id)
                {
                    let _ = a.handle.join();
                }
                if !active.contains_key(&port.id) {
                    let cancel = Arc::new(AtomicBool::new(false));
                    let signal = cancel.clone();
                    let state = shared.clone();
                    let id = port.id.clone();
                    let handle = thread::spawn(move || {
                        let copy = port.clone();
                        let result = match port.direction {
                            Direction::Ingress => ingress(state.clone(), port, signal),
                            Direction::Egress => egress(state.clone(), port, signal),
                        };
                        if let Err(error) = result {
                            receipt(&state, &copy, false, &error.to_string());
                            thread::sleep(Duration::from_secs(1));
                        }
                    });
                    active.insert(id, Active { cancel, handle });
                }
            }
            crate::shutdown::pause(Duration::from_millis(200));
        }
        for (_, a) in active {
            a.cancel.store(true, Ordering::Release);
            let _ = a.handle.join();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_core::resources::{ChatId, ImageId, Lease, Owner, Resource, Scope};
    pub(super) fn fixture() -> (State, String, String) {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Run a tool", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        run.attempts.push(crate::usage::Attempt {
            workspace_id: Some("attempt-a".into()),
            ..Default::default()
        });
        let run_id = run.id.clone();
        let resource = Resource {
            id: ResourceId::try_from("svc-a".to_owned()).unwrap(),
            owner: Owner {
                node: NodeId::try_from("node-a".to_owned()).unwrap(),
                chat: ChatId::try_from(chat.clone()).unwrap(),
            },
            generation: Generation::INITIAL,
            image: ImageId::try_from(format!("sha256:{}", "a".repeat(64))).unwrap(),
            scope: Scope::PRIVATE_OFFLINE,
            lease: Lease::new(now(), 300).unwrap(),
            phase: Phase::Running(ContainerId::try_from("b".repeat(64)).unwrap()),
        };
        state.services.push(serde_json::from_value(json!({"resource":resource,"origin":{"run":run_id,"attempt":"attempt-a"},"actor":Actor::LocalOperator,"command":"sleep 300","uid":1000,"gid":1000,"diagnostic":""})).unwrap());
        (state, run_id, chat)
    }
    fn input(direction: Direction) -> Input {
        Input {
            service_id: ResourceId::try_from("svc-a".to_owned()).unwrap(),
            name: Name::try_from("echo".to_owned()).unwrap(),
            direction,
            service_port: NonZeroU16::new(31000).unwrap(),
            target: if direction == Direction::Egress {
                Some(Endpoint {
                    host: "127.0.0.1".into(),
                    port: NonZeroU16::new(31001).unwrap(),
                })
            } else {
                None
            },
        }
    }
    fn request(state: &mut State, run: &str, direction: Direction) -> io::Result<Value> {
        admit(
            state,
            run,
            "attempt-a",
            "nonce-a",
            input(direction),
            "node-a".to_owned().try_into().unwrap(),
            |_| Ok(()),
        )
    }
    #[test]
    fn pending_requests_are_idempotent_owned_and_rollback_on_save_failure() {
        let (mut s, run, _) = fixture();
        let mut saves = 0;
        assert!(
            admit(
                &mut s,
                &run,
                "attempt-a",
                "nonce-a",
                input(Direction::Ingress),
                "node-a".to_owned().try_into().unwrap(),
                |_| Err(fail("disk full"))
            )
            .is_err()
        );
        assert!(s.ports.is_empty());
        let first = request(&mut s, &run, Direction::Ingress).unwrap();
        let second = admit(
            &mut s,
            &run,
            "attempt-a",
            "nonce-a",
            input(Direction::Ingress),
            "node-a".to_owned().try_into().unwrap(),
            |_| {
                saves += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(saves, 0);
        assert!(request(&mut s, &run, Direction::Egress).is_err());
        assert!(
            admit(
                &mut s,
                &run,
                "old-attempt",
                "other",
                input(Direction::Ingress),
                "node-a".to_owned().try_into().unwrap(),
                |_| Ok(())
            )
            .is_err()
        );
        s.services[0].resource.owner.chat = "another-chat".to_owned().try_into().unwrap();
        assert!(
            admit(
                &mut s,
                &run,
                "attempt-a",
                "another",
                input(Direction::Ingress),
                "node-a".to_owned().try_into().unwrap(),
                |_| Ok(())
            )
            .is_err()
        );
    }
    #[test]
    fn approval_is_exact_endpoint_and_durable_service_generation_not_a_network_grant() {
        let (mut s, run, _) = fixture();
        request(&mut s, &run, Direction::Egress).unwrap();
        let id = s.ports[0].id.as_str().to_owned();
        let time = now();
        let target = "127.0.0.1:31001".parse().unwrap();
        assert!(approve(&mut s, &id, 60, false, Some(target), time, |_| Ok(())).is_err());
        assert!(
            approve(&mut s, &id, 60, true, Some(target), time, |_| Err(fail(
                "disk full"
            )))
            .is_err()
        );
        assert_eq!(s.ports[0].status, Status::Pending);
        assert!(s.ports[0].approval.is_none());
        approve(&mut s, &id, 60, true, Some(target), time, |_| Ok(())).unwrap();
        let port = s.ports[0].clone();
        assert!(authorized(&s, &port, time).is_ok());
        assert!(authorized(&s, &port, time - 1).is_err());
        assert!(authorized(&s, &port, time + 60).is_err());
        let restored: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert!(!restored.ports[0].ready);
        assert!(authorized(&restored, &port, time).is_ok());
        s.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
        assert!(authorized(&s, &port, time).is_err());
        s.services[0].resource.generation = Generation::INITIAL;
        s.services[0].resource.phase = Phase::Running("c".repeat(64).try_into().unwrap());
        assert!(authorized(&s, &port, time).is_err());
        s.services[0].resource.phase = Phase::Running("b".repeat(64).try_into().unwrap());
        s.ports[0].status = Status::Stopping;
        assert!(authorized(&s, &port, time).is_err());
    }
    #[test]
    fn local_targets_require_exact_literal_operator_approval_and_no_dns_bypass() {
        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: NonZeroU16::new(8080).unwrap(),
        };
        let local = "127.0.0.1:8080".parse().unwrap();
        assert!(validate_destination(&endpoint, &[local], false).is_err());
        assert_eq!(
            validate_destination(&endpoint, &[local], true).unwrap(),
            local
        );
        for host in ["localhost", "2130706433", "example.com"] {
            let e = Endpoint {
                host: host.into(),
                ..endpoint.clone()
            };
            assert!(validate_destination(&e, &[local], true).is_err());
        }
        for target in [
            "169.254.169.254:8080",
            "192.168.1.1:8080",
            "224.0.0.1:8080",
            "127.0.0.2:8080",
            "127.0.0.1:8081",
        ] {
            assert!(validate_destination(&endpoint, &[target.parse().unwrap()], true).is_err());
        }
        let public = Endpoint {
            host: "example.com".into(),
            port: NonZeroU16::new(443).unwrap(),
        };
        assert!(validate_destination(&public, &["8.8.8.8:443".parse().unwrap()], false).is_ok());
        assert!(
            validate_destination(
                &public,
                &[
                    "8.8.8.8:443".parse().unwrap(),
                    "127.0.0.1:443".parse().unwrap()
                ],
                false
            )
            .is_err()
        );
        for host in ["example.com/path", "user@host", "host?x", "[::1]"] {
            assert!(
                Endpoint {
                    host: host.into(),
                    ..endpoint.clone()
                }
                .url()
                .is_err()
            );
        }
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
    };
    struct Server {
        child: Child,
    }
    impl Server {
        fn stop(&mut self) {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            };
            assert!(self.child.wait().unwrap().success());
        }
        fn crash(&mut self) {
            self.child.kill().unwrap();
            let _ = self.child.wait();
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    fn wait(mut condition: impl FnMut() -> bool, seconds: u64, label: &str) {
        let start = Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < Duration::from_secs(seconds),
                "Timed out: {label}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn assert_closed(result: io::Result<usize>) {
        assert!(
            matches!(result, Ok(0))
                || result.is_err_and(|e| matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                )),
            "Connection did not close"
        );
    }
    struct Echo {
        address: SocketAddr,
        accepted: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
        cancel: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }
    impl Echo {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let accepted = Arc::new(AtomicUsize::new(0));
            let closed = Arc::new(AtomicUsize::new(0));
            let cancel = Arc::new(AtomicBool::new(false));
            let a = accepted.clone();
            let b = closed.clone();
            let c = cancel.clone();
            let handle = thread::spawn(move || {
                let mut clients = Vec::new();
                while !c.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            let a = a.clone();
                            let b = b.clone();
                            let c = c.clone();
                            clients.push(thread::spawn(move || {
                                socket
                                    .set_read_timeout(Some(Duration::from_millis(200)))
                                    .unwrap();
                                let mut data = Vec::new();
                                let mut buffer = [0; 64];
                                let mut counted = false;
                                loop {
                                    match socket.read(&mut buffer) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            data.extend_from_slice(&buffer[..n]);
                                            if !counted {
                                                a.fetch_add(1, Ordering::AcqRel);
                                                counted = true;
                                            }
                                        }
                                        Err(e)
                                            if matches!(
                                                e.kind(),
                                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                                            ) =>
                                        {
                                            if c.load(Ordering::Acquire) {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                                let _ = socket.write_all(b"host:");
                                let _ = socket.write_all(&data);
                                let _ = socket.shutdown(Shutdown::Write);
                                b.fetch_add(1, Ordering::AcqRel);
                            }));
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25))
                        }
                        Err(_) => break,
                    }
                }
                for client in clients {
                    let _ = client.join();
                }
            });
            Self {
                address,
                accepted,
                closed,
                cancel,
                handle: Some(handle),
            }
        }
    }
    impl Drop for Echo {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Release);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
    const PROGRAM: &str = r#"
use std::{io::{Read,Write},net::{TcpListener,TcpStream,Shutdown},thread,time::Duration};
fn main(){
 let echo=TcpListener::bind("127.0.0.1:33001").unwrap();
 thread::spawn(move||for stream in echo.incoming(){let mut s=stream.unwrap();thread::spawn(move||{
   let mut body=Vec::new();let _=s.read_to_end(&mut body);std::fs::write("/workspace/inbound-size",body.len().to_string()).unwrap();
   let _=s.write_all(b"service:");let _=s.write_all(&body);let _=s.shutdown(Shutdown::Write);
 });});
 thread::spawn(||{let listener=TcpListener::bind("127.0.0.1:33003").unwrap();for stream in listener.incoming(){thread::spawn(move||{let mut socket=stream.unwrap();thread::sleep(Duration::from_secs(7));let mut buffer=[0;8192];let(mut count,mut sum)=(0u64,0u64);loop{match socket.read(&mut buffer){Ok(0)=>break,Ok(n)=>{count+=n as u64;sum+=buffer[..n].iter().map(|b|u64::from(*b)).sum::<u64>();},Err(_)=>return}}let _=write!(socket,"received:{count}:{sum}");let _=socket.shutdown(Shutdown::Write);});}});
 thread::spawn(||{let listener=TcpListener::bind("127.0.0.1:33005").unwrap();for stream in listener.incoming(){let mut s=stream.unwrap();thread::spawn(move||{s.write_all(b"banner\n").unwrap();let mut body=Vec::new();let _=s.read_to_end(&mut body);let _=s.write_all(b"after:");let _=s.write_all(&body);let _=s.shutdown(Shutdown::Write);});}});
 let mut first=true;loop{if let Ok(mut s)=TcpStream::connect("127.0.0.1:33002"){if first{s.write_all(b"outbound").unwrap();s.shutdown(Shutdown::Write).unwrap();let mut reply=Vec::new();s.read_to_end(&mut reply).unwrap();if reply==b"host:outbound"{std::fs::write("/workspace/outbound-result",b"half-close-ok").unwrap();first=false;}}else{let _=s.write_all(b"hold");let mut reply=Vec::new();let _=s.read_to_end(&mut reply);}}thread::sleep(Duration::from_millis(100));}
}"#;
    #[test]
    #[ignore = "requires CONTROL_QA_SERVICE_IMAGE, CONTROL_QA_CONTROL_BIN and Docker; isolated generic TCP fixtures, no models"]
    fn live_tcp_ports_preserve_half_close_and_recover_after_host_death() {
        let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
        let binary = std::env::var("CONTROL_QA_CONTROL_BIN").unwrap();
        let echo = Echo::start();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(format!(
            "/private/tmp/control-tcp-live-{}-{stamp}",
            std::process::id()
        ));
        let state_dir = root.join("state");
        let config_dir = root.join("config");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir(&config_dir).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, root.to_string_lossy().as_bytes());
        let node = digest.as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        fs::write(state_dir.join("node-id"), &node).unwrap();
        fs::set_permissions(state_dir.join("node-id"), fs::Permissions::from_mode(0o600)).unwrap();
        // Synthetic credential for this private fixture only; never read personal credentials.
        let cookie = "c".repeat(64);
        fs::write(state_dir.join("operator-session"), &cookie).unwrap();
        fs::set_permissions(
            state_dir.join("operator-session"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let url = format!("http://{address}");
        let (mut state, run, _) = super::tests::fixture();
        state.runtime.runs[0].status = "completed".into();
        let service = &mut state.services[0];
        service.resource.owner.node = node.clone().try_into().unwrap();
        service.resource.id = format!("svc-{node}").try_into().unwrap();
        service.resource.image = image.clone().try_into().unwrap();
        service.resource.phase = Phase::Requested;
        service.resource.lease = control_core::resources::Lease::new(now(), 180).unwrap();
        let mut value = serde_json::to_value(&*service).unwrap();
        value["workspace_exec"] = json!(true);
        value["command"] = json!(format!(
            "cat > /tmp/fixture.rs <<'CONTROL_RUST'\n{PROGRAM}\nCONTROL_RUST\nrustc --edition=2024 -O /tmp/fixture.rs -o /workspace/fixture\nexec /workspace/fixture"
        ));
        state.services[0] = serde_json::from_value(value).unwrap();
        let write = |s: &State| {
            fs::write(state_dir.join("state.json"), serde_json::to_vec(s).unwrap()).unwrap()
        };
        let read = || {
            serde_json::from_slice::<State>(&fs::read(state_dir.join("state.json")).unwrap())
                .unwrap()
        };
        write(&state);
        let launch = || {
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("server.log"))
                .unwrap();
            Server {
                child: Command::new(&binary)
                    .arg("serve")
                    .env("CONTROL_STATE_DIR", &state_dir)
                    .env("CONTROL_CONFIG_DIR", &config_dir)
                    .env("CONTROL_ADDR", address.to_string())
                    .env_remove("CONTROL_WORKER_IMAGE")
                    .env_remove("CONTROL_CLAUDE_IMAGE")
                    .stdin(Stdio::null())
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            }
        };
        let mut server = launch();
        wait(
            || matches!(read().services[0].resource.phase, Phase::Running(_)),
            25,
            "service running",
        );
        server.stop();
        state = read();
        let service_id = state.services[0].resource.id.clone();
        let container = state.services[0]
            .resource
            .phase
            .container()
            .unwrap()
            .clone();
        crate::services::relay_capable(&state.services[0]).unwrap();
        crate::services::port_binding(&state.services[0], &container).unwrap();
        struct Cleanup(ContainerId);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("docker")
                    .args(["rm", "-f", self.0.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _cleanup = Cleanup(container.clone());
        state.runtime.runs[0].status = "running".into();
        for (name, direction, service_port, target) in [
            ("ingress", Direction::Ingress, 33001, None),
            (
                "egress",
                Direction::Egress,
                33002,
                Some(Endpoint {
                    host: "127.0.0.1".into(),
                    port: NonZeroU16::new(echo.address.port()).unwrap(),
                }),
            ),
            ("expiry", Direction::Ingress, 33001, None),
            ("death", Direction::Ingress, 33001, None),
            ("stall", Direction::Ingress, 33003, None),
            ("banner", Direction::Ingress, 33005, None),
            (
                "idle",
                Direction::Egress,
                33004,
                Some(Endpoint {
                    host: "127.0.0.1".into(),
                    port: NonZeroU16::new(echo.address.port()).unwrap(),
                }),
            ),
        ] {
            admit(
                &mut state,
                &run,
                "attempt-a",
                name,
                Input {
                    service_id: service_id.clone(),
                    name: name.to_owned().try_into().unwrap(),
                    direction,
                    service_port: NonZeroU16::new(service_port).unwrap(),
                    target,
                },
                node.clone().try_into().unwrap(),
                |_| Ok(()),
            )
            .unwrap();
        }
        for i in 0..2 {
            let id = state.ports[i].id.as_str().to_owned();
            approve(
                &mut state,
                &id,
                100,
                i == 1,
                if i == 1 { Some(echo.address) } else { None },
                now(),
                |_| Ok(()),
            )
            .unwrap();
        }
        state.runtime.runs[0].status = "completed".into();
        write(&state);
        server = launch();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let post = |id: &str, fields: &str| {
            let response = client
                .post(format!("{url}/tools/ports"))
                .header("Cookie", format!("control_operator={cookie}"))
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(format!("id={id}&{fields}"))
                .send()
                .unwrap();
            assert_eq!(response.status(), 200, "port operator action failed");
        };
        let named = |name: &str| read().ports.into_iter().find(|p| p.name.0 == name).unwrap();
        wait(
            || {
                client
                    .get(format!("{url}/health"))
                    .send()
                    .is_ok_and(|r| r.status().is_success())
            },
            5,
            "HTTP server ready",
        );
        let ui_ready = |name: &str| {
            client
                .get(format!("{url}/tools/ports"))
                .header("Cookie", format!("control_operator={cookie}"))
                .send()
                .ok()
                .and_then(|r| r.text().ok())
                .is_some_and(|html| {
                    let document = scraper::Html::parse_document(&html);
                    document
                        .select(&scraper::Selector::parse("article").unwrap())
                        .any(|a| {
                            a.select(&scraper::Selector::parse("h3").unwrap())
                                .any(|h| h.text().collect::<String>() == name)
                                && a.text().collect::<String>().contains(" · ready")
                        })
                })
        };
        post(named("idle").id.as_str(), "seconds=100&loopback=yes");
        wait(
            || ui_ready("idle"),
            5,
            "idle egress helper listening before crash",
        );
        wait(
            || named("ingress").approval.as_ref().unwrap().host_port != 0,
            8,
            "ingress bind receipt",
        );
        let published = named("ingress").approval.unwrap().host_port;
        let roundtrip = |port: u16| {
            let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket.write_all(b"hello").unwrap();
            socket.shutdown(Shutdown::Write).unwrap();
            let mut reply = Vec::new();
            socket.read_to_end(&mut reply).unwrap();
            assert_eq!(reply, b"service:hello");
        };
        roundtrip(published);
        wait(
            || echo.accepted.load(Ordering::Acquire) >= 2,
            8,
            "outbound half-close and held session",
        );
        assert_eq!(
            crate::runtime::docker_query(&[
                "exec",
                container.as_str(),
                "cat",
                "/workspace/outbound-result"
            ])
            .unwrap(),
            "half-close-ok"
        );
        let before = echo.accepted.load(Ordering::Acquire);
        server.crash();
        let restart = Instant::now();
        server = launch();
        wait(
            || echo.accepted.load(Ordering::Acquire) > before,
            10,
            "outbound listener released and reattached after host death",
        );
        wait(
            || ui_ready("idle"),
            8,
            "idle helper releases port after abrupt host death",
        );
        post(named("idle").id.as_str(), "revoke=yes");
        assert_eq!(named("ingress").approval.unwrap().host_port, published);
        roundtrip(published);
        println!(
            "Abrupt host restart and egress rebind: {:?}",
            restart.elapsed()
        );
        post(named("banner").id.as_str(), "seconds=30");
        wait(
            || {
                named("banner")
                    .approval
                    .as_ref()
                    .is_some_and(|a| a.host_port != 0)
            },
            3,
            "server-first banner bind",
        );
        let mut banner =
            TcpStream::connect(("127.0.0.1", named("banner").approval.unwrap().host_port)).unwrap();
        banner
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut greeting = [0; 7];
        banner.read_exact(&mut greeting).unwrap();
        assert_eq!(&greeting, b"banner\n");
        banner.write_all(b"thanks").unwrap();
        banner.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        banner.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"after:thanks");
        post(named("banner").id.as_str(), "revoke=yes");
        for (prefix, remainder) in [
            (
                b"F".as_slice(),
                format!(
                    "OO / HTTP/1.1\r\nHost: localhost\r\nCookie: control_operator={cookie}\r\n\r\n"
                )
                .into_bytes(),
            ),
            (
                b"POST / HTTP/1.1\r\n".as_slice(),
                format!("Cookie: control_operator={cookie}\r\nContent-Length: 1\r\n\r\n\0")
                    .into_bytes(),
            ),
            (&[0x16], vec![3, 1, 0, 4, 1, 0, 0, 0]),
        ] {
            let mut browser = TcpStream::connect(("127.0.0.1", published)).unwrap();
            browser.set_nodelay(true).unwrap();
            browser
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            browser.write_all(prefix).unwrap();
            thread::sleep(Duration::from_millis(25));
            let _ = browser.write_all(&remainder);
            let mut bytes = [0; 32];
            assert_closed(browser.read(&mut bytes));
            wait(
                || {
                    crate::runtime::docker_query(&[
                        "exec",
                        container.as_str(),
                        "cat",
                        "/workspace/inbound-size",
                    ])
                    .is_ok_and(|n| n == "0")
                },
                3,
                "browser bytes rejected before service forwarding",
            );
        }
        let mut held = TcpStream::connect(("127.0.0.1", published)).unwrap();
        held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        held.write_all(b"held").unwrap();
        let revoke = Instant::now();
        post(named("ingress").id.as_str(), "revoke=yes");
        let mut buffer = [0; 64];
        assert_closed(held.read(&mut buffer));
        wait(
            || named("ingress").status == Status::Stopped,
            5,
            "ingress stop acknowledgement",
        );
        assert!(TcpStream::connect(("127.0.0.1", published)).is_err());
        println!("Ingress revocation closed socket: {:?}", revoke.elapsed());
        let closed = echo.closed.load(Ordering::Acquire);
        post(named("egress").id.as_str(), "revoke=yes");
        wait(
            || echo.closed.load(Ordering::Acquire) > closed,
            5,
            "egress remote socket closed on revoke",
        );
        post(named("expiry").id.as_str(), "seconds=2");
        wait(
            || {
                named("expiry")
                    .approval
                    .as_ref()
                    .is_some_and(|a| a.host_port != 0)
            },
            2,
            "expiry listener bind",
        );
        let expiry_port = named("expiry").approval.unwrap().host_port;
        let mut expiring = TcpStream::connect(("127.0.0.1", expiry_port)).unwrap();
        expiring
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        expiring.write_all(b"wait").unwrap();
        assert_closed(expiring.read(&mut buffer));
        wait(
            || named("expiry").status == Status::Stopped,
            4,
            "expiry acknowledgement",
        );
        post(named("stall").id.as_str(), "seconds=30");
        wait(
            || {
                named("stall")
                    .approval
                    .as_ref()
                    .is_some_and(|a| a.host_port != 0)
            },
            3,
            "stall listener bind",
        );
        let stalled =
            TcpStream::connect(("127.0.0.1", named("stall").approval.unwrap().host_port)).unwrap();
        stalled
            .set_read_timeout(Some(Duration::from_secs(25)))
            .unwrap();
        let mut sending = stalled.try_clone().unwrap();
        sending
            .set_write_timeout(Some(Duration::from_secs(25)))
            .unwrap();
        let writer = thread::spawn(move || {
            let data: [u8; 65536] = std::array::from_fn(|i| i as u8);
            for _ in 0..256 {
                sending.write_all(&data)?;
            }
            sending.shutdown(Shutdown::Write)
        });
        let mut stalled = stalled;
        let stall = Instant::now();
        let mut response = String::new();
        stalled.read_to_string(&mut response).unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(
            response,
            format!(
                "received:{}:{}",
                65536u64 * 256,
                (0..65536).map(|i| u64::from(i as u8)).sum::<u64>() * 256
            )
        );
        assert!(stall.elapsed() > Duration::from_secs(5));
        println!(
            "Healthy slow reader resumed with exact payload after {:?}",
            stall.elapsed()
        );
        let mut held =
            TcpStream::connect(("127.0.0.1", named("stall").approval.unwrap().host_port)).unwrap();
        held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut sending = held.try_clone().unwrap();
        sending
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let blocked = thread::spawn(move || {
            let data = [0; 65536];
            for _ in 0..256 {
                if sending.write_all(&data).is_err() {
                    break;
                }
            }
        });
        thread::sleep(Duration::from_secs(1));
        let revoked = Instant::now();
        post(named("stall").id.as_str(), "revoke=yes");
        assert_closed(held.read(&mut buffer));
        let _ = blocked.join();
        assert!(revoked.elapsed() < Duration::from_secs(5));
        println!(
            "Revocation closed a backpressured connection after {:?}",
            revoked.elapsed()
        );
        post(named("death").id.as_str(), "seconds=30");
        wait(
            || {
                named("death")
                    .approval
                    .as_ref()
                    .is_some_and(|a| a.host_port != 0)
            },
            3,
            "death listener bind",
        );
        let mut held =
            TcpStream::connect(("127.0.0.1", named("death").approval.unwrap().host_port)).unwrap();
        held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        held.write_all(b"waiting").unwrap();
        let current = read();
        crate::services::port_binding(&current.services[0], &container).unwrap();
        assert!(
            Command::new("docker")
                .args(["kill", container.as_str()])
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        assert_closed(held.read(&mut buffer));
        wait(
            || named("death").status == Status::Stopped,
            6,
            "service death revokes its port",
        );
        server.stop();
        println!("TCP fixture retained at {}", root.display());
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    #[test]
    fn stale_pending_and_foreign_node_resources_are_retired_before_any_listener_effect() {
        let (mut state, run, _) = super::tests::fixture();
        let input:Input=serde_json::from_value(json!({"service_id":"svc-a","name":"pending","direction":"ingress","service_port":3000})).unwrap();
        admit(
            &mut state,
            &run,
            "attempt-a",
            "pending",
            input,
            "node-a".to_owned().try_into().unwrap(),
            |_| Ok(()),
        )
        .unwrap();
        let p = state.ports[0].clone();
        let node = "node-a".to_owned().try_into().unwrap();
        assert_eq!(intent(&state, &p, &node, now()), Intent::Wait);
        assert_eq!(
            intent(&state, &p, &"node-b".to_owned().try_into().unwrap(), now()),
            Intent::Stop
        );
        assert_eq!(
            intent(
                &state,
                &p,
                &node,
                state.services[0].resource.lease.expires()
            ),
            Intent::Stop
        );
        state.services[0].resource.phase =
            Phase::Stopped(control_core::resources::StopReason::Exited);
        assert_eq!(intent(&state, &p, &node, now()), Intent::Stop);
    }
}
