//! Service-owned TCP ports over verified stdio relays. Containers stay offline.
mod ingress_guard;
mod decisions;
mod multiplex;
#[path = "../tools/tcp-relay/multiplex.rs"]
mod multiplex_transport;
#[path = "../tools/tcp-relay/protocol.rs"]
mod protocol;
mod tools;
pub use tools::{PortCall, PortOps, Tools};
use crate::{
    State,
    identity::{self, Action, Actor},
    services::{RelayMode, Service},
    tool_registry::AttemptRef,
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
const MAX_PORTS: usize = 64;
const PORTS_PER_CHAT: usize = 8;
const MAX_RECORDS: usize = 128;
const MAX_CONNECTIONS: usize = 8;
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if="Option::is_none")]
    peer: Option<ResourceId>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    peer_grant: Option<crate::services::Grant>,
    status: Status,
    approval: Option<Approval>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    decision: Option<decisions::Receipt>,
    #[serde(default)]
    diagnostic: String,
    #[serde(skip)]
    ready: bool,
}
impl Port {
    fn summary(&self) -> Value {
        json!({"port_id":self.id,"service_id":self.service,"name":self.name,"direction":self.direction,"service_port":self.service_port,"target":self.target,"peer_port_id":self.peer,"status":self.status,"ready":self.ready,"local_address":self.approval.as_ref().filter(|_|self.direction==Direction::Ingress).filter(|a|a.host_port!=0).map(|a|format!("127.0.0.1:{}",a.host_port)),"pinned_target":self.approval.as_ref().and_then(|a|a.pinned),"expires_unix":self.approval.as_ref().map(|a|a.expires).filter(|expires|*expires!=u64::MAX),"until_stopped":self.approval.as_ref().is_some_and(|a|a.expires==u64::MAX),"diagnostic":self.diagnostic,"approval_url":"/tools/ports"})
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    service_id: ResourceId,
    name: Name,
    direction: Direction,
    service_port: NonZeroU16,
    target: Option<Endpoint>,
    #[serde(default)]
    peer_port_id: Option<ResourceId>,
}
fn accessible(state: &State, port: &Port, run: &crate::runtime::Run, node: &NodeId) -> bool {
    port.node == *node && port.actor == run.actor && state.services.iter().any(|service|
        service.resource.id == port.service && service.resource.generation == port.generation
        && service.resource.owner.chat.as_str() == port.chat
        && crate::services::accessible(service,node,run))
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
fn peer_endpoint(state: &State, port: &Port, time: u64) -> io::Result<SocketAddr> {
    let id = port.peer.as_ref().ok_or_else(|| fail("Not a peer route"))?;
    let peer = state
        .ports
        .iter()
        .find(|p| {
            &p.id == id
                && p.direction == Direction::Ingress
                && p.node == port.node
                && p.actor == port.actor
                && p.service != port.service
        })
        .ok_or_else(|| fail("Peer ingress is not accessible to this route"))?;
    let peer_service=authorized(state, peer, time).map_err(|e| fail(format!("Peer ingress unavailable: {e}")))?;
    // Persist the exact authority admitted for this route. A service outlives
    // its model run; sharing an actor or a delegation root is not authority.
    if !port.peer_grant.as_ref().map_or(peer.chat==port.chat,|grant|grant.matches(&peer_service)) {
        return Err(fail("Peer service ownership or generation changed"));
    }
    match peer.approval.as_ref().map(|a| a.host_port) {
        Some(port) if port != 0 => Ok(SocketAddr::from(([127, 0, 0, 1], port))),
        _ => Err(fail("Peer ingress is not ready yet")),
    }
}
fn peer_listening(state: &State, port: &Port) -> bool {
    port.peer.as_ref().is_none_or(|id| state.ports.iter().any(|p| &p.id == id && p.ready))
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
        || (approval.expires != u64::MAX && approval.expires.saturating_sub(approval.issued) > 3600)
    {
        return Err(fail("Port approval expired"));
    }
    let service = service(state, current, time)?;
    if approval.expires > service.resource.lease.expires() {
        return Err(fail("Port lease exceeds service lease"));
    }
    match current.direction {
        Direction::Ingress if approval.pinned.is_none() && current.peer.is_none() => {}
        Direction::Egress if current.peer.is_some() => {
            if current.target.is_some()
                || approval.pinned != Some(peer_endpoint(state, current, time)?)
            {
                return Err(fail("Peer ingress endpoint changed"));
            }
        }
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
    Ok(service.binding_snapshot())
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
    let routes = usize::from(input.target.is_some()) + usize::from(input.peer_port_id.is_some());
    if (input.direction == Direction::Egress) != (routes == 1) || routes > 1 {
        return Err(fail(
            "Egress requires exactly one of target or peer_port_id; ingress takes neither",
        ));
    }
    if let Some(target) = &input.target {
        target.url()?;
    }
    let service = state
        .services
        .iter()
        .find(|s| {
            s.resource.id == input.service_id
                && crate::services::accessible(s, &node, run)
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
            || old.peer != input.peer_port_id
        {
            return Err(fail("Nonce reused with a different port request"));
        }
        return Ok(old.summary());
    }
    let peer_grant = input.peer_port_id.as_ref().map(|peer| {
        let port=state.ports.iter().find(|p| {
            &p.id == peer
                && p.direction == Direction::Ingress
                && p.status != Status::Stopped
                && p.service != input.service_id
                && accessible(state, p, run, &node)
        }).ok_or_else(||fail("peer_port_id must be an active ingress of another owned or explicitly granted service"))?;
        self::service(state,port,now()).map(Service::grant)
    }).transpose()?;
    let owner = service.resource.owner.chat.as_str();
    let (active, owned) = state.ports.iter().filter(|p| p.status != Status::Stopped)
        .fold((0, 0), |(all, chat), p| (all + 1, chat + usize::from(p.chat == owner)));
    if owned >= PORTS_PER_CHAT {
        return Err(fail(format!("This chat's services already hold {PORTS_PER_CHAT} active ports. Stop finished routes, such as completed downloads, with control_port_stop")));
    }
    if active >= MAX_PORTS {
        return Err(fail(format!("Control's node-wide limit of {MAX_PORTS} active ports is reached. Stop finished routes with control_port_stop or ask the operator")));
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
    let owner_chat = owner.to_owned();
    let port = Port {
        id,
        service: input.service_id,
        generation: service.resource.generation,
        container: service.resource.phase.container().unwrap().clone(),
        node,
        chat: owner_chat,
        actor: run.actor.clone(),
        origin_run: run_id.into(),
        origin_attempt: generation.into(),
        name: input.name,
        direction: input.direction,
        service_port: input.service_port,
        target: input.target,
        peer: input.peer_port_id,
        peer_grant,
        status: Status::Pending,
        approval: None,
        decision: None,
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
pub(crate) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: PortCall) -> io::Result<Value> {
    let (run, generation) = (attempt.run, attempt.generation);
    let node = NodeId::try_from(crate::config::node_id().map_err(fail)?).map_err(fail)?;
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let (stop, port_id) = match call {
        PortCall::Request(input) => return admit(&mut state, run, generation, attempt.nonce, input, node, crate::storage::save),
        PortCall::Status(port_id) => (false, port_id),
        PortCall::Stop(port_id) => (true, port_id),
    };
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
    r.chat_id.as_ref().ok_or_else(|| fail("A chat is required"))?;
    let owned = |p: &&Port| accessible(&state,p,r,&node);
    if !stop && port_id.is_none() {
        return Ok(
            json!({"ports":state.ports.iter().filter(owned).filter(|p|p.status!=Status::Stopped).chain(state.ports.iter().rev().filter(owned).filter(|p|p.status==Status::Stopped).take(10)).map(Port::summary).collect::<Vec<_>>()}),
        );
    }
    let index = state
        .ports
        .iter()
        .position(|p| {
            accessible(&state,p,r,&node)
                && Some(&p.id) == port_id.as_ref()
        })
        .ok_or_else(|| fail("Owned port not found"))?;
    if stop && state.ports[index].status != Status::Stopped {
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
    seconds: i64,
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
    let expires = match seconds {
        -1 => service.resource.lease.expires(),
        1..=3600 => time.checked_add(seconds as u64)
            .ok_or_else(||fail("Invalid lease"))?.min(service.resource.lease.expires()),
        _ => return Err(fail("Choose service lifetime (-1) or 1–3600 seconds")),
    };
    match port.direction {
        Direction::Ingress if pinned.is_none() => {}
        Direction::Egress if port.peer.is_some() => {
            if pinned.is_none() || pinned != Some(peer_endpoint(state, port, time)?) {
                return Err(fail("Peer ingress endpoint changed; review the request again"));
            }
        }
        Direction::Egress => {
            validate_destination(
                port.target.as_ref().ok_or_else(|| fail("Missing target"))?,
                &[pinned.ok_or_else(|| fail("Missing pinned address"))?],
                allow_loopback,
            )?;
        }
        _ => return Err(fail("Invalid approval")),
    }
    let allow_loopback = allow_loopback && port.peer.is_none();
    let old = state.ports[index].clone();
    state.ports[index].approval = Some(Approval {
        issued: time,
        expires,
        host_port: 0,
        pinned,
        allow_loopback,
    });
    state.ports[index].status = Status::Approved;
    state.ports[index].decision = Some(decisions::Receipt::pending(true));
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
    http_with_save(stream,shared,method,path,body,crate::storage::save)
}
pub(crate) fn http_with_save(
    stream:&mut TcpStream,shared:&Arc<Mutex<State>>,method:&str,path:&str,body:&[u8],
    save:impl Fn(&State)->io::Result<()>,
) -> bool {
    if path != "/tools/ports" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let asynchronous = fields.get("response").is_some_and(|value|value == "json");
    let return_chat = fields.get("return_chat").filter(|id|crate::chat::valid_id(id)).cloned();
    let result = (|| -> io::Result<String> {
        if method == "POST" {
            let id = fields.get("id").ok_or_else(|| fail("Missing port ID"))?;
            if let Some(chat) = &return_chat {
                let state=shared.lock().map_err(|_|fail("State unavailable"))?;
                if !pending_for_chat(&state,chat).iter().any(|port|port.id.as_str() == id) {return Err(fail("Port request is no longer pending in this conversation"));}
            }
            if fields.contains_key("revoke") {
                let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
                let p = state
                    .ports
                    .iter_mut()
                    .find(|p| p.id.as_str() == id)
                    .ok_or_else(|| fail("Port not found"))?;
                let old = p.clone();
                if p.status == Status::Pending { p.decision = Some(decisions::Receipt::pending(false)); }
                if p.status != Status::Stopped {
                    p.status = Status::Stopping;
                }
                if let Err(e) = save(&state) {
                    state
                        .ports
                        .iter_mut()
                        .find(|p| p.id.as_str() == id)
                        .unwrap()
                        .clone_from(&old);
                    return Err(e);
                }
            } else {
                let (request, service, peer) = {
                    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
                    let p = state
                        .ports
                        .iter()
                        .find(|p| p.id.as_str() == id && p.status == Status::Pending)
                        .ok_or_else(|| fail("Pending port not found"))?;
                    let peer = p.peer.is_some().then(|| peer_endpoint(&state, p, now())).transpose()?;
                    (p.clone(), service(&state, p, now())?.binding_snapshot(), peer)
                };
                if request.node.as_str() != crate::config::node_id().map_err(fail)? {
                    return Err(fail("Port belongs to another node"));
                }
                crate::services::relay_capable(&service)?;
                if service.resource.lease.is_until_stopped() && fields.get("seconds").is_some_and(|v|v == "-1") {
                    let marker = crate::engine::query(&["image","inspect",service.resource.image.as_str(),"--format","{{index .Config.Labels \"dev.control.tcp-relay\"}}"])?;
                    if !matches!(marker.trim(),"3"|"4") {return Err(fail("Rebuild worker images before approving service-lifetime ports"));}
                }
                crate::services::port_binding(&service, &request.container)?;
                let allow_loopback = fields.get("loopback").is_some_and(|v| v == "yes");
                let pinned = match peer {
                    Some(peer) => Some(peer),
                    None => request
                        .target
                        .as_ref()
                        .map(|e| e.pin(allow_loopback))
                        .transpose()?,
                };
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
                    &save,
                )?;
            }
        } else if method != "GET" {
            return Err(fail("Use GET or POST"));
        }
        if method=="POST" && (asynchronous || return_chat.is_some()) {return Ok(String::new());}
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let ports=state.ports.iter().rev().filter(|p|p.status!=Status::Stopped).take(MAX_PORTS).map(|p|{
            let detail=route(&state,p);
            let action=if p.status==Status::Pending{format!("<label>Allow for<select name='seconds'><option value='-1'>Service lifetime</option><option value='900'>Up to 15 minutes</option><option value='3600'>Up to 1 hour</option></select></label><p class='muted'>Access ends when this service stops. You can revoke it at any time.</p>{}<button class='primary'>Allow port</button><button name='revoke' value='yes' formnovalidate>Deny</button>",if p.target.is_some(){"<label><input type='checkbox' name='loopback' value='yes'> Allow this exact literal loopback target</label>"}else{""})}else{"<button name='revoke' value='yes'>Revoke</button>".into()};
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
    if asynchronous {
        let (status,value) = match result {Ok(_) => ("200 OK",json!({"ok":true})),Err(error) => ("400 Bad Request",json!({"error":error.to_string()}))};
        let _=control_http::respond(stream,status,"application/json",value.to_string().as_bytes(),"",control_http::Policy::Page);
        return true;
    }
    if result.is_ok() && method == "POST" && let Some(chat) = return_chat {
        let _=control_http::respond(stream,"303 See Other","text/plain",b"",&format!("Location: /chat/{chat}\r\n"),control_http::Policy::Page);
        return true;
    }
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

pub(crate) fn pending_for_chat<'a>(state: &'a State, id: &str) -> Vec<&'a Port> {
    let Some(chat)=state.chats.iter().find(|chat|chat.id==id) else{return vec![];};
    let mut runs=std::collections::HashSet::new();
    for run in state.runtime.runs.iter().filter(|run|run.chat_id.as_deref()==Some(id)&&run.actor==chat.actor) {
        runs.insert(run.id.as_str());
        if let Ok((_,children))=crate::delegated_results::scope(state,run){runs.extend(children);}
    }
    state.ports.iter().filter(|port|port.status==Status::Pending && port.actor==chat.actor
        && (port.chat==id || runs.contains(port.origin_run.as_str()))).collect()
}

pub(crate) fn pending_json(state: &State,id: &str)->Vec<Value> {
    pending_for_chat(state,id).into_iter().map(|port| {
        let mut value=port.summary();
        value["loopback_required"]=json!(port.target.as_ref().is_some_and(|target|target.host.parse::<IpAddr>().is_ok_and(|ip|ip.is_loopback())));
        value
    }).collect()
}

fn route(state: &State, port: &Port) -> String {
    match (&port.target, &port.peer) {
        (Some(target), _) => format!("Service :{} → {}:{}", port.service_port, crate::escape(&target.host), target.port),
        (None, Some(peer)) => {
            let ingress = state.ports.iter().find(|p| &p.id == peer);
            format!(
                "Service :{} → peer ingress {} (service {} :{})",
                port.service_port,
                crate::escape(ingress.map_or(peer.as_str(), |p| p.name.0.as_str())),
                crate::escape(ingress.map_or("unknown", |p| p.service.as_str())),
                ingress.map_or(0, |p| p.service_port.get()),
            )
        }
        (None, None) => format!("Host loopback → service :{}", port.service_port),
    }
}
pub(crate) fn pending_panel(state:&State,id:&str)->String {
    pending_for_chat(state,id).into_iter().map(|port| {
        let detail=route(state,port);
        let loopback=if port.target.as_ref().is_some_and(|target|target.host.parse::<IpAddr>().is_ok_and(|ip|ip.is_loopback())) {
            "<label><input type='checkbox' name='loopback' value='yes' required> Allow this exact loopback target</label>"
        }else{""};
        format!("<article class='assistant-message port-request' data-transcript-key='port-{}'><h3>Port access · {}</h3><p>{detail}</p><form method='post' action='/tools/ports' target='_top'><input type='hidden' name='id' value='{}'><input type='hidden' name='return_chat' value='{}'><label>Allow for<select name='seconds'><option value='-1'>Service lifetime</option><option value='900'>Up to 15 minutes</option><option value='3600'>Up to 1 hour</option></select></label>{loopback}<button class='primary'>Allow port</button><button name='revoke' value='yes' formnovalidate>Deny</button><p role='alert' hidden></p></form></article>",port.id.as_str(),crate::escape(&port.name.0),port.id.as_str(),crate::escape(id))
    }).collect()
}

pub(crate) fn decision_revision(state:&State)->u64 {
    use std::hash::{Hash,Hasher};
    let mut hash=std::collections::hash_map::DefaultHasher::new();
    for port in &state.ports {(&port.id,&port.status).hash(&mut hash);}
    hash.finish()
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
    let started = Instant::now();
    let result = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(fail("Relay handshake closed"));
            }
            Err(_) => {}
        }
        if started.elapsed() >= wait || live_service(shared, port, cancel).is_err() {
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
    let service=live_service(&shared,&port,&cancel)?;
    if crate::services::relay_version(&service)? >= 4 {return multiplex::egress(shared,port,cancel);}
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
            receipt(&shared, &port, false, "");
            live_service(&shared, &port, &cancel)?;
            if !shared.lock().is_ok_and(|state| peer_listening(&state, &port)) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail("Peer ingress is not listening"));
            }
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
        // A successful connection needs a new helper listener immediately.
        // Back off only on failures; an unconditional pause made consecutive
        // dependency downloads hit a needless refusal window.
        let Err(error) = result else {continue;};
        receipt(&shared, &port, false, &error.to_string());
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
        let changes = crate::storage::changes().clone();
        let shutdown = crate::shutdown::token();
        while !crate::shutdown::requested() {
            // Saves made during this pass, including its own, cause one more pass.
            let seen = changes.generation();
            let (mut start, mut stop) = (Vec::new(), Vec::new());
            {
                let Ok(mut state) = shared.lock() else { break };
                let _ = decisions::advance(&mut state, crate::storage::save);
                let time = now();
                for port in state.ports.iter().filter(|p| p.status != Status::Stopped) {
                    match intent(&state, port, &node, time) {
                        Intent::Serve => {
                            if active.get(&port.id).is_none_or(|a| a.handle.is_finished()) {
                                start.push(port.clone());
                            }
                        }
                        decision => stop.push((port.id.clone(), decision == Intent::Stop)),
                    }
                }
            }
            for (id, record) in stop {
                if let Some(a) = active.remove(&id) {
                    a.cancel.store(true, Ordering::Release);
                    let _ = a.handle.join();
                }
                if record {
                    stop_record(&shared, &id);
                }
            }
            for port in start {
                if let Some(a) = active.remove(&port.id) {
                    let _ = a.handle.join();
                }
                {
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
            // Approvals, revocations and service changes are saved and wake this
            // loop at once; the bound covers time-based expiry and relay restarts.
            let _ = changes.wait_newer(seen, Some(Instant::now() + Duration::from_secs(1)), &shutdown);
        }
        for (_, a) in active {
            a.cancel.store(true, Ordering::Release);
            let _ = a.handle.join();
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use control_core::resources::{ChatId, ImageId, Lease, Owner, Resource, Scope};
    pub(crate) fn pending_fixture() -> (State,String,String) {
        let (mut state,run,chat)=fixture();
        request(&mut state,&run,Direction::Egress).unwrap();
        (state,run,chat)
    }
    #[test]
    fn pending_approvals_are_visible_only_in_the_request_scope() {
        let (mut state,_,chat)=pending_fixture();
        let other=crate::chat::create(&mut state);
        state.chats.last_mut().unwrap().parent_chat=Some(chat.clone());
        assert!(pending_json(&state,&other).is_empty());
        assert_eq!(pending_json(&state,&chat).len(),1);
        let html=crate::chat::transcript(&state,&chat).unwrap();
        assert!(html.contains("class='assistant-message port-request'"));
        assert!(html.contains("value='-1'>Service lifetime"));
        assert!(html.contains("target='_top'"));
    }
    pub(crate) fn fixture() -> (State, String, String) {
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
            peer_port_id: None,
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
    fn port_capacity_is_per_chat_with_a_node_wide_cap() {
        let (mut state, run, chat) = fixture();
        request(&mut state, &run, Direction::Ingress).unwrap();
        let template = state.ports[0].clone();
        let fill = |state: &mut State, owner: &str, count: usize| {
            for index in 0..count {
                let mut port = template.clone();
                port.id = ResourceId::try_from(format!("port-{owner}-{index}")).unwrap();
                port.chat = owner.into();
                port.name = Name::try_from(format!("held{index}")).unwrap();
                state.ports.push(port);
            }
        };
        let ask = |state: &mut State, nonce: &str, name: &str| {
            let mut input = input(Direction::Ingress);
            input.name = Name::try_from(name.to_owned()).unwrap();
            admit(state, &run, "attempt-a", nonce, input, "node-a".to_owned().try_into().unwrap(), |_| Ok(()))
        };
        fill(&mut state, &chat, PORTS_PER_CHAT - 1);
        let error = ask(&mut state, "nonce-b", "extra").unwrap_err().to_string();
        assert!(error.contains("control_port_stop") && error.contains("This chat"), "{error}");
        state.ports.retain(|port| port.id == template.id);
        fill(&mut state, "c-other", PORTS_PER_CHAT * 4);
        assert!(ask(&mut state, "nonce-c", "second").is_ok(), "another chat at its cap does not block this one");
        fill(&mut state, "c-third", MAX_PORTS);
        let error = ask(&mut state, "nonce-d", "third").unwrap_err().to_string();
        assert!(error.contains("node-wide"), "{error}");
    }
    #[test]
    #[ignore = "requires local Chrome/Firefox WebDriver; isolated fixture, no credentials/models"]
    fn live_port_controls_have_equal_buttons_and_themed_checkbox() {
        use reqwest::blocking::Client;
        use std::io::{BufRead, Write};
        use std::sync::atomic::{AtomicBool,Ordering};
        let (mut state, run, _) = fixture();
        request(&mut state,&run,Direction::Egress).unwrap();
        let state = Arc::new(Mutex::new(state));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}",listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let server = std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream,_)) = listener.accept() else {std::thread::sleep(Duration::from_millis(10));continue;};
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut reader=std::io::BufReader::new(stream.try_clone().unwrap());
                let mut first=String::new();
                if reader.read_line(&mut first).is_err() {continue;}
                let path=first.split_whitespace().nth(1).unwrap_or("/");
                if !crate::serve_stylesheet(&mut stream,path) {
                    if path=="/tools/ports" {http(&mut stream,&state,"GET",path,&[]);}
                    else {let _=stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");}
                }
            }
        });
        struct Server(Arc<AtomicBool>,Option<std::thread::JoinHandle<()>>);
        impl Drop for Server {fn drop(&mut self){self.0.store(true,Ordering::Relaxed);self.1.take().unwrap().join().unwrap();}}
        let _server=Server(stop,Some(server));
        let client=Client::builder().timeout(Duration::from_secs(20)).build().unwrap();
        let driver=std::env::var("CONTROL_WEBDRIVER_URL").unwrap();
        let chrome=std::env::var("CONTROL_QA_BROWSER").is_ok_and(|b|b=="chrome");
        let caps=if chrome {json!({"browserName":"chrome","goog:chromeOptions":{"binary":"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome","args":["--headless=new","--no-first-run"]}})} else {json!({"browserName":"firefox","moz:firefoxOptions":{"binary":"/Applications/Firefox.app/Contents/MacOS/firefox","args":["-headless"]}})};
        let session:Value=client.post(format!("{driver}/session")).json(&json!({"capabilities":{"alwaysMatch":caps}})).send().unwrap().json().unwrap();
        let id=session["value"]["sessionId"].as_str().expect("Browser session");
        let base=format!("{driver}/session/{id}");
        struct Browser(Client,String);
        impl Drop for Browser {fn drop(&mut self){let _=self.0.delete(&self.1).send();}}
        let _browser=Browser(client.clone(),base.clone());
        let post=|path:&str,args:Value|->Value {
            let response=client.post(format!("{base}{path}")).json(&args).send().unwrap();
            assert!(response.status().is_success(),"WebDriver {path}");
            response.json::<Value>().unwrap()["value"].clone()
        };
        let get=|path:&str|->Value {client.get(format!("{base}{path}")).send().unwrap().json::<Value>().unwrap()["value"].clone()};
        post("/url",json!({"url":format!("{url}/tools/ports")}));
        let elements=post("/elements",json!({"using":"css selector","value":"form[action='/tools/ports'] > button"}));
        let buttons=elements.as_array().unwrap();assert_eq!(buttons.len(),2);
        let element=|value:&Value|value["element-6066-11e4-a52e-4f735466cecf"].as_str().unwrap().to_owned();
        let allow=element(&buttons[0]);let deny=element(&buttons[1]);
        let a=get(&format!("/element/{allow}/rect"));let b=get(&format!("/element/{deny}/rect"));
        assert_eq!(a["height"],b["height"]);assert_eq!(a["width"],b["width"]);
        assert_eq!(get(&format!("/element/{allow}/css/border-top-left-radius")),"3px");
        let checkbox=element(&post("/element",json!({"using":"css selector","value":"input[type=checkbox]"})));
        assert_eq!(get(&format!("/element/{checkbox}/css/appearance")),"none");
        let before=get(&format!("/element/{checkbox}/css/background-color"));
        post(&format!("/element/{checkbox}/click"),json!({}));
        assert_eq!(get(&format!("/element/{checkbox}/property/checked")),true);
        assert_ne!(get(&format!("/element/{checkbox}/css/background-color")),before);
    }
    #[test]
    fn captured_resource_handoff_is_exact_and_does_not_reown_ports() {
        let (mut state, parent, parent_chat) = fixture();
        let node: NodeId = "node-a".to_owned().try_into().unwrap();
        let grants = crate::services::grants(&state,&state.runtime.runs[0]);
        let child_chat = crate::chat::create(&mut state);
        let mut child = state.runtime.runs[0].clone();
        child.id = "child-run".into();
        child.chat_id = Some(child_chat);
        state.runtime.runs.push(child);
        assert!(!crate::services::accessible(&state.services[0],&node,&state.runtime.runs[1]));
        state.runtime.runs[1].resource_grants = grants;
        assert!(crate::services::accessible(&state.services[0],&node,&state.runtime.runs[1]));
        let result = admit(&mut state,"child-run","attempt-a","child-request",input(Direction::Ingress),node.clone(), |_|Ok(())).unwrap();
        assert_eq!(state.ports[0].chat,parent_chat);
        assert_eq!(state.ports[0].origin_run,"child-run");
        assert!(accessible(&state,&state.ports[0],&state.runtime.runs[0],&node));
        assert!(crate::services::enqueue_test_command(&mut state,"child-run","exec","svc-a","echo probe",60).is_ok());
        let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(accessible(&restored,&restored.ports[0],&restored.runtime.runs[1],&node));
        let mut denied=restored.runtime.runs[1].clone();
        denied.actor=Actor::Unknown;
        assert!(!crate::services::accessible(&restored.services[0],&node,&denied));
        denied=restored.runtime.runs[1].clone();
        let mut persona=crate::delegation::lineage(&denied);
        persona.role=crate::delegation::Role::Judge;denied.persona=Some(persona);
        assert!(!crate::services::accessible(&restored.services[0],&node,&denied));
        assert!(!crate::services::accessible(&restored.services[0],&"other-node".to_owned().try_into().unwrap(),&restored.runtime.runs[1]));
        state.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
        assert!(!accessible(&state,&state.ports[0],&state.runtime.runs[1],&node));
        assert!(!crate::services::accessible(&state.services[0],&node,&state.runtime.runs[1]));
        assert!(result["port_id"].is_string());
        assert_eq!(state.runtime.runs[0].id,parent);
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
    fn service_lifetime_approval_is_explicit_revocable_and_survives_reload() {
        let (mut state, run, _) = fixture();
        let time = now();
        state.services[0].resource.lease = Lease::until_stopped(time).unwrap();
        request(&mut state,&run,Direction::Ingress).unwrap();
        let id = state.ports[0].id.as_str().to_owned();
        approve(&mut state,&id,-1,false,None,time,|_|Ok(())).unwrap();
        assert!(state.ports[0].summary()["expires_unix"].is_null());
        assert_eq!(state.ports[0].summary()["until_stopped"],true);
        let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(authorized(&restored,&restored.ports[0],time+86400).is_ok());
        state.ports[0].status = Status::Stopping;
        assert!(authorized(&state,&state.ports[0],time+86400).is_err());
        state.ports[0].status = Status::Approved;
        state.services[0].resource.phase = Phase::Stopping {container:None,reason:control_core::resources::StopReason::Requested};
        assert!(authorized(&state,&state.ports[0],time+86400).is_err());
    }
    #[test]
    fn approval_clamps_to_service_deadline_after_operator_review() {
        let (mut state, run, _) = fixture();
        request(&mut state, &run, Direction::Ingress).unwrap();
        let id = state.ports[0].id.as_str().to_owned();
        let deadline = state.services[0].resource.lease.expires();
        // The form was opened with 312 seconds left; 13 seconds pass before approval.
        approve(&mut state, &id, 312, false, None, deadline - 299, |_| Ok(())).unwrap();
        assert_eq!(state.ports[0].approval.as_ref().unwrap().expires, deadline);
        state.ports[0].status = Status::Pending;
        assert!(approve(&mut state, &id, 0, false, None, deadline - 1, |_| Ok(())).is_err());
        assert!(approve(&mut state, &id, 3601, false, None, deadline - 1, |_| Ok(())).is_err());
        assert!(approve(&mut state, &id, 300, false, None, deadline, |_| Ok(())).is_err());
        assert_eq!(state.ports[0].status, Status::Pending);
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
    fn peer_fixture() -> (State, String, NodeId, String) {
        let (mut state, run, _) = fixture();
        let mut client = serde_json::to_value(&state.services[0]).unwrap();
        client["resource"]["id"] = json!("svc-b");
        client["resource"]["phase"] = json!(Phase::Running(ContainerId::try_from("c".repeat(64)).unwrap()));
        state.services.push(serde_json::from_value(client).unwrap());
        let node: NodeId = "node-a".to_owned().try_into().unwrap();
        let ingress = admit(&mut state, &run, "attempt-a", "server", input(Direction::Ingress), node.clone(), |_| Ok(())).unwrap();
        let ingress = ingress["port_id"].as_str().unwrap().to_owned();
        (state, run, node, ingress)
    }
    fn peer_input(peer: &str) -> Input {
        Input {
            service_id: "svc-b".to_owned().try_into().unwrap(),
            name: "game-route".to_owned().try_into().unwrap(),
            direction: Direction::Egress,
            service_port: NonZeroU16::new(25565).unwrap(),
            target: None,
            peer_port_id: Some(peer.to_owned().try_into().unwrap()),
        }
    }
    #[test]
    fn peer_routes_admit_only_owned_ingress_of_another_service() {
        let (mut state, run, node, ingress) = peer_fixture();
        let egress = admit(&mut state, &run, "attempt-a", "client", Input { name: "download".to_owned().try_into().unwrap(), ..input(Direction::Egress) }, node.clone(), |_| Ok(())).unwrap();
        let egress = egress["port_id"].as_str().unwrap().to_owned();
        let both = Input { target: input(Direction::Egress).target, ..peer_input(&ingress) };
        let same_service = Input { service_id: "svc-a".to_owned().try_into().unwrap(), service_port: NonZeroU16::new(31002).unwrap(), ..peer_input(&ingress) };
        let ingress_with_peer = Input { direction: Direction::Ingress, ..peer_input(&ingress) };
        for (nonce, request) in [("both", both), ("self", same_service), ("ingress", ingress_with_peer), ("egress-peer", peer_input(&egress)), ("missing", peer_input("port-missing"))] {
            assert!(admit(&mut state, &run, "attempt-a", nonce, request, node.clone(), |_| Ok(())).is_err(), "{nonce}");
        }
        let foreign = crate::chat::create(&mut state);
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().chat = foreign.clone();
        assert!(admit(&mut state, &run, "attempt-a", "foreign", peer_input(&ingress), node.clone(), |_| Ok(())).is_err());
        let owner = state.services[0].resource.owner.chat.as_str().to_owned();
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().chat = owner;
        let value = admit(&mut state, &run, "attempt-a", "route", peer_input(&ingress), node.clone(), |_| Ok(())).unwrap();
        assert_eq!(value["peer_port_id"], ingress.as_str());
        assert!(value["target"].is_null());
        let changed = Input { peer_port_id: Some(egress.clone().try_into().unwrap()), ..peer_input(&ingress) };
        assert!(admit(&mut state, &run, "attempt-a", "route", changed, node.clone(), |_| Ok(())).is_err());
        let route = route(&state, state.ports.last().unwrap());
        assert!(route.contains("peer ingress echo (service svc-a :31000)"), "{route}");
        let peer = state.ports.iter().position(|p| p.id.as_str() == ingress).unwrap();
        state.ports[peer].status = Status::Stopped;
        assert_eq!(admit(&mut state, &run, "attempt-a", "route", peer_input(&ingress), node.clone(), |_| Ok(())).unwrap()["port_id"], value["port_id"]);
        state.ports[peer].status = Status::Pending;
        state.ports[peer].actor = Actor::Unknown;
        let other = Input { name: "other-route".to_owned().try_into().unwrap(), service_port: NonZeroU16::new(25566).unwrap(), ..peer_input(&ingress) };
        assert!(admit(&mut state, &run, "attempt-a", "actor", other, node, |_| Ok(())).is_err());
    }
    #[test]
    fn peer_route_connects_only_while_the_ingress_listens() {
        let (mut state, run, node, ingress) = peer_fixture();
        admit(&mut state, &run, "attempt-a", "route", peer_input(&ingress), node, |_| Ok(())).unwrap();
        let egress = state.ports.last().unwrap().clone();
        assert!(!peer_listening(&state, &egress));
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().ready = true;
        assert!(peer_listening(&state, &egress));
        let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(!peer_listening(&restored, &egress));
        assert!(peer_listening(&state, &state.ports[0]));
    }
    #[test]
    fn delegated_peer_routes_capture_exact_authority_across_owner_chats() {
        let (mut state,run,node,ingress)=peer_fixture();
        let child=crate::chat::create(&mut state);
        let parent_grant=state.services[0].grant();
        state.services[1].resource.owner.chat=child.clone().try_into().unwrap();
        state.runtime.runs[0].chat_id=Some(child);
        // A shared actor, guessed peer ID or UI parent is insufficient.
        assert!(admit(&mut state,&run,"attempt-a","route",peer_input(&ingress),node.clone(),|_|Ok(())).is_err());
        state.runtime.runs[0].resource_grants=vec![parent_grant];
        let value=admit(&mut state,&run,"attempt-a","route",peer_input(&ingress),node,|_|Ok(())).unwrap();
        let route_id=value["port_id"].as_str().unwrap();
        let time=now();
        approve(&mut state,&ingress,60,false,None,time,|_|Ok(())).unwrap();
        state.ports[0].approval.as_mut().unwrap().host_port=40001;
        approve(&mut state,route_id,60,false,Some("127.0.0.1:40001".parse().unwrap()),time,|_|Ok(())).unwrap();
        let route=state.ports.last().unwrap().clone();
        assert_ne!(route.chat,state.ports[0].chat);
        assert!(authorized(&state,&route,time).is_ok());
        // The captured route survives model-turn history removal and restart.
        state.runtime.runs.clear();
        let restored:State=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(authorized(&restored,&route,time).is_ok());
        state.ports.last_mut().unwrap().peer_grant=None;
        assert!(authorized(&state,&route,time).is_err());
        *state.ports.last_mut().unwrap()=route.clone();
        state.ports[0].actor=Actor::Unknown;
        assert!(authorized(&state,&route,time).is_err());
        state.ports[0].actor=route.actor.clone();
        state.services[0].resource.generation=Generation::INITIAL.checked_next().unwrap();
        assert!(authorized(&state,&route,time).is_err());
    }
    #[test]
    fn peer_route_follows_the_approved_ingress_and_closes_with_it() {
        let (mut state, run, node, ingress) = peer_fixture();
        let egress = admit(&mut state, &run, "attempt-a", "route", peer_input(&ingress), node, |_| Ok(())).unwrap();
        let egress = egress["port_id"].as_str().unwrap().to_owned();
        let time = now();
        let listener: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        assert!(approve(&mut state, &egress, 60, false, Some(listener), time, |_| Ok(())).is_err());
        approve(&mut state, &ingress, 60, false, None, time, |_| Ok(())).unwrap();
        assert!(approve(&mut state, &egress, 60, false, Some(listener), time, |_| Ok(())).is_err());
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().approval.as_mut().unwrap().host_port = 40001;
        for wrong in [None, Some("127.0.0.1:40002".parse().unwrap()), Some("8.8.8.8:40001".parse().unwrap())] {
            assert!(approve(&mut state, &egress, 60, true, wrong, time, |_| Ok(())).is_err());
        }
        approve(&mut state, &egress, 60, true, Some(listener), time, |_| Ok(())).unwrap();
        let port = state.ports.iter().find(|p| p.id.as_str() == egress).unwrap().clone();
        assert!(!port.approval.as_ref().unwrap().allow_loopback);
        assert!(authorized(&state, &port, time).is_ok());
        assert_eq!(intent(&state, &port, &port.node, time), Intent::Serve);
        let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(authorized(&restored, &port, time).is_ok());
        let mut legacy=serde_json::to_value(&state).unwrap();
        for port in legacy["ports"].as_array_mut().unwrap(){port.as_object_mut().unwrap().remove("peer_grant");}
        let legacy:State=serde_json::from_value(legacy).unwrap();
        assert!(authorized(&legacy,&port,time).is_ok(),"Existing same-chat peer approvals remain valid");
        let peer = |state: &mut State| state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().clone();
        let original = peer(&mut state);
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().approval.as_mut().unwrap().host_port = 40002;
        assert!(authorized(&state, &port, time).is_err());
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().clone_from(&original);
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().status = Status::Stopping;
        assert!(authorized(&state, &port, time).is_err());
        assert_eq!(intent(&state, &port, &port.node, time), Intent::Stop);
        state.ports.iter_mut().find(|p| p.id.as_str() == ingress).unwrap().clone_from(&original);
        state.services[0].resource.generation = Generation::INITIAL.checked_next().unwrap();
        assert!(authorized(&state, &port, time).is_err());
    }
    #[test]
    fn legacy_port_records_without_peer_still_load() {
        let (mut state, run, _) = fixture();
        request(&mut state, &run, Direction::Egress).unwrap();
        let mut value = serde_json::to_value(&state.ports[0]).unwrap();
        assert!(value.get("peer").is_none());
        value.as_object_mut().unwrap().remove("peer");
        let port: Port = serde_json::from_value(value).unwrap();
        assert!(port.peer.is_none());
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
    #[ignore = "requires CONTROL_QA_SERVICE_IMAGE Full profile and CONTROL_QA_CONTROL_BIN; isolated service commands, downloads and peer routing, no models"]
    fn live_service_commands_download_and_connect_two_private_services() {
        let image=std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
        let binary=std::env::var("CONTROL_QA_CONTROL_BIN").unwrap();
        let root=PathBuf::from("/private/tmp").join(format!("control-service-interaction-{}-{}",std::process::id(),now()));
        let state_dir=root.join("state");let config=root.join("config");
        fs::create_dir_all(&state_dir).unwrap();fs::create_dir(&config).unwrap();
        let node=ring::digest::digest(&ring::digest::SHA256,root.to_string_lossy().as_bytes()).as_ref()[..16].iter().map(|b|format!("{b:02x}")).collect::<String>();
        fs::write(state_dir.join("node-id"),&node).unwrap();fs::set_permissions(state_dir.join("node-id"),fs::Permissions::from_mode(0o600)).unwrap();
        let cookie="d".repeat(64);fs::write(state_dir.join("operator-session"),&cookie).unwrap();fs::set_permissions(state_dir.join("operator-session"),fs::Permissions::from_mode(0o600)).unwrap();
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();drop(listener);let url=format!("http://{address}");
        let (mut state,run,_)=super::tests::fixture();
        let template=serde_json::to_value(&state.services[0]).unwrap();state.services.clear();
        let mut ids=Vec::new();
        for name in ["source","client","probe"] {
            let mut value=template.clone();let id=format!("svc-{name}-{node}");ids.push(id.clone());
            value["resource"]["id"]=json!(id);value["resource"]["owner"]["node"]=json!(node);
            value["resource"]["image"]=json!(image);value["resource"]["phase"]=json!(Phase::Requested);
            value["resource"]["lease"]=serde_json::to_value(if name=="probe" {control_core::resources::Lease::new(now(),180).unwrap()} else {control_core::resources::Lease::until_stopped(now()).unwrap()}).unwrap();
            value["workspace_exec"]=json!(true);value["profile"]=json!(crate::worker_profile::Profile::inspect(&image).unwrap());
            value["command"]=json!(if name=="probe" {"printf 'short probe stdout\\n'; printf 'short probe stderr\\n' >&2; exit 7"}else{"exec sleep 180"});
            state.services.push(serde_json::from_value(value).unwrap());
        }
        // Mirror a delegated worker creating its own client while holding a
        // grant to the parent's server, rather than testing only one owner.
        let client_chat=crate::chat::create(&mut state);
        state.services[1].resource.owner.chat=client_chat.clone().try_into().unwrap();
        state.runtime.runs[0].chat_id=Some(client_chat);
        state.runtime.runs[0].resource_grants=vec![state.services[0].grant(),state.services[2].grant()];
        state.runtime.runs[0].status="completed".into();
        let write=|state:&State|fs::write(state_dir.join("state.json"),serde_json::to_vec(state).unwrap()).unwrap();
        let read=||serde_json::from_slice::<State>(&fs::read(state_dir.join("state.json")).unwrap()).unwrap();
        write(&state);
        let launch=||{
            let log=fs::OpenOptions::new().create(true).append(true).open(root.join("server.log")).unwrap();
            Server{child:Command::new(&binary).arg("serve").env("CONTROL_STATE_DIR",&state_dir).env("CONTROL_CONFIG_DIR",&config).env("CONTROL_CODEX_AUTH_FILE",root.join("missing-qa-auth.json")).env("CONTROL_CLAUDE_AUTH_FILE",root.join("missing-qa-claude.json")).env("CONTROL_ADDR",address.to_string()).env_remove("CONTROL_WORKER_IMAGE").env_remove("CONTROL_CLAUDE_IMAGE").stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap()}
        };
        struct Cleanup {node:String,root:PathBuf}
        impl Drop for Cleanup {fn drop(&mut self){
            if let Ok(bytes)=fs::read(self.root.join("state/state.json")) && let Ok(state)=serde_json::from_slice::<State>(&bytes) {
                for service in &state.services {if let Some(id)=service.resource.phase.container() && crate::services::port_binding(service,id).is_ok(){let _=crate::engine::cli().args(["rm","-f",id.as_str()]).stdout(Stdio::null()).stderr(Stdio::null()).status();}}
            }
            println!("Service interaction fixture retained at {} (node {})",self.root.display(),self.node);
        }}
        let _cleanup=Cleanup{node:node.clone(),root:root.clone()};
        let mut server=launch();
        wait(||{let state=read();matches!(state.services[0].resource.phase,Phase::Running(_))&&matches!(state.services[1].resource.phase,Phase::Running(_))&&matches!(state.services[2].resource.phase,Phase::Stopped(_))},25,"two services and retained short probe");
        let probe=serde_json::to_value(&read().services[2]).unwrap();
        assert!(probe["output"].as_str().unwrap().contains("short probe stdout"));assert!(probe["output"].as_str().unwrap().contains("short probe stderr"));assert_eq!(probe["exit_code"],7);
        server.stop();state=read();state.runtime.runs[0].status="running".into();
        let source_command=r#"set -eu
cat > /workspace/server.rs <<'RUST'
use std::{io::{Read,Write},net::{TcpListener,Shutdown}};
fn main(){for socket in TcpListener::bind("127.0.0.1:34001").unwrap().incoming(){std::thread::spawn(move||{let mut socket=socket.unwrap();let mut data=Vec::new();socket.read_to_end(&mut data).unwrap();socket.write_all(b"peer:").unwrap();socket.write_all(&data).unwrap();socket.shutdown(Shutdown::Write).unwrap();});}}
RUST
rustc --edition=2024 /workspace/server.rs -o /workspace/server
nohup /workspace/server > /workspace/server.log 2>&1 < /dev/null &
printf source-ready
"#;
        let source_receipt=crate::services::enqueue_test_command(&mut state,&run,"source-setup",&ids[0],source_command,30).unwrap();
        let staged=crate::services::enqueue_test_command(&mut state,&run,"client-files",&ids[1],"set -eu; printf persistent > /workspace/settings; if timeout 1 bash -c 'exec 3<>/dev/tcp/127.0.0.1/34003' 2>/dev/null; then exit 42; fi; printf staged-without-network",10).unwrap();
        let ingress=admit(&mut state,&run,"attempt-a","source-ingress",Input{service_id:ids[0].clone().try_into().unwrap(),name:"source-ingress".to_owned().try_into().unwrap(),direction:Direction::Ingress,service_port:NonZeroU16::new(34001).unwrap(),target:None,peer_port_id:None},node.clone().try_into().unwrap(),|_|Ok(())).unwrap();
        let ingress_id=ingress["port_id"].as_str().unwrap().to_owned();
        state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        let execution=|service:usize,id:&str|{let state=read();let value=serde_json::to_value(&state.services[service]).unwrap();value["executions"].as_array().unwrap().iter().find(|e|e["id"]==id).unwrap().clone()};
        wait(||execution(0,source_receipt["execution_id"].as_str().unwrap())["status"]=="completed"&&execution(1,staged["execution_id"].as_str().unwrap())["status"]=="completed",35,"service file creation after model turn");
        assert_eq!(execution(1,staged["execution_id"].as_str().unwrap())["exit_code"],0);
        let client=reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap();
        let post=|id:&str,fields:&str|{let response=client.post(format!("{url}/tools/ports")).header("Cookie",format!("control_operator={cookie}")).header("Content-Type","application/x-www-form-urlencoded").body(format!("id={id}&{fields}")).send().unwrap();assert_eq!(response.status(),200);};
        let port=|id:&str|read().ports.into_iter().find(|p|p.id.as_str()==id).unwrap();
        assert_eq!(port(&ingress_id).status,Status::Pending);post(&ingress_id,"seconds=-1");
        wait(||port(&ingress_id).approval.as_ref().is_some_and(|a|a.host_port!=0),8,"source ingress approval");
        let published=port(&ingress_id).approval.unwrap().host_port;
        // A private host HTTP fixture represents an explicitly approved download
        // origin. Its binary body never passes through model text or host mounts.
        let download=TcpListener::bind("127.0.0.1:0").unwrap();let download_address=download.local_addr().unwrap();
        let body=(0..262144).map(|n|(n%251)as u8).collect::<Vec<_>>();let expected=ring::digest::digest(&ring::digest::SHA256,&body).as_ref().iter().map(|b|format!("{b:02x}")).collect::<String>();
        let download_thread=thread::spawn(move||{let(mut socket,_)=download.accept().unwrap();socket.set_read_timeout(Some(Duration::from_secs(15))).unwrap();let mut request=[0;4096];let _=socket.read(&mut request).unwrap();write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();socket.write_all(&body).unwrap();});
        server.stop();state=read();state.runtime.runs[0].status="running".into();
        let mut pending=Vec::new();
        for (name,local,target) in [("peer-route",34003,None),("asset-download",34002,Some(download_address))] {
            let value=admit(&mut state,&run,"attempt-a",name,Input{service_id:ids[1].clone().try_into().unwrap(),name:name.to_owned().try_into().unwrap(),direction:Direction::Egress,service_port:NonZeroU16::new(local).unwrap(),target:target.map(|t:SocketAddr|Endpoint{host:"127.0.0.1".into(),port:NonZeroU16::new(t.port()).unwrap()}),peer_port_id:target.is_none().then(||ingress_id.clone().try_into().unwrap())},node.clone().try_into().unwrap(),|_|Ok(())).unwrap();pending.push(value["port_id"].as_str().unwrap().to_owned());
        }
        state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        wait(||client.get(format!("{url}/health")).send().is_ok_and(|r|r.status().is_success()),5,"approval UI online");
        for id in &pending {assert_eq!(port(id).status,Status::Pending);post(id,"seconds=-1&loopback=yes");}
        assert_eq!(port(&pending[0]).approval.unwrap().pinned,Some(SocketAddr::from(([127,0,0,1],published))));
        server.stop();state=read();state.runtime.runs[0].status="running".into();
        let command=format!(r#"set -eu
test "$(cat /workspace/settings)" = persistent
curl --noproxy '*' --fail --silent --show-error --retry 8 --retry-connrefused --retry-delay 1 --max-time 15 --connect-to 'download.invalid:80:127.0.0.1:34002' http://download.invalid/asset -o /workspace/asset
test "$(sha256sum /workspace/asset | cut -d ' ' -f 1)" = {expected}
cat > /workspace/client.rs <<'RUST'
use std::{{io::{{Read,Write}},net::{{TcpStream,Shutdown}},time::{{Duration,Instant}}}};
fn main(){{let rounds=Instant::now();let barrier=std::sync::Arc::new(std::sync::Barrier::new(4));let tasks=(0..4).map(|_|{{let barrier=barrier.clone();std::thread::spawn(move||{{let start=Instant::now();let mut socket=loop{{match TcpStream::connect("127.0.0.1:34003"){{Ok(s)=>break s,Err(_)if start.elapsed()<Duration::from_secs(8)=>std::thread::sleep(Duration::from_millis(100)),Err(e)=>panic!("{{e}}")}}}};socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();barrier.wait();socket.write_all(b"connected").unwrap();socket.shutdown(Shutdown::Write).unwrap();let mut reply=String::new();socket.read_to_string(&mut reply).unwrap();assert_eq!(reply,"peer:connected");}})}}).collect::<Vec<_>>();for task in tasks{{task.join().unwrap();}}println!("peer-connected; 4 concurrent connections in {{:?}}",rounds.elapsed());}}
RUST
rustc --edition=2024 /workspace/client.rs -o /workspace/client
/workspace/client
printf verified-download >> /workspace/completions
"#);
        let receipt=crate::services::enqueue_test_command(&mut state,&run,"download-connect",&ids[1],&command,60).unwrap();state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        let execution_id=receipt["execution_id"].as_str().unwrap();
        wait(||execution(1,execution_id)["status"]=="completed",65,"binary download and cross-container route");
        let outcome=execution(1,execution_id);assert_eq!(outcome["exit_code"],0,"{outcome}");assert!(outcome["output"].as_str().unwrap().contains("peer-connected"));println!("{}",outcome["output"].as_str().unwrap());download_thread.join().unwrap();
        server.stop();state=read();state.runtime.runs[0].status="running".into();
        let check=crate::services::enqueue_test_command(&mut state,&run,"verify-retention",&ids[1],"test $(wc -c < /workspace/asset) = 262144; test $(cat /workspace/completions) = verified-download; printf retained-once",10).unwrap();state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        wait(||execution(1,check["execution_id"].as_str().unwrap())["status"]=="completed",15,"private files and exactly one effect across restart");
        assert_eq!(execution(1,check["execution_id"].as_str().unwrap())["exit_code"],0);
        server.stop();state=read();state.runtime.runs[0].status="running".into();
        let failed=crate::services::enqueue_test_command(&mut state,&run,"failed-command",&ids[1],"printf expected-command-error >&2; exit 7",10).unwrap();state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        wait(||execution(1,failed["execution_id"].as_str().unwrap())["status"]=="failed",15,"nonzero command reports failure");
        assert_eq!(execution(1,failed["execution_id"].as_str().unwrap())["exit_code"],7);
        assert!(execution(1,failed["execution_id"].as_str().unwrap())["output"].as_str().unwrap().contains("expected-command-error"));
        assert!(matches!(read().services[1].resource.phase,Phase::Running(_)));
        server.stop();state=read();state.runtime.runs[0].status="running".into();
        let timeout=crate::services::enqueue_test_command(&mut state,&run,"timeout",&ids[1],"printf before-timeout; exec sleep 60",1).unwrap();state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        wait(||execution(1,timeout["execution_id"].as_str().unwrap())["status"]=="timed_out"&&matches!(read().services[1].resource.phase,Phase::Stopped(_)),15,"timed-out command stops only its service");
        assert!(matches!(read().services[0].resource.phase,Phase::Running(_)));server.stop();state=read();state.runtime.runs[0].status="running".into();
        let crash=crate::services::enqueue_test_command(&mut state,&run,"crash-command",&ids[0],"printf once >> /workspace/crash-marker; exec sleep 60",30).unwrap();state.runtime.runs[0].status="completed".into();write(&state);server=launch();
        wait(||execution(0,crash["execution_id"].as_str().unwrap())["status"]=="running",10,"command running before host crash");
        let running=read();let source=&running.services[0];let container=source.resource.phase.container().unwrap();crate::services::port_binding(source,container).unwrap();
        wait(||crate::engine::query(&["exec",container.as_str(),"cat","/workspace/crash-marker"]).is_ok_and(|text|text=="once"),5,"remote command effect before crash");
        server.crash();server=launch();
        wait(||execution(0,crash["execution_id"].as_str().unwrap())["status"]=="interrupted"&&matches!(read().services[0].resource.phase,Phase::Stopped(_)),15,"uncertain command fenced without replay after crash");server.stop();
        println!("Verified service file creation, retained failed output, explicit scoped binary download, peer routing, restart persistence and timeout cleanup.");
    }
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
        value["profile"] = json!(crate::worker_profile::Profile::inspect(&image).unwrap());
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
                    .env("CONTROL_CODEX_AUTH_FILE", root.join("missing-qa-auth.json"))
                    .env("CONTROL_CLAUDE_AUTH_FILE", root.join("missing-qa-claude.json"))
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
                let _ = crate::engine::cli()
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
                    peer_port_id: None,
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
            crate::engine::query(&[
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
                    crate::engine::query(&[
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
            crate::engine::cli()
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
