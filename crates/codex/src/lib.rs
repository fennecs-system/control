//! The Codex CLI backend: its container, the offline sandbox probe, the event
//! stream and one agent turn. Codex runs its own sandboxed shell, so its turn
//! container mounts the workspace itself.
use control_agent::{
    Agent, Cancelled, Cause, Checked, Control, Failure, Granted, Host, ImagePin, Login, Node, Outcome, Runs,
    SANDBOX_PROBE_WAIT, Stop, TextCall, Turn, Turns, WORKER_IMAGE, cli::handoff_grace_over, mailbox_wake::MailboxWake,
};
use control_effect::sync::Recv;
use control_isolation::{Access, Input, Isolation, Limits, Network, Proc, RunSpec, Size, Stream, Streams, Tmpfs, User};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub struct Codex;
impl Agent for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn label(&self) -> &'static str {
        "OpenAI / Codex"
    }
    fn name(&self) -> &'static str {
        "Codex"
    }
    fn default_model(&self) -> &'static str {
        "gpt-6.1-sol"
    }
    fn model_variable(&self) -> Option<&'static str> {
        Some("CONTROL_CODEX_MODEL")
    }
    fn image(&self) -> ImagePin {
        WORKER_IMAGE
    }
    fn image_hint(&self) -> &'static str {
        "set worker-image-id in the configuration directory to the Nix image digest"
    }
    fn login(&self) -> Option<Login> {
        Some(Login {
            checked: Checked::BeforeWorkspace,
            hint: None,
        })
    }
    fn probe(&self, node: &dyn Node, isolation: &dyn Isolation, image: &str) -> Result<(), String> {
        offline_probe(node, isolation, image)
    }
    fn text_run(&self, node: &dyn Node, isolation: &dyn Isolation, call: &TextCall<'_>) -> std::io::Result<RunSpec> {
        text_run(node, isolation, call).map_err(std::io::Error::other)
    }
    fn text(
        &self,
        reader: &mut dyn std::io::Read,
        max: usize,
        emit: &mut dyn FnMut(control_ai::Event),
    ) -> std::io::Result<control_ai::Output> {
        control_ai::json_lines(reader, control_ai::codex::Text::new(max), emit)
    }
    fn runs(&self) -> Runs<'_> {
        Runs::Cli(self)
    }
    fn private_names(&self) -> &'static [&'static str] {
        &[".codex", "auth.json"]
    }
    fn review_mount(&self) -> bool {
        true
    }
    fn own_web_search(&self) -> bool {
        true
    }
}

pub const PERMISSIONS: [&str; 6] = [
    "default_permissions=\"control\"",
    "permissions.control.extends=\":workspace\"",
    "permissions.control.filesystem={\"/home/agent/.codex\"=\"deny\",\"/sys\"=\"deny\"}",
    "permissions.control.network.enabled=false",
    "approval_policy=\"never\"",
    "cli_auth_credentials_store=\"file\"",
];
/// The embedded seccomp profile every Codex container runs under.
const SECCOMP: &[u8] = include_bytes!("../../../hosting/worker-seccomp.json");
/// The fixed message when nothing in Codex's diagnostics is recognised.
pub const UNKNOWN_FAILURE: &str = "Codex container exited unsuccessfully. Check host account authentication and the pinned worker configuration. No host fallback was attempted.";

/// The credential file a container mounts, its owner and whether the CLI may
/// write it back: the granted login, or an empty canary for a probe.
pub struct Credentials {
    pub path: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub write: control_agent::Write,
}
impl From<&Granted> for Credentials {
    fn from(granted: &Granted) -> Self {
        let User { uid, gid } = granted.owner();
        Self {
            path: granted.path().to_path_buf(),
            uid,
            gid,
            write: granted.write(),
        }
    }
}
/// Where the sign-in is in the container.
pub const AUTH: &str = "/home/agent/.codex/auth.json";

/// The pre-run sandbox probe for `control doctor`: an empty canary stands in for
/// auth.json, so no account is mounted and no model is called.
pub fn offline_probe(node: &dyn Node, isolation: &dyn Isolation, image: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let node_id = node.id()?;
    let root = node.check_root("codex-check")?;
    fs::create_dir_all(&root).map_err(|_| "Cannot prepare sandbox verification.")?;
    let root = fs::canonicalize(&root).map_err(|_| "Cannot prepare sandbox verification.")?;
    let _ = fs::remove_dir_all(root.join("sandbox-probe"));
    let owner = fs::metadata(&root).map_err(|_| "Cannot prepare sandbox verification.")?;
    let policy = root.join("worker-seccomp.json");
    fs::write(&policy, SECCOMP).map_err(|_| "Could not prepare embedded worker security profile.")?;
    let canary = Credentials {
        path: root.join("unused"),
        uid: owner.uid(),
        gid: owner.gid(),
        write: control_agent::Write::ReadOnly,
    };
    let name = format!("control-auth-check-doctor-{}-probe", std::process::id());
    sandbox_probe(node, isolation, image, &root, &canary, &name, &policy, &node_id)
}

pub fn mount_path(path: &Path) -> Result<&str, &'static str> {
    path.to_str()
        .filter(|p| path.is_absolute() && !p.contains([',', '\n', '\r']))
        .ok_or("Container mount paths must be absolute UTF-8 paths without commas or newlines.")
}

pub fn container_spec(
    image: &str,
    name: &str,
    workspace: &Path,
    auth: &Credentials,
    policy: &Path,
    node: &str,
) -> Result<RunSpec, &'static str> {
    container_spec_scoped(image, name, workspace, auth, policy, node, None)
}
pub fn container_spec_scoped(
    image: &str,
    name: &str,
    workspace: &Path,
    auth: &Credentials,
    policy: &Path,
    node: &str,
    review: Option<&Path>,
) -> Result<RunSpec, &'static str> {
    mount_path(workspace)?;
    mount_path(&auth.path)?;
    let seccomp = fs::canonicalize(policy).map_err(|_| "Worker seccomp profile is missing.")?;
    mount_path(&seccomp)?;
    review.map(mount_path).transpose()?;
    let owner = User {
        uid: auth.uid,
        gid: auth.gid,
    };
    let spec = RunSpec {
        name: name.into(),
        image: image.into(),
        user: Some(owner),
        workdir: Some("/workspace".into()),
        input: Input::Attached,
        seccomp: Some(seccomp),
        // Docker's masked proc entries prevent unprivileged bubblewrap from mounting proc.
        // Credential access is denied by the inner sandbox; no outer capabilities are granted.
        proc: Proc::Unmasked,
        limits: Limits {
            pids: Some(256),
            memory: Some(Size::Gib(2)),
            cpus: Some(2),
            ..Limits::default()
        },
        ..RunSpec::default()
    }
    .label("dev.control.node", node)
    .label("dev.control.attempt", name);
    let spec = control_agent::cli::label_purpose(
        spec,
        name.starts_with("control-auth-check-") || name.starts_with("control-summary-"),
    )
    .tmpfs(Tmpfs::new("/tmp", Size::Mib(512)))
    // Private like every CLI's home: an engine that presents a mounted file
    // as owned by whoever reads it leaves only the home's mode to keep other
    // uids from the sign-in.
    .tmpfs(Tmpfs::new("/home/agent", Size::Mib(64)).private(owner))
    .tmpfs(Tmpfs::new("/home/agent/.codex", Size::Mib(64)).owned(owner))
    .bind(workspace, "/workspace", Access::ReadWrite)
    // Codex writes a refreshed sign-in back in place (`FileAuthStorage::save`
    // truncates and rewrites `auth.json`: no temporary file, rename or lock;
    // codex-rs/login/src/auth/storage.rs at rust-v0.164.0), so the file alone
    // is mounted, writable only when the host made the login refreshable.
    .secret(
        "codex/auth.json",
        &auth.path,
        AUTH,
        match auth.write {
            control_agent::Write::Refreshable => Access::ReadWrite,
            control_agent::Write::ReadOnly => Access::ReadOnly,
        },
    )
    .env("HOME", "/home/agent")
    .env("CODEX_HOME", "/home/agent/.codex");
    let spec = match review {
        Some(source) => spec.bind(source, "/review", Access::ReadOnly),
        None => spec,
    };
    control_agent::cli::secrets_private(&spec)
        .map_err(|_| "Codex would mount its sign-in outside its private home.")?;
    Ok(spec)
}

pub fn permissions(spec: RunSpec) -> RunSpec {
    spec.args(PERMISSIONS.iter().flat_map(|setting| ["-c", setting]))
}

/// A bounded text request reuses the worker's credential isolation, without
/// workspace tools, MCP, project files or resumable provider state.
fn text_run(node: &dyn Node, isolation: &dyn Isolation, call: &TextCall<'_>) -> Result<RunSpec, String> {
    let auth = Credentials::from(call.granted().map_err(|e| e.to_string())?);
    let root = call.workspace.parent().ok_or("Invalid summary workspace")?;
    let policy = root.join("worker-seccomp.json");
    fs::write(&policy, SECCOMP).map_err(|_| "Cannot prepare text sandbox")?;
    sandbox_probe(
        node,
        isolation,
        call.image,
        root,
        &auth,
        &format!("control-auth-check-{}", call.name),
        &policy,
        call.node,
    )?;
    Ok(text_container(
        call.image,
        call.name,
        call.workspace,
        &auth,
        &policy,
        call.node,
        call.model,
    )?)
}
fn text_container(
    image: &str,
    name: &str,
    workspace: &Path,
    auth: &Credentials,
    policy: &Path,
    node: &str,
    model: &str,
) -> Result<RunSpec, &'static str> {
    let spec = permissions(container_spec(image, name, workspace, auth, policy, node)?.args([
        "codex",
        "exec",
        "--ignore-user-config",
        "--ignore-rules",
        "--ephemeral",
        "--skip-git-repo-check",
        "--json",
        "--model",
        model,
    ]));
    Ok(spec
        .args(
            [
                "shell_tool",
                "apps",
                "multi_agent",
                "browser_use",
                "computer_use",
                "code_mode",
                "js_repl",
                "view_image",
                "sleep_tool",
                "skill_search",
                "skill_mcp_dependency_install",
                "workspace_dependencies",
            ]
            .iter()
            .flat_map(|feature| ["--disable", feature]),
        )
        .args([
            "-c",
            "web_search=\"disabled\"",
            "-c",
            "agents.enabled=false",
            "-c",
            "project_doc_max_bytes=0",
            "-",
        ]))
}

#[allow(clippy::too_many_arguments)]
pub fn sandbox_probe(
    node: &dyn Node,
    isolation: &dyn Isolation,
    image: &str,
    root: &Path,
    auth: &Credentials,
    name: &str,
    policy: &Path,
    node_id: &str,
) -> Result<(), String> {
    let probe = root.join("sandbox-probe");
    let workspace = probe.join("workspace");
    fs::create_dir_all(&workspace).map_err(|_| "Cannot prepare sandbox verification.")?;
    let canary = probe.join("auth.json");
    fs::write(&canary, b"{}").map_err(|_| "Cannot create sandbox canary.")?;
    let fake = Credentials {
        path: canary,
        uid: auth.uid,
        gid: auth.gid,
        write: control_agent::Write::ReadOnly,
    };
    // PID 1 must be the inner execution supervisor, not the account-bearing harness.
    // Its memory is naturally readable by its own child; the outer harness is outside this PID namespace.
    // Each probe writes a fresh result name: Docker Desktop's shared-file cache keeps a
    // stale entry for a name an earlier container created and the host then deleted,
    // so recreating that name failed with ENOENT on alternate probes.
    let result = format!("probe-result-{}", probe_nonce()?);
    let spec = probe_container(image, name, &workspace, &fake, policy, node_id, &result)?;
    let mut child = isolation
        .spawn(&spec, Streams::new(Stream::Null, Stream::Null, Stream::Piped))
        .map_err(|_| "Cannot start sandbox verification.")?;
    let stderr = tail_reader(child.stderr.take());
    let child = control_agent::child::watch(child).map_err(|_| "Cannot observe sandbox verification.")?;
    let outcome = child.wait(Some(Instant::now() + SANDBOX_PROBE_WAIT), &node.stop_token());
    if !matches!(outcome, Recv::Item(Ok(_))) {
        if !matches!(outcome, Recv::Item(_)) {
            child.kill();
        }
        node.stop_container(isolation, name, node_id, image, &workspace)?;
    }
    let status = match &outcome {
        Recv::Item(Ok(status)) => Some(*status),
        _ => None,
    };
    let written = fs::read(workspace.join(&result)).ok();
    let _ = fs::remove_file(workspace.join(&result));
    sandbox_verdict(outcome, || written.as_deref() == Some(b"verified")).map_err(|reason| {
        if reason == SANDBOX_PROBE_TIMED_OUT {
            return reason.to_owned();
        }
        let stderr = stderr.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        format!("{reason} {}", probe_detail(status, written.as_deref(), &stderr))
    })
}
fn probe_container(
    image: &str,
    name: &str,
    workspace: &Path,
    fake: &Credentials,
    policy: &Path,
    node: &str,
    result: &str,
) -> Result<RunSpec, &'static str> {
    Ok(permissions(container_spec(image, name, workspace, fake, policy, node)?.args(["codex", "sandbox", "-P", "control"])).args(["--", "bash", "-c", "set -eu; for path in /home/agent/.codex/auth.json /proc/self/root/home/agent/.codex/auth.json /proc/1/root/home/agent/.codex/auth.json; do if cat \"$path\" >/dev/null 2>&1; then exit 41; fi; done; IFS= read -r -d '' supervisor < /proc/1/cmdline; test \"$supervisor\" = codex-linux-sandbox || exit 42; printf verified > \"/workspace/$1\"", "probe", result]))
}
pub fn probe_nonce() -> Result<String, &'static str> {
    let mut nonce = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
        .map_err(|_| "Cannot prepare sandbox verification.")?;
    Ok(nonce.iter().map(|b| format!("{b:02x}")).collect())
}
/// The last bytes a probe wrote to stderr, delivered once the stream closes.
pub fn tail_reader(stream: Option<impl Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
    const KEEP: usize = 4096;
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Some(mut stream) = stream {
        std::thread::spawn(move || {
            let (mut kept, mut chunk) = (Vec::new(), [0u8; 4096]);
            while let Ok(length @ 1..) = stream.read(&mut chunk) {
                kept.extend_from_slice(&chunk[..length]);
                if kept.len() > KEEP {
                    kept.drain(..kept.len() - KEEP);
                }
            }
            let _ = sender.send(kept);
        });
    }
    receiver
}
/// Diagnostics for a failed probe: its exit, result file and stderr tail. Only
/// an empty canary is mounted, so the stream carries no account material.
pub fn probe_detail(status: Option<std::process::ExitStatus>, written: Option<&[u8]>, stderr: &[u8]) -> String {
    use std::os::unix::process::ExitStatusExt;
    const TAIL: usize = 400;
    let exit = match status.map(|s| (s.code(), s.signal())) {
        Some((Some(code), _)) => format!("exit code {code}"),
        Some((None, Some(signal))) => format!("killed by signal {signal}"),
        Some(_) => "unknown exit".to_owned(),
        None => "exit not observed".to_owned(),
    };
    let result = match written {
        None => "result file missing",
        Some(b"verified") => "result file verified",
        Some(_) => "unexpected result file",
    };
    let text = String::from_utf8_lossy(stderr);
    let words = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty());
    let text = words.collect::<Vec<_>>().join(" ");
    let skip = text.chars().count().saturating_sub(TAIL);
    let tail: String = text.chars().skip(skip).collect();
    let tail = match (tail.is_empty(), skip > 0) {
        (true, _) => "(empty)".to_owned(),
        (false, true) => format!("…{tail}"),
        (false, false) => tail,
    };
    format!("({exit}; {result}; stderr tail: {tail})")
}

pub const SANDBOX_PROBE_TIMED_OUT: &str = "Sandbox verification timed out after 90 s: the container engine is busy or slow, which is not an isolation failure. No account credentials were mounted; retry shortly.";
/// The probe's result: a timeout is reported apart from a failed isolation check.
pub fn sandbox_verdict(
    outcome: Recv<std::io::Result<std::process::ExitStatus>>,
    verified: impl FnOnce() -> bool,
) -> Result<(), &'static str> {
    const FAILED: &str = "Credential isolation verification failed. No account credentials were mounted. Check the pinned Codex image and container sandbox support.";
    let status = match outcome {
        Recv::Elapsed => return Err(SANDBOX_PROBE_TIMED_OUT),
        Recv::Item(Ok(status)) => status,
        _ => return Err(FAILED),
    };
    match status.code() {
        Some(41) => Err("Credential canary was readable through a sandbox path. No account credentials were mounted."),
        Some(42) => Err(
            "The sandbox did not isolate the account-bearing harness in a separate PID namespace. No account credentials were mounted.",
        ),
        _ if status.success() && verified() => Ok(()),
        _ => Err(FAILED),
    }
}

/// A recognised failure in Codex's bounded diagnostics, or its fixed message.
pub fn diagnose(bytes: &[u8]) -> Failure {
    failure(control_agent::classify(bytes))
}
fn failure(cause: Option<Cause>) -> Failure {
    cause
        .map(|cause| Failure::Provider { agent: "Codex", cause })
        .unwrap_or(Failure::Unclassified(UNKNOWN_FAILURE))
}

fn capture_diagnostics(mut reader: impl Read + Send + 'static) -> std::thread::JoinHandle<Option<Cause>> {
    std::thread::spawn(move || {
        let mut captured = Vec::new();
        let mut chunk = [0; 4096];
        while let Ok(length) = reader.read(&mut chunk) {
            if length == 0 {
                break;
            }
            let keep = length.min(64000usize.saturating_sub(captured.len()));
            captured.extend_from_slice(&chunk[..keep]);
        }
        control_agent::classify(&captured)
    })
}

/// Keeps the end of the provider's event stream so its own error events can
/// be classified: Codex reports a usage limit only there, not on stderr.
struct Tail<R> {
    inner: R,
    kept: Vec<u8>,
}
impl<R: Read> Read for &mut Tail<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let length = self.inner.read(buffer)?;
        self.kept.extend_from_slice(&buffer[..length]);
        if self.kept.len() > 32_768 {
            let cut = self.kept.len() - 16_384;
            self.kept.drain(..cut);
        }
        Ok(length)
    }
}
/// The provider's `error` and `turn.failed` events in `stream`, if they name
/// a known failure. Raw text is never returned.
pub fn reported_failure(stream: &[u8]) -> Option<Cause> {
    let errors = stream
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            [&b"\"type\":\"error\""[..], b"\"type\":\"turn.failed\""]
                .iter()
                .any(|kind| line.windows(kind.len()).any(|window| window == *kind))
        })
        .collect::<Vec<_>>()
        .join(&b'\n');
    Some(&errors)
        .filter(|errors| !errors.is_empty())
        .and_then(|errors| control_agent::classify(errors))
}
/// The usage, whether a Control tool call was still unfinished, and the
/// failure the stream itself reported.
pub fn capture_codex(
    reader: impl Read,
    mut update: impl FnMut(control_ai::Activity),
) -> (control_ai::Usage, bool, Option<Cause>) {
    let mut reader = Tail {
        inner: reader,
        kept: Vec::new(),
    };
    // Completion is adapter state, independent of the bounded UI activity tail.
    let mut pending = std::collections::HashSet::new();
    let usage = control_ai::json_lines_usage(&mut reader, control_ai::codex::Codex::default(), |event| {
        if let control_ai::Event::Activity(event) = event {
            if event.kind == "mcp_tool_call" {
                if matches!(event.status.as_str(), "in_progress" | "running") {
                    pending.insert(event.id.clone());
                } else {
                    pending.remove(&event.id);
                }
            }
            update(event);
        }
    })
    .1;
    (usage, !pending.is_empty(), reported_failure(&reader.kept))
}

/// A successful CLI exit is not a completed tool exchange. Check after the
/// stdout reader joins, so a buffered completion cannot be mistaken for loss.
/// No timeout or automatic retry: a host effect may already have happened.
pub fn finish_codex_tools(host_finished: bool, unfinished: bool) -> Result<(), Failure> {
    if host_finished || !unfinished {
        return Ok(());
    }
    Err(Failure::UnfinishedTool)
}

/// Reads the turn's bounded final report without following a link the agent made.
pub fn read_report(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    // The container has exited: reject agent-created symlinks before any host read.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("Worker report must be a regular file."));
    }
    let mut output = Vec::new();
    file.take(64001).read_to_end(&mut output)?;
    let truncated = output.len() > 64000;
    output.truncate(64000);
    match std::str::from_utf8(&output) {
        Ok(text) => Ok(text.to_owned()),
        Err(error) if truncated && error.error_len().is_none() => {
            output.truncate(error.valid_up_to());
            String::from_utf8(output).map_err(std::io::Error::other)
        }
        Err(error) => Err(std::io::Error::other(error)),
    }
}

impl Turns for Codex {
    fn turn(&self, turn: Turn<'_>, host: &mut dyn Host) -> Outcome {
        // A named account's folder keeps only its sign-in: what an earlier login
        // left beside it goes after the turn (best effort, as for every CLI).
        let folder = turn
            .login
            .as_ref()
            .ok()
            .filter(|login| login.write() == control_agent::Write::Refreshable)
            .and_then(|login| login.path().parent().map(Path::to_path_buf));
        let outcome = execute(turn, host);
        if let Some(folder) = folder {
            let _ = control_agent::sweep(&folder, FOLDER_KEEPS);
        }
        outcome
    }
}
/// What a Codex account folder keeps: `auth.json` alone, which holds a
/// sign-in or the key `codex login --with-api-key` wrote.
const FOLDER_KEEPS: &[control_agent::Keep] = &[control_agent::Keep::Name("auth.json")];
fn launched(result: Result<String, Failure>, usage: Option<control_ai::Usage>) -> Outcome {
    Outcome::Launched { result, usage }
}
fn execute(turn: Turn<'_>, host: &mut dyn Host) -> Outcome {
    let Turn {
        id,
        isolation,
        node,
        image,
        model,
        workspace: absolute,
        prompt,
        review_only,
        started_unix,
        tools,
        login,
    } = turn;
    let blocked = |m: &str| Failure::Blocked(m.to_owned());
    let failed = |m: &str| Failure::Failed(m.to_owned());
    let auth = match login {
        Ok(granted) => Credentials::from(&granted),
        Err(error) => return blocked(&error.to_string()).into(),
    };
    let root = absolute.parent().unwrap();
    let policy = root.join("worker-seccomp.json");
    if fs::write(&policy, SECCOMP).is_err() {
        return failed("Could not prepare embedded worker security profile.").into();
    }
    // Verify each attempt before mounting real credentials; successes are not cached.
    if let Err(reason) = sandbox_probe(
        host.node(),
        isolation,
        image,
        root,
        &auth,
        &format!("control-{id}-probe"),
        &policy,
        node,
    ) {
        return blocked(&reason).into();
    }
    let container = format!("control-{id}");
    // Codex transport and final report need writable scratch. Its review source
    // is a distinct immutable mount, never the writable mailbox at /workspace.
    let review_source = root.join("review-source");
    if review_only {
        if fs::rename(absolute, &review_source).is_err() {
            return failed("Could not isolate judge source.").into();
        }
        if fs::create_dir(absolute).is_err() {
            return failed("Could not prepare judge scratch.").into();
        }
    }
    let spec = match turn_container(
        image,
        &container,
        absolute,
        &auth,
        &policy,
        node,
        review_only.then_some(review_source.as_path()),
        model,
        tools,
    ) {
        Ok(spec) => spec,
        Err(message) => return blocked(message).into(),
    };
    if host.cancelled() {
        return Failure::Cancelled(Cancelled::BeforeLaunch(None)).into();
    }
    let Ok(mut child) = isolation.spawn(&spec, Streams::new(Stream::Piped, Stream::Piped, Stream::Piped)) else {
        return failed("Could not start the container worker.").into();
    };
    let diagnostics = capture_diagnostics(child.stderr.take().unwrap());
    let stdout = child.stdout.take().unwrap();
    let events = host.events();
    let acted = Arc::new(AtomicU64::new(0));
    let acting = Arc::clone(&acted);
    let accounting = std::thread::spawn(move || {
        capture_codex(stdout, |event| {
            if matches!(event.kind.as_str(), "command_execution" | "file_change") {
                acting.fetch_add(1, Ordering::Relaxed);
            }
            if event.kind == "file_change" {
                if !review_only && !["running", "in_progress"].contains(&event.status.as_str()) {
                    events.changed();
                }
                return;
            }
            if !review_only && event.kind == "command_execution" && event.status == "completed" {
                events.changed();
            }
            // Progress is disposable; lifecycle/result checkpoints persist the
            // accumulated activity. Never fsync the entire history per event.
            events.activity(event);
        })
    });
    let prompt = host.observed(prompt);
    if child
        .stdin
        .take()
        .is_none_or(|mut stdin| stdin.write_all(prompt.as_bytes()).is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        let failure = match host.stop(Stop::Admitted) {
            Err(message) => Failure::Interrupted(message),
            Ok(()) => failed("Could not deliver the objective."),
        };
        return launched(Err(failure), None);
    }
    let started = Instant::now();
    let admitted = control_agent::admitted(started_unix);
    let wake = MailboxWake::new(absolute);
    let mut timed_out = None;
    let mut handoff = None;
    // When the handoff was first seen accepted, and the shell/file actions started by then.
    let mut accepted: Option<(Instant, u64)> = None;
    let stopped = |host: &mut dyn Host| host.stop(Stop::Live).map_err(Failure::Interrupted);
    let result: Result<bool, ()> = loop {
        if host.design_requested() || host.cancelled() || host.unavailable() {
            break Err(());
        }
        host.pump(absolute);
        if host.design_requested() || host.cancelled() {
            break Err(());
        }
        match host.poll(started.elapsed() + Duration::from_secs(admitted)) {
            Control::Finish(summary) => {
                // Let the turn end itself and report its usage, unless it
                // keeps working (shell or file changes after its handoff) or
                // outlasts the grace.
                let acted_now = acted.load(Ordering::Relaxed);
                let (at, before) = *accepted.get_or_insert((Instant::now(), acted_now));
                handoff = Some(summary);
                if acted_now > before || handoff_grace_over(at, Instant::now()) {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Err(failure) = stopped(host) {
                        return launched(Err(failure), None);
                    }
                    break Ok(true);
                }
            }
            Control::TimedOut(limit) => {
                timed_out = Some(limit);
                break Err(());
            }
            Control::Continue => {}
        }
        match child.try_wait() {
            // An accepted handoff is the result however the turn then ended.
            Ok(Some(_)) if handoff.is_some() => {
                if let Err(failure) = stopped(host) {
                    return launched(Err(failure), None);
                }
                break Ok(true);
            }
            Ok(Some(status)) => break Ok(status.success()),
            Err(_) => break Err(()),
            _ => {}
        }
        wake.wait(Duration::from_millis(250));
    };
    // Killing the CLI alone may leave the daemon-owned container running.
    let Ok(success) = result else {
        let _ = child.kill();
        let _ = child.wait();
        if let Err(failure) = stopped(host) {
            return launched(Err(failure), None);
        }
        let usage = accounting.join().ok().map(|(usage, _, _)| usage);
        let failure = if host.cancelled() {
            Failure::Cancelled(Cancelled::Running)
        } else if host.design_requested() {
            Failure::Waiting
        } else if let Some(limit) = timed_out {
            Failure::TimedOut(limit)
        } else {
            failed("Worker stopped after its deadline or a process error.")
        };
        return launched(Err(failure), usage);
    };
    let Ok((usage, unfinished, reported)) = accounting.join() else {
        return launched(Err(failed("Could not read the provider's final tool results.")), None);
    };
    let usage = Some(usage);
    if !success {
        let stderr = diagnostics.join().unwrap_or(None);
        return launched(Err(failure(stderr.or(reported))), usage);
    }
    let _ = diagnostics.join();
    if let Err(failure) = finish_codex_tools(handoff.is_some(), unfinished) {
        return launched(Err(failure), usage);
    }
    let report = || read_report(&absolute.join(".control-result"));
    // A requested turn that ended itself replies with its full last message,
    // not the handoff's clipped one.
    let output = match handoff {
        Some(reply) if host.requested() => report().ok().filter(|text| !text.trim().is_empty()).unwrap_or(reply),
        handoff => handoff
            .or_else(|| report().ok())
            .unwrap_or_else(|| "Worker finished without a safe final report.".into()),
    };
    let summary: String = output.chars().take(16000).collect();
    launched(Ok(summary), usage)
}
/// One turn's container and Codex arguments; `tools` adds Control's room tools.
#[allow(clippy::too_many_arguments)]
fn turn_container(
    image: &str,
    container: &str,
    absolute: &Path,
    auth: &Credentials,
    policy: &Path,
    node: &str,
    review: Option<&Path>,
    model: &str,
    tools: bool,
) -> Result<RunSpec, &'static str> {
    let spec = container_spec_scoped(image, container, absolute, auth, policy, node, review)?.args([
        "codex",
        "exec",
        "--ignore-user-config",
        "--ephemeral",
        "--skip-git-repo-check",
        "--color",
        "never",
        "--json",
        "--model",
        model,
        "--output-last-message",
        "/workspace/.control-result",
    ]);
    let spec = permissions(spec);
    let names = control_context::worker_mcp_names();
    let rooms = tools.then(|| {
        [
            "mcp_servers.control_rooms.command=\"/bin/control-room-mcp\"".to_owned(),
            "mcp_servers.control_rooms.required=true".to_owned(),
            format!(
                "mcp_servers.control_rooms.enabled_tools={}",
                serde_json::to_string(&names).unwrap()
            ),
        ]
        .into_iter()
        .chain(
            names
                .iter()
                .map(|tool| format!("mcp_servers.control_rooms.tools.{tool}.approval_mode=\"approve\"")),
        )
        .collect::<Vec<_>>()
    });
    Ok(spec
        .args(
            rooms
                .into_iter()
                .flatten()
                .flat_map(|setting| ["-c".to_owned(), setting]),
        )
        .args(["-"]))
}

#[cfg(test)]
mod tests;

/// `control codex login | key | check` for the account the host chose:
/// `login` signs in with a device code and `key` with the API key the host
/// read, each inside the pinned worker image, keeping the sign-in as a file in
/// the account's directory; `check` runs the offline sandbox probe. The host
/// refuses `login` for an account whose login this directory cannot hold
/// (the operator's own `~/.codex`).
pub fn command(command: control_agent::Command<'_>) -> std::io::Result<()> {
    const USAGE: &str = "Use control codex login --account NAME | key --account NAME | check";
    let fail = |message: &str| std::io::Error::other(message.to_owned());
    let control_agent::Command {
        args,
        node,
        isolation,
        image,
        directory,
        key,
        ..
    } = command;
    let [action] = args else { return Err(fail(USAGE)) };
    let image = || {
        image
            .clone()
            .ok_or_else(|| fail("Build and pin the Codex worker image first: nix run .#setup"))
    };
    let sign_in = match (action.as_str(), key) {
        ("--help" | "-h" | "help", _) => {
            println!(
                "control codex login --account NAME: sign a named account in with a device code inside the pinned worker image\ncontrol codex key --account NAME: save an API-key account's key from standard input with Codex's own offline API-key login\ncontrol codex check: offline sandbox check, then a fake-credential canary in a turn container (the operator's own sign-in read-only, a named account's writable by the CLI alone, a key read-only); no real credentials or model calls"
            );
            return Ok(());
        }
        ("check", _) => {
            let image = image()?;
            offline_probe(node, isolation, &image).map_err(|e| fail(&e))?;
            println!("Codex offline sandbox verified. No account credentials were mounted or model called.");
            return canary(node, isolation, &image);
        }
        ("login", _) => SignIn::Device,
        ("key", Some(key)) => SignIn::Key(key),
        _ => return Err(fail(USAGE)),
    };
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory)?;
    let meta = fs::symlink_metadata(&directory)?;
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
        return Err(fail(
            "Codex login directory must be private and owned by a non-root user",
        ));
    }
    let spec = login_container(
        &format!("control-codex-login-{}", std::process::id()),
        &node.id().map_err(|e| fail(&e))?,
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
        &directory,
        &image()?,
        &sign_in,
    )?;
    let finished = match sign_in {
        SignIn::Device => isolation.spawn(&spec, Streams::INHERIT)?.wait()?,
        // The key goes to the CLI's standard input only; its own output is
        // discarded and it never echoes the key.
        SignIn::Key(key) => {
            let mut child = isolation.spawn(&spec, Streams::new(Stream::Piped, Stream::Null, Stream::Inherit))?;
            let written = child
                .stdin
                .take()
                .ok_or_else(|| fail("Codex key login has no standard input"))
                .and_then(|mut stdin| stdin.write_all(key.expose()));
            let status = child.wait()?;
            written?;
            status
        }
    };
    if !finished.success() {
        return Err(fail("Codex login did not complete"));
    }
    // Signing in again replaces auth.json in place; the logs and state the
    // login left in its home go, since turns mount auth.json alone.
    control_agent::sweep(&directory, FOLDER_KEEPS)?;
    fs::set_permissions(directory.join("auth.json"), fs::Permissions::from_mode(0o600))
}
/// The credential canary in a turn's own container, on a fake credential:
/// the operator's own sign-in must not be writable, a named account's must be
/// writable by the CLI alone, and a key is read-only.
fn canary(node: &dyn Node, isolation: &dyn Isolation, image: &str) -> std::io::Result<()> {
    use control_agent::cli::{Canary, canary_check};
    let node_id = node.id().map_err(std::io::Error::other)?;
    let root = fs::canonicalize(node.check_root("codex-check").map_err(std::io::Error::other)?)?;
    let policy = root.join("worker-seccomp.json");
    fs::write(&policy, SECCOMP)?;
    let workspace = root.join("canary-workspace");
    fs::create_dir_all(&workspace)?;
    let name = format!("control-auth-check-{}-canary", std::process::id());
    [
        Canary::ReadOnly(control_agent::Form::Session),
        Canary::Refreshable(control_agent::Keeps::InPlace),
        Canary::ReadOnly(control_agent::Form::ApiKey),
    ]
    .into_iter()
    .try_for_each(|login| {
        canary_check(isolation, &root, login, AUTH, |granted| {
            let auth = Credentials::from(granted);
            turn_container(
                image, &name, &workspace, &auth, &policy, &node_id, None, "canary", false,
            )
            .map_err(std::io::Error::other)
        })
    })
}
/// How `control codex` signs an account in.
enum SignIn<'a> {
    /// A device code the operator confirms in a browser.
    Device,
    /// `codex login --with-api-key`, which only writes the key into
    /// auth.json (verified offline: "API key saved. Authentication is
    /// checked when you send a request."), so it runs with no network.
    Key(&'a control_agent::ApiKey),
}
/// The login: Codex's home is the account directory, writable, and the
/// sign-in is kept as a file there (`cli_auth_credentials_store`), not in a
/// keyring the container does not have.
fn login_container(
    name: &str,
    node: &str,
    owner: User,
    directory: &Path,
    image: &str,
    sign_in: &SignIn<'_>,
) -> std::io::Result<RunSpec> {
    let (input, network, method) = match sign_in {
        SignIn::Device => (Input::Terminal, Network::Engine, "--device-auth"),
        SignIn::Key(_) => (Input::Attached, Network::None, "--with-api-key"),
    };
    Ok(RunSpec {
        image: image.into(),
        input,
        network,
        ..control_agent::cli::base(name, node, owner)
    }
    .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
    .secret(
        "codex/login",
        control_agent::cli::checked(directory)?,
        "/home/agent/.codex",
        Access::ReadWrite,
    )
    .env("CODEX_HOME", "/home/agent/.codex")
    .args(["codex", "login", method, "-c", "cli_auth_credentials_store=\"file\""]))
}
