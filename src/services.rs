//! Chat-owned, bounded offline services. Attempts only attach; a pure core
//! reducer decides effects, and one supervisor interprets durable intents.
mod commands;
mod page;
pub use page::http;
#[cfg(test)]
pub(crate) use page::stop;
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
        json!({"service_id":self.resource.id,"platform":format!("linux/{}", std::env::consts::ARCH),"phase":self.resource.phase,"scope":self.resource.scope,"storage":self.storage,"workspace_executable":self.workspace_exec,"expires_unix":(!self.resource.lease.is_until_stopped()).then(||self.resource.lease.expires()),"until_stopped":self.resource.lease.is_until_stopped(),"diagnostic":self.diagnostic,"output":self.output,"exit_code":self.exit_code,"output_limit_bytes":OUTPUT,"executions":self.executions.iter().map(|e|e.brief()).collect::<Vec<_>>()})
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
    let version = marker.trim().parse::<u8>().unwrap_or(0);
    if !(1..=4).contains(&version) || (input.lease_seconds == -1 && version < 2) {
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
/// The verified, running container of `service` and the user to run as, for
/// commands Control runs in it on a chat's behalf. Queries the engine: call
/// without the state lock.
pub(crate) fn exec_target(service: &Service) -> io::Result<(String, String)> {
    identity::authorize(&service.actor, Action::Task).map_err(fail)?;
    let id = service.resource.phase.container().ok_or_else(|| fail("Service is not running"))?;
    let text = crate::engine::query(&["inspect", "--format", INSPECT, id.as_str()])?;
    let observed: Inspection = serde_json::from_str(text.trim()).map_err(fail)?;
    verify(service, &observed)?;
    if observed.status != "running" || service.resource.lease.expired(now()) {
        return Err(fail("Service is no longer running"));
    }
    Ok((id.as_str().to_owned(), format!("{}:{}", service.uid, service.gid)))
}
/// Whether the service's image runner supervises watches (label 3 or later).
pub(crate) fn supports_watches(service: &Service) -> io::Result<bool> {
    let label = crate::engine::query(&["image", "inspect", "--format", "{{index .Config.Labels \"dev.control.service-runner\"}}", service.resource.image.as_str()])?;
    Ok(label.trim().parse::<u8>().is_ok_and(|version| version >= 3))
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
    /// Check the recorded ID without label filters before declaring it absent.
    fn exists(&mut self, id: &ContainerId) -> io::Result<bool>;
    fn create(&mut self, service: &Service) -> io::Result<ContainerId>;
    fn start(&mut self, binding: &Verified<'_>) -> io::Result<()>;
    fn remove(&mut self, binding: &Verified<'_>) -> io::Result<()>;
    fn logs(&mut self, _binding: &Verified<'_>) -> io::Result<String> { Ok(String::new()) }
}
struct Docker;
/// A live verified offline service capability, never an arbitrary container ID.
pub(crate) struct PortBinding { id: ContainerId, user: String, expires: u64 }
#[derive(Clone, Copy)]
pub(crate) enum RelayMode { Connect, Multiplex }
impl PortBinding {
    pub(crate) fn spawn(&self, mode: RelayMode, port: std::num::NonZeroU16, expires: u64) -> io::Result<std::process::Child> {
        if expires <= now() || expires > self.expires { return Err(fail("Port lease exceeds its service")); }
        crate::engine::cli().args(["exec", "-i", "--user", &self.user, self.id.as_str(), "/bin/control-tcp-relay", match mode { RelayMode::Connect => "connect", RelayMode::Multiplex => "listen-multi" }, &port.to_string(), &expires.to_string()])
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn()
    }
}
/// Relay version this host speaks; images built for another fail closed.
const RELAY: &str = "5";
pub(crate) fn relay_capable(service: &Service) -> io::Result<()> {
    // Image IDs are immutable. Avoid an extra engine query on every connection.
    static CAPABLE:std::sync::OnceLock<Mutex<std::collections::HashSet<ImageId>>>=std::sync::OnceLock::new();
    let cache=CAPABLE.get_or_init(Mutex::default);
    if cache.lock().map_err(|_|fail("Relay capability cache unavailable"))?.contains(&service.resource.image) {return Ok(());}
    let marker = crate::engine::query(&["image", "inspect", service.resource.image.as_str(), "--format", "{{index .Config.Labels \"dev.control.tcp-relay\"}}"])?;
    if marker.trim() != RELAY {
        return Err(fail("Rebuild the immutable worker images with Nix setup, then start a new service to enable TCP relays"));
    }
    let mut cached=cache.lock().map_err(|_|fail("Relay capability cache unavailable"))?;
    if cached.len()<MAX_RECORDS{cached.insert(service.resource.image.clone());}
    Ok(())
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
    fn exists(&mut self, id: &ContainerId) -> io::Result<bool> {
        crate::engine::query(&[
            "ps", "-a", "--no-trunc", "--filter", &format!("id={}", id.as_str()),
            "--format", "{{.ID}}",
        ]).map(|ids| !ids.trim().is_empty())
    }
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
                                _ => EngineState::Other,
                            };
                            Event::Observed {
                                container: o.id.clone().try_into().map_err(fail)?,
                                state,
                            }
                        }
                    } else {
                        match service.resource.phase.container() {
                            Some(id) if engine.exists(id)? => Event::Fence(Fault::IdentityMismatch),
                            _ => Event::Missing,
                        }
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
mod tests;
