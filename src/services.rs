//! Chat-owned, bounded offline services. Attempts only attach; a pure core
//! reducer decides effects, and one supervisor interprets durable intents.
mod commands;
mod tools;
pub use tools::{ServiceCall, ServiceOps, Tools};
pub(crate) use commands::activity as command_activity;
#[cfg(test)]
pub(crate) use commands::enqueue_test as enqueue_test_command;
use crate::{
    State,
    identity::{self, Action, Actor},
    tool_registry::AttemptRef,
};
use control_core::resources::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const MAX_ACTIVE: usize = 8;
const MAX_RECORDS: usize = 128;
const OUTPUT: usize = 16000;
fn fail(s: impl ToString) -> io::Error {
    io::Error::other(s.to_string())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct CommandText(Arc<str>);
impl TryFrom<String> for CommandText {
    type Error = io::Error;
    fn try_from(s: String) -> io::Result<Self> {
        if s.trim().is_empty() || s.len() > 16000 || s.contains('\0') {
            Err(fail("Command must contain 1–16000 bytes without NUL"))
        } else {
            Ok(Self(s.into()))
        }
    }
}
impl<'de> Deserialize<'de> for CommandText {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}
/// Missing durable fields mean the original tmpfs layout. Never reinterpret a
/// running legacy container when the operator upgrades Control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Storage {
    #[default]
    Memory,
    Disk { limit_mb: Option<NonZeroU32> },
}
impl Storage {
    fn is_memory(&self) -> bool { *self == Self::Memory }
    fn options(self) -> HashMap<String, String> {
        match self {
            Self::Disk { limit_mb: Some(limit) } => HashMap::from([("size".into(), format!("{limit}M"))]),
            _ => HashMap::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub resource: Resource,
    origin: Origin,
    actor: Actor,
    command: CommandText,
    uid: NonZeroU32,
    gid: u32,
    #[serde(default, skip_serializing_if="std::ops::Not::not")]
    workspace_exec: bool,
    #[serde(default, skip_serializing_if = "crate::worker_profile::Profile::is_lite")]
    profile: crate::worker_profile::Profile,
    #[serde(default, skip_serializing_if = "Storage::is_memory")]
    storage: Storage,
    #[serde(default)]
    diagnostic: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    executions: Vec<commands::Execution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    command_receipts: Vec<commands::Receipt>,
}

/// Captured at a trusted delegation transition, never accepted as tool input.
/// Grants bind one exact resource generation and preserve its original owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    id: ResourceId,
    owner: control_core::resources::Owner,
    generation: control_core::resources::Generation,
}
impl Grant {
    pub(crate) fn matches(&self, service: &Service) -> bool {
        self.id == service.resource.id && self.owner == service.resource.owner
            && self.generation == service.resource.generation
    }
}
pub(crate) fn accessible(service: &Service, node: &NodeId, run: &crate::runtime::Run) -> bool {
    service.resource.owner.node == *node && service.actor == run.actor
        && !crate::delegation::is_judge(run)
        && (run.chat_id.as_deref() == Some(service.resource.owner.chat.as_str())
            || run.resource_grants.iter().any(|grant|grant.matches(service)))
}
pub(crate) fn grants(state: &State, run: &crate::runtime::Run) -> Vec<Grant> {
    state.services.iter().filter(|service|
        accessible(service, &service.resource.owner.node, run)
    ).map(Service::grant).collect()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    run: RunId,
    attempt: AttemptId,
}
impl Service {
    pub(crate) fn grant(&self) -> Grant {
        Grant { id:self.resource.id.clone(),owner:self.resource.owner.clone(),generation:self.resource.generation }
    }
    pub(crate) fn binding_snapshot(&self)->Self {
        Self {resource:self.resource.clone(),origin:self.origin.clone(),actor:self.actor.clone(),command:self.command.clone(),uid:self.uid,gid:self.gid,workspace_exec:self.workspace_exec,profile:self.profile,storage:self.storage,diagnostic:String::new(),output:String::new(),exit_code:None,executions:Vec::new(),command_receipts:Vec::new()}
    }
    pub(crate) fn owned_by(&self, node: &NodeId, chat: &str, actor: &Actor) -> bool {
        self.resource.owner.node == *node
            && self.resource.owner.chat.as_str() == chat
            && self.actor == *actor
    }
    fn name(&self) -> String {
        format!(
            "control-service-{}-{}",
            self.resource.id.as_str(),
            self.resource.generation.get()
        )
    }
    fn readonly_root(&self) -> bool {
        self.storage == Storage::Memory && self.profile.readonly_root()
    }
    fn summary(&self) -> Value {
        json!({"service_id":self.resource.id,"phase":self.resource.phase,"scope":self.resource.scope,"storage":self.storage,"workspace_executable":self.workspace_exec,"expires_unix":(!self.resource.lease.is_until_stopped()).then(||self.resource.lease.expires()),"until_stopped":self.resource.lease.is_until_stopped(),"diagnostic":self.diagnostic,"output":self.output,"exit_code":self.exit_code,"output_limit_bytes":OUTPUT,"executions":self.executions.iter().map(|e|e.brief()).collect::<Vec<_>>()})
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    command: CommandText,
    #[serde(default = "default_lease")]
    lease_seconds: i64,
    #[serde(default)]
    storage_limit_mb: Option<NonZeroU32>,
}
fn default_lease() -> i64 {
    -1
}
fn caller<'a>(
    s: &'a State,
    run: &str,
    generation: &str,
) -> io::Result<(&'a crate::runtime::Run, &'a str)> {
    let run = s
        .runtime
        .runs
        .iter()
        .find(|r| r.id == run && r.status == "running")
        .ok_or_else(|| fail("Run is not active"))?;
    identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::is_judge(run)
        || run.attempts.last().and_then(|a| a.workspace_id.as_deref()) != Some(generation)
    {
        return Err(fail(
            "Service request belongs to an inactive or read-only attempt",
        ));
    }
    Ok((
        run,
        run.chat_id
            .as_deref()
            .ok_or_else(|| fail("Service requires a chat"))?,
    ))
}
pub(crate) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: ServiceCall) -> io::Result<Value> {
    let (run, generation, nonce) = (attempt.run, attempt.generation, attempt.nonce);
    let node: NodeId = crate::config::node_id()
        .map_err(fail)?
        .try_into()
        .map_err(fail)?;
    let (stop, list, id) = match call {
        ServiceCall::Start(input) => return start(shared, run, generation, nonce, node, input),
        ServiceCall::Exec(input) => return commands::exec(shared, run, generation, nonce, &node, input),
        ServiceCall::ExecStatus(handle) => return commands::status(shared, run, generation, &node, handle),
        ServiceCall::Status(id) => (false, true, id),
        ServiceCall::Attach(id) => (false, false, id),
        ServiceCall::Stop(id) => (true, false, id),
    };
    let mut s = shared.lock().map_err(|_| fail("State unavailable"))?;
    let (active, _) = caller(&s, run, generation)?;
    let owned = |service: &&Service| accessible(service, &node, active);
    let Some(id) = id else {
        return if list {
            Ok(
                json!({"services":s.services.iter().filter(owned).filter(|service|service.resource.phase.reserves_capacity()).chain(s.services.iter().rev().filter(owned).filter(|service|!service.resource.phase.reserves_capacity()).take(10)).map(Service::summary).collect::<Vec<_>>()}),
            )
        } else {
            Err(fail("service_id is required"))
        };
    };
    let index = s
        .services
        .iter()
        .position(|service| service.resource.id.as_str() == id && owned(&service))
        .ok_or_else(|| fail("Service is not owned by this chat"))?;
    if stop {
        transition(
            &mut s,
            index,
            Event::Stop(StopReason::Requested),
            now(),
            crate::storage::save,
        )?;
        return Ok(s.services[index].summary());
    }
    let service = s.services[index].binding_snapshot();
    let mut value = s.services[index].summary();
    drop(s);
    if let Some(id) = service.resource.phase.container() {
        // Verify identity before reading another process's output.
        let inspected = Docker.inventory(service.resource.owner.node.as_str())?;
        if let Some(observation) = inspected.get(id.as_str()) {
            verify(&service, observation)?;
            let output = commands::logs(id.as_str())?;
            value["output"] = json!(output);
            value["output_limit_bytes"] = json!(OUTPUT);
        }
    }
    Ok(value)
}
fn start(shared: &Arc<Mutex<State>>, run: &str, generation: &str, nonce: &str, node: NodeId, input: Input) -> io::Result<Value> {
    let s = shared.lock().map_err(|_| fail("State unavailable"))?;
    let (active, _) = caller(&s, run, generation)?;
    let image: ImageId = active
        .attempts
        .last()
        .and_then(|a| a.image.clone())
        .ok_or_else(|| fail("Missing captured image"))?
        .try_into()
        .map_err(fail)?;
    drop(s);
    let marker = crate::engine::query(&[
        "image",
        "inspect",
        "--format",
        "{{index .Config.Labels \"dev.control.service-runner\"}}",
        image.as_str(),
    ])?;
    if !matches!(marker.trim(), "1" | "2") || (input.lease_seconds == -1 && marker.trim() != "2") {
        return Err(fail(
            "Rebuild worker/Claude images with Nix setup before starting persistent services",
        ));
    }
    let profile = crate::worker_profile::Profile::inspect(image.as_str())?;
    let mut s = shared.lock().map_err(|_| fail("State unavailable"))?;
    // Revalidate the captured image after the engine query outside the state lock.
    if caller(&s, run, generation)?.0.attempts.last().and_then(|a| a.image.as_deref()) != Some(image.as_str()) {
        return Err(fail("Captured service image changed"));
    }
    admit_profile(
        &mut s,
        Request {
            run,
            generation,
            nonce,
        },
        input,
        node,
        now(),
        profile,
        crate::storage::save,
    )
}
#[derive(Clone, Copy)]
struct Request<'a> {
    run: &'a str,
    generation: &'a str,
    nonce: &'a str,
}
#[cfg(test)]
fn admit(s: &mut State, request: Request<'_>, input: Input, node: NodeId, time: u64, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<Value> {
    admit_profile(s, request, input, node, time, crate::worker_profile::Profile::Lite, save)
}
fn admit_profile(
    s: &mut State,
    request: Request<'_>,
    input: Input,
    node: NodeId,
    time: u64,
    profile: crate::worker_profile::Profile,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<Value> {
    let Request {
        run,
        generation,
        nonce,
    } = request;
    let (run, chat) = caller(s, run, generation)?;
    let lease = if input.lease_seconds == -1 {
        Lease::until_stopped(time)
    } else {
        u64::try_from(input.lease_seconds).map_err(|_|control_core::resources::Invalid).and_then(|seconds|Lease::new(time,seconds))
    }.map_err(fail)?;
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(fail("Invalid request identity"));
    }
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!(
            "{}\0{chat}\0{}\0{generation}\0{nonce}",
            node.as_str(),
            run.id
        )
        .as_bytes(),
    );
    let id: ResourceId = format!(
        "svc-{}",
        digest.as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
    .try_into()
    .map_err(fail)?;
    if let Some(existing) = s.services.iter().find(|service| service.resource.id == id) {
        if existing.command.0 != input.command.0
            || existing.resource.lease.seconds() != lease.seconds()
            || match existing.storage { Storage::Disk { limit_mb } => limit_mb != input.storage_limit_mb, Storage::Memory => input.storage_limit_mb.is_some() }
        {
            return Err(fail("Request identity already has a different command or storage policy"));
        }
        return Ok(existing.summary());
    }
    if s.services
        .iter()
        .filter(|service| service.resource.phase.reserves_capacity())
        .count()
        >= MAX_ACTIVE
    {
        return Err(fail(
            "Service capacity reached; stop an owned service before starting more",
        ));
    }
    let actor = run.actor.clone();
    let origin = Origin {
        run: run.id.clone().try_into().map_err(fail)?,
        attempt: generation.to_owned().try_into().map_err(fail)?,
    };
    let image = run
        .attempts
        .last()
        .and_then(|a| a.image.clone())
        .ok_or_else(|| fail("Missing captured image"))?
        .try_into()
        .map_err(fail)?;
    let resource = Resource {
        id,
        owner: Owner {
            node,
            chat: chat.to_owned().try_into().map_err(fail)?,
        },
        image,
        generation: Generation::INITIAL,
        scope: Scope::PRIVATE_OFFLINE,
        lease,
        phase: Phase::Requested,
    };
    let uid = NonZeroU32::new(unsafe { libc::getuid() })
        .ok_or_else(|| fail("Services require a non-root operator"))?;
    let service = Service {
        resource,
        origin,
        actor,
        command: input.command,
        uid,
        gid: unsafe { libc::getgid() },
        workspace_exec: true,
        profile,
        storage: Storage::Disk { limit_mb: input.storage_limit_mb },
        diagnostic: String::new(),
        output: String::new(),
        exit_code: None,
        executions: Vec::new(),
        command_receipts: Vec::new(),
    };
    // Retain nonce receipts while their origin attempt can still submit retries.
    // Older stopped records may be evicted; they can no longer launch effects.
    let evict = if s.services.len() >= MAX_RECORDS {
        Some(
            s.services
                .iter()
                .position(|service| {
                    matches!(service.resource.phase, Phase::Stopped(_))
                        && !s.runtime.runs.iter().any(|run| {
                            run.id == service.origin.run.as_str()
                                && run.status == "running"
                                && run.attempts.last().and_then(|a| a.workspace_id.as_deref())
                                    == Some(service.origin.attempt.as_str())
                        })
                })
                .ok_or_else(|| {
                    fail("Service history is full for active attempts; continue in a new turn")
                })?,
        )
    } else {
        None
    };
    let evicted = evict.map(|index| (index, s.services.remove(index)));
    let value = service.summary();
    s.services.push(service);
    if let Err(error) = save(s) {
        s.services.pop();
        if let Some((index, service)) = evicted {
            s.services.insert(index, service);
        }
        return Err(error);
    }
    Ok(value)
}
fn transition(
    s: &mut State,
    index: usize,
    event: Event,
    time: u64,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<Option<Effect>> {
    let observed = matches!(&event, Event::Observed { .. });
    let resource = &s.services[index].resource;
    let decision = decide(resource, resource.stamp(), event, time).map_err(fail)?;
    if resource.phase != decision.phase || (observed && !s.services[index].diagnostic.is_empty()) {
        let previous = std::mem::replace(&mut s.services[index].resource.phase, decision.phase);
        let diagnostic = std::mem::take(&mut s.services[index].diagnostic);
        if let Err(error) = save(s) {
            s.services[index].resource.phase = previous;
            s.services[index].diagnostic = diagnostic;
            return Err(error);
        }
    }
    Ok(decision.effect)
}

#[derive(Clone, Deserialize)]
struct Inspection {
    id: String,
    name: String,
    image: String,
    node: Option<String>,
    resource: Option<String>,
    chat: Option<String>,
    generation: Option<String>,
    status: String,
    #[serde(default)]
    exit_code: Option<i32>,
    network: String,
    readonly: bool,
    privileged: bool,
    user: String,
    pid_mode: String,
    restart: String,
    caps: Vec<String>,
    security: Vec<String>,
    memory: u64,
    cpu: u64,
    pids: u64,
    mounts: Vec<Value>,
    tmpfs: HashMap<String, String>,
    #[serde(default)]
    storage_opts: Option<HashMap<String, String>>,
    entrypoint: Vec<String>,
    command: Vec<String>,
}
const INSPECT: &str = r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},"node":{{json (index .Config.Labels "dev.control.node")}},"resource":{{json (index .Config.Labels "dev.control.resource")}},"chat":{{json (index .Config.Labels "dev.control.chat")}},"generation":{{json (index .Config.Labels "dev.control.generation")}},"status":{{json .State.Status}},"exit_code":{{json .State.ExitCode}},"network":{{json .HostConfig.NetworkMode}},"readonly":{{json .HostConfig.ReadonlyRootfs}},"privileged":{{json .HostConfig.Privileged}},"user":{{json .Config.User}},"pid_mode":{{json .HostConfig.PidMode}},"restart":{{json .HostConfig.RestartPolicy.Name}},"caps":{{json .HostConfig.CapDrop}},"security":{{json .HostConfig.SecurityOpt}},"memory":{{json .HostConfig.Memory}},"cpu":{{json .HostConfig.NanoCpus}},"pids":{{json .HostConfig.PidsLimit}},"mounts":{{json .Mounts}},"tmpfs":{{if index .HostConfig "Tmpfs"}}{{json (index .HostConfig "Tmpfs")}}{{else}}{}{{end}},"storage_opts":{{json (index .HostConfig "StorageOpt")}},"entrypoint":{{json .Config.Entrypoint}},"command":{{json .Config.Cmd}}}"#;
fn tmpfs(service: &Service) -> HashMap<String, String> {
    [("/workspace", service.profile.service_workspace_mb()), ("/tmp", 128), ("/home/agent", 32)]
        .into_iter()
        .filter(|(path, _)| service.storage == Storage::Memory || *path == "/tmp")
        .map(|(path, size)| {
            (
                path.to_owned(),
                format!(
                    "rw,{}nosuid,nodev,uid={},gid={},size={size}m",
                    if path=="/workspace"&&service.workspace_exec {"exec,"}else{""}, service.uid, service.gid
                ),
            )
        })
        .collect()
}
struct Verified<'a> {
    id: ContainerId,
    service: &'a Service,
}
fn verify<'a>(service: &'a Service, inspection: &Inspection) -> io::Result<Verified<'a>> {
    let r = &service.resource;
    let deadline = r.lease.expires().to_string();
    let expected_tmpfs = tmpfs(service);
    let expected_storage = service.storage.options();
    // The initial remaining TTL is recorded in the container. Its monotonic PID-1
    // watchdog remains authoritative across Control restart; never extend it.
    if inspection.name != format!("/{}", service.name())
        || inspection.node.as_deref() != Some(r.owner.node.as_str())
        || inspection.resource.as_deref() != Some(r.id.as_str())
        || inspection.chat.as_deref() != Some(r.owner.chat.as_str())
        || inspection.generation.as_deref() != Some(r.generation.get().to_string().as_str())
        || inspection.image != r.image.as_str()
        || r.phase
            .container()
            .is_some_and(|id| id.as_str() != inspection.id)
        || inspection.network != "none"
        || inspection.readonly != service.readonly_root()
        || inspection.privileged
        || !inspection.pid_mode.is_empty()
        || inspection.restart != "no"
        || inspection.user != format!("{}:{}", service.uid, service.gid)
        || inspection.caps != ["ALL"]
        || !inspection.security.iter().any(|s| s == "no-new-privileges")
        || inspection.memory != service.profile.service_memory()
        || inspection.cpu != 1_000_000_000
        || inspection.pids != service.profile.service_pids()
        || inspection.tmpfs != expected_tmpfs
        || match &inspection.storage_opts {
            Some(options) => *options != expected_storage,
            None => !expected_storage.is_empty(),
        }
        || inspection.mounts.iter().any(|m| {
            m["Type"] != "tmpfs"
                || !expected_tmpfs.contains_key(m["Destination"].as_str().unwrap_or(""))
        })
        || inspection.entrypoint != ["control-service-runner"]
        || inspection.command.len() != 3
        || inspection.command.first().map(String::as_str) != Some(deadline.as_str())
        || inspection.command.get(1) != Some(&r.lease.seconds().to_string())
        || inspection.command.get(2).map(String::as_str) != Some(service.command.0.as_ref())
        || matches!(
            r.phase,
            Phase::Requested | Phase::Stopped(_) | Phase::Fenced { .. }
        )
        || (matches!(
            r.phase,
            Phase::Creating
                | Phase::Stopping {
                    container: None,
                    ..
                }
        ) && inspection.status != "created")
    {
        return Err(fail(
            "Service container does not match its durable ownership and offline scope",
        ));
    }
    Ok(Verified {
        id: ContainerId::try_from(inspection.id.clone()).map_err(fail)?,
        service,
    })
}
trait Engine {
    fn inventory(&mut self, node: &str) -> io::Result<HashMap<String, Inspection>>;
    fn create(&mut self, service: &Service) -> io::Result<ContainerId>;
    fn start(&mut self, binding: &Verified<'_>) -> io::Result<()>;
    fn remove(&mut self, binding: &Verified<'_>) -> io::Result<()>;
    fn logs(&mut self, _binding: &Verified<'_>) -> io::Result<String> { Ok(String::new()) }
}
struct Docker;
/// A live verified offline service capability, never an arbitrary container ID.
pub(crate) struct PortBinding { id: ContainerId, user: String, expires: u64 }
#[derive(Clone, Copy)]
pub(crate) enum RelayMode { Connect, Listen, Multiplex }
impl PortBinding {
    pub(crate) fn spawn(&self, mode: RelayMode, port: std::num::NonZeroU16, expires: u64) -> io::Result<std::process::Child> {
        if expires <= now() || expires > self.expires { return Err(fail("Port lease exceeds its service")); }
        crate::engine::cli().args(["exec", "-i", "--user", &self.user, self.id.as_str(), "/bin/control-tcp-relay", match mode { RelayMode::Connect => "connect", RelayMode::Listen => "listen", RelayMode::Multiplex => "listen-multi" }, &port.to_string(), &expires.to_string()])
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn()
    }
}
pub(crate) fn relay_capable(service: &Service) -> io::Result<()> {
    relay_version(service).map(|_|())
}
pub(crate) fn relay_version(service: &Service) -> io::Result<u8> {
    // Image IDs are immutable. Avoid an extra engine query on every connection.
    static CAPABILITIES:std::sync::OnceLock<Mutex<HashMap<ImageId,u8>>>=std::sync::OnceLock::new();
    let cache=CAPABILITIES.get_or_init(Mutex::default);
    let cached=cache.lock().map_err(|_|fail("Relay capability cache unavailable"))?.get(&service.resource.image).copied();
    if let Some(version)=cached{return Ok(version);}
    let marker = crate::engine::query(&["image", "inspect", service.resource.image.as_str(), "--format", "{{index .Config.Labels \"dev.control.tcp-relay\"}}"])?;
    let version=marker.trim().parse::<u8>().ok().filter(|version|matches!(version,2..=4))
        .ok_or_else(||fail("Rebuild the immutable worker images with Nix setup, then start a new service to enable TCP relays"))?;
    let mut cached=cache.lock().map_err(|_|fail("Relay capability cache unavailable"))?;
    if cached.len()<MAX_RECORDS{cached.insert(service.resource.image.clone(),version);}
    Ok(version)
}
pub(crate) fn port_binding(service: &Service, expected: &ContainerId) -> io::Result<PortBinding> {
    if !matches!(&service.resource.phase, Phase::Running(id) if id == expected) || service.resource.lease.expired(now()) { return Err(fail("Service is not running under this lease and generation")); }
    relay_capable(service)?;
    let data = crate::engine::query(&["inspect", "--format", INSPECT, expected.as_str()])?;
    let observation: Inspection = serde_json::from_str(data.trim()).map_err(fail)?;
    let binding = verify(service, &observation)?;
    if observation.status != "running" { return Err(fail("Service is not running")); }
    Ok(PortBinding { id: binding.id, user: format!("{}:{}",service.uid,service.gid), expires: service.resource.lease.expires() })
}
impl Engine for Docker {
    fn logs(&mut self, binding: &Verified<'_>) -> io::Result<String> {
        commands::logs(binding.id.as_str())
    }
    fn inventory(&mut self, node: &str) -> io::Result<HashMap<String, Inspection>> {
        let text = crate::engine::query(&[
            "ps",
            "-a",
            "--no-trunc",
            "--filter",
            &format!("label=dev.control.node={node}"),
            "--filter",
            "label=dev.control.purpose=service",
            "--format",
            "{{.ID}}",
        ])?;
        if text.trim().is_empty() {
            return Ok(HashMap::new());
        }
        let ids = text
            .split_whitespace()
            .map(|id| ContainerId::try_from(id.to_owned()).map_err(fail))
            .collect::<io::Result<Vec<_>>>()?;
        if ids.len() > MAX_RECORDS {
            return Err(fail("Too many service containers; inspect node resources"));
        }
        let mut args = vec!["inspect", "--format", INSPECT];
        args.extend(ids.iter().map(ContainerId::as_str));
        crate::engine::query(&args)?
            .lines()
            .map(|line| {
                let record: Inspection = serde_json::from_str(line).map_err(fail)?;
                Ok((record.id.clone(), record))
            })
            .collect()
    }
    fn create(&mut self, service: &Service) -> io::Result<ContainerId> {
        let r = &service.resource;
        let mut args = vec![
            "create".to_owned(),
            "--pull=never".into(),
            "--restart=no".into(),
            "--network=none".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            format!("--pids-limit={}", service.profile.service_pids()),
            format!("--memory={}", service.profile.service_memory()),
            "--cpus=1".into(),
            "--log-driver=local".into(),
            "--log-opt=max-size=1m".into(),
            "--log-opt=max-file=2".into(),
            "--name".into(),
            service.name(),
            "--user".into(),
            format!("{}:{}", service.uid, service.gid),
            "--env".into(),
            "HOME=/home/agent".into(),
            "--workdir=/workspace".into(),
            "--entrypoint=control-service-runner".into(),
        ];
        if service.readonly_root() { args.push("--read-only".into()); }
        for (key, value) in service.storage.options() {
            args.extend(["--storage-opt".into(), format!("{key}={value}")]);
        }
        for (key, value) in [
            ("node", r.owner.node.as_str().to_owned()),
            ("purpose", "service".into()),
            ("resource", r.id.as_str().to_owned()),
            ("chat", r.owner.chat.as_str().to_owned()),
            ("generation", r.generation.get().to_string()),
        ] {
            args.extend(["--label".into(), format!("dev.control.{key}={value}")]);
        }
        for (path, options) in tmpfs(service) {
            args.extend(["--tmpfs".into(), format!("{path}:{options}")]);
        }
        args.extend([
            r.image.as_str().to_owned(),
            r.lease.expires().to_string(),
            r.lease.seconds().to_string(),
            service.command.0.to_string(),
        ]);
        crate::engine::create(&args)
            .map_err(|error| if !service.storage.options().is_empty() && ["storage-opt", "quota", "StorageOpt"].iter().any(|word|error.to_string().contains(word)) {
                fail(format!("Could not create capped service: {error}. storage_limit_mb requires Docker storage-driver quota support; omit it for uncapped disk storage"))
            } else {
                error
            })?
            .trim()
            .to_owned()
            .try_into()
            .map_err(fail)
    }
    fn start(&mut self, binding: &Verified<'_>) -> io::Result<()> {
        if !matches!(binding.service.resource.phase, Phase::Starting(_))
            || binding.service.resource.lease.expired(now())
        {
            return Err(fail("Service is no longer eligible to start"));
        }
        crate::engine::query(&["start", binding.id.as_str()]).map(|_| ())
    }
    fn remove(&mut self, binding: &Verified<'_>) -> io::Result<()> {
        if !matches!(binding.service.resource.phase, Phase::Stopping { .. }) {
            return Err(fail("Service stop intent is not recorded"));
        }
        crate::engine::query(&["rm", "-f", binding.id.as_str()]).map(|_| ())
    }
}

fn apply(
    shared: &Arc<Mutex<State>>,
    snapshot: &Service,
    event: Event,
    time: u64,
    save: &impl Fn(&State) -> io::Result<()>,
) -> io::Result<Option<(Service, Effect)>> {
    let mut s = shared.lock().map_err(|_| fail("State unavailable"))?;
    let Some(index) = s.services.iter().position(|s| {
        s.resource.id == snapshot.resource.id
            && s.resource.generation == snapshot.resource.generation
    }) else {
        return Ok(None);
    };
    if s.services[index].resource.phase != snapshot.resource.phase
        && !matches!(event, Event::Created(_))
    {
        return Ok(None);
    }
    let effect = transition(&mut s, index, event, time, save)?;
    Ok(effect.map(|effect| (s.services[index].binding_snapshot(), effect)))
}
fn retain_output(shared:&Arc<Mutex<State>>, snapshot:&Service, output:String, exit_code:Option<i32>, save:&impl Fn(&State)->io::Result<()>) -> io::Result<()> {
    let mut state=shared.lock().map_err(|_|fail("State unavailable"))?;
    let Some(index)=state.services.iter().position(|s|s.resource==snapshot.resource) else { return Err(fail("Service changed before retaining output")); };
    let previous=std::mem::replace(&mut state.services[index].output,output);
    let previous_exit=std::mem::replace(&mut state.services[index].exit_code,exit_code);
    if let Err(error)=save(&state) {state.services[index].output=previous;state.services[index].exit_code=previous_exit;return Err(error);}
    Ok(())
}
fn drive(
    shared: &Arc<Mutex<State>>,
    engine: &mut impl Engine,
    node: &str,
    time: u64,
    save: impl Fn(&State) -> io::Result<()>,
) -> io::Result<()> {
    let services = {
        let s = shared.lock().map_err(|_| fail("State unavailable"))?;
        s.services
            .iter()
            .filter(|s| {
                s.resource.phase.reserves_capacity()
                    && !matches!(s.resource.phase, Phase::Fenced { .. })
            })
            .map(Service::binding_snapshot)
            .collect::<Vec<_>>()
    };
    if services.is_empty() {
        return Ok(());
    }
    let observations = engine.inventory(node)?;
    let mut authorize = identity::authorizer();
    for service in services {
        let r = &service.resource;
        if r.owner.node.as_str() != node {
            apply(
                shared,
                &service,
                Event::Fence(Fault::IdentityMismatch),
                time,
                &save,
            )?;
            continue;
        }
        let event = if !matches!(r.phase, Phase::Stopping { .. })
            && authorize(&service.actor, Action::Task).is_err()
        {
            Event::Stop(StopReason::Revoked)
        } else {
            Event::Tick
        };
        let Some((service, effect)) = apply(shared, &service, event, time, &save)? else {
            continue;
        };
        let result = (|| -> io::Result<()> {
            match effect {
                Effect::Create => {
                    if eligible(shared, &service, time)? {
                        let id = engine.create(&service)?;
                        apply(shared, &service, Event::Created(id), time, &save)?;
                    }
                }
                Effect::Inspect => {
                    let observation = service
                        .resource
                        .phase
                        .container()
                        .and_then(|id| observations.get(id.as_str()))
                        .or_else(|| {
                            observations
                                .values()
                                .find(|o| o.name == format!("/{}", service.name()))
                        });
                    let event = if let Some(o) = observation {
                        if verify(&service, o).is_err() {
                            Event::Fence(Fault::IdentityMismatch)
                        } else {
                            let state = match o.status.as_str() {
                                "created" => EngineState::Dormant,
                                "running" => EngineState::Running,
                                "exited" | "dead" => EngineState::Exited,
                                _ => {
                                    apply(
                                        shared,
                                        &service,
                                        Event::Fence(Fault::UnexpectedEngineState),
                                        time,
                                        &save,
                                    )?;
                                    return Ok(());
                                }
                            };
                            Event::Observed {
                                container: o.id.clone().try_into().map_err(fail)?,
                                state,
                            }
                        }
                    } else {
                        Event::Missing
                    };
                    if let Some((next, effect)) = apply(shared, &service, event, time, &save)? {
                        match effect {
                            Effect::Create => {
                                if eligible(shared, &next, time)? {
                                    let id = engine.create(&next)?;
                                    apply(shared, &next, Event::Created(id), time, &save)?;
                                }
                            }
                            Effect::Start => {
                                if eligible(shared, &next, time)? {
                                    engine.start(&verify(
                                        &next,
                                        observation
                                            .ok_or_else(|| fail("Missing service observation"))?,
                                    )?)?;
                                }
                            }
                            Effect::Remove => {
                                let binding=verify(&next, observation.ok_or_else(||fail("Missing service observation"))?)?;
                                let output=engine.logs(&binding)?;
                                retain_output(shared,&next,output,observation.and_then(|o|o.exit_code).filter(|_|observation.is_some_and(|o|o.status=="exited"||o.status=="dead")),&save)?;
                                engine.remove(&binding)?;
                            }
                            Effect::Inspect => {}
                        }
                    }
                }
                _ => return Err(fail("Unexpected service effect")),
            }
            Ok(())
        })();
        if let Err(error) = result {
            diagnose(shared, Some(&service.resource.id), &error, &save)?;
        }
    }
    Ok(())
}
fn eligible(shared: &Arc<Mutex<State>>, expected: &Service, time: u64) -> io::Result<bool> {
    identity::authorize(&expected.actor, Action::Task).map_err(fail)?;
    let s = shared.lock().map_err(|_| fail("State unavailable"))?;
    Ok(!expected.resource.lease.expired(time)
        && s.services.iter().any(|s| {
            s.resource.id == expected.resource.id
                && s.resource.generation == expected.resource.generation
                && s.resource.phase == expected.resource.phase
        }))
}
fn diagnose(
    shared: &Arc<Mutex<State>>,
    id: Option<&ResourceId>,
    error: &io::Error,
    save: &impl Fn(&State) -> io::Result<()>,
) -> io::Result<()> {
    let mut s = shared.lock().map_err(|_| fail("State unavailable"))?;
    let diagnostic = error.to_string().chars().take(256).collect::<String>();
    let mut changed = false;
    for service in &mut s.services {
        if service.resource.phase.reserves_capacity()
            && id.is_none_or(|id| id == &service.resource.id)
            && service.diagnostic != diagnostic
        {
            service.diagnostic.clone_from(&diagnostic);
            changed = true;
        }
    }
    if changed {
        save(&s)?;
    }
    Ok(())
}
pub fn supervise(shared: Arc<Mutex<State>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(node) = crate::config::node_id() else {
            return;
        };
        while let Err(error)=commands::recover(&shared,crate::storage::save) {
            let _=diagnose(&shared,None,&error,&crate::storage::save);
            if crate::shutdown::pause(Duration::from_secs(2)) {return;}
        }
        let mut executions = Vec::new();
        while !crate::shutdown::requested() {
            if let Err(error) = drive(&shared, &mut Docker, &node, now(), crate::storage::save) {
                let _ = diagnose(&shared, None, &error, &crate::storage::save);
            }
            if let Err(error)=commands::dispatch(&shared, &mut executions) {
                let _=diagnose(&shared,None,&error,&crate::storage::save);
            }
            crate::shutdown::pause(Duration::from_secs(2));
        }
        for execution in executions { let _=execution.join(); }
    })
}
/// Other resource reconciliation may skip only fully verified registered service
/// identities, never every container bearing a service label.
pub fn verified_names(services: &[Service], node: &str) -> io::Result<Vec<String>> {
    let observations = Docker.inventory(node)?;
    verified_discovery_names(services,&observations)
}
fn verified_discovery_names(services:&[Service],observations:&HashMap<String,Inspection>)->io::Result<Vec<String>>{
    let mut names = Vec::new();
    for o in observations.values() {
        let service = services
            .iter()
            .find(|s| o.name == format!("/{}", s.name()))
            .ok_or_else(|| fail("Unregistered service container"))?;
        if matches!(service.resource.phase,Phase::Requested){
            // Dispatch can snapshot Requested just before the supervisor saves
            // Creating and creates a dormant container. Classify only; do not
            // relax the capability verifier or adopt an unknown execution.
            let mut creating=service.clone();creating.resource.phase=Phase::Creating;verify(&creating,o)?;
        }else{verify(service, o)?;}
        names.push(service.name());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    pub(super) fn fixture() -> (State, String, String, u64) {
        let mut s = State::default();
        let chat = crate::chat::create(&mut s);
        crate::chat::send(&mut s, &chat, "Run a persistent counter", "test").unwrap();
        let run = &mut s.runtime.runs[0];
        run.status = "running".into();
        run.attempts.push(crate::usage::Attempt {
            workspace_id: Some("attempt-a".into()),
            image: Some(format!("sha256:{}", "a".repeat(64))),
            ..Default::default()
        });
        let run = run.id.clone();
        let time = now();
        (s, run, chat, time)
    }
    fn input(command: &str) -> Input {
        Input {
            command: command.to_owned().try_into().unwrap(),
            lease_seconds: 60,
            storage_limit_mb: None,
        }
    }
    pub(super) fn admit_fixture(
        s: &mut State,
        run: &str,
        time: u64,
        save: impl FnOnce(&State) -> io::Result<()>,
    ) -> io::Result<Value> {
        admit(
            s,
            Request {
                run,
                generation: "attempt-a",
                nonce: "nonce-a",
            },
            input("printf counter; sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            save,
        )
    }
    fn inspection(s: &Service, id: &str) -> Inspection {
        Inspection {
            id: id.into(),
            name: format!("/{}", s.name()),
            image: s.resource.image.as_str().into(),
            node: Some(s.resource.owner.node.as_str().into()),
            resource: Some(s.resource.id.as_str().into()),
            chat: Some(s.resource.owner.chat.as_str().into()),
            generation: Some(s.resource.generation.get().to_string()),
            status: "created".into(),
            exit_code: None,
            network: "none".into(),
            readonly: s.readonly_root(),
            privileged: false,
            user: format!("{}:{}", s.uid, s.gid),
            pid_mode: String::new(),
            restart: "no".into(),
            caps: vec!["ALL".into()],
            security: vec!["no-new-privileges".into()],
            memory: 512 * 1024 * 1024,
            cpu: 1_000_000_000,
            pids: 64,
            mounts: Vec::new(),
            tmpfs: tmpfs(s),
            storage_opts: Some(s.storage.options()),
            entrypoint: vec!["control-service-runner".into()],
            command: vec![
                s.resource.lease.expires().to_string(),
                s.resource.lease.seconds().to_string(),
                s.command.0.to_string(),
            ],
        }
    }
    #[derive(Default)]
    struct Fake {
        records: HashMap<String, Inspection>,
        creates: usize,
        starts: usize,
        removes: usize,
        inspections: usize,
    }
    impl Engine for Fake {
        fn logs(&mut self, _: &Verified<'_>) -> io::Result<String> { Ok("retained final diagnostic".into()) }
        fn inventory(&mut self, _: &str) -> io::Result<HashMap<String, Inspection>> {
            self.inspections += 1;
            Ok(self.records.clone())
        }
        fn create(&mut self, s: &Service) -> io::Result<ContainerId> {
            self.creates += 1;
            let id = "b".repeat(64);
            self.records.insert(id.clone(), inspection(s, &id));
            id.try_into().map_err(fail)
        }
        fn start(&mut self, binding: &Verified<'_>) -> io::Result<()> {
            self.starts += 1;
            self.records.get_mut(binding.id.as_str()).unwrap().status = "running".into();
            Ok(())
        }
        fn remove(&mut self, binding: &Verified<'_>) -> io::Result<()> {
            self.removes += 1;
            self.records.remove(binding.id.as_str());
            Ok(())
        }
    }
    #[test]
    fn exited_service_keeps_output_before_removal_and_failed_save_prevents_removal() {
        let (state,run,_,time)=fixture();let shared=Arc::new(Mutex::new(state));
        admit_fixture(&mut shared.lock().unwrap(),&run,time,|_|Ok(())).unwrap();
        let mut engine=Fake::default();
        for offset in 0..4 {drive(&shared,&mut engine,"node-a",time+offset,|_|Ok(())).unwrap();}
        let id=shared.lock().unwrap().services[0].resource.phase.container().unwrap().as_str().to_owned();
        engine.records.get_mut(&id).unwrap().status="exited".into();
        engine.records.get_mut(&id).unwrap().exit_code=Some(7);
        drive(&shared,&mut engine,"node-a",time+4,|state|if state.services[0].output.is_empty(){Ok(())}else{Err(fail("disk full"))}).unwrap();
        assert_eq!(engine.removes,0);assert!(shared.lock().unwrap().services[0].output.is_empty());
        drive(&shared,&mut engine,"node-a",time+5,|_|Ok(())).unwrap();
        assert_eq!(engine.removes,1);
        drive(&shared,&mut engine,"node-a",time+6,|_|Ok(())).unwrap();
        let restored:State=serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
        assert!(matches!(restored.services[0].resource.phase,Phase::Stopped(_)));
        assert_eq!(restored.services[0].summary()["output"],"retained final diagnostic");
        assert_eq!(restored.services[0].summary()["exit_code"],7);
    }
    #[test]
    fn admission_is_bounded_deduplicated_and_rolls_back_before_any_effect() {
        let (mut s, run, _, time) = fixture();
        assert!(admit_fixture(&mut s, &run, time, |_| Err(fail("disk full"))).is_err());
        assert!(s.services.is_empty());
        let first = admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        assert_eq!(
            admit_fixture(&mut s, &run, time + 1, |_| panic!(
                "Duplicate must not save"
            ))
            .unwrap(),
            first
        );
        assert_eq!(s.services.len(), 1);
        assert!(
            admit(
                &mut s,
                Request {
                    run: &run,
                    generation: "attempt-a",
                    nonce: "nonce-a"
                },
                input("different"),
                "node-a".to_owned().try_into().unwrap(),
                time,
                |_| Ok(())
            )
            .is_err()
        );
        assert!(
            admit(
                &mut s,
                Request {
                    run: &run,
                    generation: "stale",
                    nonce: "other"
                },
                input("different"),
                "node-a".to_owned().try_into().unwrap(),
                time,
                |_| Ok(())
            )
            .is_err()
        );
        for n in 1..MAX_ACTIVE {
            admit(
                &mut s,
                Request {
                    run: &run,
                    generation: "attempt-a",
                    nonce: &format!("nonce-{n}"),
                },
                input("sleep 30"),
                "node-a".to_owned().try_into().unwrap(),
                time,
                |_| Ok(()),
            )
            .unwrap();
        }
        assert!(
            admit(
                &mut s,
                Request {
                    run: &run,
                    generation: "attempt-a",
                    nonce: "overflow"
                },
                input("sleep 30"),
                "node-a".to_owned().try_into().unwrap(),
                time,
                |_| Ok(())
            )
            .is_err()
        );
        let mut poisoned = serde_json::to_value(&s).unwrap();
        poisoned["services"][0]["resource"]["owner"]["chat"] = json!("../../host");
        assert!(serde_json::from_value::<State>(poisoned).is_err());
        let mut poisoned = serde_json::to_value(&s).unwrap();
        poisoned["services"][0]["uid"] = json!(0);
        assert!(serde_json::from_value::<State>(poisoned).is_err());
    }
    #[test]
    fn persistence_failures_cannot_launch_and_created_id_is_recovered_before_start() {
        let (mut s, run, _, time) = fixture();
        admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        let shared = Arc::new(Mutex::new(s));
        let mut engine = Fake::default();
        assert!(
            drive(&shared, &mut engine, "node-a", time, |_| Err(fail(
                "disk full"
            )))
            .is_err()
        );
        assert_eq!(engine.creates, 0);
        let writes = Cell::new(0);
        drive(&shared, &mut engine, "node-a", time, |_| {
            writes.set(writes.get() + 1);
            if writes.get() == 2 {
                Err(fail("lost created-ID save"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!((engine.creates, engine.starts), (1, 0));
        assert_eq!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Creating
        );
        let restored: State =
            serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
        let shared = Arc::new(Mutex::new(restored));
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        assert_eq!((engine.creates, engine.starts), (1, 0));
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        assert_eq!((engine.creates, engine.starts), (1, 1));
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        assert!(matches!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Running(_)
        ));
    }
    #[test]
    fn completed_history_is_bounded_without_replaying_live_attempt_receipts() {
        let (mut s, run, chat, time) = fixture();
        admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        let service = s.services[0].clone();
        assert!(Arc::ptr_eq(&service.command.0, &service.clone().command.0));
        assert!(service.owned_by(&service.resource.owner.node, &chat, &Actor::LocalOperator));
        assert!(!service.owned_by(
            &service.resource.owner.node,
            "another-chat",
            &Actor::LocalOperator
        ));
        assert!(!service.owned_by(&service.resource.owner.node, &chat, &Actor::Unknown));
        assert!(!service.owned_by(
            &"another-node".to_owned().try_into().unwrap(),
            &chat,
            &Actor::LocalOperator
        ));
        s.services = (0..MAX_RECORDS)
            .map(|n| {
                let mut service = service.clone();
                service.resource.id = format!("old-{n}").try_into().unwrap();
                service.resource.phase = Phase::Stopped(StopReason::Exited);
                service
            })
            .collect();
        // The still-active origin retains every receipt, including stopped ones.
        assert!(admit_fixture(&mut s, &run, time, |_| Ok(())).is_err());
        let before = serde_json::to_vec(&s).unwrap();
        s.runtime.runs[0].attempts.last_mut().unwrap().workspace_id = Some("attempt-b".into());
        let request = Request {
            run: &run,
            generation: "attempt-b",
            nonce: "nonce-a",
        };
        assert!(
            admit(
                &mut s,
                request,
                input("sleep 30"),
                "node-a".to_owned().try_into().unwrap(),
                time,
                |_| Err(fail("disk full"))
            )
            .is_err()
        );
        assert_eq!(s.services.len(), MAX_RECORDS);
        assert_eq!(s.services[0].resource.id.as_str(), "old-0");
        let first = admit(
            &mut s,
            request,
            input("sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(s.services.len(), MAX_RECORDS);
        assert_eq!(s.services[0].resource.id.as_str(), "old-1");
        assert_ne!(first["service_id"], json!(service.resource.id));
        assert_eq!(
            first,
            admit(
                &mut s,
                request,
                input("sleep 30"),
                "node-a".to_owned().try_into().unwrap(),
                time,
                |_| panic!("Duplicate must not save")
            )
            .unwrap()
        );
        assert!(
            admit_fixture(&mut s, &run, time, |_| Ok(())).is_err(),
            "Old attempts remain fenced even after receipt eviction"
        );
        let restored: State = serde_json::from_slice(&before).unwrap();
        assert_eq!(restored.services.len(), MAX_RECORDS);
    }
    #[test]
    fn revocation_quiesces_a_running_service_and_failed_stop_save_cannot_remove_it() {
        let (mut s, run, _, time) = fixture();
        admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        let shared = Arc::new(Mutex::new(s));
        let mut engine = Fake::default();
        for _ in 0..3 {
            drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        }
        shared.lock().unwrap().services[0].actor = Actor::Unknown;
        assert!(
            drive(&shared, &mut engine, "node-a", time, |_| Err(fail(
                "disk full"
            )))
            .is_err()
        );
        assert_eq!(engine.removes, 0);
        assert!(matches!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Running(_)
        ));
        for _ in 0..3 {
            drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        }
        assert_eq!((engine.starts, engine.removes), (1, 1));
        assert_eq!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Stopped(StopReason::Revoked)
        );
    }
    #[test]
    fn services_survive_turns_and_restoration_without_a_second_start() {
        let (mut s, run, _, time) = fixture();
        admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        let shared = Arc::new(Mutex::new(s));
        let mut engine = Fake::default();
        for _ in 0..3 {
            drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        }
        {
            let mut s = shared.lock().unwrap();
            s.runtime.runs[0].status = "completed".into();
            s.runtime.runs[0].attempts.clear();
        }
        let restored: State =
            serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
        let shared = Arc::new(Mutex::new(restored));
        for _ in 0..3 {
            drive(&shared, &mut engine, "node-a", time + 1, |_| {
                panic!("Unchanged Running services must not save")
            })
            .unwrap();
        }
        assert_eq!((engine.creates, engine.starts, engine.removes), (1, 1, 0));
        assert!(matches!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Running(_)
        ));
        drive(&shared, &mut engine, "node-a", time + 60, |_| Ok(())).unwrap();
        assert_eq!(engine.removes, 0, "Expiry persists stop intent first");
        drive(&shared, &mut engine, "node-a", time + 60, |_| Ok(())).unwrap();
        assert_eq!(engine.removes, 1);
        drive(&shared, &mut engine, "node-a", time + 60, |_| Ok(())).unwrap();
        assert_eq!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Stopped(StopReason::Expired)
        );
    }
    #[test]
    fn scope_or_identity_mismatch_never_authorizes_start_or_removal() {
        let (mut s, run, _, time) = fixture();
        admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        let mut service = s.services[0].clone();
        service.resource.phase = Phase::Creating;
        assert!(verify(&service, &inspection(&service, &"b".repeat(64))).is_ok());
        for field in [
            "network",
            "mount",
            "image",
            "node",
            "entrypoint",
            "deadline",
            "pid",
        ] {
            let mut o = inspection(&service, &"b".repeat(64));
            match field {
                "network" => o.network = "bridge".into(),
                "mount" => o
                    .mounts
                    .push(json!({"Type":"bind","Source":"/host","Destination":"/workspace"})),
                "image" => o.image = "latest".into(),
                "node" => o.node = Some("other".into()),
                "entrypoint" => o.entrypoint = vec!["bash".into()],
                "deadline" => o.command[0] = "99999999999".into(),
                "pid" => o.pid_mode = "host".into(),
                _ => unreachable!(),
            }
            assert!(verify(&service, &o).is_err(), "{field} must be enforced");
        }
        let shared = Arc::new(Mutex::new(s));
        let mut engine = Fake::default();
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        engine.records.values_mut().next().unwrap().network = "bridge".into();
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        assert_eq!((engine.starts, engine.removes), (0, 0));
        let s = shared.lock().unwrap();
        assert!(matches!(s.services[0].resource.phase, Phase::Fenced { .. }));
        assert!(
            s.services[0].resource.phase.container().is_some(),
            "Fencing must retain the known container identity"
        );
    }
    #[test]
    fn idle_supervision_does_not_query_the_engine() {
        let shared = Arc::new(Mutex::new(State::default()));
        let mut engine = Fake::default();
        drive(&shared, &mut engine, "node-a", now(), |_| {
            panic!("Idle must not save")
        })
        .unwrap();
        assert_eq!(engine.inspections, 0);
    }
    #[test]
    fn requested_dispatch_snapshot_classifies_only_matching_dormant_creation(){
        let(mut state,run,_,time)=fixture();admit_fixture(&mut state,&run,time,|_|Ok(())).unwrap();let requested=state.services[0].clone();assert!(matches!(requested.resource.phase,Phase::Requested));
        let mut creating=requested.clone();creating.resource.phase=Phase::Creating;let observation=inspection(&creating,&"b".repeat(64));
        assert!(verify(&requested,&observation).is_err(),"Execution capabilities must remain strict");
        let observations=HashMap::from([(observation.id.clone(),observation.clone())]);assert_eq!(verified_discovery_names(std::slice::from_ref(&requested),&observations).unwrap(),vec![requested.name()]);assert!(matches!(requested.resource.phase,Phase::Requested));
        for field in ["running","exited","image","node","mount","name"]{let mut bad=observation.clone();match field{"running"=>bad.status="running".into(),"exited"=>bad.status="exited".into(),"image"=>bad.image=format!("sha256:{}","c".repeat(64)),"node"=>bad.node=Some("foreign".into()),"mount"=>bad.mounts.push(json!({"Type":"bind","Destination":"/workspace"})),_=>bad.name="/unregistered".into()};assert!(verified_discovery_names(std::slice::from_ref(&requested),&HashMap::from([(bad.id.clone(),bad)])).is_err(),"{field}");}
    }
    #[test]
    fn executable_workspace_policy_is_captured_and_legacy_services_remain_unchanged() {
        let(mut state,run,_,time)=fixture();admit_fixture(&mut state,&run,time,|_|Ok(())).unwrap();
        let mut current=state.services[0].clone();assert!(current.workspace_exec);current.resource.phase=Phase::Creating;current.storage=Storage::Memory;
        let mut old=serde_json::to_value(&current).unwrap();assert_eq!(old["workspace_exec"],true);old.as_object_mut().unwrap().remove("workspace_exec");let legacy:Service=serde_json::from_value(old).unwrap();assert!(!legacy.workspace_exec);assert!(serde_json::to_value(&legacy).unwrap().get("workspace_exec").is_none());
        assert!(tmpfs(&current)["/workspace"].contains("rw,exec,nosuid,nodev"));assert!(!tmpfs(&legacy)["/workspace"].contains("exec"));assert_eq!(tmpfs(&current)["/tmp"],tmpfs(&legacy)["/tmp"]);
        let old_observation=inspection(&legacy,&"b".repeat(64));let new_observation=inspection(&current,&"b".repeat(64));assert!(verify(&legacy,&old_observation).is_ok());assert!(verify(&current,&new_observation).is_ok());assert!(verify(&legacy,&new_observation).is_err());assert!(verify(&current,&old_observation).is_err());
    }
    #[test]
    fn disk_storage_defaults_and_captured_quota_are_verified() {
        let (mut state, run, _, time) = fixture();
        admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
        let mut service = state.services[0].clone();
        service.resource.phase = Phase::Creating;
        assert_eq!(service.storage, Storage::Disk { limit_mb: None });
        assert_eq!(tmpfs(&service).len(), 1);
        assert!(tmpfs(&service).contains_key("/tmp"));
        assert!(!service.readonly_root());
        let observed = inspection(&service, &"b".repeat(64));
        assert!(verify(&service, &observed).is_ok());
        service.storage = Storage::Disk { limit_mb: NonZeroU32::new(4096) };
        assert!(verify(&service, &observed).is_err(), "Missing quota must fail closed");
        let mut capped = inspection(&service, &"b".repeat(64));
        assert_eq!(capped.storage_opts.as_ref().unwrap()["size"], "4096M");
        assert!(verify(&service, &capped).is_ok());
        capped.storage_opts.as_mut().unwrap().insert("size".into(), "8192M".into());
        assert!(verify(&service, &capped).is_err());
        let restored: Service = serde_json::from_value(serde_json::to_value(&service).unwrap()).unwrap();
        assert_eq!(restored.storage, service.storage);
        assert_eq!(service.binding_snapshot().storage, service.storage);
        let mut old = serde_json::to_value(&service).unwrap();
        old.as_object_mut().unwrap().remove("storage");
        let legacy: Service = serde_json::from_value(old).unwrap();
        assert_eq!(legacy.storage, Storage::Memory);
        assert!(legacy.readonly_root());
        assert!(verify(&legacy, &inspection(&legacy, &"b".repeat(64))).is_ok());
        assert!(verify(&legacy, &observed).is_err());
        for limit in [json!(0), json!(-1), json!(4294967296u64), json!("1g")] {
            assert!(serde_json::from_value::<Input>(json!({"command":"sleep 30","storage_limit_mb":limit})).is_err());
        }
        let request = Request { run: &run, generation: "attempt-a", nonce: "nonce-a" };
        let mut retry = input("printf counter; sleep 30");
        retry.storage_limit_mb = NonZeroU32::new(4096);
        assert!(admit(&mut state, request, retry, "node-a".to_owned().try_into().unwrap(), time, |_| panic!("Quota changes must not save")).is_err());
    }
    #[test]
    fn full_profile_captures_private_store_policy_and_legacy_stays_readonly() {
        let (mut state, run, _, time) = fixture();
        admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
        let mut service = state.services[0].clone();
        service.storage = Storage::Memory;
        assert!(serde_json::to_value(&service).unwrap().get("profile").is_none());
        service.resource.phase = Phase::Creating;
        let mut observed = inspection(&service, &"b".repeat(64));
        service.profile = crate::worker_profile::Profile::Full;
        assert!(verify(&service, &observed).is_err());
        observed.memory = service.profile.service_memory();
        observed.pids = service.profile.service_pids();
        observed.tmpfs = tmpfs(&service);
        observed.readonly = false;
        assert!(verify(&service, &observed).is_ok());
        let restored: Service = serde_json::from_value(serde_json::to_value(&service).unwrap()).unwrap();
        assert_eq!(restored.profile, crate::worker_profile::Profile::Full);
        assert!(verify(&restored, &observed).is_ok());
        service.profile = crate::worker_profile::Profile::Lite;
        assert!(verify(&service, &observed).is_err());
    }
    #[test]
    #[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated storage probe, no models or credentials"]
    fn live_disk_storage_exceeds_old_workspace_limit_and_survives_container_restart() {
        let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
        let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
        let (mut state, run, _, time) = fixture();
        let node = format!("storage-qa-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
        admit_profile(&mut state, Request { run: &run, generation: "attempt-a", nonce: "disk-probe" }, input("sleep 120"), node.clone().try_into().unwrap(), time, profile, |_| Ok(())).unwrap();
        let mut service = state.services.remove(0);
        service.resource.image = image.try_into().unwrap();
        service.resource.phase = Phase::Creating;
        let id = Docker.create(&service).unwrap();
        struct Container(ContainerId);
        impl Drop for Container {
            fn drop(&mut self) { let _ = crate::engine::query(&["rm", "-f", self.0.as_str()]); }
        }
        let cleanup = Container(id.clone());
        let observations = Docker.inventory(&node).unwrap();
        verify(&service, &observations[id.as_str()]).unwrap();
        service.resource.phase = Phase::Starting(id.clone());
        let binding = verify(&service, &observations[id.as_str()]).unwrap();
        Docker.start(&binding).unwrap();
        let output = crate::engine::query(&["exec", id.as_str(), "bash", "-c", "set -eu; test ! -w /bin; test ! -w /bin/control-tcp-relay; dd if=/dev/zero of=/workspace/large.bin bs=1048576 count=540; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; printf persisted > /tmp/marker; printf persisted > /home/agent/marker; printf disk-probe-ok"]).unwrap();
        assert!(output.contains("disk-probe-ok"));
        crate::engine::query(&["stop", "--time=1", id.as_str()]).unwrap();
        crate::engine::query(&["start", id.as_str()]).unwrap();
        let output = crate::engine::query(&["exec", id.as_str(), "bash", "-c", "set -eu; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; test ! -e /tmp/marker; test \"$(cat /home/agent/marker)\" = persisted; printf restart-ok"]).unwrap();
        assert_eq!(output.trim(), "restart-ok");
        let observations = Docker.inventory(&node).unwrap();
        service.resource.phase = Phase::Running(id.clone());
        verify(&service, &observations[id.as_str()]).unwrap();
        assert!(observations[id.as_str()].mounts.iter().all(|m|m["Type"] == "tmpfs" && m["Destination"] == "/tmp"));
        drop(cleanup);
        assert!(Docker.inventory(&node).unwrap().is_empty());
        service.resource.phase = Phase::Creating;
        service.storage = Storage::Disk { limit_mb: NonZeroU32::new(1024) };
        match Docker.create(&service) {
            Ok(id) => {
                let cleanup = Container(id.clone());
                let observations = Docker.inventory(&node).unwrap();
                verify(&service, &observations[id.as_str()]).unwrap();
                println!("Engine accepted and retained the explicit 1024 MiB quota");
                drop(cleanup);
            }
            Err(error) => {
                assert!(error.to_string().contains("storage_limit_mb requires Docker storage-driver quota support"), "Unexpected engine failure: {error}");
                assert!(Docker.inventory(&node).unwrap().is_empty(), "Rejected caps must not create an uncapped container");
                println!("Engine rejected explicit cap; no uncapped fallback: {error}");
            }
        }
    }
    #[test]
    #[ignore = "requires CONTROL_QA_SERVICE_IMAGE (new pinned worker image), CONTROL_QA_CONTROL_BIN and a local Linux engine; no credentials/models"]
    fn live_service_survives_server_restart_and_expires_without_control() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
            time::Instant,
        };
        let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
        let binary =
            std::fs::canonicalize(std::env::var("CONTROL_QA_CONTROL_BIN").unwrap()).unwrap();
        let root = std::env::temp_dir().join(format!(
            "control-services-live-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let state_dir = root.join("state");
        let config = root.join("config");
        fs::create_dir(&state_dir).unwrap();
        fs::create_dir(&config).unwrap();
        let node = ring::digest::digest(&ring::digest::SHA256, root.to_string_lossy().as_bytes())
            .as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        fs::write(state_dir.join("node-id"), &node).unwrap();
        fs::set_permissions(state_dir.join("node-id"), fs::Permissions::from_mode(0o600)).unwrap();
        let (mut s, run, _, time) = fixture();
        let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
        let value=admit_profile(&mut s,Request{run:&run,generation:"attempt-a",nonce:"live"},Input{command:"printf durable > /workspace/counter; if (exec 3<> /proc/1/mem) 2>/dev/null; then exit 41; fi; kill -STOP 1 2>/dev/null || true; echo ready; sleep 60".to_owned().try_into().unwrap(),lease_seconds:20,storage_limit_mb:None},node.clone().try_into().unwrap(),time,profile,|_|Ok(())).unwrap();
        assert!(value["service_id"].is_string());
        s.services[0].resource.image = image.try_into().unwrap();
        s.runtime.runs[0].status = "completed".into();
        fs::write(
            state_dir.join("state.json"),
            serde_json::to_vec(&s).unwrap(),
        )
        .unwrap();
        struct Server(std::process::Child);
        impl Server {
            fn stop(&mut self) {
                unsafe {
                    libc::kill(self.0.id() as i32, libc::SIGTERM);
                }
                let start = Instant::now();
                while self.0.try_wait().unwrap().is_none() {
                    if start.elapsed() > Duration::from_secs(10) {
                        let _ = self.0.kill();
                        let _ = self.0.wait();
                        panic!("Server did not stop");
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let launch = || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            drop(listener);
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("server.log"))
                .unwrap();
            Server(
                Command::new(&binary)
                    .arg("serve")
                    .env("CONTROL_ADDR", addr)
                    .env("CONTROL_CONFIG_DIR", &config)
                    .env("CONTROL_STATE_DIR", &state_dir)
                    .env_remove("CONTROL_WORKER_IMAGE")
                    .env_remove("CONTROL_CLAUDE_IMAGE")
                    .stdin(Stdio::null())
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            )
        };
        let read = || {
            serde_json::from_slice::<State>(&fs::read(state_dir.join("state.json")).unwrap())
                .unwrap()
        };
        let wait_running = |server: &mut Server| {
            let started = Instant::now();
            loop {
                let state = read();
                if matches!(state.services[0].resource.phase, Phase::Running(_)) {
                    return state.services[0].clone();
                }
                assert!(
                    server.0.try_wait().unwrap().is_none(),
                    "Server exited; inspect {}",
                    root.display()
                );
                assert!(
                    started.elapsed() < Duration::from_secs(12),
                    "Service did not run: {}",
                    state.services[0].summary()
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        struct Cleanup {
            service: Service,
            root: std::path::PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(bytes) = fs::read(self.root.join("state/state.json"))
                    && let Ok(state) = serde_json::from_slice::<State>(&bytes)
                    && let Some(service) = state.services.into_iter().next()
                {
                    self.service = service;
                }
                if let Ok(all) = Docker.inventory(self.service.resource.owner.node.as_str())
                    && let Some(o) = all
                        .values()
                        .find(|o| o.name == format!("/{}", self.service.name()))
                    && (self.service.resource.phase.container().is_some()
                        || (matches!(self.service.resource.phase, Phase::Creating)
                            && o.status == "created"))
                {
                    self.service.resource.phase = Phase::Stopping {
                        container: self.service.resource.phase.container().cloned(),
                        reason: StopReason::Requested,
                    };
                    if let Ok(binding) = verify(&self.service, o) {
                        let _ = Docker.remove(&binding);
                    }
                }
                if !std::thread::panicking() {
                    let _ = fs::remove_dir_all(&self.root);
                }
            }
        }
        let mut cleanup = Cleanup {
            service: s.services[0].clone(),
            root: root.clone(),
        };
        let mut first = launch();
        let service = wait_running(&mut first);
        let id = service.resource.phase.container().unwrap().clone();
        cleanup.service = service;
        first.stop();
        let all = Docker.inventory(&node).unwrap();
        verify(&cleanup.service, all.get(id.as_str()).unwrap()).unwrap();
        assert_eq!(all[id.as_str()].status, "running");
        // Simulate a crash after Docker start but before its Running receipt.
        let mut receipt_lost = read();
        receipt_lost.services[0].resource.phase = Phase::Starting(id.clone());
        fs::write(
            state_dir.join("state.json"),
            serde_json::to_vec(&receipt_lost).unwrap(),
        )
        .unwrap();
        let mut second = launch();
        let restored = wait_running(&mut second);
        assert_eq!(restored.resource.phase.container(), Some(&id));
        let contents =
            crate::engine::query(&["exec", id.as_str(), "cat", "/workspace/counter"])
                .unwrap();
        assert_eq!(contents, "durable");
        second.stop();
        let wait = Instant::now();
        loop {
            let all = Docker.inventory(&node).unwrap();
            let observation = all.get(id.as_str()).unwrap();
            if observation.status == "exited" {
                break;
            }
            assert!(
                wait.elapsed() < Duration::from_secs(24),
                "PID-1 deadline must survive Control shutdown and child signals"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let log = crate::engine::query(&["logs", id.as_str()]).unwrap();
        assert!(
            log.contains("ready"),
            "Supervisor memory isolation failed or command did not run"
        );
    }
}
