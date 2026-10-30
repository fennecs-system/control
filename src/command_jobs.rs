//! Durable, credential-free commands. Model requests select capabilities, never mounts.
use crate::{usage::Attempt, State};
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
    pub deadline_unix: u64,
    pub container_id: Option<String>,
    pub status: String,
    pub output: String,
    pub exit_code: Option<i64>,
}
impl Job {
    /// Reconciliation needs ownership fields, never retained command/output buffers.
    pub fn identity(&self)->Self {
        Self{id:self.id.clone(),generation:self.generation.clone(),image:self.image.clone(),node:self.node.clone(),command:String::new(),read_only:self.read_only,build_environment:self.build_environment,review_source:self.review_source,deadline_unix:self.deadline_unix,container_id:self.container_id.clone(),status:self.status.clone(),output:String::new(),exit_code:self.exit_code}
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
        json!({"job_id":self.id,"status":self.status,"output":self.output,"exit_code":self.exit_code,"output_limit_bytes":OUTPUT_BYTES})
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

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"control_workspace_start","description":"Start a credential-free offline workspace command and return a job handle immediately. Output streams into the chat. Poll status until complete; cancel before editing concurrently. Only one command writer per attempt. worker uses the captured worker image; build uses the operator-declared build environment. No host shell, network, credentials or image mutation. Jobs are cancelled when the attempt ends; await completion before returning your answer.","inputSchema":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":16000},"timeout_seconds":{"type":"integer","minimum":1,"maximum":1800,"default":600},"environment":{"type":"string","enum":["worker","build"],"default":"worker"}},"required":["command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}}),
        json!({"name":"control_workspace_status","description":"Read the bounded live output, status and exit code of a workspace job in this attempt. Status does not wait for completion.","inputSchema":{"type":"object","properties":{"job_id":{"type":"string"}},"required":["job_id"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
        json!({"name":"control_workspace_cancel","description":"Cancel an exact workspace job in this attempt. The job remains stopping until its verified container has exited.","inputSchema":{"type":"object","properties":{"job_id":{"type":"string"}},"required":["job_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}}),
    ]
}
pub fn known(name: &str) -> bool {
    [
        "control_workspace_start",
        "control_workspace_status",
        "control_workspace_cancel",
    ]
    .contains(&name)
}
fn environment_image(attempt: &Attempt, environment: &str) -> io::Result<String> {
    let image = match environment {
        "worker" => attempt.image.clone().ok_or_else(|| fail("Missing captured worker image"))?,
        "build" => std::env::var("CONTROL_BUILD_IMAGE").ok().or_else(|| {
            fs::read_to_string(crate::config::directory().ok()?.join("build-image-id")).ok()
        }).ok_or_else(|| fail("No declared build environment; build/load the Nix build-image and configure build-image-id"))?.trim().to_owned(),
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
pub fn call(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    nonce: &str,
    name: &str,
    input: &Value,
) -> io::Result<Value> {
    if name == "control_workspace_start" {
        let input: Input = serde_json::from_value(input.clone())
            .map_err(|_| fail("Expected command, timeout_seconds and declared environment"))?;
        return start(shared, run, generation, nonce, input);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Handle {
        job_id: String,
    }
    let handle: Handle =
        serde_json::from_value(input.clone()).map_err(|_| fail("Expected job_id"))?;
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let attempt = current(&mut state, run, generation)?;
    let job = attempt
        .command_jobs
        .iter_mut()
        .find(|j| j.id == handle.job_id)
        .ok_or_else(|| fail("Unknown job in this attempt"))?;
    if name == "control_workspace_cancel" && job.active() {
        let old = job.status.clone();
        job.status = "stopping".into();
        let summary = job.summary();
        if let Err(e) = crate::storage::save(&state) {
            current(&mut state, run, generation)?
                .command_jobs
                .iter_mut()
                .find(|j| j.id == handle.job_id)
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
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let attempt = current(&mut state, run, generation)?;
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
        image: environment_image(attempt, &input.environment)?,
        node: attempt
            .node_id
            .clone()
            .ok_or_else(|| fail("Missing captured node identity"))?,
        command: input.command,
        read_only: attempt.review_only,
        build_environment: input.environment == "build",
        review_source: attempt.review_only && attempt.backend == crate::backend::Backend::Codex,
        deadline_unix: now().saturating_add(input.timeout_seconds),
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
    let summary = job.summary();
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
fn create(job: &Job) -> io::Result<String> {
    let workspace = job.workspace()?.canonicalize()?;
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
        "--read-only",
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
        crate::runtime::docker_query(&arguments.iter().map(String::as_str).collect::<Vec<_>>())?;
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
    let result = crate::runtime::docker_query(&["inspect", "--format", INSPECT, &job.name()])?;
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
        || !inspection.readonly_root
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
    if !crate::runtime::docker_query(&["ps", "--all", "--quiet", "--filter", &filter])?
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
    let mut child = crate::claude::docker()
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
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
    if crate::runtime::docker_query(&["start", job.container_id.as_deref().unwrap()]).is_err() {
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
        std::thread::sleep(Duration::from_millis(250));
    }
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
mod tests {
    use super::*;
    #[test]
    fn output_is_bounded_utf8_and_strips_terminal_controls() {
        let output = bounded_output(&format!("\x1b{}\0", "é".repeat(20000)));
        assert!(output.len() <= OUTPUT_BYTES + 30);
        assert!(output.starts_with("[earlier output omitted]"));
        assert!(!output.contains(['\x1b', '\0']));
    }
    #[test]
    fn unknown_command_options_cannot_set_mounts_or_images() {
        assert!(
            serde_json::from_value::<Input>(json!({"command":"true","image":"ubuntu"})).is_err()
        );
        assert!(serde_json::from_value::<Input>(json!({"command":"true","mount":"/"})).is_err());
    }
    fn fixture() -> (State, String, String) {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "fixture", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        let generation = format!("{}-attempt-1", run.id);
        run.attempts.push(Attempt {
            workspace_id: Some(generation.clone()),
            ..Default::default()
        });
        let id = run.id.clone();
        (state, id, generation)
    }
    #[test]
    fn status_cannot_cross_attempt_or_job_boundaries() {
        let (state, run, generation) = fixture();
        let state = Arc::new(Mutex::new(state));
        assert!(call(
            &state,
            &run,
            "stale",
            "nonce",
            "control_workspace_status",
            &json!({"job_id":"anything"})
        )
        .unwrap_err()
        .to_string()
        .contains("inactive attempt"));
        assert!(call(
            &state,
            &run,
            &generation,
            "nonce",
            "control_workspace_status",
            &json!({"job_id":"anything"})
        )
        .unwrap_err()
        .to_string()
        .contains("Unknown job"));
        state.lock().unwrap().runtime.runs[0].status = "cancelled".into();
        assert!(call(
            &state,
            &run,
            &generation,
            "nonce",
            "control_workspace_status",
            &json!({"job_id":"anything"})
        )
        .unwrap_err()
        .to_string()
        .contains("not active"));
    }
    #[test]
    fn checkpoint_barrier_fails_closed_for_unconfirmed_writer() {
        let (mut state, run, generation) = fixture();
        state.runtime.runs[0].attempts[0].command_jobs.push(Job {
            id: "fixture".into(),
            generation: generation.clone(),
            image: "fixture".into(),
            node: "fixture".into(),
            command: "sleep".into(),
            read_only: false,
            build_environment: false,
            review_source: false,
            deadline_unix: now() + 100,
            container_id: None,
            status: "preparing".into(),
            output: String::new(),
            exit_code: None,
        });
        let state = Arc::new(Mutex::new(state));
        assert!(quiesce_until(&state, &run, &generation, Instant::now())
            .unwrap_err()
            .to_string()
            .contains("checkpoint remains unchanged"));
        assert_eq!(
            state.lock().unwrap().runtime.runs[0].attempts[0].command_jobs[0].status,
            "stopping"
        );
    }
    #[test]
    fn immutable_job_identity_and_isolation_are_checked_before_adoption() {
        let job = Job {
            id: "fixture".into(),
            generation: "generation".into(),
            image: "pinned-image".into(),
            node: "node".into(),
            command: "true".into(),
            read_only: true,
            build_environment: false,
            review_source: false,
            deadline_unix: 0,
            container_id: Some("b".repeat(64)),
            status: "running".into(),
            output: String::new(),
            exit_code: None,
        };
        let inspection = Inspection {
            id: "b".repeat(64),
            name: format!("/{}", job.name()),
            node: job.node.clone(),
            attempt: job.name(),
            image: job.image.clone(),
            source: "/fixture".into(),
            writable: false,
            network: "none".into(),
            readonly_root: true,
            running: true,
            exit_code: 0,
        };
        let expected = std::path::Path::new("/fixture");
        let mut retained=job.clone();retained.output="diagnostic".repeat(2000);
        let identity=retained.identity();
        assert!(identity.command.is_empty()&&identity.output.is_empty());
        assert_eq!(identity.command.capacity()+identity.output.capacity(),0);
        assert!(verify_identity(&identity, &inspection, expected).is_ok());
        let mut changed = inspection.clone();
        changed.id = "a".repeat(64);
        assert!(verify_identity(&identity,&changed,expected).is_err());
        assert!(verify_identity(&job, &changed, expected).is_err());
        let mut changed = inspection.clone();
        changed.source = "/host".into();
        assert!(verify_identity(&job, &changed, expected).is_err());
        let mut changed = inspection.clone();
        changed.image = "other-image".into();
        assert!(verify_identity(&job, &changed, expected).is_err());
        let mut changed = inspection.clone();
        changed.writable = true;
        assert!(verify_identity(&job, &changed, expected).is_err());
        let mut changed = inspection.clone();
        changed.network = "bridge".into();
        assert!(verify_identity(&job, &changed, expected).is_err());
        let mut changed = inspection.clone();
        changed.readonly_root = false;
        assert!(verify_identity(&job, &changed, expected).is_err());
    }
    #[test]
    #[ignore = "requires isolated CONTROL_STATE_DIR/CONTROL_CONFIG_DIR under /private/tmp/control-command-qa and CONTROL_QA_IMAGE; no credentials/model calls"]
    fn live_commands_stream_past_short_deadline_cancel_and_reconcile_exact_restart_identity() {
        let state_dir = crate::config::state_directory().unwrap();
        assert!(state_dir
            .parent()
            .is_some_and(|p| p.parent() == Some(std::path::Path::new("/private/tmp"))
                && p.file_name()
                    .is_some_and(|s| s.to_string_lossy().starts_with("control-command-qa."))));
        crate::storage::initialize().unwrap();
        let image = std::env::var("CONTROL_QA_IMAGE").unwrap();
        assert!(crate::runtime::pinned_image(&image));
        let (mut state, run, generation) = fixture();
        let workspace = crate::runtime::workspace(&generation).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(workspace.parent().unwrap().join("baseline")).unwrap();
        let attempt = &mut state.runtime.runs[0].attempts[0];
        attempt.image = Some(image);
        attempt.node_id = Some(crate::config::node_id().unwrap());
        let state = Arc::new(Mutex::new(state));
        struct Cleanup(Arc<Mutex<State>>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let jobs = self
                    .0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .runtime
                    .runs[0]
                    .attempts
                    .iter()
                    .flat_map(|a| a.command_jobs.clone())
                    .collect::<Vec<_>>();
                for job in jobs {
                    let _ = stop(&job);
                }
            }
        }
        let _cleanup = Cleanup(state.clone());
        let command="set -eu; test ! -e /home/agent/.codex/auth.json; test ! -e /home/agent/.claude/.credentials.json; test ! -S /var/run/docker.sock; test ! -S /nix/var/nix/daemon-socket/socket; test -z \"${OPENAI_API_KEY-}${ANTHROPIC_API_KEY-}\"; if touch /nix/store/forbidden-write 2>/dev/null; then exit 42; fi; if timeout 1 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then exit 43; fi; printf 'early-output\n'; sleep 27; printf late-output; printf verified > result";
        let input = Input {
            command: command.into(),
            timeout_seconds: 60,
            environment: "worker".into(),
        };
        let initial = start(&state, &run, &generation, "long-fixture", input).unwrap();
        let id = initial["job_id"].as_str().unwrap();
        let until = Instant::now() + Duration::from_secs(45);
        let mut saw_live = false;
        loop {
            let value = call(
                &state,
                &run,
                &generation,
                "status",
                "control_workspace_status",
                &json!({"job_id":id}),
            )
            .unwrap();
            if value["status"] == "running"
                && value["output"].as_str().unwrap().contains("early-output")
            {
                saw_live = true;
                let saw_activity = state.lock().unwrap().runtime.runs[0].attempts[0]
                    .activity
                    .iter()
                    .any(|a| a.output.contains("early-output"));
                assert!(saw_activity);
            }
            if value["status"] == "completed" {
                assert_eq!(value["exit_code"], 0);
                assert!(value["output"].as_str().unwrap().contains("late-output"));
                break;
            }
            assert!(Instant::now() < until, "command did not finish: {value}");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(saw_live);
        assert_eq!(fs::read(workspace.join("result")).unwrap(), b"verified");
        let command = "printf cancelling; sleep 100; printf bad > after-cancel";
        let handle = start(
            &state,
            &run,
            &generation,
            "cancel-fixture",
            Input {
                command: command.into(),
                timeout_seconds: 180,
                environment: "worker".into(),
            },
        )
        .unwrap();
        assert!(start(
            &state,
            &run,
            &generation,
            "other-writer",
            Input {
                command: "true".into(),
                timeout_seconds: 10,
                environment: "worker".into()
            }
        )
        .is_err());
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let value = call(
                &state,
                &run,
                &generation,
                "status",
                "control_workspace_status",
                &json!({"job_id":handle["job_id"]}),
            )
            .unwrap();
            if value["status"] == "running" {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(50));
        }
        call(
            &state,
            &run,
            &generation,
            "cancel",
            "control_workspace_cancel",
            &json!({"job_id":handle["job_id"]}),
        )
        .unwrap();
        quiesce(&state, &run, &generation).unwrap();
        assert!(!workspace.join("after-cancel").exists());
        let handle = start(
            &state,
            &run,
            &generation,
            "deadline-fixture",
            Input {
                command: "printf 'deadline-output\n'; sleep 100".into(),
                timeout_seconds: 1,
                environment: "worker".into(),
            },
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let value = call(
                &state,
                &run,
                &generation,
                "status",
                "control_workspace_status",
                &json!({"job_id":handle["job_id"]}),
            )
            .unwrap();
            if value["status"] == "timed_out" {
                break;
            }
            assert!(Instant::now() < until, "deadline was not enforced: {value}");
            std::thread::sleep(Duration::from_millis(50));
        }
        {
            let mut locked = state.lock().unwrap();
            let attempt = &mut locked.runtime.runs[0].attempts[0];
            attempt.review_only = true;
            attempt.backend = crate::backend::Backend::Claude;
        }
        let handle=start(&state,&run,&generation,"readonly-fixture",Input{command:"set -eu; if touch /workspace/forbidden-judge-write 2>/dev/null; then exit 42; fi; test \"$CARGO_TARGET_DIR\" = /build-target; printf scratch > /build-target/allowed-judge-write; printf 'readonly-verified\n'".into(),timeout_seconds:10,environment:"worker".into()}).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let value = call(
                &state,
                &run,
                &generation,
                "status",
                "control_workspace_status",
                &json!({"job_id":handle["job_id"]}),
            )
            .unwrap();
            if value["status"] == "completed" {
                assert_eq!(value["exit_code"], 0);
                assert!(value["output"]
                    .as_str()
                    .unwrap()
                    .contains("readonly-verified"));
                break;
            }
            assert!(
                Instant::now() < until,
                "read-only verification failed: {value}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!workspace.join("forbidden-judge-write").exists());
        state.lock().unwrap().runtime.runs[0].attempts[0].review_only = false;
        let handle = start(
            &state,
            &run,
            &generation,
            "restart-fixture",
            Input {
                command: "printf restart-output; sleep 100".into(),
                timeout_seconds: 180,
                environment: "worker".into(),
            },
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let job = loop {
            let locked = state.lock().unwrap();
            let job = locked.runtime.runs[0].attempts[0]
                .command_jobs
                .iter()
                .find(|j| j.id == handle["job_id"])
                .unwrap()
                .clone();
            if job.container_id.is_some() {
                break job;
            }
            assert!(Instant::now() < until);
            drop(locked);
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut replacement = job.clone();
        replacement.container_id = Some("a".repeat(64));
        assert!(stop(&replacement)
            .unwrap_err()
            .to_string()
            .contains("identity changed"));
        assert!(inspect(&job).unwrap().running);
        // Reconstruct owner records exactly as a restarted process would. Inactive attempts
        // retain their independent captured image and immutable container ID.
        let recovered: State =
            serde_json::from_slice(&serde_json::to_vec(&*state.lock().unwrap()).unwrap()).unwrap();
        let mut recovered = recovered;
        recovered.runtime.runs[0].status = "interrupted".into();
        crate::lifecycle::reconcile(&crate::lifecycle::owners(&recovered.runtime.runs)).unwrap();
        assert!(inspect(&job).is_err());
        // Let the original supervisor observe loss and release its final state fence.
        quiesce(&state, &run, &generation).unwrap();
        let handle = start(
            &state,
            &run,
            &generation,
            "stale-fixture",
            Input {
                command: "printf 'stale-start\n'; sleep 100; touch after-stale".into(),
                timeout_seconds: 180,
                environment: "worker".into(),
            },
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let value = call(
                &state,
                &run,
                &generation,
                "status",
                "control_workspace_status",
                &json!({"job_id":handle["job_id"]}),
            )
            .unwrap();
            if value["status"] == "running" {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(50));
        }
        state.lock().unwrap().runtime.runs[0]
            .attempts
            .push(Attempt {
                workspace_id: Some(format!("{generation}-retry")),
                ..Default::default()
            });
        assert!(call(
            &state,
            &run,
            &generation,
            "stale-status",
            "control_workspace_status",
            &json!({"job_id":handle["job_id"]})
        )
        .is_err());
        quiesce(&state, &run, &generation).unwrap();
        let locked = state.lock().unwrap();
        assert!(locked.runtime.runs[0].attempts[1].activity.is_empty());
        assert_eq!(
            locked.runtime.runs[0].attempts[0]
                .command_jobs
                .last()
                .unwrap()
                .status,
            "cancelled"
        );
        drop(locked);
        assert!(!workspace.join("after-stale").exists());
    }
}
