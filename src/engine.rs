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
pub fn query(arguments: &[&str]) -> io::Result<String> {
    crate::runtime::engine_output(cli().args(arguments), Duration::from_secs(5))
}
/// Creation requests here contain no credential mounts or provider arguments.
/// Retain bounded engine diagnostics so unsupported quotas are distinguishable
/// from other failures. Other engine queries keep their existing redaction.
pub(crate) fn create(arguments: &[String]) -> io::Result<String> {
    crate::runtime::engine_output_diagnostic(cli().args(arguments), Duration::from_secs(5), true)
}
