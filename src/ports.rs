//! Service-owned TCP ports over verified stdio relays. Containers stay offline.
mod decisions;
mod guidance;
mod ingress_guard;
pub(crate) use guidance::{egress_hint, wait_topic};
mod multiplex;
mod proxy;
pub(crate) use proxy::{env as proxy_env, exports as proxy_exports};
#[path = "../tools/tcp-relay/multiplex.rs"]
mod multiplex_transport;
#[path = "../tools/tcp-relay/protocol.rs"]
mod protocol;
mod tools;
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
    time::{Duration, Instant},
};
pub use tools::{PortCall, PortOps, Selection, Tools};
const MAX_PORTS: usize = 256;
const PORTS_PER_CHAT: usize = 16;
const MAX_RECORDS: usize = 2 * MAX_PORTS;
const MAX_CONNECTIONS: usize = 32;
use crate::config::unix_seconds as now;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct Name(String);
impl TryFrom<String> for Name {
    type Error = &'static str;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.is_empty() || s.len() > 48 || !s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
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
pub enum Direction {
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
            return Err(io::Error::other("Invalid target host"));
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
                return Err(io::Error::other("Target must be a hostname or literal IP"));
            }
            self.host.to_ascii_lowercase()
        };
        reqwest::Url::parse(&format!("http://{host}:{}/", self.port))
            .map_err(|error| io::Error::other(error.to_string()))
    }
    fn pin(&self, allow_loopback: bool) -> io::Result<SocketAddr> {
        let url = self.url()?;
        let answers = crate::web_access::resolve(&url)?;
        validate_destination(self, &answers, allow_loopback)
    }
}
fn validate_destination(endpoint: &Endpoint, answers: &[SocketAddr], allow_loopback: bool) -> io::Result<SocketAddr> {
    let explicit_loopback = allow_loopback && endpoint.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if answers.is_empty()
        || answers.len() > 32
        || answers.iter().any(|a| {
            a.port() != endpoint.port.get()
                || !(crate::web_access::public_ip(a.ip())
                    || explicit_loopback && a.ip().to_string() == endpoint.host && a.ip().is_loopback())
        })
    {
        return Err(io::Error::other(
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peer: Option<ResourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peer_grant: Option<crate::services::Grant>,
    status: Status,
    approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<decisions::Receipt>,
    #[serde(default)]
    diagnostic: String,
    #[serde(skip)]
    ready: bool,
}
/// A service's live ports, briefly, for its status: name, direction, the
/// service-side port and whether it is usable yet.
pub(crate) fn of_service(ports: &[Port], service: &ResourceId) -> Vec<Value> {
    ports.iter().filter(|p| p.service == *service && p.status != Status::Stopped).map(|p| {
        let full = p.summary();
        let mut value = json!({"port_id":p.id,"name":p.name,"direction":p.direction,"service_port":p.service_port,"status":p.status,"ready":p.ready});
        for key in ["local_address", "target", "next"] {
            if !full[key].is_null() {
                value[key] = full[key].clone();
            }
        }
        value
    }).collect()
}
impl Port {
    fn summary(&self) -> Value {
        let mut value = json!({"port_id":self.id,"service_id":self.service,"name":self.name,"direction":self.direction,"service_port":self.service_port,"target":self.target,"peer_port_id":self.peer,"status":self.status,"ready":self.ready,"local_address":self.approval.as_ref().filter(|_|self.direction==Direction::Ingress).filter(|a|a.host_port!=0).map(|a|format!("127.0.0.1:{}",a.host_port)),"pinned_target":self.approval.as_ref().and_then(|a|a.pinned),"expires_unix":self.approval.as_ref().map(|a|a.expires).filter(|expires|*expires!=u64::MAX),"until_stopped":self.approval.as_ref().is_some_and(|a|a.expires==u64::MAX),"diagnostic":self.diagnostic,"approval_url":"/tools/ports"});
        // `ready` is live relay state, never persisted: false until the relay
        // has started after approval (and after a Control restart).
        if let Some(usage) = self
            .usage()
            .filter(|_| matches!(self.status, Status::Pending | Status::Approved))
        {
            value["usage"] = json!(usage);
        }
        if let Some(wait) = self.wait() {
            value["next"] = json!(wait);
        }
        value
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
/// What a port's authority is checked against: the port grants, the services
/// they bind and the chats that own them. Read-only borrows, so a check can
/// neither reach the rest of `State` nor change anything.
#[derive(Clone, Copy)]
pub(crate) struct PortAuthority<'a> {
    ports: &'a [Port],
    services: &'a [Service],
    chats: &'a [crate::chat::Session],
}
impl<'a> PortAuthority<'a> {
    pub(crate) fn of(state: &'a State) -> Self {
        Self {
            ports: &state.ports,
            services: &state.services,
            chats: &state.chats,
        }
    }
}
fn accessible(state: &State, port: &Port, run: &crate::runtime::Run, node: &NodeId) -> bool {
    port.node == *node
        && port.actor == run.actor
        && state.services.iter().any(|service| {
            service.resource.id == port.service
                && service.resource.generation == port.generation
                && service.resource.owner.chat.as_str() == port.chat
                && crate::services::accessible(state, service, node, run)
        })
}
fn service<'a>(authority: PortAuthority<'a>, port: &Port, time: u64) -> io::Result<&'a Service> {
    identity::authorize(&port.actor, Action::Task).map_err(io::Error::other)?;
    if !authority
        .chats
        .iter()
        .any(|c| c.id == port.chat && c.actor == port.actor)
    {
        return Err(io::Error::other("Port owner no longer has this chat"));
    }
    authority
        .services
        .iter()
        .find(|s| {
            s.resource.id == port.service
                && s.resource.generation == port.generation
                && s.owned_by(&port.node, &port.chat, &port.actor)
                && matches!(&s.resource.phase,Phase::Running(id)if id==&port.container)
                && !s.resource.lease.expired(time)
        })
        .ok_or_else(|| io::Error::other("Port service ownership, generation or lifetime changed"))
}
fn peer_endpoint(authority: PortAuthority<'_>, port: &Port, time: u64) -> io::Result<SocketAddr> {
    let id = port.peer.as_ref().ok_or_else(|| io::Error::other("Not a peer route"))?;
    let peer = authority
        .ports
        .iter()
        .find(|p| {
            &p.id == id
                && p.direction == Direction::Ingress
                && p.node == port.node
                && p.actor == port.actor
                && p.service != port.service
        })
        .ok_or_else(|| io::Error::other("Peer ingress is not accessible to this route"))?;
    let peer_service =
        authorized(authority, peer, time).map_err(|e| io::Error::other(format!("Peer ingress unavailable: {e}")))?;
    // Persist the exact authority admitted for this route. A service outlives
    // its model run; sharing an actor or a delegation root is not authority.
    if !port
        .peer_grant
        .as_ref()
        .map_or(peer.chat == port.chat, |grant| grant.matches(&peer_service))
    {
        return Err(io::Error::other("Peer service ownership or generation changed"));
    }
    match peer.approval.as_ref().map(|a| a.host_port) {
        Some(port) if port != 0 => Ok(SocketAddr::from(([127, 0, 0, 1], port))),
        _ => Err(io::Error::other("Peer ingress is not ready yet")),
    }
}
fn peer_listening(ports: &[Port], port: &Port) -> bool {
    port.peer
        .as_ref()
        .is_none_or(|id| ports.iter().any(|p| &p.id == id && p.ready))
}
fn authorized(authority: PortAuthority<'_>, port: &Port, time: u64) -> io::Result<Service> {
    let current = authority
        .ports
        .iter()
        .find(|p| p.id == port.id && p.status == Status::Approved && p.approval == port.approval)
        .ok_or_else(|| io::Error::other("Port was revoked"))?;
    let approval = current
        .approval
        .as_ref()
        .ok_or_else(|| io::Error::other("Port has no approval"))?;
    if time < approval.issued
        || time >= approval.expires
        || (approval.expires != u64::MAX && approval.expires.saturating_sub(approval.issued) > 3600)
    {
        return Err(io::Error::other("Port approval expired"));
    }
    let service = service(authority, current, time)?;
    if approval.expires > service.resource.lease.expires() {
        return Err(io::Error::other("Port lease exceeds service lease"));
    }
    match current.direction {
        Direction::Ingress if approval.pinned.is_none() && current.peer.is_none() => {}
        Direction::Egress if current.peer.is_some() => {
            if current.target.is_some() || approval.pinned != Some(peer_endpoint(authority, current, time)?) {
                return Err(io::Error::other("Peer ingress endpoint changed"));
            }
        }
        Direction::Egress => {
            let endpoint = current
                .target
                .as_ref()
                .ok_or_else(|| io::Error::other("Missing target"))?;
            validate_destination(
                endpoint,
                &[approval
                    .pinned
                    .ok_or_else(|| io::Error::other("Missing pinned target"))?],
                approval.allow_loopback,
            )?;
        }
        _ => return Err(io::Error::other("Invalid port approval")),
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
        .ok_or_else(|| io::Error::other("Run is not active"))?;
    identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
    if crate::delegation::is_judge(run)
        || run.attempts.last().and_then(|a| a.workspace_id.as_deref()) != Some(generation)
    {
        return Err(io::Error::other("Inactive or read-only attempt"));
    }
    let chat = run
        .chat_id
        .as_ref()
        .ok_or_else(|| io::Error::other("Port requires a chat"))?;
    if nonce.is_empty() || nonce.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(io::Error::other("Invalid request nonce"));
    }
    let routes = usize::from(input.target.is_some()) + usize::from(input.peer_port_id.is_some());
    if (input.direction == Direction::Egress) != (routes == 1) || routes > 1 {
        return Err(io::Error::other(
            "Egress requires exactly one of target or peer_port_id; ingress takes neither",
        ));
    }
    if let Some(target) = &input.target {
        target.url()?;
    }
    if input.service_port == proxy::PROXY_PORT {
        return Err(io::Error::other(format!(
            "Service port {} is reserved for Control's egress proxy (HTTPS_PROXY); choose another",
            proxy::PROXY_PORT
        )));
    }
    let service = state
        .services
        .iter()
        .find(|s| {
            s.resource.id == input.service_id
                && crate::services::accessible(state, s, &node, run)
                && matches!(s.resource.phase, Phase::Running(_))
                && !s.resource.lease.expired(now())
        })
        .ok_or_else(|| io::Error::other("A running service owned by this chat is required"))?;
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{}\0{chat}\0{run_id}\0{generation}\0{nonce}", node.as_str()).as_bytes(),
    );
    let suffix = digest.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let id = ResourceId::try_from(format!("port-{suffix}")).map_err(|error| io::Error::other(error.to_string()))?;
    if let Some(old) = state.ports.iter().find(|p| p.id == id) {
        if old.service != input.service_id
            || old.name != input.name
            || old.direction != input.direction
            || old.service_port != input.service_port
            || old.target != input.target
            || old.peer != input.peer_port_id
        {
            return Err(io::Error::other("Nonce reused with a different port request"));
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
        }).ok_or_else(||io::Error::other("peer_port_id must be an active ingress of another owned or explicitly granted service. If a lead created the service after spawning you, ask it to send control_agent_message with services:[service_id]. The grant applies to your queued follow-up turn; port approval remains separate."))?;
        self::service(PortAuthority::of(state),port,now()).map(Service::grant)
    }).transpose()?;
    let owner = service.resource.owner.chat.as_str();
    let (active, owned) = state
        .ports
        .iter()
        .filter(|p| p.status != Status::Stopped)
        .fold((0, 0), |(all, chat), p| (all + 1, chat + usize::from(p.chat == owner)));
    if owned >= PORTS_PER_CHAT {
        return Err(io::Error::other(format!(
            "This chat's services already hold {PORTS_PER_CHAT} active ports. Stop finished routes, such as completed downloads, with control_port_stop"
        )));
    }
    if active >= MAX_PORTS {
        return Err(io::Error::other(format!(
            "Control's node-wide limit of {MAX_PORTS} active ports is reached. Stop finished routes with control_port_stop or ask the operator"
        )));
    }
    if state.ports.iter().any(|p| {
        p.service == input.service_id
            && p.status != Status::Stopped
            && (p.name == input.name
                || (p.direction == Direction::Egress
                    && input.direction == Direction::Egress
                    && p.service_port == input.service_port))
    }) {
        return Err(io::Error::other(
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
                            && r.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(&p.origin_attempt)
                    })
            })
            .ok_or_else(|| io::Error::other("Port history belongs to active attempts; begin a new turn"))?;
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
    let node = NodeId::try_from(crate::config::node_id().map_err(io::Error::other)?)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let call = match call {
        PortCall::Request(input) => {
            let value = admit(
                &mut *shared.lock().map_err(|_| io::Error::other("State unavailable"))?,
                run,
                generation,
                attempt.nonce,
                input,
                node,
                crate::storage::save,
            )?;
            // The policy is read and the endpoint pinned without the state lock.
            let policy = control::permissions::read().auto_approval;
            let Some(id) = value["port_id"].as_str().filter(|_| value["status"] == "pending") else {
                return Ok(value);
            };
            return Ok(auto_approve(
                shared,
                id,
                &policy,
                crate::storage::save,
                |service, container| {
                    crate::services::port_binding(crate::engine::isolation(), service, container).map(|_| ())
                },
                |endpoint, loopback| endpoint.pin(loopback),
            )
            .unwrap_or(value));
        }
        other => other,
    };
    let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
    let (stop, port_id) = match call {
        PortCall::Request(_) => unreachable!("handled above"),
        PortCall::Status(port_id) => (None, port_id),
        PortCall::Stop(Selection::Port(port_id)) => (Some(None), Some(port_id)),
        PortCall::Stop(Selection::Matching { service, direction }) => (Some(Some((service, direction))), None),
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
        .ok_or_else(|| io::Error::other("Attempt is not active"))?;
    identity::authorize(&r.actor, r.action).map_err(io::Error::other)?;
    if crate::delegation::is_judge(r) {
        return Err(io::Error::other("Judge cannot manage ports"));
    }
    r.chat_id
        .as_ref()
        .ok_or_else(|| io::Error::other("A chat is required"))?;
    let owned = |p: &&Port| accessible(&state, p, r, &node);
    if let Some(Some((service, direction))) = stop {
        let chosen: Vec<usize> = state
            .ports
            .iter()
            .enumerate()
            .filter(|(_, p)| p.status != Status::Stopped && owned(p))
            .filter(|(_, p)| {
                service.as_ref().is_none_or(|s| &p.service == s) && direction.is_none_or(|d| p.direction == d)
            })
            .map(|(index, _)| index)
            .collect();
        return stop_all(&mut state, &chosen, crate::storage::save);
    }
    if stop.is_none() && port_id.is_none() {
        return Ok(
            json!({"ports":state.ports.iter().filter(owned).filter(|p|p.status!=Status::Stopped).chain(state.ports.iter().rev().filter(owned).filter(|p|p.status==Status::Stopped).take(10)).map(Port::summary).collect::<Vec<_>>()}),
        );
    }
    let index = state
        .ports
        .iter()
        .position(|p| accessible(&state, p, r, &node) && Some(&p.id) == port_id.as_ref())
        .ok_or_else(|| io::Error::other("Owned port not found"))?;
    if stop.is_some() && state.ports[index].status != Status::Stopped {
        let old = state.ports[index].status;
        state.ports[index].status = Status::Stopping;
        if let Err(e) = crate::storage::save(&state) {
            state.ports[index].status = old;
            return Err(e);
        }
    }
    Ok(state.ports[index].summary())
}

fn stop_all(state: &mut State, chosen: &[usize], save: impl Fn(&State) -> io::Result<()>) -> io::Result<Value> {
    let old: Vec<Status> = chosen.iter().map(|&i| state.ports[i].status).collect();
    chosen.iter().for_each(|&i| state.ports[i].status = Status::Stopping);
    if let Err(e) = save(state) {
        chosen
            .iter()
            .zip(old)
            .for_each(|(&i, status)| state.ports[i].status = status);
        return Err(e);
    }
    Ok(json!({"stopped":chosen.iter().map(|&i| state.ports[i].summary()).collect::<Vec<_>>()}))
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
        .ok_or_else(|| io::Error::other("Pending request not found"))?;
    let port = &state.ports[index];
    let service = service(PortAuthority::of(state), port, time)?;
    let expires = match seconds {
        -1 => service.resource.lease.expires(),
        1..=3600 => time
            .checked_add(seconds as u64)
            .ok_or_else(|| io::Error::other("Invalid lease"))?
            .min(service.resource.lease.expires()),
        _ => return Err(io::Error::other("Choose service lifetime (-1) or 1–3600 seconds")),
    };
    match port.direction {
        Direction::Ingress if pinned.is_none() => {}
        Direction::Egress if port.peer.is_some() => {
            if pinned.is_none() || pinned != Some(peer_endpoint(PortAuthority::of(state), port, time)?) {
                return Err(io::Error::other(
                    "Peer ingress endpoint changed; review the request again",
                ));
            }
        }
        Direction::Egress => {
            validate_destination(
                port.target.as_ref().ok_or_else(|| io::Error::other("Missing target"))?,
                &[pinned.ok_or_else(|| io::Error::other("Missing pinned address"))?],
                allow_loopback,
            )?;
        }
        _ => return Err(io::Error::other("Invalid approval")),
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
/// What every approval checks first: the pending request, a snapshot of its
/// service and, for a peer route, the peer ingress endpoint.
fn review(authority: PortAuthority<'_>, id: &str) -> io::Result<(Port, Service, Option<SocketAddr>)> {
    let p = authority
        .ports
        .iter()
        .find(|p| p.id.as_str() == id && p.status == Status::Pending)
        .ok_or_else(|| io::Error::other("Pending port not found"))?;
    let peer = p
        .peer
        .is_some()
        .then(|| peer_endpoint(authority, p, now()))
        .transpose()?;
    Ok((p.clone(), service(authority, p, now())?.binding_snapshot(), peer))
}
/// The chat whose agent asked: the requesting run's chat, else the owner.
fn requester<'a>(runs: &'a [crate::runtime::Run], port: &'a Port) -> &'a str {
    runs.iter()
        .find(|run| run.id == port.origin_run)
        .and_then(|run| run.chat_id.as_deref())
        .unwrap_or(&port.chat)
}
/// The facts the approval policy decides on.
fn facts<'a>(state: &'a State, port: &'a Port) -> crate::auto_approval::PortFacts<'a> {
    let root = crate::chat::team_root(state, requester(&state.runtime.runs, port));
    let in_task = |chat: &str| crate::chat::team_root(state, chat) == root;
    crate::auto_approval::PortFacts {
        actor: &port.actor,
        ingress: port.direction == Direction::Ingress,
        service_port: port.service_port.get(),
        target: port
            .target
            .as_ref()
            .map(|target| (target.host.as_str(), target.port.get())),
        peer_in_task: port.peer.as_ref().map(|peer| {
            state
                .ports
                .iter()
                .any(|p| &p.id == peer && p.status == Status::Approved && in_task(&p.chat))
        }),
        service_in_task: in_task(&port.chat),
        task_ports: state
            .ports
            .iter()
            .filter(|p| p.status != Status::Stopped && in_task(&p.chat))
            .count(),
    }
}
/// Plain text for records and notices.
fn describe(port: &Port) -> String {
    let route = match (&port.target, &port.peer) {
        (Some(target), _) => format!("service :{} → {}:{}", port.service_port, target.host, target.port),
        (None, Some(peer)) => format!("service :{} → peer ingress {}", port.service_port, peer.as_str()),
        (None, None) => format!("host 127.0.0.1 → service :{}", port.service_port),
    };
    format!(
        "{} {} ({route})",
        match port.direction {
            Direction::Ingress => "ingress",
            Direction::Egress => "egress",
        },
        port.name.0
    )
}
/// Applies the node's automatic approval policy (`crate::auto_approval`) to
/// the pending request `id`. An approval makes the operator's transition for
/// the service's lifetime, after the same service and endpoint checks; it
/// returns the approved summary. Anything else, including a failed check or
/// a name that pins to a private address, leaves the request with the
/// operator and returns `None`.
fn auto_approve(
    shared: &Arc<Mutex<State>>,
    id: &str,
    policy: &control::permissions::AutoApproval,
    save: impl FnOnce(&State) -> io::Result<()>,
    verify: impl FnOnce(&Service, &ContainerId) -> io::Result<()>,
    pin: impl FnOnce(&Endpoint, bool) -> io::Result<SocketAddr>,
) -> Option<Value> {
    use crate::auto_approval::{Request, Verdict, classify};
    let (rule, request, service, peer) = {
        let state = shared.lock().ok()?;
        let port = state
            .ports
            .iter()
            .find(|p| p.id.as_str() == id && p.status == Status::Pending)?;
        let Verdict::Approve(rule) = classify(policy, &Request::Port(facts(&state, port))) else {
            return None;
        };
        let (request, service, peer) = review(PortAuthority::of(&state), id).ok()?;
        (rule, request, service, peer)
    };
    verify(&service, &request.container).ok()?;
    // Only a listed standing target may be a literal loopback address.
    let loopback = rule == "standing-target"
        && request
            .target
            .as_ref()
            .is_some_and(|target| target.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()));
    let pinned = match peer {
        Some(peer) => Some(peer),
        None => match request.target.as_ref() {
            Some(target) => Some(pin(target, loopback).ok()?),
            None => None,
        },
    };
    let mut state = shared.lock().ok()?;
    let index = state.ports.iter().position(|p| p.id.as_str() == id)?;
    let (old, decisions, notices) = (
        state.ports[index].clone(),
        state.auto_decisions.clone(),
        state.bots.notices.clone(),
    );
    approve(&mut state, id, -1, loopback, pinned, now(), |_| Ok(())).ok()?;
    // The tool result carries the decision, so no decision turn follows.
    state.ports[index].decision = Some(decisions::Receipt::settled(true));
    let chat = requester(&state.runtime.runs, &old).to_owned();
    crate::auto_approval::record(&mut state, policy, "port", id, &chat, describe(&old), rule);
    if save(&state).is_err() {
        state.ports[index] = old;
        state.auto_decisions = decisions;
        state.bots.notices = notices;
        return None;
    }
    let mut value = state.ports[index].summary();
    value["decided_by"] = json!("policy");
    value["rule"] = json!(rule);
    Some(value)
}
/// Open ports, one line each, for the Permissions page.
pub(crate) fn overview(ports: &[Port], decisions: &[crate::auto_approval::Record]) -> Vec<String> {
    ports
        .iter()
        .filter(|p| p.status != Status::Stopped)
        .map(|p| {
            let auto = crate::auto_approval::rule(decisions, p.id.as_str())
                .map(|rule| format!(" <span class='tag'>auto · {}</span>", crate::escape(&rule)))
                .unwrap_or_default();
            format!(
                "<p>{} · {} · {:?}{auto}</p>",
                crate::escape(&p.name.0),
                route(ports, p),
                p.status
            )
        })
        .collect()
}
pub fn http(stream: &mut TcpStream, shared: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8]) -> bool {
    http_with_save(stream, shared, method, path, body, crate::storage::save)
}
pub(crate) fn http_with_save(
    stream: &mut TcpStream,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    body: &[u8],
    save: impl Fn(&State) -> io::Result<()>,
) -> bool {
    if path != "/tools/ports" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let asynchronous = fields.get("response").is_some_and(|value| value == "json");
    let return_chat = fields
        .get("return_chat")
        .filter(|id| crate::chat::valid_id(id))
        .cloned();
    let result = (|| -> io::Result<String> {
        if method == "POST" {
            let id = fields.get("id").ok_or_else(|| io::Error::other("Missing port ID"))?;
            if let Some(chat) = &return_chat {
                let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
                if !pending_for_chat(&state, chat).iter().any(|port| port.id.as_str() == id) {
                    return Err(io::Error::other(
                        "Port request is no longer pending in this conversation",
                    ));
                }
            }
            if fields.contains_key("revoke") {
                let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
                let p = state
                    .ports
                    .iter_mut()
                    .find(|p| p.id.as_str() == id)
                    .ok_or_else(|| io::Error::other("Port not found"))?;
                let old = p.clone();
                if p.status == Status::Pending {
                    p.decision = Some(decisions::Receipt::pending(false));
                }
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
                let (request, service, peer) = review(
                    PortAuthority::of(&*shared.lock().map_err(|_| io::Error::other("State unavailable"))?),
                    id,
                )?;
                if request.node.as_str() != crate::config::node_id().map_err(io::Error::other)? {
                    return Err(io::Error::other("Port belongs to another node"));
                }
                // The image must speak this host's relay version, for any lifetime.
                crate::services::relay_capable(crate::engine::isolation(), &service)?;
                crate::services::port_binding(crate::engine::isolation(), &service, &request.container)?;
                let allow_loopback = fields.get("loopback").is_some_and(|v| v == "yes");
                let pinned = match peer {
                    Some(peer) => Some(peer),
                    None => request.target.as_ref().map(|e| e.pin(allow_loopback)).transpose()?,
                };
                let seconds = fields
                    .get("seconds")
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| io::Error::other("Expected lease seconds"))?;
                let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
                approve(&mut state, id, seconds, allow_loopback, pinned, now(), &save)?;
            }
        } else if method != "GET" {
            return Err(io::Error::other("Use GET or POST"));
        }
        if method == "POST" && (asynchronous || return_chat.is_some()) {
            return Ok(String::new());
        }
        let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
        let ports=state.ports.iter().rev().filter(|p|p.status!=Status::Stopped).take(MAX_PORTS).map(|p|{
            let detail=route(&state.ports,p);
            let action=if p.status==Status::Pending{format!("<label>Allow for<select name='seconds'><option value='-1'>Service lifetime</option><option value='900'>Up to 15 minutes</option><option value='3600'>Up to 1 hour</option></select></label><p class='muted'>Access ends when this service stops. You can revoke it at any time.</p>{}<button class='primary'>Allow port</button><button name='revoke' value='yes' formnovalidate>Deny</button>",if p.target.is_some(){"<label><input type='checkbox' name='loopback' value='yes'> Allow this exact literal loopback target</label>"}else{""})}else{"<button name='revoke' value='yes'>Revoke</button>".into()};
            let address=p.approval.as_ref().filter(|a|a.host_port!=0).map(|a|format!(" · 127.0.0.1:{}",a.host_port)).unwrap_or_default();
            let auto=crate::auto_approval::rule(&state.auto_decisions,p.id.as_str()).map(|rule|format!("<span class='tag'>auto · {}</span>",crate::escape(&rule))).unwrap_or_default();
            format!("<article>{auto}<h3>{}</h3><p>{detail}{address}</p><p>{:?} · {} · {} · {}</p><p>{}</p><form class='decision-form' method='post' action='/tools/ports'><input type='hidden' name='id' value='{}'>{action}</form></article>",crate::escape(&p.name.0),p.status,crate::escape(&p.chat),if p.ready{"ready"}else{"waiting"},crate::escape(p.service.as_str()),crate::escape(&p.diagnostic),crate::escape(p.id.as_str()))
        }).collect::<String>();
        Ok(crate::shell(
            "tool-page",
            &format!(
                "<h2>TCP ports</h2>{ports}<p class='muted'>Request a named service port from a DM. Access remains limited to this service and lease.</p>"
            ),
        ))
    })();
    if asynchronous {
        let (status, value) = match result {
            Ok(_) => ("200 OK", json!({"ok":true})),
            Err(error) => ("400 Bad Request", json!({"error":error.to_string()})),
        };
        let _ = control_http::respond(
            stream,
            status,
            "application/json",
            value.to_string().as_bytes(),
            "",
            control_http::Policy::Page,
        );
        return true;
    }
    if result.is_ok()
        && method == "POST"
        && let Some(chat) = return_chat
    {
        let _ = control_http::respond(
            stream,
            "303 See Other",
            "text/plain",
            b"",
            &format!("Location: /dm/{chat}\r\n"),
            control_http::Policy::Page,
        );
        return true;
    }
    let (status, page) = match result {
        Ok(p) => ("200 OK", p),
        Err(e) => ("400 Bad Request", crate::error_page(&e.to_string(), "/tools/ports")),
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
    let Some(chat) = state.chats.iter().find(|chat| chat.id == id) else {
        return vec![];
    };
    let mut runs = std::collections::HashSet::new();
    for run in state
        .runtime
        .runs
        .iter()
        .filter(|run| run.chat_id.as_deref() == Some(id) && run.actor == chat.actor)
    {
        runs.insert(run.id.as_str());
        if let Ok((_, children)) = crate::delegated_results::scope(state, run) {
            runs.extend(children);
        }
    }
    let requester = |port: &Port| {
        state
            .runtime
            .runs
            .iter()
            .find(|run| run.id == port.origin_run)
            .and_then(|run| run.chat_id.as_deref())
    };
    state
        .ports
        .iter()
        .filter(|port| {
            port.status == Status::Pending
                && port.actor == chat.actor
                && (crate::chat::within(state, &port.chat, id)
                    || runs.contains(port.origin_run.as_str())
                    || requester(port).is_some_and(|c| crate::chat::within(state, c, id)))
        })
        .collect()
}

/// Owning chats of every pending port request, for the node-wide view.
pub(crate) fn pending_chats(ports: &[Port]) -> impl Iterator<Item = &str> {
    ports
        .iter()
        .filter(|port| port.status == Status::Pending)
        .map(|port| port.chat.as_str())
}
pub(crate) fn pending_json(state: &State, id: &str) -> Vec<Value> {
    pending_for_chat(state, id)
        .into_iter()
        .map(|port| {
            let mut value = port.summary();
            value["loopback_required"] = json!(
                port.target
                    .as_ref()
                    .is_some_and(|target| target.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
            );
            value
        })
        .collect()
}

fn route(ports: &[Port], port: &Port) -> String {
    match (&port.target, &port.peer) {
        (Some(target), _) => format!(
            "Service :{} → {}:{}",
            port.service_port,
            crate::escape(&target.host),
            target.port
        ),
        (None, Some(peer)) => {
            let ingress = ports.iter().find(|p| &p.id == peer);
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
pub(crate) fn pending_panel(state: &State, id: &str) -> String {
    pending_for_chat(state,id).into_iter().map(|port| {
        let detail=route(&state.ports,port);
        let loopback=if port.target.as_ref().is_some_and(|target|target.host.parse::<IpAddr>().is_ok_and(|ip|ip.is_loopback())) {
            "<label><input type='checkbox' name='loopback' value='yes' required> Allow this exact loopback target</label>"
        }else{""};
        format!("<article class='assistant-message port-request' data-transcript-key='port-{}'><h3>Port access · {}</h3><p>{detail}</p><form class='decision-form' method='post' action='/tools/ports' target='_top'><input type='hidden' name='id' value='{}'><input type='hidden' name='return_chat' value='{}'><label>Allow for<select name='seconds'><option value='-1'>Service lifetime</option><option value='900'>Up to 15 minutes</option><option value='3600'>Up to 1 hour</option></select></label>{loopback}<button class='primary'>Allow port</button><button name='revoke' value='yes' formnovalidate>Deny</button><p role='alert' hidden></p></form></article>",port.id.as_str(),crate::escape(&port.name.0),port.id.as_str(),crate::escape(id))
    }).collect()
}

pub(crate) fn decision_revision(ports: &[Port]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for port in ports {
        (&port.id, &port.status).hash(&mut hash);
    }
    hash.finish()
}
fn live_service(shared: &Arc<Mutex<State>>, port: &Port, cancel: &AtomicBool) -> io::Result<Service> {
    if cancel.load(Ordering::Acquire) || crate::shutdown::requested() {
        return Err(io::Error::other("Port is stopping"));
    }
    let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
    authorized(PortAuthority::of(&state), port, now())
}
fn receipt(shared: &Arc<Mutex<State>>, port: &Port, ready: bool, diagnostic: &str) {
    if let Ok(mut state) = shared.lock()
        && let Some(p) = state
            .ports
            .iter_mut()
            .find(|p| p.id == port.id && p.approval == port.approval && p.status == Status::Approved)
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
    alive: &dyn Fn() -> bool,
) -> io::Result<ChildStdout> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Relay output unavailable"))?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = vec![0; expected.len()];
        let result = stdout.read_exact(&mut bytes).and_then(|()| {
            if bytes == expected {
                Ok(stdout)
            } else {
                Err(io::Error::other("Unexpected relay handshake"))
            }
        });
        let _ = tx.send(result);
    });
    let started = Instant::now();
    let result = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(io::Error::other("Relay handshake closed"));
            }
            Err(_) => {}
        }
        if started.elapsed() >= wait || !alive() {
            break Err(io::Error::other("Relay handshake expired or was revoked"));
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
                .ok_or_else(|| io::Error::other("Relay input unavailable"))?,
        ));
        let alive = Arc::new(AtomicBool::new(true));
        if let Err(error) = protocol::write(
            &mut *input.lock().map_err(|_| io::Error::other("Relay unavailable"))?,
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
                    .map_err(|_| io::Error::other("Relay unavailable"))
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
                                .map_err(|_| io::Error::other("Relay unavailable"))
                                .and_then(|mut w| protocol::write(&mut *w, protocol::Kind::Data, &bytes))
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
                .map_err(|_| io::Error::other("Relay unavailable"))
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
                        .map_err(|_| io::Error::other("Relay unavailable"))
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
                Ok((protocol::Kind::Data, n)) if !fin && permission.swap(false, Ordering::AcqRel) => {
                    if tx.try_send(Some(buffer[..n].to_vec())).is_err() {
                        failure.store(true, Ordering::Release);
                        break;
                    }
                }
                Ok((protocol::Kind::Fin, _)) if !fin && permission.swap(false, Ordering::AcqRel) => {
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
fn connect_relay(shared: &Arc<Mutex<State>>, port: &Port, cancel: &AtomicBool) -> io::Result<(Relay, ChildStdout)> {
    let service = live_service(shared, port, cancel)?;
    let binding = crate::services::port_binding(crate::engine::isolation(), &service, &port.container)?;
    live_service(shared, port, cancel)?;
    let mut child = Relay::new(binding.spawn(
        crate::engine::isolation(),
        RelayMode::Connect,
        port.service_port,
        port.approval.as_ref().unwrap().expires,
    )?)?;
    let output = handshake(&mut child, b"CONTROL_RELAY_V2_READY\n", Duration::from_secs(3), &|| {
        live_service(shared, port, cancel).is_ok()
    })?;
    Ok((child, output))
}
fn ingress(shared: Arc<Mutex<State>>, mut port: Port, cancel: Arc<AtomicBool>) -> io::Result<()> {
    let service = live_service(&shared, &port, &cancel)?;
    crate::services::port_binding(crate::engine::isolation(), &service, &port.container)?;
    let listener = TcpListener::bind(("127.0.0.1", port.approval.as_ref().unwrap().host_port))?;
    listener.set_nonblocking(true)?;
    let assigned = listener.local_addr()?.port();
    if port.approval.as_ref().unwrap().host_port == 0 {
        let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
        authorized(PortAuthority::of(&state), &port, now())?;
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
        if active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            thread::sleep(Duration::from_millis(25));
            continue;
        }
        match listener.accept() {
            Ok((socket, _)) => {
                active.fetch_add(1, Ordering::AcqRel);
                let active = active.clone();
                let shared = shared.clone();
                let port = port.clone();
                let cancel = cancel.clone();
                sessions.push(thread::spawn(move || {
                    let result = connect_relay(&shared, &port, &cancel)
                        .and_then(|(child, output)| bridge(socket, child, output, &shared, &port, &cancel));
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
    multiplex::egress(shared, port, cancel)
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
fn intent(authority: PortAuthority<'_>, port: &Port, node: &NodeId, time: u64) -> Intent {
    if &port.node != node {
        return Intent::Stop;
    }
    match port.status {
        Status::Pending if service(authority, port, time).is_ok() => Intent::Wait,
        Status::Approved if authorized(authority, port, time).is_ok() => Intent::Serve,
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
        let Ok(node) = crate::config::node_id().and_then(|s| NodeId::try_from(s).map_err(|_| "Invalid node ID")) else {
            return;
        };
        let mut active: HashMap<ResourceId, Active> = HashMap::new();
        let mut proxies = proxy::Proxies::default();
        let changes = crate::storage::changes().clone();
        let shutdown = crate::shutdown::token();
        while !crate::shutdown::requested() {
            // Saves made during this pass, including its own, cause one more pass.
            let seen = changes.generation();
            let (mut start, mut stop, mut proxied) = (Vec::new(), Vec::new(), Vec::new());
            {
                let Ok(mut state) = shared.lock() else { break };
                let _ = decisions::advance(&mut state, crate::storage::save);
                let time = now();
                for port in state.ports.iter().filter(|p| p.status != Status::Stopped) {
                    match intent(PortAuthority::of(&state), port, &node, time) {
                        Intent::Serve => {
                            let key = proxy::Key::of(port);
                            if port.direction == Direction::Egress && port.target.is_some() && !proxied.contains(&key) {
                                proxied.push(key);
                            }
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
            proxies.sync(&shared, &proxied);
            // Approvals, revocations and service changes are saved and wake this
            // loop at once; the bound covers time-based expiry and relay restarts.
            let _ = changes.wait_newer(seen, Some(Instant::now() + Duration::from_secs(1)), &shutdown);
        }
        proxies.stop();
        for (_, a) in active {
            a.cancel.store(true, Ordering::Release);
            let _ = a.handle.join();
        }
    })
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
#[path = "ports/tests/live_tests.rs"]
mod live_tests;

#[cfg(test)]
#[path = "ports/tests/restore_tests.rs"]
mod restore_tests;

#[cfg(test)]
#[path = "ports/tests/auto_tests.rs"]
mod auto_tests;
