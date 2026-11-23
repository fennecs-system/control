//! The Docker CLI backend. [`Docker::run_arguments`] is a pure function from
//! a [`RunSpec`] to one `docker run` argument vector; the trait methods are
//! the only place this crate starts a process.
use crate::{Access, Exec, Input, Isolation, Logs, Mount, Network, Proc, RunSpec, SecretMount, Size, Source, Streams, Tmpfs};
use std::{
    io,
    path::Path,
    process::{Child, Command, Stdio},
};

// Shared with standalone tools (the browser), which also include it.
#[path = "../../../tools/engine-cli.rs"]
mod engine_cli;

/// Engine client settings a scrubbed CLI keeps from the environment.
const CONNECTION: [&str; 7] = ["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"];

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
        let mounts = spec.mounts.iter().map(mount).chain(spec.secrets.iter().map(secret)).collect::<io::Result<Vec<_>>>()?;
        let seccomp = spec.seccomp.as_deref().map(path).transpose()?;
        let limits = &spec.limits;
        Ok([
            words(["run"]),
            switch("--interactive", spec.input != Input::Closed),
            switch("--tty", spec.input == Input::Terminal),
            words(["--rm", "--pull=never"]),
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
            mounts.iter().flat_map(|mount| pair("--mount", mount)).collect(),
            spec.workdir.as_deref().map(|workdir| pair("--workdir", workdir)).unwrap_or_default(),
            spec.env.iter().flat_map(|(name, value)| pair("--env", &format!("{name}={value}"))).collect(),
            joined("--entrypoint", spec.entrypoint.as_deref()),
            words([spec.image.as_str()]),
            spec.argv.clone(),
        ]
        .concat())
    }
}
impl Isolation for Docker {
    fn spawn(&self, spec: &RunSpec, streams: Streams) -> io::Result<Child> {
        streams.apply(Self::scrubbed().args(Self::run_arguments(spec)?)).spawn()
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        Self::scrubbed()
            .args(["rm", "--force", name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
            .then_some(())
            .ok_or_else(|| io::Error::other("Container removal failed"))
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
    let options = ["rw".to_owned(), "nosuid".to_owned()]
        .into_iter()
        .chain(tmpfs.nodev.then(|| "nodev".to_owned()))
        .chain(exec)
        .chain(tmpfs.mode.map(|mode| format!("mode={mode:o}")))
        .chain(tmpfs.owner.map(|owner| format!("uid={},gid={}", owner.uid, owner.gid)))
        .chain([format!("size={}", size(tmpfs.size))])
        .collect::<Vec<_>>();
    format!("{}:{}", tmpfs.target, options.join(","))
}
