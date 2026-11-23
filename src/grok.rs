//! xAI's Grok Build CLI, run like Claude Code: its own tools are removed, its
//! only tools are Control's over MCP, and executable work runs in separate
//! offline containers through the host shell tool.
use crate::{backend::Backend, claude, usage::Attempt, State};
use control_isolation::{Access, Input, Isolation, RunSpec, Size, Stream, Streams, Tmpfs, User};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(crate) struct Grok;
impl crate::backend::Agent for Grok {
    fn id(&self) -> &'static str {
        "grok"
    }
    fn label(&self) -> &'static str {
        "xAI / Grok Build"
    }
    fn name(&self) -> &'static str {
        "Grok"
    }
    fn default_model(&self) -> String {
        "grok-4.7".into()
    }
    fn image(&self) -> Option<String> {
        fs::read_to_string(crate::config::directory().ok()?.join("grok-image-id"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| crate::runtime::pinned_image(s))
    }
    fn image_hint(&self) -> &'static str {
        "run nix run .#setup -- --grok, or set grok-image-id in the configuration directory to the Nix image digest"
    }
    fn credentials_ready(&self) -> Result<(), String> {
        credential().map(|_| ()).map_err(|_| LOGIN.to_owned())
    }
    fn probe(&self, isolation: &dyn Isolation, image: &str) -> Result<(), String> {
        crate::config::node_id()
            .map_err(io::Error::other)
            .and_then(|node| probe(isolation, image, &node, &crate::config::state("grok-check").map_err(io::Error::other)?, &format!("control-grok-probe-{}", std::process::id())))
            .map_err(|error| match error.kind() {
                io::ErrorKind::TimedOut => PROBE_TIMED_OUT.to_owned(),
                _ => "the offline Grok tool-isolation probe failed; check the pinned image and container sandbox support".to_owned(),
            })
    }
    fn text_run(&self, _: &dyn Isolation, call: &crate::backend::TextCall<'_>) -> io::Result<RunSpec> {
        text_container(call, &credential()?)
    }
    fn text(&self, reader: &mut dyn Read, _max: usize, emit: &mut dyn FnMut(control_ai::Event)) -> io::Result<control_ai::Output> {
        control_ai::json_lines(reader, control_ai::grok::Grok::text(), emit)
    }
    fn host_shell(&self) -> bool {
        true
    }
    fn mailbox(&self) -> Option<&'static str> {
        Some(MAILBOX)
    }
    fn probe_workspace(&self) -> &'static str {
        PROBE
    }
}
const MAILBOX: &str = "grok-mailbox";
const PROBE: &str = "grok-isolation-probe";
const LOGIN: &str = "run ./target/release/control grok login; its private login stays outside the checkout";
const PROBE_TIMED_OUT: &str = "Grok tool isolation probe timed out after 90 s: the container engine is busy or slow, which is not an isolation failure. No account credentials were mounted; retry shortly.";
/// Headless, unattended and stateless: no update check, subagents, web search,
/// memory or plan mode, and every remaining tool call approved.
const FLAGS: [&str; 9] = ["--output-format", "streaming-json", "--no-auto-update", "--always-approve", "--no-subagents", "--disable-web-search", "--no-memory", "--no-plan", "--verbatim"];
/// An allowlist naming no built-in tool. `mcp__` entries are never unknown to
/// Grok, so this cannot trip its fall-back to the full toolset; what remains is
/// the two MCP meta-tools.
const NO_TOOLS: &str = "mcp__control";
/// Control's tools, as the one MCP server in Grok's user configuration.
const CONFIG: &str = "[mcp_servers.control]\ncommand = \"/bin/control-room-mcp\"\n";

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn auth_directory() -> io::Result<PathBuf> {
    Ok(crate::config::directory().map_err(fail)?.join("grok"))
}
/// The login file's path and metadata only; its contents are never read.
fn credential() -> io::Result<PathBuf> {
    let path = if let Some(path) = std::env::var_os("CONTROL_GROK_AUTH_FILE") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(fail("Grok credential path must be absolute"));
        }
        path
    } else {
        let directory = auth_directory()?;
        let meta = fs::symlink_metadata(&directory)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 {
            return Err(fail("Grok login directory must be private and not a symlink"));
        }
        directory.join("auth.json")
    };
    let meta = fs::symlink_metadata(&path).map_err(|_| fail("Run control grok login to sign in inside the Nix container"))?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
        return Err(fail("Grok credential must be a private regular file owned by a non-root user"));
    }
    Ok(path)
}
/// The shared CLI container with Grok's home on a tmpfs, so sessions and logs
/// die with it, and the login mounted read-only.
fn container(name: &str, node: &str, auth: &Path, image: &str) -> io::Result<RunSpec> {
    let meta = fs::symlink_metadata(auth)?;
    let owner = User { uid: meta.uid(), gid: meta.gid() };
    Ok(RunSpec { image: image.into(), input: Input::Attached, ..claude::base(name, node, owner) }
        .env("GROK_HOME", "/home/agent/.grok")
        .tmpfs(Tmpfs::new("/home/agent/.grok", Size::Mib(64)).owned(owner))
        .secret("grok/auth.json", claude::checked(auth)?, "/home/agent/.grok/auth.json", Access::ReadOnly))
}
fn text_container(call: &crate::backend::TextCall<'_>, auth: &Path) -> io::Result<RunSpec> {
    // A text request carries no prompt file: the caller writes the prompt to
    // standard input, which `sh` hands to Grok as a file it can read.
    Ok(container(call.name, call.node, auth, call.image)?
        .bind(claude::checked(call.workspace)?, "/workspace", Access::ReadOnly)
        .args(["bash", "-c", "cat > /tmp/prompt && exec grok \"$@\" --prompt-file /tmp/prompt", "grok"])
        .args(FLAGS)
        .args(["--tools", NO_TOOLS, "--disallowed-tools", "search_tool,use_tool", "--model", call.model]))
}
/// One turn's container: Grok's configuration and prompt read-only, the mailbox writable.
#[allow(clippy::too_many_arguments)]
fn turn_container(name: &str, node: &str, auth: &Path, config: &Path, turn: &Path, mailbox: &Path, image: &str, model: &str) -> io::Result<RunSpec> {
    Ok(container(name, node, auth, image)?
        .bind(claude::checked(config)?, "/home/agent/.grok/config.toml", Access::ReadOnly)
        .bind(claude::checked(turn)?, "/home/agent/prompt", Access::ReadOnly)
        .bind(claude::checked(mailbox)?, "/workspace", Access::ReadWrite)
        .args(["grok", "--prompt-file", "/home/agent/prompt"])
        .args(FLAGS)
        .args(["--tools", NO_TOOLS, "--model", model]))
}
fn login_container(name: &str, node: &str, owner: User, directory: &Path, image: &str) -> io::Result<RunSpec> {
    Ok(RunSpec { image: image.into(), input: Input::Terminal, ..claude::base(name, node, owner) }
        .env("GROK_HOME", "/home/agent/.grok")
        .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
        .secret("grok/login", claude::checked(directory)?, "/home/agent/.grok", Access::ReadWrite)
        .args(["grok", "login", "--device-auth"]))
}
/// The offline isolation probe; a timeout fails with [`io::ErrorKind::TimedOut`].
fn probe(isolation: &dyn Isolation, image: &str, node: &str, root: &Path, name: &str) -> io::Result<()> {
    let directory = root.join(PROBE);
    fs::create_dir_all(&directory)?;
    let command = "set -eu; test ! -e /home/agent/.grok/auth.json; test ! -S /var/run/docker.sock; test ! -e /proc/1/root/home/agent/.grok/auth.json; test -z \"${XAI_API_KEY-}\"; printf verified > isolation-check";
    let output = claude::shell_bounded(isolation, image, name, node, &directory, command, false, crate::worker::SANDBOX_PROBE_WAIT).map_err(|error| match error.kind() {
        io::ErrorKind::TimedOut => io::Error::new(io::ErrorKind::TimedOut, PROBE_TIMED_OUT),
        _ => error,
    })?;
    if output["exit_code"] != 0 || fs::read(directory.join("isolation-check"))? != b"verified" {
        return Err(fail("Grok workspace isolation check failed; no credentials were mounted"));
    }
    Ok(())
}
/// A fixed message for a failure the shared classifier knows (a usage limit, a
/// rejected login); otherwise Grok's own bounded ending.
fn diagnose(ended: Option<&str>, stderr: &[u8]) -> String {
    let known = |bytes: &[u8]| Some(crate::worker::worker_failure(bytes)).filter(|m| *m != crate::worker::UNKNOWN_FAILURE).map(|m| m.replace("Codex", "Grok"));
    ended.and_then(|e| known(e.as_bytes()))
        .or_else(|| ended.map(str::to_owned))
        .or_else(|| known(stderr))
        .unwrap_or_else(|| "Grok stopped without success. Check its login, selected model and usage limits; credentials remain read-only.".into())
}
/// A private file beside the workspace for the container to read.
fn private(path: &Path, contents: &str) -> io::Result<()> {
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// One turn. `prompt` is the same captured turn context or framing Codex and
/// local models receive.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute(
    isolation: &dyn Isolation,
    id: &str,
    prompt: String,
    attempt: &mut Attempt,
    bridge: Option<(&Arc<Mutex<State>>, &str)>,
    workspace: &Path,
    image: &str,
    node: &str,
) -> Result<String, (&'static str, String)> {
    let blocked = |message: &str| ("blocked", message.to_owned());
    let failed = |message: &str| ("failed", message.to_owned());
    let auth = credential().map_err(|_| blocked("Grok login unavailable. Run control grok login; no other login on this host is used."))?;
    let root = workspace.parent().ok_or_else(|| blocked("Invalid attempt workspace"))?;
    probe(isolation, image, node, root, &format!("control-{id}-probe")).map_err(|error| match error.kind() {
        io::ErrorKind::TimedOut => blocked(PROBE_TIMED_OUT),
        _ => blocked("Grok tool isolation probe failed. No account credentials were mounted."),
    })?;
    let mailbox = root.join(MAILBOX);
    fs::create_dir_all(&mailbox).map_err(|_| failed("Cannot prepare Grok mailbox"))?;
    let (config, turn) = (root.join("grok-config.toml"), root.join("grok-prompt"));
    private(&config, CONFIG).and_then(|_| private(&turn, &crate::observe::append(bridge, prompt))).map_err(|_| failed("Cannot prepare Grok configuration"))?;
    let name = format!("control-{id}");
    // The container's own mount failure reads as a credential problem; the
    // others are checked first so each keeps its own message.
    container(&name, node, &auth, image).map_err(|_| blocked("Grok credential metadata unavailable"))?;
    [&config, &turn, &mailbox].into_iter().try_for_each(|path| claude::checked(path).map(drop)).map_err(|_| blocked("Invalid Grok mount path"))?;
    let spec = turn_container(&name, node, &auth, &config, &turn, &mailbox, image, &attempt.model).map_err(|_| blocked("Grok credential metadata unavailable"))?;
    if crate::lifecycle::cancelled(bridge) {
        return Err(("cancelled", "Cancelled before Grok launch.".into()));
    }
    let child = isolation.spawn(&spec, Streams::new(Stream::Null, Stream::Piped, Stream::Piped)).map_err(|_| failed("Cannot start the Grok Nix container"))?;
    let cli = claude::Cli {
        name: "Grok", id, mailbox: &mailbox, workspace, stdin: None,
        capture: |reader, update, progress, bad_tools| claude::capture_with(reader, control_ai::grok::Grok::agent(), "Grok", update, progress, bad_tools),
        failure: diagnose,
    };
    claude::supervise(cli, child, attempt, bridge)
}

pub fn command(args: &[String]) -> io::Result<()> {
    const USAGE: &str = "Use control grok login | check";
    let [action] = args else { return Err(fail(USAGE)) };
    if matches!(action.as_str(), "--help" | "-h" | "help") {
        println!("control grok login: sign in with a device code inside the pinned Nix Grok container\ncontrol grok check: offline tool isolation check; no credentials or model calls");
        return Ok(());
    }
    let image = || Backend::Grok.image().ok_or_else(|| fail("Build and pin the Grok image first: nix run .#setup -- --grok"));
    let node = || crate::config::node_id().map_err(fail);
    match action.as_str() {
        "login" => {
            use std::os::unix::fs::DirBuilderExt;
            let directory = auth_directory()?;
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory)?;
            let meta = fs::symlink_metadata(&directory)?;
            if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
                return Err(fail("Grok login directory must be private and owned by a non-root user"));
            }
            let spec = login_container(&format!("control-grok-login-{}", std::process::id()), &node()?, User { uid: meta.uid(), gid: meta.gid() }, &directory, &image()?)?;
            if !crate::engine::isolation().spawn(&spec, Streams::INHERIT)?.wait()?.success() {
                return Err(fail("Grok login did not complete"));
            }
            fs::set_permissions(directory.join("auth.json"), fs::Permissions::from_mode(0o600))
        }
        "check" => {
            probe(crate::engine::isolation(), &image()?, &node()?, &crate::config::state("grok-check").map_err(fail)?, &format!("control-grok-probe-{}", std::process::id()))?;
            println!("Grok offline workspace isolation verified. No account credentials were mounted or model called.");
            Ok(())
        }
        _ => Err(fail(USAGE)),
    }
}
#[cfg(test)]
mod tests;
