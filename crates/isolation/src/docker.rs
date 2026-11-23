//! The Docker CLI backend. Pure functions turn specs into argument vectors
//! ([`Docker::run_arguments`] and the private renderers) and engine output
//! into typed values (`parse`); the trait methods are the only place this
//! crate starts a process.
use crate::{
    Access, ContainerId, Exec, ExecSpec, ImageId, Input, Isolation, Listed, ListedVolume, Logs, Mount, Network, ObservedVolume, Observed, Pid1, Proc, Process, RunSpec,
    SecretMount, Select, Size, Source, States, Stats, Streams, Target, Tmpfs,
};
use std::{
    collections::HashMap,
    io::{self, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

mod parse;
pub use parse::quota_capable;

// Shared with standalone tools (the browser), which also include it.
#[path = "../../../tools/engine-cli.rs"]
mod engine_cli;

/// Engine client settings a scrubbed CLI keeps from the environment.
const CONNECTION: [&str; 7] = ["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"];
/// How long an engine request may take before it is abandoned.
const REQUEST: Duration = Duration::from_secs(5);
/// A cold Docker Desktop or colima engine can take well over 5 s to answer `info`.
const READY: Duration = Duration::from_secs(20);
const LISTING: &str = "{{.ID}}\t{{.Names}}\t{{.Labels}}";
const VOLUMES: &str = "{{.Name}}\t{{.Labels}}";
const STATS: &str = "{{.ID}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.PIDs}}";

/// How a container's lifetime relates to the call that makes it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Attached to the caller and removed when it exits.
    Attached,
    /// Started in the background.
    Detached,
    /// Created, not started.
    Created,
}

pub struct Docker;
impl Docker {
    /// The engine CLI with the caller's environment.
    pub fn cli() -> Command {
        engine_cli::command()
    }
    /// The engine CLI with only engine connection settings from the
    /// environment; used for containers that run model or workspace code.
    pub fn scrubbed() -> Command {
        let mut command = Self::cli();
        command.env_clear().envs(CONNECTION.iter().filter_map(|name| Some((name, std::env::var_os(name)?))));
        command
    }
    /// The `run` arguments for an attached container removed when it exits.
    pub fn run_arguments(spec: &RunSpec) -> io::Result<Vec<String>> {
        arguments(Mode::Attached, spec)
    }
    /// The `run --detach` arguments.
    pub fn detached_arguments(spec: &RunSpec) -> io::Result<Vec<String>> {
        arguments(Mode::Detached, spec)
    }
    /// The `create` arguments.
    pub fn create_arguments(spec: &RunSpec) -> io::Result<Vec<String>> {
        arguments(Mode::Created, spec)
    }
    /// The `exec` arguments.
    pub fn exec_arguments(spec: &ExecSpec) -> Vec<String> {
        [
            words(["exec"]),
            switch("--interactive", spec.input != Input::Closed),
            switch("--tty", spec.input == Input::Terminal),
            spec.user.map(|user| pair("--user", &format!("{}:{}", user.uid, user.gid))).unwrap_or_default(),
            spec.env.iter().flat_map(|(name, value)| pair("--env", &format!("{name}={value}"))).collect(),
            spec.workdir.as_deref().map(|workdir| pair("--workdir", workdir)).unwrap_or_default(),
            words([spec.container.as_str()]),
            spec.argv.clone(),
        ]
        .concat()
    }
}
fn arguments(mode: Mode, spec: &RunSpec) -> io::Result<Vec<String>> {
    let mounts = spec.mounts.iter().map(mount).chain(spec.secrets.iter().map(secret)).collect::<io::Result<Vec<_>>>()?;
    let seccomp = spec.seccomp.as_deref().map(path).transpose()?;
    let limits = &spec.limits;
    Ok([
        match mode {
            Mode::Attached | Mode::Detached => words(["run"]),
            Mode::Created => words(["create"]),
        },
        switch("--detach", mode == Mode::Detached),
        switch("--interactive", spec.input != Input::Closed),
        switch("--tty", spec.input == Input::Terminal),
        switch("--rm", mode == Mode::Attached),
        words(["--pull=never"]),
        switch("--init", spec.pid1 == Pid1::Init),
        // The engine's default, stated: a created container never restarts itself.
        switch("--restart=no", mode == Mode::Created),
        pair("--name", &spec.name),
        spec.labels.iter().flat_map(|(name, value)| pair("--label", &format!("{name}={value}"))).collect(),
        switch("--network=none", spec.network == Network::None),
        spec.user.map(|user| pair("--user", &format!("{}:{}", user.uid, user.gid))).unwrap_or_default(),
        switch("--read-only", spec.root == Access::ReadOnly),
        words(["--cap-drop=ALL", "--security-opt=no-new-privileges"]),
        seccomp.map(|profile| pair("--security-opt", &format!("seccomp={profile}"))).unwrap_or_default(),
        switch("--security-opt=systempaths=unconfined", spec.proc == Proc::Unmasked),
        joined("--pids-limit", limits.pids),
        joined("--memory", limits.memory.map(size)),
        joined("--memory-swap", limits.memory_swap.map(size)),
        joined("--cpus", limits.cpus),
        logs(&spec.logs),
        spec.tmpfs.iter().flat_map(|fs| pair("--tmpfs", &tmpfs(fs))).collect(),
        spec.disk.map(|disk| pair("--storage-opt", &format!("size={}", disk_size(disk)))).unwrap_or_default(),
        mounts.iter().flat_map(|mount| pair("--mount", mount)).collect(),
        spec.workdir.as_deref().map(|workdir| pair("--workdir", workdir)).unwrap_or_default(),
        spec.env.iter().flat_map(|(name, value)| pair("--env", &format!("{name}={value}"))).collect(),
        joined("--entrypoint", spec.entrypoint.as_deref()),
        words([spec.image.as_str()]),
        spec.argv.clone(),
    ]
    .concat())
}
fn target(target: Target<'_>) -> &str {
    match target {
        Target::Id(id) => id.as_str(),
        Target::Name(name) => name,
    }
}
/// `ps` arguments for a listing.
fn list_arguments(states: States, select: &Select) -> Vec<String> {
    let filters = match select {
        Select::Id(id) => vec![format!("id={}", id.as_str())],
        Select::Name(name) => vec![format!("name=^/{name}$")],
        Select::Labels(labels) => labels.iter().map(|(name, value)| format!("label={name}={value}")).collect(),
        Select::Any => Vec::new(),
    };
    [
        words(["ps"]),
        switch("--all", states == States::All),
        words(["--no-trunc"]),
        filters.iter().flat_map(|filter| pair("--filter", filter)).collect(),
        words(["--format", LISTING]),
    ]
    .concat()
}
/// A bounded engine request with the caller's environment.
fn query<S: AsRef<std::ffi::OsStr>>(arguments: impl IntoIterator<Item = S>) -> io::Result<String> {
    bounded_output(Docker::cli().args(arguments), REQUEST, false)
}
/// A process the caller reads, with only the engine connection settings.
fn reader<S: AsRef<std::ffi::OsStr>>(arguments: impl IntoIterator<Item = S>, stdout: Stdio) -> io::Result<Child> {
    Docker::scrubbed().args(arguments).stdin(Stdio::null()).stdout(stdout).stderr(Stdio::null()).spawn()
}
impl Isolation for Docker {
    fn ready(&self) -> bool {
        bounded_output(Docker::cli().args(["info", "--format", "{{.OSType}}"]), READY, false).is_ok_and(|output| output.trim() == "linux")
    }
    fn disk_quota(&self) -> io::Result<bool> {
        // Probed once per process; a failed probe is retried.
        static PROBED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if let Some(known) = PROBED.get() {
            return Ok(*known);
        }
        let info = query(["info", "--format", "{{.Driver}} {{json .DriverStatus}}"])?;
        Ok(*PROBED.get_or_init(|| quota_capable(&info)))
    }
    fn image_labels(&self, image: &ImageId) -> io::Result<HashMap<String, String>> {
        parse::image_labels(&query(["image", "inspect", image.as_str()])?)
    }
    fn spawn(&self, spec: &RunSpec, streams: Streams) -> io::Result<Child> {
        streams.apply(Self::scrubbed().args(Self::run_arguments(spec)?)).spawn()
    }
    fn run_detached(&self, spec: &RunSpec) -> io::Result<ContainerId> {
        parse::container_id(&query(Self::detached_arguments(spec)?)?)
    }
    fn create(&self, spec: &RunSpec) -> io::Result<ContainerId> {
        // Creation requests carry no credential mounts or provider arguments, so
        // bounded engine diagnostics are kept: an unsupported quota is told apart.
        parse::container_id(&bounded_output(Docker::cli().args(Self::create_arguments(spec)?), REQUEST, true)?)
    }
    fn start(&self, id: &ContainerId) -> io::Result<()> {
        query(["start", id.as_str()]).map(drop)
    }
    fn kill(&self, id: &ContainerId) -> io::Result<()> {
        query(["kill", id.as_str()]).map(drop)
    }
    fn remove(&self, container: Target<'_>) -> io::Result<()> {
        query(["rm", "--force", target(container)]).map(drop)
    }
    fn inspect(&self, targets: &[Target<'_>]) -> io::Result<Vec<Observed>> {
        parse::inspection(&query(["inspect"].into_iter().chain(targets.iter().map(|container| target(*container))))?)
    }
    fn list(&self, states: States, select: &Select) -> io::Result<Vec<Listed>> {
        parse::listing(&query(list_arguments(states, select))?)
    }
    fn exec(&self, spec: &ExecSpec, streams: Streams) -> io::Result<Child> {
        streams.apply(Self::scrubbed().args(Self::exec_arguments(spec))).spawn()
    }
    fn wait(&self, id: &ContainerId) -> io::Result<Child> {
        reader(["wait", id.as_str()], Stdio::null())
    }
    fn logs(&self, id: &ContainerId, tail: u32) -> io::Result<Child> {
        reader(["logs".to_owned(), "--tail".to_owned(), tail.to_string(), id.as_str().to_owned()], Stdio::piped())
    }
    fn stats(&self, ids: &[&ContainerId]) -> io::Result<HashMap<String, Stats>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(parse::stats(&query(["stats", "--no-stream", "--no-trunc", "--format", STATS].into_iter().chain(ids.iter().map(|id| id.as_str())))?))
    }
    fn top(&self, id: &ContainerId) -> io::Result<Vec<Process>> {
        Ok(parse::top(&query(["top", id.as_str(), "-o", "pid,user,args"])?))
    }
    fn volume_create(&self, name: &str, labels: &[(&str, &str)]) -> io::Result<()> {
        let labels = labels.iter().flat_map(|(key, value)| ["--label".to_owned(), format!("{key}={value}")]);
        query(["volume", "create", "--driver", "local"].map(str::to_owned).into_iter().chain(labels).chain([name.to_owned()])).map(drop)
    }
    fn volume_inspect(&self, name: &str) -> io::Result<Vec<ObservedVolume>> {
        parse::volumes(&query(["volume", "inspect", name])?)
    }
    fn volumes(&self, labels: &[(&str, &str)]) -> io::Result<Vec<ListedVolume>> {
        let filters = labels.iter().flat_map(|(key, value)| ["--filter".to_owned(), format!("label={key}={value}")]);
        parse::volume_listing(&query(["volume", "ls"].map(str::to_owned).into_iter().chain(filters).chain(["--format".to_owned(), VOLUMES.to_owned()]))?)
    }
    fn volume_remove(&self, name: &str) -> io::Result<()> {
        query(["volume", "rm", name]).map(drop)
    }
}

/// Runs an engine request to completion within `timeout` and returns its
/// standard output. Pipes are drained while it runs (waiting first can
/// deadlock on a full pipe); oversized output fails rather than returning a
/// prefix. `diagnostic` keeps the first 4 KiB of standard error in the error.
pub fn bounded_output(command: &mut Command, timeout: Duration, diagnostic: bool) -> io::Result<String> {
    const OUTPUT_LIMIT: u64 = 1024 * 1024;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if diagnostic { Stdio::piped() } else { Stdio::null() })
        .spawn()?;
    let errors = child.stderr.take().map(|stderr| {
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr.take(4097).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
        receiver
    });
    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("No engine response"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout.take(OUTPUT_LIMIT + 1).read_to_end(&mut output).map(|_| output);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                if let Some(errors) = errors {
                    let bytes = errors
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|_| io::Error::other("Container engine diagnostic timed out"))??;
                    let message = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
                    return Err(io::Error::other(format!("Container engine request failed: {}", message.trim())));
                }
                return Err(io::Error::other("Container engine request failed"));
            }
            let output = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| io::Error::other("Container engine response timed out"))??;
            if output.len() as u64 > OUTPUT_LIMIT {
                return Err(io::Error::other("Container engine response exceeded its limit"));
            }
            return String::from_utf8(output).map_err(io::Error::other);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("Container engine response timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn words<const N: usize>(words: [&str; N]) -> Vec<String> {
    words.map(str::to_owned).to_vec()
}
fn switch(flag: &str, on: bool) -> Vec<String> {
    on.then(|| flag.to_owned()).into_iter().collect()
}
fn pair(flag: &str, value: &str) -> Vec<String> {
    words([flag, value])
}
fn joined(flag: &str, value: Option<impl std::fmt::Display>) -> Vec<String> {
    value.map(|value| format!("{flag}={value}")).into_iter().collect()
}
fn size(size: Size) -> String {
    match size {
        Size::Bytes(bytes) => bytes.to_string(),
        Size::Mib(mib) => format!("{mib}m"),
        Size::Gib(gib) => format!("{gib}g"),
    }
}
/// The storage driver's size syntax, in upper-case units.
fn disk_size(size: Size) -> String {
    match size {
        Size::Bytes(bytes) => bytes.to_string(),
        Size::Mib(mib) => format!("{mib}M"),
        Size::Gib(gib) => format!("{gib}G"),
    }
}
fn logs(logs: &Logs) -> Vec<String> {
    match logs {
        Logs::Engine => Vec::new(),
        Logs::None => words(["--log-driver=none"]),
        Logs::Local { max_size, max_files } => vec!["--log-driver=local".into(), format!("--log-opt=max-size={}", size(*max_size)), format!("--log-opt=max-file={max_files}")],
    }
}
/// The engine's mount syntax is comma-separated, so a path must not carry one.
fn path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .filter(|p| path.is_absolute() && !p.contains([',', '\n', '\r']))
        .ok_or_else(|| io::Error::other("Container mount paths must be absolute UTF-8 paths without commas or newlines."))
}
fn readonly(access: Access) -> &'static str {
    match access {
        Access::ReadOnly => ",readonly",
        Access::ReadWrite => "",
    }
}
fn mount(mount: &Mount) -> io::Result<String> {
    Ok(match &mount.source {
        Source::Bind(source) => format!("type=bind,src={},dst={}{}", path(source)?, mount.target, readonly(mount.access)),
        Source::Volume(name) => format!("type=volume,source={name},target={}{}", mount.target, readonly(mount.access)),
    })
}
fn secret(secret: &SecretMount) -> io::Result<String> {
    Ok(format!("type=bind,src={},dst={}{}", path(&secret.source)?, secret.target, readonly(secret.access)))
}
fn tmpfs(tmpfs: &Tmpfs) -> String {
    let exec = match tmpfs.exec {
        Exec::Engine => None,
        Exec::Allow => Some("exec".to_owned()),
        Exec::Deny => Some("noexec".to_owned()),
    };
    // The engine reports these options as given, and services verify them
    // verbatim, so this order is part of the contract.
    let options = ["rw".to_owned()]
        .into_iter()
        .chain(exec)
        .chain(["nosuid".to_owned()])
        .chain(tmpfs.nodev.then(|| "nodev".to_owned()))
        .chain(tmpfs.mode.map(|mode| format!("mode={mode:o}")))
        .chain(tmpfs.owner.map(|owner| format!("uid={},gid={}", owner.uid, owner.gid)))
        .chain([format!("size={}", size(tmpfs.size))])
        .collect::<Vec<_>>();
    format!("{}:{}", tmpfs.target, options.join(","))
}

#[cfg(test)]
mod tests;
