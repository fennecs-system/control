//! Google's Antigravity CLI (`agy`), run like Grok Build: its own tools are
//! denied, its only tools are Control's over MCP, and executable work runs in
//! separate offline containers through the host shell tool.
use crate::{backend::Backend, claude, usage::Attempt, State};
use control_isolation::{Access, Input, Isolation, RunSpec, Size, Stream, Streams, Tmpfs, User};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(crate) struct Antigravity;
impl crate::backend::Agent for Antigravity {
    fn id(&self) -> &'static str {
        "antigravity"
    }
    fn label(&self) -> &'static str {
        "Google / Antigravity CLI"
    }
    fn name(&self) -> &'static str {
        "Antigravity"
    }
    fn default_model(&self) -> String {
        "gemini-4-neon-medium".into()
    }
    fn image(&self) -> Option<String> {
        fs::read_to_string(crate::config::directory().ok()?.join("antigravity-image-id"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| crate::runtime::pinned_image(s))
    }
    fn image_hint(&self) -> &'static str {
        "run nix run .#setup -- --antigravity, or set antigravity-image-id in the configuration directory to the Nix image digest"
    }
    fn credentials_ready(&self) -> Result<(), String> {
        login().map(|_| ()).map_err(|_| LOGIN.to_owned())
    }
    fn probe(&self, isolation: &dyn Isolation, image: &str) -> Result<(), String> {
        crate::config::node_id()
            .map_err(io::Error::other)
            .and_then(|node| probe(isolation, image, &node, &crate::config::state("antigravity-check").map_err(io::Error::other)?, &format!("control-antigravity-probe-{}", std::process::id())))
            .map_err(|error| match error.kind() {
                io::ErrorKind::TimedOut => PROBE_TIMED_OUT.to_owned(),
                _ => "the offline Antigravity tool-isolation probe failed; check the pinned image and container sandbox support".to_owned(),
            })
    }
    fn text_run(&self, _: &dyn Isolation, call: &crate::backend::TextCall<'_>) -> io::Result<RunSpec> {
        let login = login()?;
        // The text call's scratch root is private and removed after a success.
        let root = call.workspace.parent().ok_or_else(|| fail("Invalid text-call workspace"))?;
        let private = prepare(root, &login, None, None)?;
        text_container(call, &login, &private)
    }
    fn text(&self, reader: &mut dyn Read, _max: usize, emit: &mut dyn FnMut(control_ai::Event)) -> io::Result<control_ai::Output> {
        control_ai::json_lines(reader, control_ai::antigravity::Antigravity::text(), emit)
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
const MAILBOX: &str = "antigravity-mailbox";
const PROBE: &str = "antigravity-isolation-probe";
/// Control's files for one container, beside the workspace and never in it.
const PRIVATE: &str = "antigravity-private";
const LOGIN: &str = "run control antigravity login, or save a Gemini API key; the login stays outside the checkout";
const PROBE_TIMED_OUT: &str = "Antigravity tool isolation probe timed out after 90 s: the container engine is busy or slow, which is not an isolation failure. No account credentials were mounted; retry shortly.";
/// Headless and unattended. Approval prompts are skipped, so nothing is
/// soft-denied; what stays blocked is the deny list in [`settings`], which
/// applies in every mode.
const FLAGS: [&str; 4] = ["--dangerously-skip-permissions", "--disable-slash-commands", "--output-format", "stream-json"];
/// Every built-in action class the CLI's permission engine names.
const DENY: [&str; 7] = ["command(*)", "unsandboxed(*)", "read_file(*)", "write_file(*)", "read_url(*)", "execute_url(*)", "download_url(*)"];
/// Where the sign-in token, API key and ADC files live inside the container.
/// In a container the CLI keeps a Google sign-in in this file rather than the
/// OS keyring; its own log says so ("container environment detected").
const TOKEN: &str = "/home/agent/.gemini/antigravity-cli/antigravity-oauth-token";
/// The same file as `control antigravity login` saved it, in the login directory.
const SAVED: &str = "login/gemini/antigravity-cli/antigravity-oauth-token";
const KEY: &str = "/home/agent/.control-key/gemini-api-key";
const ADC: &str = "/home/agent/.config/gcloud/application_default_credentials.json";

/// How the CLI signs in, chosen by which private file the operator created.
#[derive(Debug, PartialEq, Eq)]
enum Login {
    /// `gemini-api-key`: `modelProvider: gemini` with `GEMINI_API_KEY`.
    ApiKey(PathBuf),
    /// `adc.json`: Google Cloud Application Default Credentials, `AGY_ADC_AUTH`.
    Adc(PathBuf),
    /// `login/`: the Google sign-in `control antigravity login` saved.
    Google(PathBuf),
}

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn auth_directory() -> io::Result<PathBuf> {
    Ok(crate::config::directory().map_err(fail)?.join("antigravity"))
}
fn private_owned(meta: &fs::Metadata) -> bool {
    meta.mode() & 0o077 == 0 && meta.uid() != 0
}
/// Metadata only; no credential is ever read.
fn login() -> io::Result<Login> {
    login_in(&auth_directory()?)
}
fn login_in(directory: &Path) -> io::Result<Login> {
    let meta = fs::symlink_metadata(directory).map_err(|_| fail("Run control antigravity login, or save a Gemini API key"))?;
    if !meta.is_dir() || !private_owned(&meta) {
        return Err(fail("Antigravity login directory must be private, owned by a non-root user and not a symlink"));
    }
    let file = |name: &str| {
        let path = directory.join(name);
        fs::symlink_metadata(&path).ok().filter(|m| m.is_file() && private_owned(m)).map(|_| path)
    };
    if let Some(path) = file("gemini-api-key") {
        return Ok(Login::ApiKey(path));
    }
    if let Some(path) = file("adc.json") {
        return Ok(Login::Adc(path));
    }
    if let Some(path) = file(SAVED) {
        return Ok(Login::Google(path));
    }
    Err(fail("No Antigravity login: run control antigravity login, or save a private gemini-api-key or adc.json"))
}
/// The settings every run gets. Deny rules win over allow rules and apply in
/// every mode (https://antigravity.google/docs/permissions), so the CLI keeps
/// no file, shell or web access; a turn may call only Control's MCP tools.
fn settings(login: &Login, tools: bool) -> Value {
    let mut settings = json!({
        "permissions": {"allow": if tools { vec!["mcp(control/*)"] } else { vec![] }, "deny": DENY},
        "artifactReviewPolicy": "always-proceed",
        "enableTerminalSandbox": false,
        "allowNonWorkspaceAccess": false,
        "disableSlashCommands": true,
        "trustedWorkspaces": ["/workspace"],
    });
    if matches!(login, Login::ApiKey(_)) {
        settings["modelProvider"] = json!("gemini");
    }
    settings
}
/// Control's tools as the one MCP server. Each is loaded eagerly: a lazy tool
/// is found by reading its schema file, which the deny list forbids
/// (https://antigravity.google/docs/mcp#tool-loading-lazy-vs-eager).
fn mcp_config() -> Value {
    let tools: serde_json::Map<String, Value> = control_context::worker_mcp_names().into_iter().map(|name| (name.to_owned(), json!({"eager": true}))).collect();
    json!({"mcpServers": {"control": {"command": "/bin/control-room-mcp", "tools": tools}}})
}
fn private_file(path: &Path, contents: &str) -> io::Result<()> {
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
/// Writes the private directory a container reads its settings, MCP server
/// and prompt from.
fn prepare(root: &Path, login: &Login, mcp: Option<&Value>, prompt: Option<&str>) -> io::Result<PathBuf> {
    let directory = root.join(PRIVATE);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory)?;
    private_file(&directory.join("settings.json"), &settings(login, mcp.is_some()).to_string())?;
    if let Some(mcp) = mcp {
        private_file(&directory.join("mcp_config.json"), &mcp.to_string())?;
    }
    if let Some(prompt) = prompt {
        private_file(&directory.join("prompt"), prompt)?;
    }
    Ok(directory)
}
/// The shared CLI container: the CLI's home is a tmpfs, so sessions, logs and
/// its tool cache die with it; the login is mounted for this container only.
fn container(name: &str, node: &str, login: &Login, private: &Path, image: &str) -> io::Result<RunSpec> {
    let meta = match login {
        Login::ApiKey(path) | Login::Adc(path) | Login::Google(path) => fs::symlink_metadata(path)?,
    };
    let owner = User { uid: meta.uid(), gid: meta.gid() };
    let spec = RunSpec { image: image.into(), input: Input::Attached, ..claude::base(name, node, owner) }
        .tmpfs(Tmpfs::new("/home/agent/.gemini", Size::Mib(256)).owned(owner))
        .bind(claude::checked(private)?, "/home/agent/.control", Access::ReadOnly);
    login_secret(spec, login)
}
fn text_container(call: &crate::backend::TextCall<'_>, login: &Login, private: &Path) -> io::Result<RunSpec> {
    Ok(container(call.name, call.node, login, private, call.image)?
        .bind(claude::checked(call.workspace)?, "/workspace", Access::ReadOnly)
        .args(["control-agy", "text"])
        .args(FLAGS)
        .args(["--model", call.model]))
}
fn turn_container(name: &str, node: &str, login: &Login, private: &Path, mailbox: &Path, image: &str, model: &str) -> io::Result<RunSpec> {
    Ok(container(name, node, login, private, image)?
        .bind(claude::checked(mailbox)?, "/workspace", Access::ReadWrite)
        .args(["control-agy", "turn"])
        .args(FLAGS)
        .args(["--model", model]))
}
fn login_container(name: &str, node: &str, owner: User, home: &Path, config: &Path, image: &str) -> io::Result<RunSpec> {
    Ok(RunSpec { image: image.into(), input: Input::Terminal, ..claude::base(name, node, owner) }
        // The sign-in's own home and configuration, kept: the CLI writes its token there.
        .secret("antigravity/login-home", claude::checked(home)?, "/home/agent/.gemini", Access::ReadWrite)
        .secret("antigravity/login-config", claude::checked(config)?, "/home/agent/.config/antigravity", Access::ReadWrite)
        // No browser can open here. Over SSH the CLI prints an address
        // and reads the code back (https://antigravity.google/docs/install).
        .env("SSH_CONNECTION", "control-login 0 container 22")
        .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
        .args(["control-agy", "login"]))
}
/// Lists models with the saved login, without a workspace.
fn check_container(name: &str, node: &str, login: &Login, private: &Path, image: &str) -> io::Result<RunSpec> {
    Ok(container(name, node, login, private, image)?
        .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
        .args(["control-agy", "check"]))
}
fn login_secret(spec: RunSpec, login: &Login) -> io::Result<RunSpec> {
    Ok(match login {
        Login::ApiKey(path) => spec.secret("antigravity/api-key", claude::checked(path)?, KEY, Access::ReadOnly),
        Login::Adc(path) => spec.env("AGY_ADC_AUTH", "true").secret("antigravity/adc", claude::checked(path)?, ADC, Access::ReadOnly),
        // Read-only like every backend's login. That a token refresh then
        // stays in memory is unconfirmed until a real sign-in (docs/ANTIGRAVITY.md).
        Login::Google(path) => spec.secret("antigravity/google-token", claude::checked(path)?, TOKEN, Access::ReadOnly),
    })
}
/// The offline isolation probe; a timeout fails with [`io::ErrorKind::TimedOut`].
fn probe(isolation: &dyn Isolation, image: &str, node: &str, root: &Path, name: &str) -> io::Result<()> {
    let directory = root.join(PROBE);
    fs::create_dir_all(&directory)?;
    let command = "set -eu; test ! -e /home/agent/.gemini; test ! -e /home/agent/.control-key; test ! -e /home/agent/.config/gcloud; test ! -S /var/run/docker.sock; test ! -e /proc/1/root/home/agent/.gemini; test -z \"${GEMINI_API_KEY-}${AGY_ADC_AUTH-}\"; printf verified > isolation-check";
    let output = claude::shell_bounded(isolation, image, name, node, &directory, command, false, crate::worker::SANDBOX_PROBE_WAIT).map_err(|error| match error.kind() {
        io::ErrorKind::TimedOut => io::Error::new(io::ErrorKind::TimedOut, PROBE_TIMED_OUT),
        _ => error,
    })?;
    if output["exit_code"] != 0 || fs::read(directory.join("isolation-check"))? != b"verified" {
        return Err(fail("Antigravity workspace isolation check failed; no credentials were mounted"));
    }
    Ok(())
}
/// A fixed message for a failure the shared classifier knows (a usage limit, a
/// rejected login), after the CLI's own sign-in wording; otherwise its ending.
fn diagnose(ended: Option<&str>, stderr: &[u8]) -> String {
    let own = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        if ["authentication required", "authentication failed", "authentication expired", "log back in", "not logged into", "api key not valid", "gemini_api_key is not set"].iter().any(|s| text.contains(s)) {
            Some("Antigravity's login is missing, expired or was rejected. Run control antigravity check, then sign in again with control antigravity login or replace the API key.".to_owned())
        } else if text.contains("resource_exhausted") || text.contains("out of quota") {
            Some("The signed-in Antigravity account reached a usage or rate limit.".to_owned())
        } else {
            Some(crate::worker::worker_failure(bytes)).filter(|m| *m != crate::worker::UNKNOWN_FAILURE).map(|m| m.replace("Codex", "Antigravity"))
        }
    };
    ended.and_then(|e| own(e.as_bytes()))
        .or_else(|| ended.map(str::to_owned))
        .or_else(|| own(stderr))
        .unwrap_or_else(|| "Antigravity stopped without success. Check its login, selected model and usage limits.".into())
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
    let login = login().map_err(|_| blocked("Antigravity login unavailable. Run control antigravity login or save a Gemini API key; no other login on this host is used."))?;
    let root = workspace.parent().ok_or_else(|| blocked("Invalid attempt workspace"))?;
    probe(isolation, image, node, root, &format!("control-{id}-probe")).map_err(|error| match error.kind() {
        io::ErrorKind::TimedOut => blocked(PROBE_TIMED_OUT),
        _ => blocked("Antigravity tool isolation probe failed. No account credentials were mounted."),
    })?;
    let mailbox = root.join(MAILBOX);
    fs::create_dir_all(&mailbox).map_err(|_| failed("Cannot prepare Antigravity mailbox"))?;
    let private = prepare(root, &login, Some(&mcp_config()), Some(&crate::observe::append(bridge, prompt)))
        .map_err(|_| failed("Cannot prepare Antigravity configuration"))?;
    let name = format!("control-{id}");
    container(&name, node, &login, &private, image).map_err(|_| blocked("Antigravity login metadata or mount path unavailable"))?;
    claude::checked(&mailbox).map_err(|_| blocked("Invalid Antigravity mount path"))?;
    let spec = turn_container(&name, node, &login, &private, &mailbox, image, &attempt.model).map_err(|_| blocked("Antigravity login metadata or mount path unavailable"))?;
    if crate::lifecycle::cancelled(bridge) {
        return Err(("cancelled", "Cancelled before Antigravity launch.".into()));
    }
    let child = isolation.spawn(&spec, Streams::new(Stream::Null, Stream::Piped, Stream::Piped)).map_err(|_| failed("Cannot start the Antigravity Nix container"))?;
    let cli = claude::Cli {
        name: "Antigravity", id, mailbox: &mailbox, workspace, stdin: None,
        capture: |reader, update, progress, bad_tools| claude::capture_with(reader, control_ai::antigravity::Antigravity::agent(), "Antigravity", update, progress, bad_tools),
        failure: diagnose,
    };
    claude::supervise(cli, child, attempt, bridge)
}

pub fn command(args: &[String]) -> io::Result<()> {
    const USAGE: &str = "Use control antigravity login | check";
    let [action] = args else { return Err(fail(USAGE)) };
    if matches!(action.as_str(), "--help" | "-h" | "help") {
        println!("control antigravity login: sign in to Google inside the pinned Nix Antigravity container; the token stays in a private directory outside the checkout\ncontrol antigravity check: offline tool isolation, then the binary, the saved login and the model list it can see");
        return Ok(());
    }
    let image = || Backend::Antigravity.image().ok_or_else(|| fail("Build and pin the Antigravity image first: nix run .#setup -- --antigravity"));
    let node = || crate::config::node_id().map_err(fail);
    let directory = auth_directory()?;
    match action.as_str() {
        "login" => {
            let (home, config) = (directory.join("login/gemini"), directory.join("login/config"));
            for path in [&home, &config] {
                fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
            }
            let meta = fs::symlink_metadata(&directory)?;
            if !meta.is_dir() || !private_owned(&meta) {
                return Err(fail("Antigravity login directory must be private and owned by a non-root user"));
            }
            // The sign-in's own home, kept: the CLI writes its token there.
            let spec = login_container(&format!("control-antigravity-login-{}", std::process::id()), &node()?, User { uid: meta.uid(), gid: meta.gid() }, &home, &config, &image()?)?;
            if !crate::engine::isolation().spawn(&spec, Streams::INHERIT)?.wait()?.success() {
                return Err(fail("Antigravity login did not complete"));
            }
            let token = directory.join(SAVED);
            if fs::symlink_metadata(&token).is_ok_and(|m| m.is_file()) {
                fs::set_permissions(&token, fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }
        "check" => {
            let image = image()?;
            let node = node()?;
            let state = crate::config::state("antigravity-check").map_err(fail)?;
            probe(crate::engine::isolation(), &image, &node, &state, &format!("control-antigravity-probe-{}", std::process::id()))?;
            println!("Antigravity offline workspace isolation verified. No account credentials were mounted or model called.");
            let login = login_in(&directory)?;
            let private = prepare(&state, &login, None, None)?;
            let spec = check_container(&format!("control-antigravity-probe-{}-login", std::process::id()), &node, &login, &private, &image)?;
            if !crate::engine::isolation().spawn(&spec, Streams::new(Stream::Null, Stream::Inherit, Stream::Inherit))?.wait()?.success() {
                return Err(fail("Antigravity could not list models with the saved login"));
            }
            Ok(())
        }
        _ => Err(fail(USAGE)),
    }
}
#[cfg(test)]
mod tests;
