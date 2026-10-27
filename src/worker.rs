use crate::identity::{self, Action, Actor, Workspace};
use crate::{runtime, storage, State};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PERMISSIONS: [&str; 6] = [
    "default_permissions=\"control\"",
    "permissions.control.extends=\":workspace\"",
    "permissions.control.filesystem={\"/home/agent/.codex\"=\"deny\",\"/sys\"=\"deny\"}",
    "permissions.control.network.enabled=false",
    "approval_policy=\"never\"",
    "cli_auth_credentials_store=\"file\"",
];

struct Credentials {
    path: PathBuf,
    uid: u32,
    gid: u32,
}
impl Credentials {
    fn locate() -> Result<Self, &'static str> {
        let path = std::env::var_os("CONTROL_CODEX_AUTH_FILE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("CODEX_HOME").map(|p| PathBuf::from(p).join("auth.json")))
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex/auth.json")))
            .ok_or("Cannot locate Codex auth.json. Sign in with codex login on the host.")?;
        Self::validate(&path)
    }

    // Inspect metadata only. Only Codex inside the worker consumes credential contents.
    fn validate(path: &Path) -> Result<Self, &'static str> {
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

fn mount_path(path: &Path) -> Result<&str, &'static str> {
    path.to_str()
        .filter(|p| path.is_absolute() && !p.contains([',', '\n', '\r']))
        .ok_or("Container mount paths must be absolute UTF-8 paths without commas or newlines.")
}

fn container_command(
    image: &str,
    name: &str,
    workspace: &Path,
    auth: &Credentials,
) -> Result<Command, &'static str> {
    let mount = format!("type=bind,src={},dst=/workspace", mount_path(workspace)?);
    let credential_mount = format!(
        "type=bind,src={},dst=/home/agent/.codex/auth.json,readonly",
        mount_path(&auth.path)?
    );
    let seccomp = fs::canonicalize("hosting/worker-seccomp.json")
        .map_err(|_| "Worker seccomp profile is missing.")?;
    let seccomp = format!("seccomp={}", mount_path(&seccomp)?);
    let user = format!("{}:{}", auth.uid, auth.gid);
    let home = format!(
        "/home/agent:rw,nosuid,uid={},gid={},size=64m",
        auth.uid, auth.gid
    );
    let codex_home = home.replacen("/home/agent:", "/home/agent/.codex:", 1);
    let mut command = Command::new("docker");
    command
        .args([
            "run",
            "--interactive",
            "--rm",
            "--pull=never",
            "--name",
            name,
            "--user",
            &user,
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--security-opt",
            &seccomp,
            // Docker's masked proc entries prevent unprivileged bubblewrap from mounting proc.
            // Credential access is denied by the inner sandbox; no outer capabilities are granted.
            "--security-opt=systempaths=unconfined",
            "--pids-limit=256",
            "--memory=2g",
            "--cpus=2",
            "--tmpfs",
            "/tmp:rw,nosuid,size=512m",
            "--tmpfs",
            &home,
            "--tmpfs",
            &codex_home,
            "--mount",
            &mount,
            "--mount",
            &credential_mount,
            "--workdir",
            "/workspace",
            "--env",
            "HOME=/home/agent",
            "--env",
            "CODEX_HOME=/home/agent/.codex",
            image,
        ])
        .env_clear();
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
    Ok(command)
}

fn permissions(command: &mut Command) {
    for setting in PERMISSIONS {
        command.args(["-c", setting]);
    }
}

fn sandbox_probe(
    image: &str,
    root: &Path,
    auth: &Credentials,
    name: &str,
) -> Result<(), &'static str> {
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
    let mut command = container_command(image, name, &workspace, &fake)?;
    command.args(["codex", "sandbox", "-P", "control"]);
    permissions(&mut command);
    // PID 1 must be the inner execution supervisor, not the account-bearing harness.
    // Its memory is naturally readable by its own child; the outer harness is outside this PID namespace.
    command.args(["--", "bash", "-c", "set -eu; for path in /home/agent/.codex/auth.json /proc/self/root/home/agent/.codex/auth.json /proc/1/root/home/agent/.codex/auth.json; do if cat \"$path\" >/dev/null 2>&1; then exit 41; fi; done; IFS= read -r -d '' supervisor < /proc/1/cmdline; test \"$supervisor\" = codex-linux-sandbox || exit 42; printf verified > /workspace/probe-result"])
        .stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot start sandbox verification.")?;
    let result = wait(&mut child, Duration::from_secs(30));
    if result.is_err() {
        remove_container(name);
    }
    match child.try_wait().ok().flatten().and_then(|s| s.code()) {
        Some(41) => return Err("Credential canary was readable through a sandbox path. No account credentials were mounted."),
        Some(42) => return Err("The sandbox did not isolate the account-bearing harness in a separate PID namespace. No account credentials were mounted."),
        _ => {}
    }
    if !result.unwrap_or(false)
        || fs::read(workspace.join("probe-result")).ok().as_deref() != Some(b"verified")
    {
        return Err("Credential isolation verification failed. No account credentials were mounted. Check the pinned Codex image and container sandbox support.");
    }
    Ok(())
}

fn remove_container(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "--force", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

const UNKNOWN_FAILURE: &str = "Codex container exited unsuccessfully. Check host account authentication and the pinned worker configuration. No host fallback was attempted.";

fn worker_failure(bytes: &[u8]) -> &'static str {
    // Classify captured diagnostics internally; never emit raw errors or credential-bearing text.
    let message = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if message.contains("not supported") || message.contains("model_not_found") {
        "The signed-in account does not support the requested Codex model or runtime feature."
    } else if message.contains("read-only file system")
        || message.contains("refresh token")
        || message.contains("token refresh")
    {
        "Codex could not use or refresh the read-only account session. Refresh the host login and submit a new run. The credential mount was not made writable."
    } else if message.contains("permission denied") {
        "Codex encountered a filesystem permission denial. Check credential ownership, ephemeral state, and sandbox compatibility."
    } else if message.contains("401")
        || message.contains("unauthorized")
        || message.contains("not logged in")
    {
        "Codex account authentication was rejected. Refresh the host login and submit a new run."
    } else if message.contains("certificate") || message.contains("ssl") {
        "Codex could not validate the provider TLS connection. Check the Nix worker certificate configuration."
    } else if message.contains("unexpected argument")
        || message.contains("error parsing")
        || message.contains("unknown variant")
        || message.contains("unknown permission")
    {
        "The pinned Codex worker rejected its runtime configuration. Check CLI and permission-profile compatibility."
    } else if message.contains("quota")
        || message.contains("usage limit")
        || message.contains("rate limit")
    {
        "The signed-in Codex account reached a usage or rate limit."
    } else if message.contains("stream disconnected")
        || message.contains("error sending request")
        || message.contains("connection")
    {
        "Codex could not complete its provider connection. Check container network connectivity and account availability."
    } else {
        UNKNOWN_FAILURE
    }
}

fn capture_diagnostics(
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

fn wait(child: &mut Child, limit: Duration) -> std::io::Result<bool> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Worker deadline reached",
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
pub fn start(state: Arc<Mutex<State>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        ingest_discord(&state);
        let task = {
            let mut s = state.lock().unwrap();
            let Some(run) = s.runtime.runs.iter_mut().find(|r| r.status == "queued") else {
                continue;
            };
            run.status = "running".into();
            let task = (
                run.id.clone(),
                run.objective.clone(),
                run.actor.clone(),
                run.action,
            );
            if storage::save(&s).is_err() {
                continue;
            }
            task
        };
        let outcome = execute(&task.0, &task.1, &task.2, task.3);
        let mut s = state.lock().unwrap();
        if let Some(r) = s.runtime.runs.iter_mut().find(|r| r.id == task.0) {
            match outcome {
                Ok(summary) => {
                    r.status = "completed".into();
                    r.result = summary;
                }
                Err((status, message)) => {
                    r.status = status.into();
                    r.result = message;
                }
            }
        }
        if storage::save(&s).is_err() {
            eprintln!("Could not persist worker result.");
        }
    });
}
fn ingest_discord(state: &Arc<Mutex<State>>) {
    let Ok(entries) = fs::read_dir(".control/discord-inbox") else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json")
            || entry.file_type().is_ok_and(|t| t.is_symlink())
        {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.len() > 16000 {
            continue;
        }
        let Ok(message) = serde_json::from_slice::<crate::discord::Incoming>(&bytes) else {
            continue;
        };
        if !message.direct {
            continue;
        }
        let valid = if message.direct {
            crate::discord::direct_command(&message.objective).is_some()
        } else {
            crate::discord::command(&format!("!control {}", message.objective)).is_some()
        };
        if !valid {
            continue;
        }
        let mut s = state.lock().unwrap();
        let message_id = message.message_id.to_string();
        if s.discord_seen.contains(&message_id) {
            continue;
        }
        let actor = message.actor();
        if s.runtime
            .enqueue_for(
                message.objective.clone(),
                if message.action == Action::Improve {
                    "self-improvement"
                } else {
                    "task"
                },
                &format!("discord:{}", message.message_id),
                actor,
                message.action,
            )
            .is_err()
        {
            continue;
        }
        if !message.direct {
            s.collective.room.push((
                "general".into(),
                format!("Discord / {}: {}", message.user_id, message.objective),
            ));
        }
        s.discord_seen.push(message_id);
        if storage::save(&s).is_err() {
            eprintln!("Could not persist Discord task.");
        }
    }
}
fn execute(
    id: &str,
    objective: &str,
    actor: &Actor,
    action: Action,
) -> Result<String, (&'static str, String)> {
    let blocked = |m: &str| ("blocked", m.to_owned());
    let failed = |m: &str| ("failed", m.to_owned());
    let scope = identity::authorize(actor, action).map_err(&blocked)?;
    let image = runtime::worker_image().ok_or_else(|| blocked("No pinned worker image. Configure CONTROL_WORKER_IMAGE or .control/worker-image-id with the Nix-built image's sha256 ID."))?;
    if !runtime::engine_ready() {
        return Err(blocked("Linux container engine unavailable. Start Docker on this laptop; Control will not fall back to host execution."));
    }
    let auth = Credentials::locate().map_err(&blocked)?;
    let workspace = runtime::workspace(id);
    let baseline = workspace.parent().unwrap().join("baseline");
    if baseline.exists() || workspace.exists() {
        return Err(blocked("A source snapshot already exists for this run. Inspect its artifacts; submit a new run to avoid overwriting evidence."));
    }
    prepare_workspace(scope, &baseline, &workspace)
        .map_err(|_| failed("Could not prepare the authorized workspace."))?;
    let absolute =
        fs::canonicalize(&workspace).map_err(|_| failed("Could not resolve worker workspace."))?;
    sandbox_probe(
        &image,
        absolute.parent().unwrap(),
        &auth,
        &format!("control-{id}-probe"),
    )
    .map_err(&blocked)?;
    let container = format!("control-{id}");
    let mut command = container_command(&image, &container, &absolute, &auth).map_err(&blocked)?;
    command.args([
        "codex",
        "exec",
        "--ignore-user-config",
        "--ephemeral",
        "--skip-git-repo-check",
        "--color",
        "never",
        "--output-last-message",
        "/workspace/.control-result",
    ]);
    permissions(&mut command);
    command.arg("-");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| failed("Could not start the container worker."))?;
    let diagnostics = capture_diagnostics(child.stderr.take().unwrap());
    let output_diagnostics = capture_diagnostics(child.stdout.take().unwrap());
    let context = if scope == Workspace::ControlSource {
        "You are working on an authorized, isolated source snapshot of Control."
    } else {
        "You are working in an isolated user task workspace. Control source, configuration, and operator state are not provided. You have no authority to adapt or administer Control."
    };
    let prompt = format!("{context} Use Rust/WASM for application code and Nix only for infrastructure. Never inspect credentials or .env files. Do not change authority policy, deploy, or contact Discord. Produce a concise final report with changes, checks, and limitations. Objective:\n{objective}");
    if child
        .stdin
        .take()
        .is_none_or(|mut stdin| stdin.write_all(prompt.as_bytes()).is_err())
    {
        let _ = child.kill();
        let _ = child.wait();
        remove_container(&container);
        return Err(failed("Could not deliver the objective."));
    }
    let result = wait(&mut child, Duration::from_secs(600));
    // Killing the CLI alone may leave the daemon-owned container running.
    if result.is_err() {
        remove_container(&container);
        return Err(failed(
            "Worker stopped after its deadline or a process error.",
        ));
    }
    if !result.unwrap_or(false) {
        let stderr = diagnostics.join().unwrap_or(UNKNOWN_FAILURE);
        let stdout = output_diagnostics.join().unwrap_or(UNKNOWN_FAILURE);
        return Err(failed(if stderr == UNKNOWN_FAILURE {
            stdout
        } else {
            stderr
        }));
    }
    let _ = diagnostics.join();
    let _ = output_diagnostics.join();
    let output = read_report(&workspace.join(".control-result"))
        .unwrap_or_else(|_| "Worker finished without a safe final report.".into());
    let summary: String = output.chars().take(16000).collect();
    Ok(format!("{summary}\n\nReview workspace: {}\nBaseline: {}\nNo changes have been applied to the live source. Checks in the report are worker-reported, not independently verified.",workspace.display(),baseline.display()))
}

fn read_report(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    // The container has exited: reject agent-created symlinks before any host read.
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other(
            "Worker report must be a regular file.",
        ));
    }
    let mut output = String::new();
    fs::File::open(path)?
        .take(64000)
        .read_to_string(&mut output)?;
    Ok(output)
}

/// Exercise the real worker path without printing account information or model output.
pub fn check() -> std::io::Result<()> {
    let id = format!(
        "auth-check-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    execute(&id,
        "Run a shell command that writes exactly control-account-check-ok to /workspace/connection-check. Do not inspect credentials. Then reply with one short confirmation.",
        &Actor::LocalOperator, Action::Task)
        .map_err(|(_, message)| std::io::Error::other(message))?;
    let marker = runtime::workspace(&id).join("connection-check");
    if read_report(&marker)?.trim() != "control-account-check-ok" {
        return Err(std::io::Error::other(
            "Codex returned without the expected sandbox execution artifact.",
        ));
    }
    println!("Codex account authentication and sandboxed tool execution verified. Credentials were mounted read-only; no credential contents were displayed.");
    Ok(())
}
fn prepare_workspace(
    scope: Workspace,
    baseline: &std::path::Path,
    workspace: &std::path::Path,
) -> std::io::Result<()> {
    if scope == Workspace::ControlSource {
        runtime::snapshot(std::path::Path::new("."), baseline)?;
        runtime::snapshot(baseline, workspace)
    } else {
        fs::create_dir_all(workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_workspace_has_no_control_source_or_configuration() {
        let root = std::env::temp_dir().join(format!(
            "control-user-scope-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let baseline = root.join("baseline");
        let workspace = root.join("workspace");
        prepare_workspace(Workspace::User, &baseline, &workspace).unwrap();
        assert!(!baseline.exists());
        assert_eq!(fs::read_dir(&workspace).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn untrusted_jobs_are_rejected_before_container_or_credential_access() {
        let result = execute("test", "modify Control", &Actor::Unknown, Action::Improve);
        assert!(matches!(result, Err(("blocked", _))));
    }

    #[test]
    fn credentials_are_readonly_mounts_and_never_environment_values() {
        let auth = Credentials {
            path: PathBuf::from("/private/auth.json"),
            uid: 502,
            gid: 20,
        };
        let command = container_command(
            "sha256:test",
            "control-test",
            Path::new("/private/workspace"),
            &auth,
        )
        .unwrap();
        let args: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
        assert!(args.contains(
            &"type=bind,src=/private/auth.json,dst=/home/agent/.codex/auth.json,readonly"
        ));
        assert!(!args
            .iter()
            .any(|s| s.contains("API_KEY") || s.contains("ACCESS_TOKEN")));
        assert!(!command.get_envs().any(|(name, _)| name == "CODEX_API_KEY"
            || name == "OPENAI_API_KEY"
            || name == "CODEX_ACCESS_TOKEN"));
        assert!(!args.contains(&"--privileged"));
        assert!(args.contains(&"--interactive"));
        assert!(!args.contains(&"--security-opt=seccomp=unconfined"));
        assert!(PERMISSIONS.contains(&"approval_policy=\"never\""));
    }

    #[test]
    fn diagnostics_return_fixed_messages_without_echoing_secret_text() {
        let failure = worker_failure(b"401 Unauthorized bearer fake-secret-for-test");
        assert!(failure.contains("authentication was rejected"));
        assert!(!failure.contains("fake-secret-for-test"));
        assert_eq!(
            worker_failure(b"unexpected raw text fake-secret-for-test"),
            UNKNOWN_FAILURE
        );
    }

    #[test]
    fn mounts_reject_csv_injection_and_reports_reject_symlinks() {
        assert!(mount_path(Path::new("/tmp/auth,readonly=false")).is_err());
        assert!(mount_path(Path::new("relative/auth.json")).is_err());
        let root =
            std::env::temp_dir().join(format!("control-credentials-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("auth.json");
        fs::write(&file, b"harmless-canary").unwrap();
        use std::os::unix::fs::{symlink, PermissionsExt};
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Credentials::validate(&file).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Credentials::validate(&file).is_ok());
        let link = root.join("report-link");
        symlink(&file, &link).unwrap();
        assert!(Credentials::validate(&link).is_err());
        assert!(read_report(&link).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
