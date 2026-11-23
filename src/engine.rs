//! The root's entry to the container engine. Container launches are typed
//! [`control_isolation::RunSpec`]s carried out by the [`isolation`] backend;
//! the remaining operations below still pass engine arguments and move behind
//! that interface in later steps.
pub(crate) mod ephemeral;
use control_isolation::Docker;
use std::{io, process::Command, time::Duration};

/// The engine CLI with the caller's environment.
pub(crate) fn cli() -> Command {
    Docker::cli()
}
/// The engine CLI with only engine connection settings from the environment;
/// used for containers that run model or workspace code.
pub(crate) fn scrubbed() -> Command {
    Docker::scrubbed()
}
pub fn ready() -> bool {
    // A cold Docker Desktop or colima engine can take well over 5 s to answer `info`.
    crate::runtime::engine_output(cli().args(["info", "--format", "{{.OSType}}"]), Duration::from_secs(20))
        .is_ok_and(|output| output.trim() == "linux")
}
/// A program run as `user` inside a running container, optionally from `workdir`,
/// with `env` set when it starts.
/// The caller owns its I/O and lifetime; process semantics stay in the program.
pub(crate) fn exec(container: &str, user: &str, workdir: Option<&str>, env: &[(&str, &str)], program: &[&str]) -> Command {
    exec_with(false, container, user, workdir, env, program)
}
/// Like [`exec`], with the caller's stdin forwarded to the program, e.g. to
/// stream an archive into it without a host mount.
pub(crate) fn exec_input(container: &str, user: &str, workdir: Option<&str>, env: &[(&str, &str)], program: &[&str]) -> Command {
    exec_with(true, container, user, workdir, env, program)
}
fn exec_with(interactive: bool, container: &str, user: &str, workdir: Option<&str>, env: &[(&str, &str)], program: &[&str]) -> Command {
    let mut command = cli();
    command.arg("exec");
    if interactive {
        command.arg("--interactive");
    }
    command.args(["--user", user]);
    for (name, value) in env {
        command.args(["--env", &format!("{name}={value}")]);
    }
    if let Some(workdir) = workdir {
        command.args(["--workdir", workdir]);
    }
    command.arg(container).args(program);
    command
}
pub fn query(arguments: &[&str]) -> io::Result<String> {
    crate::runtime::engine_output(cli().args(arguments), Duration::from_secs(5))
}
/// Creation requests here contain no credential mounts or provider arguments.
/// Retain bounded engine diagnostics so unsupported quotas are distinguishable
/// from other failures. Other engine queries keep their existing redaction.
pub(crate) fn create(arguments: &[String]) -> io::Result<String> {
    crate::runtime::engine_output_diagnostic(cli().args(arguments), Duration::from_secs(5), true)
}

/// Whether the storage driver can cap a container's writable layer
/// (`--storage-opt size=`). Probed once per process; a failed probe is retried.
pub(crate) fn storage_quota() -> io::Result<bool> {
    static PROBED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if let Some(known) = PROBED.get() {
        return Ok(*known);
    }
    let info = query(&["info", "--format", "{{.Driver}} {{json .DriverStatus}}"])?;
    Ok(*PROBED.get_or_init(|| quota_capable(&info)))
}
/// Drivers Docker documents size quotas for; overlay2 only over xfs. `info`
/// does not report xfs's required pquota mount option, so creation can still
/// refuse. Docker Desktop's containerd snapshotter (`overlayfs`) has none.
fn quota_capable(info: &str) -> bool {
    let (driver, status) = info.trim().split_once(' ').unwrap_or((info.trim(), ""));
    match driver {
        "btrfs" | "zfs" | "devicemapper" | "windowsfilter" => true,
        "overlay2" => serde_json::from_str::<Vec<[String; 2]>>(status)
            .is_ok_and(|status| status.iter().any(|[key, value]| key == "Backing Filesystem" && value == "xfs")),
        _ => false,
    }
}

/// A point-in-time resource sample for one container, as reported by the engine.
pub struct Stats {
    pub cpu: String,
    pub memory: String,
    pub pids: String,
}
/// One sample per container ID; the engine measures for about a second.
pub fn stats(ids: &[&str]) -> io::Result<std::collections::HashMap<String, Stats>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let mut arguments = vec!["stats", "--no-stream", "--no-trunc", "--format", "{{.ID}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.PIDs}}"];
    arguments.extend_from_slice(ids);
    Ok(parse_stats(&query(&arguments)?))
}
fn parse_stats(text: &str) -> std::collections::HashMap<String, Stats> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.trim();
            let (cpu, memory, pids) = (fields.next()?, fields.next()?, fields.next()?);
            Some((id.to_owned(), Stats { cpu: cpu.trim().into(), memory: memory.trim().into(), pids: pids.trim().into() }))
        })
        .collect()
}
pub struct Process {
    pub pid: String,
    pub user: String,
    pub command: String,
}
/// Processes inside one container, at most `limit`.
pub fn top(id: &str, limit: usize) -> io::Result<Vec<Process>> {
    Ok(parse_top(&query(&["top", id, "-o", "pid,user,args"])?, limit))
}
fn parse_top(text: &str, limit: usize) -> Vec<Process> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let (user, command) = rest.trim_start().split_once(char::is_whitespace).unwrap_or((rest.trim(), ""));
            Some(Process { pid: pid.into(), user: user.into(), command: command.trim().into() })
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod golden;
