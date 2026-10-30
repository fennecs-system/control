//! Unmodified Claude Code harness; executable tools run in separate offline containers.
use crate::{
    backend::Backend,
    usage::{Activity, Attempt, Tokens},
    State,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type Outcome = Result<String, (&'static str, String)>;
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn auth_directory() -> io::Result<PathBuf> {
    Ok(crate::config::directory().map_err(fail)?.join("claude"))
}
fn credential() -> io::Result<PathBuf> {
    let path = if let Some(path) = std::env::var_os("CONTROL_CLAUDE_AUTH_FILE") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(fail("Claude credential path must be absolute"));
        }
        path
    } else {
        let directory = auth_directory()?;
        let meta = fs::symlink_metadata(&directory)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 {
            return Err(fail(
                "Claude login directory must be private and not a symlink",
            ));
        }
        directory.join(".credentials.json")
    };
    let meta = fs::symlink_metadata(&path)
        .map_err(|_| fail("Run control claude login to sign in inside the Nix container"))?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
        return Err(fail(
            "Claude credential must be a private regular file owned by a non-root user",
        ));
    }
    Ok(path)
}
pub fn credentials_ready() -> io::Result<()> {
    credential().map(|_| ())
}
fn mount(path: &Path, target: &str, readonly: bool) -> io::Result<String> {
    let path = path
        .to_str()
        .filter(|s| path.is_absolute() && !s.contains([',', '\n', '\r']))
        .ok_or_else(|| fail("Unsafe container mount path"))?;
    Ok(format!(
        "type=bind,src={path},dst={target}{}",
        if readonly { ",readonly" } else { "" }
    ))
}
pub(crate) fn docker() -> Command {
    let mut command = Command::new("docker");
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
fn base(name: &str, node: &str, uid: u32, gid: u32) -> Command {
    let mut command = docker();
    command.args([
        "run",
        "--rm",
        "--pull=never",
        "--name",
        name,
        "--label",
        &format!("dev.control.node={node}"),
        "--label",
        &format!("dev.control.attempt={name}"),
        "--label",
        // Login and offline checks are supervised by their command, not run reconciliation.
        if name.starts_with("control-claude-login-") || name.starts_with("control-claude-probe-") { crate::lifecycle::CHECK_LABEL } else { "dev.control.purpose=run" },
        "--user",
        &format!("{uid}:{gid}"),
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=256",
        "--memory=2g",
        "--cpus=2",
        "--tmpfs",
        "/tmp:rw,nosuid,size=128m",
        "--tmpfs",
        &format!("/home/agent:rw,nosuid,uid={uid},gid={gid},size=64m"),
        "--env",
        "HOME=/home/agent",
        "--env",
        "CLAUDE_CONFIG_DIR=/home/agent/.claude",
        "--workdir",
        "/workspace",
    ]);
    command
}
fn drain(mut reader: impl Read + Send + 'static, max: usize) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let take = n.min(max.saturating_sub(kept.len()));
            kept.extend_from_slice(&buf[..take]);
        }
        kept
    })
}

/// Classify bounded Claude stderr into a fixed message; raw diagnostics are never shown.
fn diagnose(stderr: &[u8]) -> String {
    match crate::worker::worker_failure(stderr) {
        crate::worker::UNKNOWN_FAILURE => "Claude stopped without success. Check its login, selected model and usage limits; credentials remain read-only.".into(),
        known => known.replace("Codex", "Claude"),
    }
}
/// A credential-free shell is the only workspace execution capability Claude receives.
/// The caller supplies persisted attempt inputs, never Docker options from the model.
pub fn shell(
    image: &str,
    name: &str,
    node: &str,
    workspace: &Path,
    command: &str,
) -> io::Result<Value> {
    if crate::shutdown::requested() { return Err(fail("Control is stopping; workspace command was not started.")); }
    if command.is_empty() || command.len() > 16000 {
        return Err(fail("Command must contain 1–16000 bytes"));
    }
    let meta = fs::symlink_metadata(workspace)?;
    if !meta.is_dir() {
        return Err(fail("Workspace is not a directory"));
    }
    let mut child = base(name, node, meta.uid(), meta.gid());
    child
        .args([
            "--network=none",
            "--mount",
            &mount(workspace, "/workspace", false)?,
            image,
            "bash",
            "-c",
            command,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn()?;
    let stdout = drain(child.stdout.take().unwrap(), 12000);
    let stderr = drain(child.stderr.take().unwrap(), 4000);
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if !crate::shutdown::requested() && start.elapsed() < Duration::from_secs(25) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                crate::lifecycle::stop_resource(name, node, image, workspace).map_err(fail)?;
                break Err(fail(
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

pub fn definition() -> Value {
    json!({"name":"control_workspace_run","description":"Run a shell command in this task's offline Nix workspace. Use rg/fd, read and edit project files, and run Rust tools. No credentials, Control host files or network. 25 second deadline; bounded output. Commands may change task files.","inputSchema":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":16000}},"required":["command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}})
}

pub fn claim(workspace: &Path, nonce: &str) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let root = workspace
        .parent()
        .ok_or_else(|| fail("Invalid workspace"))?;
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

pub fn probe(image: &str, node: &str, root: &Path) -> io::Result<()> {
    probe_named(
        image,
        node,
        root,
        &format!("control-claude-probe-{}", std::process::id()),
    )
}
fn probe_named(image: &str, node: &str, root: &Path, name: &str) -> io::Result<()> {
    let directory = root.join("claude-isolation-probe");
    fs::create_dir_all(&directory)?;
    let command = "set -eu; test ! -e /home/agent/.claude/.credentials.json; test ! -S /var/run/docker.sock; test ! -e /proc/1/root/home/agent/.claude/.credentials.json; test -z \"${ANTHROPIC_API_KEY-}${CLAUDE_CODE_OAUTH_TOKEN-}\"; printf verified > isolation-check";
    let output = shell(image, name, node, &directory, command)?;
    if output["exit_code"] != 0 || fs::read(directory.join("isolation-check"))? != b"verified" {
        return Err(fail(
            "Claude workspace isolation check failed; no credentials were mounted",
        ));
    }
    Ok(())
}

#[derive(Default)]
pub struct Stream {
    pub tokens: Option<Tokens>,
    pub result: Option<String>,
    invalid: bool,
    initialized: bool,
    /// Set as soon as the init event lists a tool outside the allow-list.
    bad_tools: bool,
    /// Subtype and error text of an unsuccessful result event, for the user.
    failure: Option<String>,
}
impl Stream {
    fn event(&mut self, value: &Value) -> Option<Activity> {
        match value["type"].as_str()? {
            "system" if value["subtype"] == "init" => {
                self.initialized = true;
                let allowed = [
                    "mcp__control__control_workspace_run",
                    "mcp__control__control_web_search",
                    "mcp__control__control_rust_run",
                    "mcp__control__control_room_read",
                    "mcp__control__control_room_post",
                    "mcp__control__control_design_ask",
                    "mcp__control__control_project_request",
                    "mcp__control__control_preview_start",
                    "mcp__control__control_preview_stop",
                    "EndConversation",
                ];
                if !value["tools"].as_array().is_some_and(|tools| {
                    tools
                        .iter()
                        .all(|t| t.as_str().is_some_and(|s| allowed.contains(&s)))
                }) {
                    self.invalid = true;
                    self.bad_tools = true;
                }
            }
            "result" => {
                if value["is_error"] != false || value["subtype"] != "success" {
                    let detail: String = value["result"].as_str().or(value["error"].as_str()).unwrap_or("").chars().filter(|c| !c.is_control()).take(300).collect();
                    self.failure = Some(format!("Claude ended with {}{}{}", value["subtype"].as_str().unwrap_or("an error").chars().take(40).collect::<String>(), if detail.is_empty() { "" } else { ": " }, detail));
                }
                if self.result.is_some()
                    || value["is_error"] != false
                    || value["subtype"] != "success"
                    || !self.initialized
                {
                    self.invalid = true;
                    return None;
                }
                let Some(text) = value["result"].as_str() else {
                    self.invalid = true;
                    return None;
                };
                self.result = Some(text.chars().take(16000).collect());
                let usage = &value["usage"];
                if let (Some(input), Some(output)) = (
                    usage["input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                ) {
                    let cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    let created = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    self.tokens = Some(Tokens {
                        input_tokens: input.saturating_add(cached).saturating_add(created),
                        cached_input_tokens: cached,
                        output_tokens: output,
                    });
                }
            }
            "assistant" => {
                let text = value["message"]["content"]
                    .as_array()?
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    return Some(Activity {
                        id: "claude-response".into(),
                        kind: "message".into(),
                        text: text.chars().take(4000).collect(),
                        output: String::new(),
                        status: "running".into(),
                    });
                }
            }
            _ => {}
        }
        None
    }
    fn finish(self) -> io::Result<(String, Option<Tokens>)> {
        if let Some(failure) = &self.failure {
            return Err(fail(&format!("{failure}.")));
        }
        if self.bad_tools {
            return Err(fail("Claude started with an unexpected tool configuration and was stopped before doing work."));
        }
        if self.invalid {
            return Err(fail(
                "Claude returned an invalid result or unexpected tool configuration",
            ));
        }
        self.result.map(|text| (text, self.tokens)).ok_or_else(|| {
            fail("Claude did not return a successful final result; check login or usage limits")
        })
    }
}
#[cfg(test)]
fn capture(reader: impl Read, update: impl FnMut(Activity)) -> io::Result<(String, Option<Tokens>)> {
    capture_with(reader, update, &std::sync::atomic::AtomicBool::new(false))
}
fn capture_with(
    mut reader: impl Read,
    mut update: impl FnMut(Activity),
    bad_tools: &std::sync::atomic::AtomicBool,
) -> io::Result<(String, Option<Tokens>)> {
    let mut stream = Stream::default();
    let mut line = Vec::new();
    let mut oversized = false;
    let mut bytes = [0; 8192];
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        for &b in &bytes[..n] {
            if b == b'\n' {
                if oversized {
                    stream.invalid = true;
                } else if !line.is_empty() {
                    match serde_json::from_slice(&line) {
                        Ok(value) => {
                            if let Some(event) = stream.event(&value) {
                                update(event);
                            }
                            if stream.bad_tools {
                                bad_tools.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Err(_) => stream.invalid = true,
                    }
                }
                line.clear();
                oversized = false;
            } else if line.len() < 1_048_576 && !oversized {
                line.push(b);
            } else {
                oversized = true;
                line.clear();
            }
        }
    }
    if !line.is_empty() || oversized {
        stream.invalid = true;
    }
    stream.finish()
}

pub fn execute(
    id: &str,
    objective: &str,
    attempt: &mut Attempt,
    bridge: Option<(&Arc<Mutex<State>>, &str)>,
    workspace: &Path,
    image: &str,
    node: &str,
) -> Outcome {
    let blocked = |message: &str| ("blocked", message.into());
    let failed = |message: &str| ("failed", message.into());
    let auth = credential().map_err(|_| blocked("Claude login unavailable. Run control claude login; host keychain credentials are never extracted."))?;
    let root = workspace
        .parent()
        .ok_or_else(|| blocked("Invalid attempt workspace"))?;
    probe_named(image, node, root, &format!("control-{id}-probe")).map_err(|_| {
        blocked("Claude tool isolation probe failed. No account credentials were mounted.")
    })?;
    let meta = fs::symlink_metadata(&auth)
        .map_err(|_| blocked("Claude credential metadata unavailable"))?;
    let mailbox = root.join("claude-mailbox");
    fs::create_dir_all(&mailbox).map_err(|_| failed("Cannot prepare Claude mailbox"))?;
    let name = format!("control-{id}");
    let mut command = base(&name, node, meta.uid(), meta.gid());
    command.args(["--interactive", "--tmpfs", &format!("/home/agent/.claude:rw,nosuid,uid={},gid={},size=32m", meta.uid(), meta.gid()), "--mount", &mount(&auth, "/home/agent/.claude/.credentials.json", true).map_err(|_| blocked("Invalid Claude credential path"))?,
        "--mount", &mount(&mailbox, "/workspace", false).map_err(|_| blocked("Invalid Claude mailbox path"))?, image,
        "claude", "--print", "--output-format", "stream-json", "--verbose", "--no-session-persistence",
        "--restricted", "--tools", "", "--disable-slash-commands", "--setting-sources", "", "--strict-mcp-config",
        "--mcp-config", "{\"mcpServers\":{\"control\":{\"command\":\"/bin/control-room-mcp\"}}}",
        "--permission-mode", "dontAsk", "--allowedTools", "mcp__control__control_workspace_run,mcp__control__control_web_search,mcp__control__control_rust_run,mcp__control__control_room_read,mcp__control__control_room_post,mcp__control__control_design_ask,mcp__control__control_project_request,mcp__control__control_preview_start,mcp__control__control_preview_stop",
        "--model", &attempt.model])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if crate::lifecycle::cancelled(bridge) {
        return Err(("cancelled", "Cancelled before Claude launch.".into()));
    }
    let mut child = command
        .spawn()
        .map_err(|_| failed("Cannot start the Claude Nix container"))?;
    let diagnostics = drain(child.stderr.take().unwrap(), 64000);
    let bad_tools = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&bad_tools);
    let stdout = child.stdout.take().unwrap();
    let live = bridge.map(|(s, r)| (Arc::clone(s), r.to_owned()));
    let generation = id.to_owned();
    let output = std::thread::spawn(move || {
        capture_with(stdout, |event| {
            if let Some((s, r)) = &live {
                if let Ok(mut s) = s.lock() {
                    crate::worker::record_activity_for(&mut s, r, &generation, event);
                }
            }
        }, &flag)
    });
    let environment = crate::worker::ENVIRONMENT_GUIDANCE;
    let prompt = format!("You are working on a task in Control. Your only project access is control_workspace_run: it runs commands in a separate offline Nix container at /workspace, with Rust, rg and fd. Use it to read/write task files. {environment} You cannot read the harness filesystem or credentials. Use Rust/WASM for application code. Work freely within the sandbox for routine edits, builds and tests. Use control_design_ask for consequential architecture, scope or behavior decisions requiring user direction: offer concrete options and a recommendation, then stop work. This is not permission escalation. Keep replies direct; omit empty report sections. You may use public search without private text. Control node policy determines authority; tools cannot administer it. Objective:\n{objective}");
    let sent = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(prompt.as_bytes()).is_ok());
    let (start, deadline, mut timed_out) = (Instant::now(), crate::config::worker_deadline(), false);
    let success = loop {
        if crate::design::requested(bridge) { break false; }
        if crate::lifecycle::cancelled(bridge) {
            break false;
        }
        // An unexpected init tool list is final: stop before the model can act.
        if !sent || bad_tools.load(std::sync::atomic::Ordering::Relaxed) {
            break false;
        }
        if start.elapsed() > deadline {
            timed_out = true;
            break false;
        }
        if let Some((s, r)) = bridge {
            if s.is_poisoned() {
                break false;
            }
            crate::worker_tools::pump_at(s, r, &mailbox, workspace);
        }
        if crate::design::requested(bridge) { break false; }
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Err(_) => break false,
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !success {
        let _ = child.kill();
        let _ = child.wait();
        crate::lifecycle::stop_attempt(attempt).map_err(|m| ("interrupted", m.to_owned()))?;
    }
    let result = output
        .join()
        .map_err(|_| failed("Claude stream reader stopped"))?;
    let stderr = diagnostics.join().unwrap_or_default();
    if !success {
        if crate::lifecycle::cancelled(bridge) {
            return Err((
                "cancelled",
                "Cancelled. The previous published workspace is unchanged.".into(),
            ));
        }
        if crate::design::requested(bridge) {
            return Err(("waiting", "Waiting for a design decision.".into()));
        }
        if timed_out {
            return Err(("failed", crate::worker::timeout_message(deadline)));
        }
        if let Err(e) = &result {
            if e.to_string().starts_with("Claude ended with") || bad_tools.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(failed(&e.to_string()));
            }
        }
        return Err(failed(&diagnose(&stderr)));
    }
    let (text, tokens) = result.map_err(|e| failed(&format!("{e} The previous workspace checkpoint is unchanged.")))?;
    attempt.tokens = tokens;
    Ok(text)
}

pub fn command(args: &[String]) -> io::Result<()> {
    // Usage must work before the Claude image is built.
    if !matches!(args, [a] if a == "login" || a == "check") {
        if matches!(args, [a] if a == "--help" || a == "-h" || a == "help") {
            println!("control claude login: sign in inside the pinned Nix Claude container\ncontrol claude check: offline tool isolation check; no credentials or model calls");
            return Ok(());
        }
        return Err(fail("Use control claude login | check"));
    }
    let image = Backend::Claude.image().ok_or_else(|| fail("Build/load the Nix claude-image and save its digest in ~/.config/control/claude-image-id"))?;
    match args.first().map(String::as_str) {
        Some("login") if args.len() == 1 => {
            use std::os::unix::fs::DirBuilderExt;
            let directory = auth_directory()?;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)?;
            let meta = fs::symlink_metadata(&directory)?;
            if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
                return Err(fail(
                    "Claude login directory must be private and owned by a non-root user",
                ));
            }
            let mut command = base(
                &format!("control-claude-login-{}", std::process::id()),
                &crate::config::node_id().map_err(fail)?,
                meta.uid(),
                meta.gid(),
            );
            command.args([
                "--interactive",
                "--tty",
                "--tmpfs",
                "/workspace:rw,nosuid,size=1m",
                "--mount",
                &mount(&directory, "/home/agent/.claude", false)?,
                &image,
                "claude",
                "auth",
                "login",
            ]);
            if !command.status()?.success() {
                return Err(fail("Claude login did not complete"));
            }
            fs::set_permissions(
                directory.join(".credentials.json"),
                fs::Permissions::from_mode(0o600),
            )?;
            Ok(())
        }
        Some("check") if args.len() == 1 => {
            let root = crate::config::state("claude-check").map_err(fail)?;
            probe(&image, &crate::config::node_id().map_err(fail)?, &root)?;
            println!("Claude offline workspace isolation verified. No account credentials were mounted or model called.");
            Ok(())
        }
        _ => Err(fail("Use control claude login | check")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_admission_survives_reload_without_replaying_an_effect() {
        let root =
            std::env::temp_dir().join(format!("control-claude-intent-{}", std::process::id()));
        fs::create_dir_all(root.join("workspace")).unwrap();
        let workspace = root.join("workspace");
        claim(&workspace, "request-1").unwrap();
        fs::write(workspace.join("effect.txt"), b"first execution").unwrap();
        // A new caller after an ambiguous acknowledgement still finds durable intent.
        assert!(claim(&workspace, "request-1").is_err());
        assert_eq!(
            fs::read(workspace.join("effect.txt")).unwrap(),
            b"first execution"
        );
        claim(&workspace, "request-2").unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires pinned Claude image and local engine; subprocess signal test, no credentials or model calls"]
    fn live_shutdown_stops_offline_workspace_command() {
        const CHILD: &str = "CONTROL_QA_SHELL_SIGNAL_CHILD";
        if let Ok(root) = std::env::var(CHILD) {
            let root = Path::new(&root);
            let name = std::env::var("CONTROL_QA_SHELL_SIGNAL_NAME").unwrap();
            crate::shutdown::install().unwrap();
            let result = shell(&Backend::Claude.image().unwrap(), &name, "shutdown-fixture", root,
                "printf ready > ready; sleep 90");
            assert!(crate::shutdown::requested());
            assert!(result.is_err());
            return;
        }
        let image = Backend::Claude.image().expect("pinned Claude image");
        let name = format!("control-shell-shutdown-{}", std::process::id());
        let root = std::env::temp_dir().join(&name);
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        struct Probe {
            child: std::process::Child,
            name: String,
            image: String,
            root: std::path::PathBuf,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                let _ = crate::lifecycle::stop_resource(&self.name, "shutdown-fixture", &self.image, &self.root);
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["claude::tests::live_shutdown_stops_offline_workspace_command", "--ignored", "--exact"])
            .env(CHILD, &root).env("CONTROL_QA_SHELL_SIGNAL_NAME", &name)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let mut probe = Probe { child, name, image, root };
        let start = Instant::now();
        while !probe.root.join("ready").is_file() {
            assert!(probe.child.try_wait().unwrap().is_none(), "probe exited before its command started");
            assert!(start.elapsed() < Duration::from_secs(15), "offline command did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(unsafe { libc::kill(probe.child.id() as libc::pid_t, libc::SIGTERM) }, 0);
        let start = Instant::now();
        loop {
            if let Some(status) = probe.child.try_wait().unwrap() {
                assert!(status.success(), "offline shutdown check failed");
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "offline command did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        let names = crate::runtime::docker_query(&["ps", "--all", "--format", "{{.Names}}", "--filter", &format!("name={}", probe.name)]).unwrap();
        assert!(names.trim().is_empty());
    }
    #[test]
    #[ignore = "requires the pinned Claude image and a Linux container engine; no credentials or model calls"]
    fn live_offline_workspace_isolated_and_persistent_across_processes() {
        let image = Backend::Claude.image().expect("Configure claude-image-id");
        let root =
            std::env::temp_dir().join(format!("control-claude-offline-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        probe(&image, "fixture-node", &root).unwrap();
        let workspace = root.join("project");
        fs::create_dir(&workspace).unwrap();
        let first = shell(
            &image,
            "control-claude-offline-first",
            "fixture-node",
            &workspace,
            "printf 1 > counter.txt; command -v rustc; command -v rg; command -v fd",
        )
        .unwrap();
        assert_eq!(first["exit_code"], 0);
        let next = shell(&image,"control-claude-offline-next","fixture-node",&workspace,"set -eu; test \"$(cat counter.txt)\" = 1; printf 2 > counter.txt; test ! -e /home/agent/.claude/.credentials.json; test ! -e /var/run/docker.sock; test -z \"${ANTHROPIC_API_KEY-}\"; test -z \"${CLAUDE_CODE_OAUTH_TOKEN-}\"; head -c 40000 /dev/zero | tr '\\0' x").unwrap();
        assert_eq!(next["exit_code"], 0);
        assert_eq!(next["stdout"].as_str().unwrap().len(), 12000);
        assert_eq!(fs::read(workspace.join("counter.txt")).unwrap(), b"2");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn failures_name_the_result_subtype_and_bad_tools_abort_early() {
        let init = "{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"mcp__control__control_workspace_run\"]}\n";
        let failed = format!("{init}{{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"result\":\"Reached max turns\"}}\n");
        let error = capture(failed.as_bytes(), |_| {}).unwrap_err().to_string();
        assert!(error.contains("error_max_turns") && error.contains("Reached max turns"));
        let flag = std::sync::atomic::AtomicBool::new(false);
        let bad = init.replace("mcp__control__control_workspace_run", "Bash");
        assert!(capture_with(bad.as_bytes(), |_| {}, &flag).unwrap_err().to_string().contains("tool configuration"));
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        assert!(diagnose(b"429 rate limit reached").contains("rate limit"));
        assert!(diagnose(b"401 unauthorized").contains("Claude account"));
        assert!(diagnose(b"secret-ish noise").starts_with("Claude stopped"));
    }
    #[test]
    fn stream_requires_valid_init_and_final_and_normalizes_caches() {
        let bytes = concat!("{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"mcp__control__control_workspace_run\"]}\n", "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":5}}\n");
        let (text, tokens) = capture(bytes.as_bytes(), |_| {}).unwrap();
        assert_eq!(text, "done");
        assert_eq!(
            tokens.unwrap(),
            Tokens {
                input_tokens: 45,
                cached_input_tokens: 30,
                output_tokens: 2
            }
        );
        assert!(capture(
            bytes
                .replace("mcp__control__control_workspace_run", "Bash")
                .as_bytes(),
            |_| {}
        )
        .is_err());
        assert!(capture(
            bytes
                .replace("\"is_error\":false", "\"is_error\":true")
                .as_bytes(),
            |_| {}
        )
        .is_err());
        assert!(capture(b"not json\n".as_slice(), |_| {}).is_err());
        assert!(capture(bytes.trim_end().as_bytes(), |_| {}).is_err());
    }
}
