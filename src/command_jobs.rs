//! Durable, credential-free commands. Model requests select capabilities, never mounts.
mod tools;
pub use tools::{JobCall, Tools};
use crate::{tool_registry::AttemptRef, usage::Attempt, State};
use control_effect::sync::{CancelToken, Recv};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_JOBS: usize = 32;
const OUTPUT_BYTES: usize = 16000;
const INSPECT: &str = r#"{"id":{{json .Id}},"name":{{json .Name}},"node":{{json (index .Config.Labels "dev.control.node")}},"attempt":{{json (index .Config.Labels "dev.control.attempt")}},"image":{{json .Config.Image}},"source":{{range .Mounts}}{{if eq .Destination "/workspace"}}{{json .Source}}{{end}}{{end}},"writable":{{range .Mounts}}{{if eq .Destination "/workspace"}}{{json .RW}}{{end}}{{end}},"network":{{json .HostConfig.NetworkMode}},"readonly_root":{{json .HostConfig.ReadonlyRootfs}},"running":{{json .State.Running}},"exit_code":{{json .State.ExitCode}}}"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub generation: String,
    pub image: String,
    pub node: String,
    pub command: String,
    pub read_only: bool,
    pub build_environment: bool,
    pub review_source: bool,
    #[serde(default, skip_serializing_if = "crate::worker_profile::Profile::is_lite")]
    pub profile: crate::worker_profile::Profile,
    pub deadline_unix: u64,
    pub container_id: Option<String>,
    pub status: String,
    pub output: String,
    pub exit_code: Option<i64>,
}
impl Job {
    /// Reconciliation needs ownership fields, never retained command/output buffers.
    pub fn identity(&self)->Self {
        Self{id:self.id.clone(),generation:self.generation.clone(),image:self.image.clone(),node:self.node.clone(),command:String::new(),read_only:self.read_only,build_environment:self.build_environment,review_source:self.review_source,profile:self.profile,deadline_unix:self.deadline_unix,container_id:self.container_id.clone(),status:self.status.clone(),output:String::new(),exit_code:self.exit_code}
    }
    pub fn name(&self) -> String {
        format!("control-{}-job-{}", self.generation, self.id)
    }
    pub fn workspace(&self) -> io::Result<std::path::PathBuf> {
        let workspace = crate::runtime::workspace(&self.generation).map_err(fail)?;
        Ok(if self.read_only && self.review_source {
            workspace.parent().unwrap().join("review-source")
        } else {
            workspace
        })
    }
    fn active(&self) -> bool {
        ["preparing", "running", "stopping"].contains(&self.status.as_str())
    }
    fn summary(&self) -> Value {
        json!({"job_id":self.id,"status":self.status,"output":self.output,"exit_code":self.exit_code,"deadline_unix":self.deadline_unix,"output_limit_bytes":OUTPUT_BYTES})
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    command: String,
    #[serde(default = "default_deadline")]
    timeout_seconds: u64,
    #[serde(default = "default_environment")]
    environment: String,
}
fn default_deadline() -> u64 {
    600
}
fn default_environment() -> String {
    "worker".into()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}

fn environment_image(attempt: &Attempt, environment: &str) -> io::Result<String> {
    let image = match environment {
        "worker" => attempt.image.clone().ok_or_else(|| fail("Missing captured worker image"))?,
        "build" => crate::build_environment::image()?.ok_or_else(|| fail("No declared build environment; build/load the Nix build-image and configure build-image-id"))?,
        _ => return Err(fail("Unknown declared environment")),
    };
    if !crate::runtime::pinned_image(&image) {
        return Err(fail("Environment image must be pinned by digest"));
    }
    Ok(image)
}
fn current<'a>(state: &'a mut State, run: &str, generation: &str) -> io::Result<&'a mut Attempt> {
    let run = state
        .runtime
        .runs
        .iter_mut()
        .find(|r| r.id == run && r.status == "running")
        .ok_or_else(|| fail("Run is not active"))?;
    crate::identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::pausing(run) {return Err(fail("Attempt is pausing for delegated work"));}
    if run.question.as_ref().is_some_and(|q| q.answer.is_none()) {
        return Err(fail("Attempt is pausing for an answer"));
    }
    run.attempts
        .last_mut()
        .filter(|a| a.workspace_id.as_deref() == Some(generation))
        .ok_or_else(|| fail("Command belongs to an inactive attempt"))
}
pub(crate) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: JobCall) -> io::Result<Value> {
    let (run, generation) = (attempt.run, attempt.generation);
    let (cancel, job_id) = match call {
        JobCall::Start(input) => return start(shared, run, generation, attempt.nonce, input),
        JobCall::Status(job_id) => (false, job_id),
        JobCall::Cancel(job_id) => (true, job_id),
    };
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let attempt = current(&mut state, run, generation)?;
    let job = attempt
        .command_jobs
        .iter_mut()
        .find(|j| j.id == job_id)
        .ok_or_else(|| fail("Unknown job in this attempt"))?;
    if cancel && job.active() {
        let old = job.status.clone();
        job.status = "stopping".into();
        let summary = job.summary();
        if let Err(e) = crate::storage::save(&state) {
            current(&mut state, run, generation)?
                .command_jobs
                .iter_mut()
                .find(|j| j.id == job_id)
                .unwrap()
                .status = old;
            return Err(e);
        }
        return Ok(summary);
    }
    Ok(job.summary())
}
fn start(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    nonce: &str,
    input: Input,
) -> io::Result<Value> {
    if crate::shutdown::requested() {
        return Err(fail("Control is stopping"));
    }
    if input.command.is_empty()
        || input.command.len() > 16000
        || !(1..=1800).contains(&input.timeout_seconds)
    {
        return Err(fail(
            "Command must contain 1–16000 bytes and timeout must be 1–1800 seconds",
        ));
    }
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(fail("Invalid request nonce"));
    }
    let id: String = ring::digest::digest(&ring::digest::SHA256, nonce.as_bytes()).as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let image = {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let attempt = current(&mut state, run, generation)?;
        if let Some(job) = attempt.command_jobs.iter().find(|j| j.id == id) { return Ok(job.summary()); }
        environment_image(attempt, &input.environment)?
    };
    let profile = if input.environment == "build" { crate::worker_profile::Profile::Lite } else { crate::worker_profile::Profile::inspect(&image)? };
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let owner_deadline = state.runtime.runs.iter().find(|r| r.id == run).and_then(|r| {
        r.attempts.last().and_then(|a| crate::attempt_control::deadline(&state, r, a).map(|limit| a.started_unix.saturating_add(limit)))
    });
    let started = now();
    let deadline_unix = effective_deadline(started, input.timeout_seconds, owner_deadline)?;
    let attempt = current(&mut state, run, generation)?;
    if attempt.turn_mailbox.as_ref().is_some_and(|m| m.accepted().is_some()) {
        return Err(fail("The attempt has handed off; no new workspace jobs may start"));
    }
    if environment_image(attempt, &input.environment)? != image { return Err(fail("Captured command image changed")); }
    if let Some(job) = attempt.command_jobs.iter().find(|j| j.id == id) {
        return Ok(job.summary());
    }
    if attempt.command_jobs.iter().any(Job::active) {
        return Err(fail(
            "An active command already owns this attempt; await or cancel it",
        ));
    }
    if attempt.command_jobs.len() >= MAX_JOBS {
        return Err(fail(
            "Attempt command limit reached; continue in a new turn",
        ));
    }
    let job = Job {
        id,
        generation: generation.into(),
        image,
        node: attempt
            .node_id
            .clone()
            .ok_or_else(|| fail("Missing captured node identity"))?,
        command: input.command,
        read_only: attempt.review_only,
        build_environment: input.environment == "build",
        review_source: attempt.review_only && attempt.backend == crate::backend::Backend::Codex,
        profile,
        deadline_unix,
        container_id: None,
        status: "preparing".into(),
        output: String::new(),
        exit_code: None,
    };
    attempt.command_jobs.push(job.clone());
    if let Err(e) = crate::storage::save(&state) {
        current(&mut state, run, generation)?.command_jobs.pop();
        return Err(e);
    }
    drop(state);
    let thread_state = Arc::clone(shared);
    let thread_run = run.to_owned();
    let thread_job = job.clone();
    let mut summary = job.summary();
    summary["requested_timeout_seconds"] = json!(input.timeout_seconds);
    summary["effective_timeout_seconds"] = json!(deadline_unix.saturating_sub(started));
    if let Err(e) = std::thread::Builder::new()
        .name("workspace-command".into())
        .spawn(move || supervise(thread_state, thread_run, thread_job))
    {
        let mut job = job;
        job.status = "failed".into();
        job.output = "Could not start command supervisor; no command was launched.".into();
        activity(shared, run, &job, true);
        return Err(e);
    }
    Ok(summary)
}
fn effective_deadline(now: u64, requested: u64, owner: Option<u64>) -> io::Result<u64> {
    let deadline = owner.map_or(now.saturating_add(requested), |end| end.min(now.saturating_add(requested)));
    if deadline <= now { return Err(fail("The attempt deadline has elapsed; use a persistent service for work that must outlive this attempt")); }
    Ok(deadline)
}
fn create(job: &Job) -> io::Result<String> {
    let workspace = job.workspace()?.canonicalize()?;
    if job.build_environment { crate::build_environment::check(&job.image, &workspace)?; }
    let meta = fs::symlink_metadata(&workspace)?;
    if !meta.is_dir() || meta.uid() == 0 {
        return Err(fail("Expected private non-root workspace"));
    }
    let path = workspace
        .to_str()
        .filter(|s| !s.contains([',', '\n', '\r']))
        .ok_or_else(|| fail("Invalid workspace mount"))?;
    let name = job.name();
    let mut arguments: Vec<String> = [
        "create",
        "--pull=never",
        "--name",
        &name,
        "--label",
        &format!("dev.control.node={}", job.node),
        "--label",
        &format!("dev.control.attempt={name}"),
        "--label",
        "dev.control.purpose=command",
        "--user",
        &format!("{}:{}", meta.uid(), meta.gid()),
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        if job.build_environment {
            "--pids-limit=512"
        } else {
            "--pids-limit=256"
        },
        if job.build_environment {
            "--memory=4g"
        } else {
            "--memory=2g"
        },
        "--cpus=2",
        "--network=none",
        "--log-driver=local",
        "--log-opt",
        "max-size=1m",
        "--log-opt",
        "max-file=2",
        "--tmpfs",
        "/tmp:rw,exec,nosuid,nodev,size=1g",
        "--tmpfs",
        &format!(
            "/home/agent:rw,nosuid,nodev,uid={},gid={},size=128m",
            meta.uid(),
            meta.gid()
        ),
        "--env",
        "HOME=/home/agent",
        "--env",
        "CARGO_BUILD_JOBS=2",
        "--env",
        if job.read_only {
            "CARGO_TARGET_DIR=/build-target"
        } else {
            "CONTROL_WORKSPACE_WRITABLE=1"
        },
        "--mount",
        &format!(
            "type=bind,src={path},dst=/workspace{}",
            if job.read_only { ",readonly" } else { "" }
        ),
        "--workdir",
        "/workspace",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if job.profile.readonly_root() { arguments.push("--read-only".into()); }
    if job.read_only {
        use std::os::unix::fs::DirBuilderExt;
        let scratch = crate::runtime::workspace(&job.generation)
            .map_err(fail)?
            .parent()
            .unwrap()
            .join("command-scratch")
            .join(&job.id);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&scratch)?;
        if !fs::symlink_metadata(&scratch)?.is_dir() {
            return Err(fail("Invalid command scratch directory"));
        }
        let scratch = scratch.canonicalize()?;
        let scratch = scratch
            .to_str()
            .filter(|s| !s.contains([',', '\n', '\r']))
            .ok_or_else(|| fail("Invalid scratch mount"))?;
        arguments.extend([
            "--mount".into(),
            format!("type=bind,src={scratch},dst=/build-target"),
        ]);
    }
    arguments.extend(
        [
            &job.image,
            "bash",
            "-c",
            "exec 2>&1; eval -- \"$1\"",
            "control-command",
            &job.command,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let text =
        crate::engine::query(&arguments.iter().map(String::as_str).collect::<Vec<_>>())?;
    let id = text.trim();
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(fail("Invalid command container identity"));
    }
    Ok(id.to_owned())
}
#[derive(Clone, Deserialize)]
struct Inspection {
    id: String,
    name: String,
    node: String,
    attempt: String,
    image: String,
    source: String,
    writable: bool,
    network: String,
    readonly_root: bool,
    running: bool,
    exit_code: i64,
}
fn inspect(job: &Job) -> io::Result<Inspection> {
    let result = crate::engine::query(&["inspect", "--format", INSPECT, &job.name()])?;
    let inspection: Inspection =
        serde_json::from_str(result.trim()).map_err(|_| fail("Invalid command inspection"))?;
    verify(job, &inspection)?;
    Ok(inspection)
}
fn verify(job: &Job, inspection: &Inspection) -> io::Result<()> {
    let workspace = job.workspace()?.canonicalize()?;
    verify_identity(job, inspection, &workspace)
}
fn verify_identity(
    job: &Job,
    inspection: &Inspection,
    workspace: &std::path::Path,
) -> io::Result<()> {
    if inspection.id.len() != 64
        || !inspection.id.bytes().all(|b| b.is_ascii_hexdigit())
        || job
            .container_id
            .as_ref()
            .is_some_and(|id| id != &inspection.id)
        || inspection.name != format!("/{}", job.name())
        || inspection.node != job.node
        || inspection.attempt != job.name()
        || inspection.image != job.image
        || std::path::Path::new(&inspection.source) != workspace
        || inspection.writable == job.read_only
        || inspection.network != "none"
        || inspection.readonly_root != job.profile.readonly_root()
    {
        return Err(fail(
            "Command container identity changed; replacement was not adopted",
        ));
    }
    Ok(())
}
pub fn stop(job: &Job) -> io::Result<()> {
    let workspace = job.workspace()?.canonicalize()?;
    crate::lifecycle::stop_resource_id(
        &job.name(),
        &job.node,
        &job.image,
        &workspace,
        job.container_id.as_deref(),
    )
    .map_err(fail)?;
    // A timed-out Docker start may still be in flight. Confirm removal, not just
    // a momentarily stopped state, so no late start can race publication.
    let filter = job
        .container_id
        .as_ref()
        .map(|id| format!("id={id}"))
        .unwrap_or_else(|| format!("name=^/{}$", job.name()));
    if !crate::engine::query(&["ps", "--all", "--quiet", "--filter", &filter])?
        .trim()
        .is_empty()
    {
        return Err(fail(
            "Command resource removal unconfirmed; checkpoint remains fenced",
        ));
    }
    Ok(())
}
fn bounded_output(text: &str) -> String {
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    if text.len() <= OUTPUT_BYTES {
        return text;
    }
    let mut offset = text.len() - OUTPUT_BYTES;
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    format!("[earlier output omitted]\n{}", &text[offset..])
}
fn logs(job: &Job) -> io::Result<String> {
    use std::process::Stdio;
    let mut child = crate::engine::scrubbed()
        .args([
            "logs",
            "--tail",
            "200",
            job.container_id
                .as_deref()
                .ok_or_else(|| fail("Missing command container ID"))?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut reader = child.stdout.take().unwrap();
    let drain = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut omitted = false;
        let mut buf = [0; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > OUTPUT_BYTES {
                omitted = true;
                kept.drain(..kept.len() - OUTPUT_BYTES);
            }
        }
        let text = bounded_output(&String::from_utf8_lossy(&kept));
        if omitted && !text.starts_with("[earlier output omitted]") {
            format!("[earlier output omitted]\n{text}")
        } else {
            text
        }
    });
    let success = match crate::child::watch(child) {
        Ok(watched) => match watched.wait(Some(Instant::now() + Duration::from_secs(5)), &CancelToken::new()) {
            Recv::Item(status) => status.is_ok_and(|status| status.success()),
            _ => {
                watched.kill();
                let _ = watched.wait(None, &CancelToken::new());
                false
            }
        },
        Err(_) => false,
    };
    let text = drain
        .join()
        .map_err(|_| fail("Command log reader stopped"))?;
    if success {
        Ok(text)
    } else {
        Err(fail("Command logs unavailable"))
    }
}
fn live<'a>(state: &'a mut State, run: &str, job: &Job) -> Option<&'a mut Job> {
    state
        .runtime
        .runs
        .iter_mut()
        .find(|r| r.id == run)?
        .attempts
        .iter_mut()
        .find(|a| a.workspace_id.as_deref() == Some(&job.generation))?
        .command_jobs
        .iter_mut()
        .find(|j| j.id == job.id)
}
fn activity(shared: &Arc<Mutex<State>>, run: &str, job: &Job, persist: bool) -> bool {
    let Ok(mut state) = shared.lock() else {
        return false;
    };
    if let Some(stored) = live(&mut state, run, job) {
        // A cancellation request wins over an in-flight progress update.
        let stopping = stored.status == "stopping" && job.active();
        let status = if stopping { "stopping" } else { &job.status };
        if stored.status == status
            && stored.output == job.output
            && stored.container_id == job.container_id
            && stored.exit_code == job.exit_code
        {
            return !persist || crate::storage::save(&state).is_ok();
        }
        *stored = job.clone();
        if stopping {
            stored.status = "stopping".into();
        }
    }
    crate::worker::record_activity_for(
        &mut state,
        run,
        &job.generation,
        crate::usage::Activity {
            id: format!("job-{}", job.id),
            kind: "command_execution".into(),
            text: job.command.chars().take(2000).collect(),
            output: job.output.clone(),
            status: job.status.clone(),
        },
    );
    !persist || crate::storage::save(&state).is_ok()
}
fn should_stop(shared: &Arc<Mutex<State>>, run: &str, job: &Job) -> bool {
    if crate::shutdown::requested() || now() >= job.deadline_unix {
        return true;
    }
    let Ok(mut state) = shared.lock() else {
        return true;
    };
    let active = current(&mut state, run, &job.generation).is_ok();
    !active || live(&mut state, run, job).is_none_or(|j| j.status == "stopping")
}
fn supervise(shared: Arc<Mutex<State>>, run: String, mut job: Job) {
    if should_stop(&shared, &run, &job) {
        job.status = if now() >= job.deadline_unix {
            "timed_out"
        } else {
            "cancelled"
        }
        .into();
        activity(&shared, &run, &job, true);
        return;
    }
    match create(&job) {
        Ok(id) => {
            job.container_id = Some(id);
            // A created container cannot execute yet. Capture its immutable ID
            // durably before sending any start request to the daemon.
            if !activity(&shared, &run, &job, true) {
                job.status = if stop(&job).is_ok() {
                    "failed"
                } else {
                    "stopping"
                }
                .into();
                job.output = "Command identity could not be saved; no start was requested.".into();
                activity(&shared, &run, &job, true);
                return;
            }
        }
        Err(_) => {
            // Launch can time out after Docker admitted it. Verify and stop by durable name.
            job.status = if stop(&job).is_ok() {
                "failed"
            } else {
                "stopping"
            }
            .into();
            job.output = "Command launch failed; inspect node container diagnostics.".into();
            activity(&shared, &run, &job, true);
            return;
        }
    }
    if should_stop(&shared, &run, &job) {
        job.status = if stop(&job).is_ok() {
            if now() >= job.deadline_unix {
                "timed_out"
            } else {
                "cancelled"
            }
        } else {
            "stopping"
        }
        .into();
        activity(&shared, &run, &job, true);
        return;
    }
    if crate::engine::query(&["start", job.container_id.as_deref().unwrap()]).is_err() {
        job.status = if stop(&job).is_ok() {
            "failed"
        } else {
            "stopping"
        }
        .into();
        job.output =
            "Command start failed; its admitted resource was stopped or remains fenced.".into();
        activity(&shared, &run, &job, true);
        return;
    }
    job.status = "running".into();
    activity(&shared, &run, &job, true);
    let mut persisted = Instant::now();
    let shutdown = crate::shutdown::token();
    let mut exited = exit_waiter(&job);
    loop {
        let inspection = inspect(&job);
        if let Ok(text) = logs(&job) {
            job.output = text;
        }
        let finish = match inspection {
            Ok(inspection) if !inspection.running => {
                job.exit_code = Some(inspection.exit_code);
                Some(if inspection.exit_code == 0 {
                    "completed"
                } else {
                    "failed"
                })
            }
            Ok(_) if should_stop(&shared, &run, &job) => Some(if now() >= job.deadline_unix {
                "timed_out"
            } else {
                "cancelled"
            }),
            Ok(_) => None,
            Err(_) => Some("interrupted"),
        };
        if let Some(status) = finish {
            // Never announce a finished writer until its exact resource is confirmed stopped.
            job.status = if stop(&job).is_ok() {
                status
            } else {
                "stopping"
            }
            .into();
            activity(&shared, &run, &job, true);
            if job.status != "stopping" && !job.read_only {
                crate::worker::observe_workspace_diff(
                    &shared,
                    &run,
                    &job.generation,
                    &crate::runtime::workspace(&job.generation).unwrap(),
                    "observed",
                );
            }
            break;
        }
        let persist = persisted.elapsed() >= Duration::from_secs(15);
        activity(&shared, &run, &job, persist);
        if persist {
            persisted = Instant::now();
        }
        let tick = Some(Instant::now() + Duration::from_millis(250));
        match exited.as_ref().map(|waiter| waiter.wait(tick, &shutdown)) {
            Some(Recv::Item(_) | Recv::Closed) => exited = None,
            Some(Recv::Cancelled | Recv::Elapsed) => {}
            None => {
                let _ = shutdown.wait(tick);
            }
        }
    }
}
/// Wakes the supervisor as soon as the container exits, instead of at the next tick.
fn exit_waiter(job: &Job) -> Option<crate::child::Watched> {
    use std::process::Stdio;
    let child = crate::engine::scrubbed()
        .args(["wait", job.container_id.as_deref()?])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    crate::child::watch(child).ok()
}

/// Before any checkpoint, wait for admitted launches and cancel every writer.
/// Failure fences publication rather than racing a background container.
pub fn quiesce(shared: &Arc<Mutex<State>>, run: &str, generation: &str) -> io::Result<()> {
    quiesce_until(
        shared,
        run,
        generation,
        Instant::now() + Duration::from_secs(20),
    )
}
fn quiesce_until(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    deadline: Instant,
) -> io::Result<()> {
    loop {
        let mut state = shared
            .lock()
            .map_err(|_| fail("State unavailable; command writers remain fenced"))?;
        let attempt = state
            .runtime
            .runs
            .iter_mut()
            .find(|r| r.id == run)
            .and_then(|r| {
                r.attempts
                    .iter_mut()
                    .find(|a| a.workspace_id.as_deref() == Some(generation))
            })
            .ok_or_else(|| fail("Missing attempt command ownership"))?;
        let mut active = false;
        for job in &mut attempt.command_jobs {
            if job.active() {
                job.status = "stopping".into();
                active = true;
            }
        }
        if !active {
            return Ok(());
        }
        drop(state);
        if Instant::now() >= deadline {
            return Err(fail(
                "Could not confirm workspace command exit; checkpoint remains unchanged",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests;
