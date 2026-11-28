//! Durable, credential-free commands. Model requests select capabilities, never mounts.
mod tools;
use crate::runtime::RunStatus;
use crate::store::Store;
use crate::{State, tool_registry::AttemptRef, usage::Attempt};
use control_effect::sync::{CancelToken, Recv};
use control_isolation::{
    Access, Exec, Isolation, Limits, Logs, Network, Observed, RunSpec, Select, Size, States, Target, Tmpfs, User,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub use tools::{JobCall, Tools};

crate::application::status_text::status_enum! {
    /// A workspace command job's lifecycle, saved as its snake_case name.
    pub enum JobStatus {
        Preparing = "preparing",
        Running = "running",
        Stopping = "stopping",
        Completed = "completed",
        Failed = "failed",
        TimedOut = "timed_out",
        Cancelled = "cancelled",
        Interrupted = "interrupted",
    }
}
impl JobStatus {
    /// Launching, executing or not yet confirmed stopped: the job still owns its attempt.
    pub fn is_active(&self) -> bool {
        match self {
            Self::Preparing | Self::Running | Self::Stopping => true,
            Self::Completed
            | Self::Failed
            | Self::TimedOut
            | Self::Cancelled
            | Self::Interrupted
            | Self::Unrecognised(_) => false,
        }
    }
}

const MAX_JOBS: usize = 32;
const OUTPUT_BYTES: usize = 16000;

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
    pub status: JobStatus,
    pub output: String,
    pub exit_code: Option<i64>,
}
impl Job {
    /// Reconciliation needs ownership fields, never retained command/output buffers.
    pub fn identity(&self) -> Self {
        Self {
            id: self.id.clone(),
            generation: self.generation.clone(),
            image: self.image.clone(),
            node: self.node.clone(),
            command: String::new(),
            read_only: self.read_only,
            build_environment: self.build_environment,
            review_source: self.review_source,
            profile: self.profile,
            deadline_unix: self.deadline_unix,
            container_id: self.container_id.clone(),
            status: self.status.clone(),
            output: String::new(),
            exit_code: self.exit_code,
        }
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
        self.status.is_active()
    }
    fn summary(&self) -> Value {
        json!({"job_id":self.id,"status":self.status,"output":self.output,"exit_code":self.exit_code,"deadline_unix":self.deadline_unix})
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
        "worker" => attempt
            .image
            .clone()
            .ok_or_else(|| fail("Missing captured worker image"))?,
        "build" => crate::build_environment::image()?.ok_or_else(|| {
            fail("No declared build environment; build/load the Nix build-image and configure build-image-id")
        })?,
        "solver" => crate::solver::image()?.ok_or_else(|| {
            fail("No pinned solver image; run nix run .#setup -- --solver to build it and configure solver-image-id")
        })?,
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
        .get_mut(run)
        .filter(|r| r.status == RunStatus::Running)
        .ok_or_else(|| fail("Run is not active"))?;
    crate::identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::pausing(run) {
        return Err(fail("Attempt is pausing for delegated work"));
    }
    if run.question.as_ref().is_some_and(|q| q.pending()) {
        return Err(fail("Attempt is pausing for an answer"));
    }
    run.attempts
        .last_mut()
        .filter(|a| a.workspace_id.as_deref() == Some(generation))
        .ok_or_else(|| fail("Command belongs to an inactive attempt"))
}
pub(crate) fn execute(shared: &Store, attempt: &AttemptRef<'_>, call: JobCall) -> io::Result<Value> {
    let (run, generation) = (attempt.run, attempt.generation);
    let (cancel, job_id) = match call {
        JobCall::Start(input) => return start(shared, run, generation, attempt.nonce, input),
        JobCall::Status(job_id) => (false, job_id),
        JobCall::Cancel(job_id) => (true, job_id),
    };
    shared.write(|state| {
        let attempt = current(state, run, generation)?;
        let job = attempt
            .command_jobs
            .iter_mut()
            .find(|j| j.id == job_id)
            .ok_or_else(|| fail("Unknown job in this attempt"))?;
        if cancel && job.active() {
            let old = job.status.clone();
            job.status = JobStatus::Stopping;
            let summary = job.summary();
            if let Err(e) = crate::storage::save(state) {
                current(state, run, generation)?
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
    })?
}
fn start(shared: &Store, run: &str, generation: &str, nonce: &str, input: Input) -> io::Result<Value> {
    if crate::shutdown::requested() {
        return Err(fail("Control is stopping"));
    }
    if input.command.is_empty() || input.command.len() > 16000 || !(1..=1800).contains(&input.timeout_seconds) {
        return Err(fail(
            "Command must contain 1–16000 bytes and timeout must be 1–1800 seconds",
        ));
    }
    if nonce.is_empty() || nonce.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(fail("Invalid request nonce"));
    }
    let id: String = ring::digest::digest(&ring::digest::SHA256, nonce.as_bytes()).as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let image = {
        let mut state = shared.guard()?;
        let attempt = current(&mut state, run, generation)?;
        if let Some(job) = attempt.command_jobs.iter().find(|j| j.id == id) {
            return Ok(job.summary());
        }
        environment_image(attempt, &input.environment)?
    };
    let profile = if input.environment != "worker" {
        crate::worker_profile::Profile::Lite
    } else {
        crate::worker_profile::Profile::inspect(crate::engine::isolation(), &image)?
    };
    let mut state = shared.guard()?;
    let owner_deadline = state.runtime.runs.get(run).and_then(|r| {
        r.attempts.last().and_then(|a| {
            crate::attempt_control::deadline(&state, r, a).map(|limit| a.started_unix.saturating_add(limit))
        })
    });
    let started = now();
    let deadline_unix = effective_deadline(started, input.timeout_seconds, owner_deadline)?;
    let attempt = current(&mut state, run, generation)?;
    if attempt.turn_mailbox.as_ref().is_some_and(|m| m.accepted().is_some()) {
        return Err(fail("The attempt has handed off; no new workspace jobs may start"));
    }
    if environment_image(attempt, &input.environment)? != image {
        return Err(fail("Captured command image changed"));
    }
    if let Some(job) = attempt.command_jobs.iter().find(|j| j.id == id) {
        return Ok(job.summary());
    }
    if attempt.command_jobs.iter().any(Job::active) {
        return Err(fail("An active command already owns this attempt; await or cancel it"));
    }
    if attempt.command_jobs.len() >= MAX_JOBS {
        return Err(fail("Attempt command limit reached; continue in a new turn"));
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
        review_source: attempt.review_only && attempt.backend.agent().review_mount(),
        profile,
        deadline_unix,
        container_id: None,
        status: JobStatus::Preparing,
        output: String::new(),
        exit_code: None,
    };
    attempt.command_jobs.push(job.clone());
    if let Err(e) = crate::storage::save(&mut state) {
        current(&mut state, run, generation)?.command_jobs.pop();
        return Err(e);
    }
    drop(state);
    let thread_state = shared.clone();
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
        job.status = JobStatus::Failed;
        job.output = "Could not start command supervisor; no command was launched.".into();
        activity(shared, run, &job, true);
        return Err(e);
    }
    Ok(summary)
}
fn effective_deadline(now: u64, requested: u64, owner: Option<u64>) -> io::Result<u64> {
    let deadline = owner.map_or(now.saturating_add(requested), |end| {
        end.min(now.saturating_add(requested))
    });
    if deadline <= now {
        return Err(fail(
            "The attempt deadline has elapsed; use a persistent service for work that must outlive this attempt",
        ));
    }
    Ok(deadline)
}
fn create(isolation: &dyn Isolation, job: &Job) -> io::Result<String> {
    let workspace = job.workspace()?.canonicalize()?;
    if job.build_environment {
        crate::build_environment::check(isolation, &job.image, &workspace)?;
    }
    let meta = fs::symlink_metadata(&workspace)?;
    if !meta.is_dir() || meta.uid() == 0 {
        return Err(fail("Expected private non-root workspace"));
    }
    let path = workspace
        .to_str()
        .filter(|s| !s.contains([',', '\n', '\r']))
        .ok_or_else(|| fail("Invalid workspace mount"))?;
    let scratch = if job.read_only {
        use std::os::unix::fs::DirBuilderExt;
        let scratch = crate::runtime::workspace(&job.generation)
            .map_err(fail)?
            .parent()
            .unwrap()
            .join("command-scratch")
            .join(&job.id);
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&scratch)?;
        if !fs::symlink_metadata(&scratch)?.is_dir() {
            return Err(fail("Invalid command scratch directory"));
        }
        let scratch = scratch.canonicalize()?;
        Some(
            scratch
                .to_str()
                .filter(|s| !s.contains([',', '\n', '\r']))
                .ok_or_else(|| fail("Invalid scratch mount"))?
                .to_owned(),
        )
    } else {
        None
    };
    let owner = User {
        uid: meta.uid(),
        gid: meta.gid(),
    };
    isolation
        .create(&container(job, path, owner, scratch.as_deref()))
        .map(|id| id.as_str().to_owned())
}
/// The container for one command; `scratch` is the build target a read-only
/// command writes to.
fn container(job: &Job, path: &str, owner: User, scratch: Option<&str>) -> RunSpec {
    let name = job.name();
    let (pids, memory) = if job.build_environment { (512, 4) } else { (256, 2) };
    let access = if job.read_only {
        Access::ReadOnly
    } else {
        Access::ReadWrite
    };
    let spec = RunSpec {
        name: name.clone(),
        image: job.image.as_str().into(),
        argv: [
            "bash",
            "-c",
            "exec 2>&1; eval -- \"$1\"",
            "control-command",
            &job.command,
        ]
        .map(str::to_owned)
        .to_vec(),
        user: Some(owner),
        workdir: Some("/workspace".into()),
        root: if job.profile.readonly_root() {
            Access::ReadOnly
        } else {
            Access::ReadWrite
        },
        limits: Limits {
            pids: Some(pids),
            memory: Some(Size::Gib(memory)),
            cpus: Some(2),
            ..Limits::default()
        },
        network: Network::None,
        logs: Logs::Local {
            max_size: Size::Mib(1),
            max_files: 2,
        },
        ..RunSpec::default()
    }
    .label("dev.control.node", &job.node)
    .label("dev.control.attempt", &name)
    .label("dev.control.purpose", "command")
    .tmpfs(Tmpfs {
        exec: Exec::Allow,
        nodev: true,
        ..Tmpfs::new("/tmp", Size::Gib(1))
    })
    .tmpfs(Tmpfs {
        nodev: true,
        ..Tmpfs::new("/home/agent", Size::Mib(128)).owned(owner)
    })
    .env("HOME", "/home/agent")
    .env("CARGO_BUILD_JOBS", "2")
    .env(
        if job.read_only {
            "CARGO_TARGET_DIR"
        } else {
            "CONTROL_WORKSPACE_WRITABLE"
        },
        if job.read_only { "/build-target" } else { "1" },
    )
    .bind(path, "/workspace", access);
    match scratch {
        Some(scratch) => spec.bind(scratch, "/build-target", Access::ReadWrite),
        None => spec,
    }
}
/// The facts `verify` checks, from an engine observation.
#[cfg_attr(test, derive(Clone))]
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
impl Inspection {
    /// `None` when a label or the `/workspace` mount every command has is missing.
    fn of(observed: Observed) -> Option<Self> {
        let workspace = observed.mount("/workspace")?;
        Some(Self {
            source: workspace.source.clone(),
            writable: workspace.writable,
            node: observed.label("dev.control.node")?.to_owned(),
            attempt: observed.label("dev.control.attempt")?.to_owned(),
            id: observed.id.as_str().to_owned(),
            name: observed.name,
            image: observed.image_ref,
            network: observed.network,
            readonly_root: observed.read_only_root,
            running: observed.running,
            exit_code: observed.exit_code,
        })
    }
}
fn inspect(isolation: &dyn Isolation, job: &Job) -> io::Result<Inspection> {
    let observed = isolation.inspect(&[Target::Name(&job.name())])?;
    let inspection = <[Observed; 1]>::try_from(observed)
        .ok()
        .and_then(|[observed]| Inspection::of(observed))
        .ok_or_else(|| fail("Invalid command inspection"))?;
    verify(job, &inspection)?;
    Ok(inspection)
}
fn verify(job: &Job, inspection: &Inspection) -> io::Result<()> {
    let workspace = job.workspace()?.canonicalize()?;
    verify_identity(job, inspection, &workspace)
}
fn verify_identity(job: &Job, inspection: &Inspection, workspace: &std::path::Path) -> io::Result<()> {
    if job.container_id.as_ref().is_some_and(|id| id != &inspection.id)
        || inspection.name != format!("/{}", job.name())
        || inspection.node != job.node
        || inspection.attempt != job.name()
        || inspection.image != job.image
        || std::path::Path::new(&inspection.source) != workspace
        || inspection.writable == job.read_only
        || inspection.network != "none"
        || inspection.readonly_root != job.profile.readonly_root()
    {
        return Err(fail("Command container identity changed; replacement was not adopted"));
    }
    Ok(())
}
pub fn stop(isolation: &dyn Isolation, job: &Job) -> io::Result<()> {
    let workspace = job.workspace()?.canonicalize()?;
    crate::lifecycle::stop_resource_id(
        isolation,
        &job.name(),
        &job.node,
        &job.image,
        &workspace,
        job.container_id.as_deref(),
    )
    .map_err(fail)?;
    // A timed-out Docker start may still be in flight. Confirm removal, not just
    // a momentarily stopped state, so no late start can race publication.
    let select = job
        .container_id
        .as_deref()
        .map_or_else(|| Select::Name(job.name()), |id| Select::Id(id.into()));
    if !isolation.list(States::All, &select)?.is_empty() {
        return Err(fail("Command resource removal unconfirmed; checkpoint remains fenced"));
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
fn logs(isolation: &dyn Isolation, job: &Job) -> io::Result<String> {
    let id = job
        .container_id
        .as_deref()
        .ok_or_else(|| fail("Missing command container ID"))?;
    let mut child = isolation.logs(&id.into(), 200)?;
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
    let text = drain.join().map_err(|_| fail("Command log reader stopped"))?;
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
        .get_mut(run)?
        .attempts
        .iter_mut()
        .find(|a| a.workspace_id.as_deref() == Some(&job.generation))?
        .command_jobs
        .iter_mut()
        .find(|j| j.id == job.id)
}
fn activity(shared: &Store, run: &str, job: &Job, persist: bool) -> bool {
    shared
        .write(|state| {
            if let Some(stored) = live(state, run, job) {
                // A cancellation request wins over an in-flight progress update.
                let stopping = stored.status == JobStatus::Stopping && job.active();
                let status = if stopping { &JobStatus::Stopping } else { &job.status };
                if &stored.status == status
                    && stored.output == job.output
                    && stored.container_id == job.container_id
                    && stored.exit_code == job.exit_code
                {
                    return !persist || crate::storage::save(state).is_ok();
                }
                *stored = job.clone();
                if stopping {
                    stored.status = JobStatus::Stopping;
                }
            }
            crate::worker::record_activity_for(
                state,
                run,
                &job.generation,
                crate::usage::Activity {
                    id: format!("job-{}", job.id),
                    kind: "command_execution".into(),
                    text: job.command.chars().take(2000).collect(),
                    output: job.output.clone(),
                    status: job.status.to_string(),
                },
            );
            !persist || crate::storage::save(state).is_ok()
        })
        .unwrap_or(false)
}
fn should_stop(shared: &Store, run: &str, job: &Job) -> bool {
    if crate::shutdown::requested() || now() >= job.deadline_unix {
        return true;
    }
    shared
        .write(|state| {
            let active = current(state, run, &job.generation).is_ok();
            !active || live(state, run, job).is_none_or(|j| j.status == JobStatus::Stopping)
        })
        .unwrap_or(true)
}
fn supervise(shared: Store, run: String, mut job: Job) {
    let isolation = crate::engine::isolation();
    if should_stop(&shared, &run, &job) {
        job.status = if now() >= job.deadline_unix {
            JobStatus::TimedOut
        } else {
            JobStatus::Cancelled
        };
        activity(&shared, &run, &job, true);
        return;
    }
    match create(isolation, &job) {
        Ok(id) => {
            job.container_id = Some(id);
            // A created container cannot execute yet. Capture its immutable ID
            // durably before sending any start request to the daemon.
            if !activity(&shared, &run, &job, true) {
                job.status = if stop(isolation, &job).is_ok() {
                    JobStatus::Failed
                } else {
                    JobStatus::Stopping
                };
                job.output = "Command identity could not be saved; no start was requested.".into();
                activity(&shared, &run, &job, true);
                return;
            }
        }
        Err(_) => {
            // Launch can time out after Docker admitted it. Verify and stop by durable name.
            job.status = if stop(isolation, &job).is_ok() {
                JobStatus::Failed
            } else {
                JobStatus::Stopping
            };
            job.output = "Command launch failed; inspect node container diagnostics.".into();
            activity(&shared, &run, &job, true);
            return;
        }
    }
    if should_stop(&shared, &run, &job) {
        job.status = if stop(isolation, &job).is_ok() {
            if now() >= job.deadline_unix {
                JobStatus::TimedOut
            } else {
                JobStatus::Cancelled
            }
        } else {
            JobStatus::Stopping
        };
        activity(&shared, &run, &job, true);
        return;
    }
    if isolation.start(&job.container_id.as_deref().unwrap().into()).is_err() {
        job.status = if stop(isolation, &job).is_ok() {
            JobStatus::Failed
        } else {
            JobStatus::Stopping
        };
        job.output = "Command start failed; its admitted resource was stopped or remains fenced.".into();
        activity(&shared, &run, &job, true);
        return;
    }
    job.status = JobStatus::Running;
    activity(&shared, &run, &job, true);
    let mut persisted = Instant::now();
    let shutdown = crate::shutdown::token();
    let mut exited = exit_waiter(isolation, &job);
    loop {
        let inspection = inspect(isolation, &job);
        if let Ok(text) = logs(isolation, &job) {
            job.output = text;
        }
        let finish = match inspection {
            Ok(inspection) if !inspection.running => {
                job.exit_code = Some(inspection.exit_code);
                Some(if inspection.exit_code == 0 {
                    JobStatus::Completed
                } else {
                    JobStatus::Failed
                })
            }
            Ok(_) if should_stop(&shared, &run, &job) => Some(if now() >= job.deadline_unix {
                JobStatus::TimedOut
            } else {
                JobStatus::Cancelled
            }),
            Ok(_) => None,
            Err(_) => Some(JobStatus::Interrupted),
        };
        if let Some(status) = finish {
            // Never announce a finished writer until its exact resource is confirmed stopped.
            job.status = if stop(isolation, &job).is_ok() {
                status
            } else {
                JobStatus::Stopping
            };
            activity(&shared, &run, &job, true);
            if job.status != JobStatus::Stopping && !job.read_only {
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
fn exit_waiter(isolation: &dyn Isolation, job: &Job) -> Option<crate::child::Watched> {
    let child = isolation.wait(&job.container_id.as_deref()?.into()).ok()?;
    crate::child::watch(child).ok()
}

/// Before any checkpoint, wait for admitted launches and cancel every writer.
/// Failure fences publication rather than racing a background container.
pub fn quiesce(shared: &Store, run: &str, generation: &str) -> io::Result<()> {
    quiesce_until(shared, run, generation, Instant::now() + Duration::from_secs(20))
}
fn quiesce_until(shared: &Store, run: &str, generation: &str, deadline: Instant) -> io::Result<()> {
    loop {
        let active = shared
            .write(|state| -> io::Result<bool> {
                let attempt = state
                    .runtime
                    .runs
                    .get_mut(run)
                    .and_then(|r| {
                        r.attempts
                            .iter_mut()
                            .find(|a| a.workspace_id.as_deref() == Some(generation))
                    })
                    .ok_or_else(|| fail("Missing attempt command ownership"))?;
                let mut active = false;
                for job in &mut attempt.command_jobs {
                    if job.active() {
                        job.status = JobStatus::Stopping;
                        active = true;
                    }
                }
                Ok(active)
            })
            .map_err(|_| fail("State unavailable; command writers remain fenced"))??;
        if !active {
            return Ok(());
        }
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
