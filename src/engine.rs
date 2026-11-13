//! The only place Control invokes the container engine. Docker is the sole
//! backend today; callers use these entry points instead of naming the CLI so
//! another backend can replace this module behind a typed interface later.
// Shared with standalone tools (the browser), which also include it.
#[allow(clippy::duplicate_mod)]
#[path = "../tools/engine-cli.rs"]
mod engine_cli;
use std::{io, process::Command, time::Duration};

/// The engine CLI with the caller's environment.
pub(crate) fn cli() -> Command {
    engine_cli::command()
}
/// The engine CLI with only engine connection settings from the environment;
/// used for containers that run model or workspace code.
pub(crate) fn scrubbed() -> Command {
    let mut command = cli();
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_CERT_PATH",
        "DOCKER_TLS_VERIFY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
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
    let mut command = cli();
    command.args(["exec", "--user", user]);
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
