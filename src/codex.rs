//! The Codex CLI backend: credential isolation, its container, the offline
//! sandbox probe, the event stream and one agent turn.
use crate::identity::Workspace;
use crate::worker::{handoff_grace_over, interrupt_unfinished_tools, observe_workspace_diff, read_report, record_activity_for, render_prompt, timeout_message, worker_failure, SANDBOX_PROBE_WAIT, UNKNOWN_FAILURE};
use crate::State;
use control_effect::sync::Recv;
use control_isolation::{Access, Input, Isolation, Limits, Proc, RunSpec, Size, Stream, Streams, Tmpfs, User};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) struct Codex;
impl crate::backend::Agent for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn label(&self) -> &'static str {
        "OpenAI / Codex"
    }
    fn name(&self) -> &'static str {
        "Codex"
    }
    fn default_model(&self) -> String {
        crate::usage::default_model()
    }
    fn image(&self) -> Option<String> {
        crate::runtime::worker_image()
    }
    fn image_hint(&self) -> &'static str {
        "set worker-image-id in the configuration directory to the Nix image digest"
    }
    fn credentials_ready(&self) -> Result<(), String> {
        credentials_ready().map_err(str::to_owned)
    }
    fn probe(&self, image: &str) -> Result<(), String> {
        offline_probe(image)
    }
    fn text_run(&self, call: &crate::backend::TextCall<'_>) -> std::io::Result<RunSpec> {
        text_run(call.image, call.name, call.workspace, call.node, call.model).map_err(std::io::Error::other)
    }
    fn text(&self, reader: &mut dyn std::io::Read, max: usize, emit: &mut dyn FnMut(control_ai::Event)) -> std::io::Result<control_ai::Output> {
        control_ai::json_lines(reader, control_ai::codex::Text::new(max), emit)
    }
    fn private_names(&self) -> &'static [&'static str] {
        &[".codex"]
    }
    fn review_mount(&self) -> bool {
        true
    }
}

pub(crate) const PERMISSIONS: [&str; 6] = [
    "default_permissions=\"control\"",
    "permissions.control.extends=\":workspace\"",
    "permissions.control.filesystem={\"/home/agent/.codex\"=\"deny\",\"/sys\"=\"deny\"}",
    "permissions.control.network.enabled=false",
    "approval_policy=\"never\"",
    "cli_auth_credentials_store=\"file\"",
];

pub(crate) struct Credentials {
    pub(crate) path: PathBuf,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}
impl Credentials {
    pub(crate) fn locate() -> Result<Self, &'static str> {
        Self::validate(&Self::path()?)
    }

    /// Tests resolve only an explicit credential path or one under the test
    /// node directory, never the operator's `CODEX_HOME`/`~/.codex`.
    #[cfg(test)]
    pub(crate) fn path() -> Result<PathBuf, &'static str> {
        Ok(std::env::var_os("CONTROL_CODEX_AUTH_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::test_root().join("codex/auth.json")))
    }

    #[cfg(not(test))]
    pub(crate) fn path() -> Result<PathBuf, &'static str> {
        std::env::var_os("CONTROL_CODEX_AUTH_FILE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("CODEX_HOME").map(|p| PathBuf::from(p).join("auth.json")))
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex/auth.json")))
            .ok_or("Cannot locate Codex auth.json. Sign in with codex login on the host.")
    }

    // Inspect metadata only. Only Codex inside the worker consumes credential contents.
    pub(crate) fn validate(path: &Path) -> Result<Self, &'static str> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            "Codex auth.json is missing. Run codex login on the host with file credential storage."
        })?;
        if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.uid() == 0 {
            return Err("Codex auth.json must be a private regular file owned by a non-root user (mode 0600). Symlink credentials are not accepted.");
        }
        let path = fs::canonicalize(path).map_err(|_| "Cannot resolve Codex auth.json.")?;
        mount_path(&path)?;
        Ok(Self {
            path,
            uid: metadata.uid(),
            gid: metadata.gid(),
        })
    }
}

/// The pre-run sandbox probe for `control doctor`: an empty canary stands in for
/// auth.json, so no account is mounted and no model is called.
pub(crate) fn offline_probe(image: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let node = crate::config::node_id()?;
    let root = crate::config::state("codex-check")?;
    fs::create_dir_all(&root).map_err(|_| "Cannot prepare sandbox verification.")?;
    let root = fs::canonicalize(&root).map_err(|_| "Cannot prepare sandbox verification.")?;
    let _ = fs::remove_dir_all(root.join("sandbox-probe"));
    let owner = fs::metadata(&root).map_err(|_| "Cannot prepare sandbox verification.")?;
    let policy = root.join("worker-seccomp.json");
    fs::write(&policy, include_bytes!("../hosting/worker-seccomp.json"))
        .map_err(|_| "Could not prepare embedded worker security profile.")?;
    let canary = Credentials { path: root.join("unused"), uid: owner.uid(), gid: owner.gid() };
    let name = format!("control-auth-check-doctor-{}-probe", std::process::id());
    sandbox_probe(crate::engine::isolation(), image, &root, &canary, &name, &policy, &node)
}
pub(crate) fn credentials_ready() -> Result<(), &'static str> {
    Credentials::locate().map(|_| ())
}

pub(crate) fn mount_path(path: &Path) -> Result<&str, &'static str> {
    path.to_str()
        .filter(|p| path.is_absolute() && !p.contains([',', '\n', '\r']))
        .ok_or("Container mount paths must be absolute UTF-8 paths without commas or newlines.")
}

pub(crate) fn container_spec(
    image: &str,
    name: &str,
    workspace: &Path,
    auth: &Credentials,
    policy: &Path,
    node: &str,
) -> Result<RunSpec, &'static str> {
    container_spec_scoped(image,name,workspace,auth,policy,node,None)
}
pub(crate) fn container_spec_scoped(
    image:&str,name:&str,workspace:&Path,auth:&Credentials,policy:&Path,node:&str,review:Option<&Path>,
) -> Result<RunSpec,&'static str> {
    mount_path(workspace)?;
    mount_path(&auth.path)?;
    let seccomp = fs::canonicalize(policy).map_err(|_| "Worker seccomp profile is missing.")?;
    mount_path(&seccomp)?;
    review.map(mount_path).transpose()?;
    let owner = User { uid: auth.uid, gid: auth.gid };
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
        limits: Limits { pids: Some(256), memory: Some(Size::Gib(2)), cpus: Some(2), ..Limits::default() },
        ..RunSpec::default()
    }
    .label("dev.control.node", node)
    .label("dev.control.attempt", name);
    let spec = crate::lifecycle::label_purpose(spec, name.starts_with("control-auth-check-") || name.starts_with("control-summary-"))
        .tmpfs(Tmpfs::new("/tmp", Size::Mib(512)))
        .tmpfs(Tmpfs::new("/home/agent", Size::Mib(64)).owned(owner))
        .tmpfs(Tmpfs::new("/home/agent/.codex", Size::Mib(64)).owned(owner))
        .bind(workspace, "/workspace", Access::ReadWrite)
        .secret("codex/auth.json", &auth.path, "/home/agent/.codex/auth.json", Access::ReadOnly)
        .env("HOME", "/home/agent")
        .env("CODEX_HOME", "/home/agent/.codex");
    Ok(match review {
        Some(source) => spec.bind(source, "/review", Access::ReadOnly),
        None => spec,
    })
}

pub(crate) fn permissions(spec: RunSpec) -> RunSpec {
    spec.args(PERMISSIONS.iter().flat_map(|setting| ["-c", setting]))
}

/// A bounded text request reuses the worker's credential isolation, without
/// workspace tools, MCP, project files or resumable provider state.
pub(crate) fn text_run(image:&str,name:&str,workspace:&Path,node:&str,model:&str)->Result<RunSpec,String> {
    let auth=Credentials::locate()?;
    let root=workspace.parent().ok_or("Invalid summary workspace")?;
    let policy=root.join("worker-seccomp.json");
    fs::write(&policy,include_bytes!("../hosting/worker-seccomp.json")).map_err(|_|"Cannot prepare text sandbox")?;
    sandbox_probe(crate::engine::isolation(),image,root,&auth,&format!("control-auth-check-{name}"),&policy,node)?;
    Ok(text_container(image,name,workspace,&auth,&policy,node,model)?)
}
fn text_container(image:&str,name:&str,workspace:&Path,auth:&Credentials,policy:&Path,node:&str,model:&str)->Result<RunSpec,&'static str> {
    let spec=permissions(container_spec(image,name,workspace,auth,policy,node)?.args(["codex","exec","--ignore-user-config","--ignore-rules","--ephemeral","--skip-git-repo-check","--json","--model",model]));
    Ok(spec.args(["shell_tool","apps","multi_agent","browser_use","computer_use","code_mode","js_repl","view_image","sleep_tool","skill_search","skill_mcp_dependency_install","workspace_dependencies"].iter().flat_map(|feature| ["--disable",feature])).args(["-c","web_search=\"disabled\"","-c","agents.enabled=false","-c","project_doc_max_bytes=0","-"]))
}

pub(crate) fn sandbox_probe(
    isolation: &dyn Isolation,
    image: &str,
    root: &Path,
    auth: &Credentials,
    name: &str,
    policy: &Path,
    node: &str,
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
    };
    // PID 1 must be the inner execution supervisor, not the account-bearing harness.
    // Its memory is naturally readable by its own child; the outer harness is outside this PID namespace.
    // Each probe writes a fresh result name: Docker Desktop's shared-file cache keeps a
    // stale entry for a name an earlier container created and the host then deleted,
    // so recreating that name failed with ENOENT on alternate probes.
    let result = format!("probe-result-{}", probe_nonce()?);
    let spec = probe_container(image, name, &workspace, &fake, policy, node, &result)?;
    let mut child = isolation
        .spawn(&spec, Streams::new(Stream::Null, Stream::Null, Stream::Piped))
        .map_err(|_| "Cannot start sandbox verification.")?;
    let stderr = tail_reader(child.stderr.take());
    let child = crate::child::watch(child).map_err(|_| "Cannot observe sandbox verification.")?;
    let outcome = child.wait(Some(Instant::now() + SANDBOX_PROBE_WAIT), &crate::shutdown::token());
    if !matches!(outcome, Recv::Item(Ok(_))) {
        if !matches!(outcome, Recv::Item(_)) {
            child.kill();
        }
        crate::lifecycle::stop_resource(name, node, image, &workspace)?;
    }
    let status = match &outcome { Recv::Item(Ok(status)) => Some(*status), _ => None };
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
fn probe_container(image: &str, name: &str, workspace: &Path, fake: &Credentials, policy: &Path, node: &str, result: &str) -> Result<RunSpec, &'static str> {
    Ok(permissions(container_spec(image, name, workspace, fake, policy, node)?.args(["codex", "sandbox", "-P", "control"])).args(["--", "bash", "-c", "set -eu; for path in /home/agent/.codex/auth.json /proc/self/root/home/agent/.codex/auth.json /proc/1/root/home/agent/.codex/auth.json; do if cat \"$path\" >/dev/null 2>&1; then exit 41; fi; done; IFS= read -r -d '' supervisor < /proc/1/cmdline; test \"$supervisor\" = codex-linux-sandbox || exit 42; printf verified > \"/workspace/$1\"", "probe", result]))
}
pub(crate) fn probe_nonce() -> Result<String, &'static str> {
    let mut nonce = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
        .map_err(|_| "Cannot prepare sandbox verification.")?;
    Ok(nonce.iter().map(|b| format!("{b:02x}")).collect())
}
/// The last bytes a probe wrote to stderr, delivered once the stream closes.
pub(crate) fn tail_reader(stream: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
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
pub(crate) fn probe_detail(status: Option<std::process::ExitStatus>, written: Option<&[u8]>, stderr: &[u8]) -> String {
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
    let words = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty());
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

pub(crate) const SANDBOX_PROBE_TIMED_OUT: &str = "Sandbox verification timed out after 90 s: the container engine is busy or slow, which is not an isolation failure. No account credentials were mounted; retry shortly.";
/// The probe's result: a timeout is reported apart from a failed isolation check.
pub(crate) fn sandbox_verdict(outcome: Recv<std::io::Result<std::process::ExitStatus>>, verified: impl FnOnce() -> bool) -> Result<(), &'static str> {
    const FAILED: &str = "Credential isolation verification failed. No account credentials were mounted. Check the pinned Codex image and container sandbox support.";
    let status = match outcome {
        Recv::Elapsed => return Err(SANDBOX_PROBE_TIMED_OUT),
        Recv::Item(Ok(status)) => status,
        _ => return Err(FAILED),
    };
    match status.code() {
        Some(41) => Err("Credential canary was readable through a sandbox path. No account credentials were mounted."),
        Some(42) => Err("The sandbox did not isolate the account-bearing harness in a separate PID namespace. No account credentials were mounted."),
        _ if status.success() && verified() => Ok(()),
        _ => Err(FAILED),
    }
}

pub(crate) fn capture_diagnostics(
    mut reader: impl std::io::Read + Send + 'static,
) -> std::thread::JoinHandle<&'static str> {
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
        worker_failure(&captured)
    })
}

/// Keeps the end of the provider's event stream so its own error events can
/// be classified: Codex reports a usage limit only there, not on stderr.
pub(crate) struct Tail<R> {
    inner: R,
    kept: Vec<u8>,
}
impl<R: std::io::Read> std::io::Read for &mut Tail<R> {
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
/// The fixed message for the provider's `error` and `turn.failed` events in
/// `stream`, if they name a known failure. Raw text is never returned.
pub(crate) fn reported_failure(stream: &[u8]) -> Option<&'static str> {
    let errors = stream.split(|byte| *byte == b'\n')
        .filter(|line| [&b"\"type\":\"error\""[..], b"\"type\":\"turn.failed\""].iter().any(|kind| line.windows(kind.len()).any(|window| window == *kind)))
        .collect::<Vec<_>>().join(&b'\n');
    Some(worker_failure(&errors)).filter(|message| !errors.is_empty() && *message != UNKNOWN_FAILURE)
}
pub(crate) fn capture_codex(
    reader: impl std::io::Read,
    mut update: impl FnMut(crate::usage::Activity),
) -> (control_ai::Usage, bool, Option<&'static str>) {
    let mut reader = Tail { inner: reader, kept: Vec::new() };
    // Completion is adapter state, independent of the bounded UI activity tail.
    let mut pending = std::collections::HashSet::new();
    let usage = crate::usage::capture_with(&mut reader, |event| {
        if event.kind == "mcp_tool_call" {
            if matches!(event.status.as_str(), "in_progress" | "running") {
                pending.insert(event.id.clone());
            } else {
                pending.remove(&event.id);
            }
        }
        update(event);
    });
    (usage, !pending.is_empty(), reported_failure(&reader.kept))
}

/// A successful CLI exit is not a completed tool exchange. Check after the
/// stdout reader joins, so a buffered completion cannot be mistaken for loss.
/// No timeout or automatic retry: a host effect may already have happened.
pub(crate) fn finish_codex_tools(
    attempt: &mut crate::usage::Attempt,
    bridge: Option<(&Arc<Mutex<State>>, &str)>,
    generation: &str,
    host_finished: bool,
    unfinished: bool,
) -> Result<(), (&'static str, String)> {
    if host_finished || !unfinished {
        return Ok(());
    }
    interrupt_unfinished_tools(bridge, generation);
    attempt.termination = Some(crate::attempt_control::Termination::ProviderError);
    Err(("interrupted", "The provider ended with an unfinished MCP tool call. Inspect pending requests and service status before retrying: the tool's outcome is unconfirmed. The published checkpoint is unchanged; check recovery evidence for unpublished work.".into()))
}

/// One Codex turn's inputs from the generic worker.
pub(crate) struct Turn<'a> {
    pub id: &'a str,
    pub objective: &'a str,
    pub scope: Workspace,
    /// The captured turn context, when the attempt has one.
    pub captured: Option<String>,
    pub bridge: Option<(&'a Arc<Mutex<State>>, &'a str)>,
    pub workspace: &'a Path,
    pub absolute: &'a Path,
    pub image: &'a str,
    pub node: &'a str,
    pub auth: Credentials,
    pub isolation: &'a dyn Isolation,
}
pub(crate) fn execute(turn: Turn<'_>, attempt: &mut crate::usage::Attempt) -> Result<String, (&'static str, String)> {
    let Turn { id, objective, scope, captured, bridge, workspace, absolute, image, node, auth, isolation } = turn;
    let blocked = |m: &str| ("blocked", m.to_owned());
    let failed = |m: &str| ("failed", m.to_owned());
    let policy = absolute.parent().unwrap().join("worker-seccomp.json");
    fs::write(&policy, include_bytes!("../hosting/worker-seccomp.json"))
        .map_err(|_| failed("Could not prepare embedded worker security profile."))?;
    // Verify each attempt before mounting real credentials; successes are not cached.
    sandbox_probe(
        isolation,
        image,
        absolute.parent().unwrap(),
        &auth,
        &format!("control-{id}-probe"),
        &policy,
        node,
    )
    .map_err(|reason| blocked(&reason))?;
    let container = format!("control-{id}");
    // Codex transport and final report need writable scratch. Its review source
    // is a distinct immutable mount, never the writable mailbox at /workspace.
    let review_source = absolute.parent().unwrap().join("review-source");
    if attempt.review_only {
        fs::rename(absolute, &review_source).map_err(|_|failed("Could not isolate judge source."))?;
        fs::create_dir(absolute).map_err(|_|failed("Could not prepare judge scratch."))?;
    }
    let spec =
        turn_container(image, &container, absolute, &auth, &policy, node, attempt.review_only.then_some(review_source.as_path()), &attempt.model, bridge.is_some()).map_err(blocked)?;
    let room = bridge.and_then(|(state, run_id)| {
        let s = state.lock().ok()?;
        let run = s.runtime.runs.iter().find(|r| r.id == run_id)?;
        s.chats
            .iter()
            .find(|c| Some(&c.id) == run.chat_id.as_ref())?
            .room
            .clone()
            .filter(|r| crate::rooms::valid(r))
    });
    if crate::lifecycle::cancelled(bridge) {
        return Err(("cancelled", "Cancelled before launch.".into()));
    }
    let mut child = isolation
        .spawn(&spec, Streams::new(Stream::Piped, Stream::Piped, Stream::Piped))
        .map_err(|_| failed("Could not start the container worker."))?;
    let diagnostics = capture_diagnostics(child.stderr.take().unwrap());
    let stdout = child.stdout.take().unwrap();
    let live = bridge.map(|(state, run)| (Arc::clone(state), run.to_owned()));
    let generation = id.to_owned();
    let diff_workspace = absolute.to_path_buf();
    let review_only=attempt.review_only;
    let acted = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let acting = Arc::clone(&acted);
    attempt.accounting = crate::usage::Accounting::Unknown;
    let accounting = std::thread::spawn(move || {
        capture_codex(stdout, |event| {
            if matches!(event.kind.as_str(), "command_execution" | "file_change") {
                acting.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let Some((state, id)) = &live else {
                return;
            };
            if event.kind == "file_change" {
                if review_only {return;}
                if !["running","in_progress"].contains(&event.status.as_str()) {
                    observe_workspace_diff(state,id,&generation,&diff_workspace,"observed");
                }
                return;
            }
            if !review_only && event.kind == "command_execution" && event.status == "completed" {
                observe_workspace_diff(state,id,&generation,&diff_workspace,"observed");
            }
            let Ok(mut state) = state.lock() else {
                return;
            };
            // Progress is disposable; lifecycle/result checkpoints persist the
            // accumulated activity. Never fsync the entire history per event.
            record_activity_for(&mut state, id, &generation, event);
        })
    });
    let prompt = crate::observe::append(bridge, captured.unwrap_or_else(|| render_prompt(objective,scope,room.as_deref(),bridge.is_some())));
    if child
        .stdin
        .take()
        .is_none_or(|mut stdin| stdin.write_all(prompt.as_bytes()).is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        crate::lifecycle::stop_attempt(attempt).map_err(|m| ("interrupted", m.to_owned()))?;
        return Err(failed("Could not deliver the objective."));
    }
    let started = Instant::now();
    let admitted = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs().saturating_sub(attempt.started_unix);
    let mailbox_wake = crate::mailbox_wake::MailboxWake::new(absolute);
    let mut timed_out = None;
    let mut handoff = None;
    // When the handoff was first seen accepted, and the shell/file actions started by then.
    let mut accepted: Option<(Instant, u64)> = None;
    let result = loop {
        if crate::design::requested(bridge) {
            break Err(std::io::Error::other("Design decision requested"));
        }
        if let Some((state, run)) = bridge {
            if crate::lifecycle::cancelled(bridge) {
                break Err(std::io::Error::other("Cancellation requested"));
            }
            if state.is_poisoned() {
                break Err(std::io::Error::other("Control state unavailable"));
            }
            crate::worker_tools::pump(state, run, absolute,attempt.context_trace.is_some());
        }
        if crate::design::requested(bridge) {
            break Err(std::io::Error::other("Design decision requested"));
        }
        if crate::lifecycle::cancelled(bridge) { break Err(std::io::Error::other("Cancellation requested")); }
        match crate::attempt_control::poll(bridge, attempt, started.elapsed() + Duration::from_secs(admitted)) {
            crate::attempt_control::Decision::Finish(summary) => {
                // Let the turn end itself and report its usage, unless it
                // keeps working (shell or file changes after its handoff) or
                // outlasts the grace.
                let acted_now = acted.load(std::sync::atomic::Ordering::Relaxed);
                let (at, before) = *accepted.get_or_insert((Instant::now(), acted_now));
                handoff = Some(summary);
                if acted_now > before || handoff_grace_over(at, Instant::now()) {
                    let _ = child.kill();
                    let _ = child.wait();
                    crate::attempt_control::stop(bridge, attempt).map_err(|m| ("interrupted", m.to_owned()))?;
                    break Ok(true);
                }
            }
            crate::attempt_control::Decision::TimedOut(seconds) => {
                attempt.termination = Some(crate::attempt_control::Termination::TimedOut);
                timed_out = Some(Duration::from_secs(seconds));
                break Err(std::io::Error::other("Worker deadline reached"));
            }
            crate::attempt_control::Decision::Continue => {}
        }
        match child.try_wait() {
            // An accepted handoff is the result however the turn then ended.
            Ok(Some(_)) if handoff.is_some() => {
                crate::attempt_control::stop(bridge, attempt).map_err(|m| ("interrupted", m.to_owned()))?;
                break Ok(true);
            }
            Ok(Some(status)) => break Ok(status.success()),
            Err(e) => break Err(e),
            _ => {}
        }
        mailbox_wake.wait(Duration::from_millis(250));
    };
    // Killing the CLI alone may leave the daemon-owned container running.
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        if let Err(message) = crate::attempt_control::stop(bridge, attempt) {
            return Err(("interrupted", message.into()));
        }
        if let Ok((usage, _, _)) = accounting.join() {
            attempt.record_usage(usage);
        }
        return Err(if crate::lifecycle::cancelled(bridge) {
            (
                "cancelled",
                "Cancelled. The previous published workspace is unchanged.".into(),
            )
        } else if crate::design::requested(bridge) {
            ("waiting", "Waiting for a design decision.".into())
        } else if let Some(limit) = timed_out {
            ("failed", timeout_message(limit))
        } else {
            failed("Worker stopped after its deadline or a process error.")
        });
    }
    let (usage, unfinished, reported) = accounting.join().map_err(|_| failed("Could not read the provider's final tool results."))?;
    attempt.record_usage(usage);
    if !result.unwrap_or(false) {
        let stderr = diagnostics.join().unwrap_or(UNKNOWN_FAILURE);
        return Err(failed(if stderr == UNKNOWN_FAILURE { reported.unwrap_or(stderr) } else { stderr }));
    }
    let _ = diagnostics.join();
    finish_codex_tools(attempt, bridge, id, handoff.is_some(), unfinished)?;
    let report = || read_report(&workspace.join(".control-result"));
    let requested = bridge.is_some_and(|(state, run)| state.lock()
        .is_ok_and(|s| s.runtime.runs.iter().any(|r| r.id == run && crate::attempt_control::requested(r))));
    // A requested turn that ended itself replies with its full last message,
    // not the handoff's clipped one.
    let output = match handoff {
        Some(reply) if requested => report().ok().filter(|text| !text.trim().is_empty()).unwrap_or(reply),
        handoff => handoff.or_else(|| report().ok()).unwrap_or_else(|| "Worker finished without a safe final report.".into()),
    };
    let summary: String = output.chars().take(16000).collect();
    Ok(summary)
}
/// One turn's container and Codex arguments; `bridge` adds the room tools.
#[allow(clippy::too_many_arguments)]
fn turn_container(image: &str, container: &str, absolute: &Path, auth: &Credentials, policy: &Path, node: &str, review: Option<&Path>, model: &str, bridge: bool) -> Result<RunSpec, &'static str> {
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
    let tools = control_context::worker_mcp_names();
    let rooms = bridge.then(|| {
        [
            "mcp_servers.control_rooms.command=\"/bin/control-room-mcp\"".to_owned(),
            "mcp_servers.control_rooms.required=true".to_owned(),
            format!("mcp_servers.control_rooms.enabled_tools={}", serde_json::to_string(&tools).unwrap()),
        ]
        .into_iter()
        .chain(tools.iter().map(|tool| format!("mcp_servers.control_rooms.tools.{tool}.approval_mode=\"approve\"")))
        .collect::<Vec<_>>()
    });
    Ok(spec.args(rooms.into_iter().flatten().flat_map(|setting| ["-c".to_owned(), setting])).args(["-"]))
}

#[cfg(test)]
mod tests;
