//! Control's own shell tool: a credential-free, offline container over the
//! attempt's workspace. Backends whose CLI shell is removed (`host_shell`)
//! work through it, and every CLI backend's isolation probe runs in it.
use control_agent::cli::{base, checked, drain};
use control_isolation::{Access, Isolation, Network, RunSpec, Stream, Streams, User};
use serde_json::{Value, json};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{Duration, Instant},
};

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
#[cfg(test)]
fn shell(image: &str, name: &str, node: &str, workspace: &Path, command: &str) -> io::Result<Value> {
    shell_scoped(crate::engine::isolation(), image, name, node, workspace, command, false)
}
/// A credential-free shell is the only workspace execution capability Claude receives.
/// The caller supplies persisted attempt inputs, never Docker options from the model.
pub fn shell_scoped(
    isolation: &dyn Isolation,
    image: &str,
    name: &str,
    node: &str,
    workspace: &Path,
    command: &str,
    readonly: bool,
) -> io::Result<Value> {
    shell_bounded(
        isolation,
        image,
        name,
        node,
        workspace,
        command,
        readonly,
        Duration::from_secs(25),
    )
}
/// A command stopped at `deadline` fails with [`io::ErrorKind::TimedOut`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn shell_bounded(
    isolation: &dyn Isolation,
    image: &str,
    name: &str,
    node: &str,
    workspace: &Path,
    command: &str,
    readonly: bool,
    deadline: Duration,
) -> io::Result<Value> {
    if crate::shutdown::requested() {
        return Err(fail("Control is stopping; workspace command was not started."));
    }
    if command.is_empty() || command.len() > 16000 {
        return Err(fail("Command must contain 1–16000 bytes"));
    }
    let meta = fs::symlink_metadata(workspace)?;
    if !meta.is_dir() {
        return Err(fail("Workspace is not a directory"));
    }
    let readonly_root = crate::worker_profile::Profile::inspect(isolation, image)?.readonly_root();
    let spec = shell_container(
        image,
        name,
        node,
        workspace,
        command,
        readonly,
        readonly_root,
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
    )?;
    let mut child = isolation.spawn(&spec, Streams::new(Stream::Null, Stream::Piped, Stream::Piped))?;
    let stdout = drain(child.stdout.take().unwrap(), 12000);
    let stderr = drain(child.stderr.take().unwrap(), 4000);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if !crate::shutdown::requested() && start.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            waited => {
                let _ = child.kill();
                let _ = child.wait();
                crate::lifecycle::stop_resource(isolation, name, node, image, workspace).map_err(fail)?;
                let kind = if matches!(waited, Ok(None)) && !crate::shutdown::requested() {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::Other
                };
                break Err(io::Error::new(
                    kind,
                    "Workspace command stopped after its deadline or a process error",
                ));
            }
        }
    };
    let output = stdout.join().unwrap_or_default();
    let error = stderr.join().unwrap_or_default();
    let status = status?;
    Ok(
        json!({"exit_code":status.code(),"stdout":String::from_utf8_lossy(&output),"stderr":String::from_utf8_lossy(&error),"output_limit_bytes":16000}),
    )
}

#[allow(clippy::too_many_arguments)]
fn shell_container(
    image: &str,
    name: &str,
    node: &str,
    workspace: &Path,
    command: &str,
    readonly: bool,
    readonly_root: bool,
    owner: User,
) -> io::Result<RunSpec> {
    // Explicitly override base's immutable-root default only for this
    // credential-free command. No login/model container uses this override.
    let root = if readonly_root {
        Access::ReadOnly
    } else {
        Access::ReadWrite
    };
    let access = if readonly { Access::ReadOnly } else { Access::ReadWrite };
    Ok(RunSpec {
        image: image.into(),
        root,
        network: Network::None,
        ..base(name, node, owner)
    }
    .bind(checked(workspace)?, "/workspace", access)
    .args(["bash", "-c", command]))
}

pub fn definition() -> Value {
    json!({"name":"control_workspace_run","description":"Run a short shell command in this task's offline Nix workspace. Use rg/fd, read and edit project files, and run Rust tools. No credentials, Control host files or network. 25 second deadline; bounded output. Use control_workspace_start/status/cancel for longer commands and build environments. Commands may change task files.","inputSchema":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":16000}},"required":["command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}})
}

pub fn claim(workspace: &Path, nonce: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let root = workspace.parent().ok_or_else(|| fail("Invalid workspace"))?;
    let hash: String = ring::digest::digest(&ring::digest::SHA256, nonce.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(root.join(format!("claude-command-{hash}")))?;
    file.sync_all()?;
    fs::File::open(root)?.sync_all()
}

#[cfg(test)]
mod tests;
