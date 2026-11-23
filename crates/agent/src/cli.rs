//! What the CLI backends whose only tools are Control's share: the base
//! container, the offline isolation probe, and one turn from probe through
//! launch to the supervised reply. Each backend supplies data and its own
//! container; nothing here names a backend.
use crate::{
    Cancelled, Control, Failure, Host, Node, Outcome, SANDBOX_PROBE_WAIT, Stop, Turn, mailbox_wake::MailboxWake,
};
use control_ai::{Activity, Tokens, Usage};
use control_isolation::{Isolation, Limits, RunSpec, Size, Stream, Streams, Tmpfs, User};
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub mod canary;

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}

/// The label that keeps a check or login container out of run reconciliation.
pub const CHECK_LABEL: &str = "dev.control.purpose=check";
/// Labels a container as a check, which its command supervises, or as part
/// of a run, which reconciliation owns.
pub fn label_purpose(spec: RunSpec, check: bool) -> RunSpec {
    let (name, value) = if check { CHECK_LABEL } else { "dev.control.purpose=run" }
        .split_once('=')
        .unwrap();
    spec.label(name, value)
}

/// A host path the engine can mount: absolute UTF-8 without commas or newlines.
pub fn checked(path: &Path) -> io::Result<&Path> {
    path.to_str()
        .filter(|s| path.is_absolute() && !s.contains([',', '\n', '\r']))
        .map(|_| path)
        .ok_or_else(|| fail("Unsafe container mount path"))
}

/// The shared CLI container; callers add the image, mounts and program.
pub fn base(name: &str, node: &str, owner: User) -> RunSpec {
    // Login and offline checks are supervised by their command, not run reconciliation.
    let check = [
        "control-codex-login-",
        "control-claude-login-",
        "control-claude-probe-",
        "control-grok-login-",
        "control-grok-probe-",
        "control-antigravity-login-",
        "control-antigravity-probe-",
        "control-summary-",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix));
    let spec = RunSpec {
        name: name.into(),
        user: Some(owner),
        workdir: Some("/workspace".into()),
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
    label_purpose(spec, check)
        .tmpfs(Tmpfs::new("/tmp", Size::Mib(128)))
        // Credentials are mounted under this home. Docker Desktop and OrbStack
        // present a bind-mounted file as owned by whichever uid reads it, so
        // only the home's own mode keeps other uids out.
        .tmpfs(Tmpfs::new("/home/agent", Size::Mib(64)).private(owner))
        .env("HOME", "/home/agent")
        .env("CLAUDE_CONFIG_DIR", "/home/agent/.claude")
}

/// Every secret of `spec` lies under a private tmpfs (mode 0700) owned by the
/// uid the program runs as, so no other uid in the container reaches it.
/// Otherwise the first secret that does not, by name.
pub fn secrets_private(spec: &RunSpec) -> Result<(), String> {
    let covered = |target: &str| {
        spec.tmpfs.iter().any(|fs| {
            fs.mode == Some(0o700)
                && fs.owner.is_some()
                && fs.owner == spec.user
                && Path::new(target)
                    .strip_prefix(&fs.target)
                    .is_ok_and(|rest| !rest.as_os_str().is_empty())
        })
    };
    spec.secrets
        .iter()
        .find(|secret| !covered(&secret.target))
        .map_or(Ok(()), |secret| Err(secret.name.clone()))
}

/// Reads `reader` to its end on a thread, keeping at most `max` bytes.
pub fn drain(mut reader: impl Read + Send + 'static, max: usize) -> std::thread::JoinHandle<Vec<u8>> {
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

/// A private file beside the workspace for the container to read.
pub fn private_file(path: &Path, contents: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// One backend's offline isolation probe: a shell command, run with no
/// account mounted, that must write `verified` to `isolation-check`.
pub struct Probe {
    /// The probe's workspace, beside the attempt's.
    pub directory: &'static str,
    pub command: &'static str,
    pub timed_out: &'static str,
    /// The refusal when the command ran but did not verify.
    pub failed: &'static str,
}
/// The offline isolation probe; a timeout fails with [`io::ErrorKind::TimedOut`].
pub fn probe(
    node: &dyn Node,
    isolation: &dyn Isolation,
    image: &str,
    node_id: &str,
    root: &Path,
    name: &str,
    probe: &Probe,
) -> io::Result<()> {
    let directory = root.join(probe.directory);
    fs::create_dir_all(&directory)?;
    let output = node
        .shell(
            isolation,
            image,
            name,
            node_id,
            &directory,
            probe.command,
            SANDBOX_PROBE_WAIT,
        )
        .map_err(|error| match error.kind() {
            io::ErrorKind::TimedOut => io::Error::new(io::ErrorKind::TimedOut, probe.timed_out),
            _ => error,
        })?;
    if output["exit_code"] != 0 || fs::read(directory.join("isolation-check"))? != b"verified" {
        return Err(fail(probe.failed));
    }
    Ok(())
}
/// `control doctor`'s and `control ID check`'s probe for backend `id` called `name`.
pub fn check(
    node: &dyn Node,
    isolation: &dyn Isolation,
    image: &str,
    id: &str,
    name: &str,
    spec: &Probe,
) -> Result<(), String> {
    node.id()
        .map_err(io::Error::other)
        .and_then(|node_id| {
            probe(
                node,
                isolation,
                image,
                &node_id,
                &node.check_root(&format!("{id}-check")).map_err(io::Error::other)?,
                &format!("control-{id}-probe-{}", std::process::id()),
                spec,
            )
        })
        .map_err(|error| match error.kind() {
            io::ErrorKind::TimedOut => spec.timed_out.to_owned(),
            _ => format!(
                "the offline {name} tool-isolation probe failed; check the pinned image and container sandbox support"
            ),
        })
}

/// Decodes a CLI's event stream to its reply and the usage it reported.
pub type Capture = fn(
    &mut dyn Read,
    &mut dyn FnMut(Activity),
    &mut dyn FnMut(&Usage),
    &AtomicBool,
) -> (io::Result<(String, Option<Tokens>)>, Usage);
/// The final reply, and the usage reported whether or not there was one.
/// `progress` sees the usage so far each time a call reports more.
pub fn capture_with(
    reader: impl Read,
    decoder: impl control_ai::Decoder,
    name: &str,
    mut update: impl FnMut(Activity),
    progress: impl FnMut(&Usage),
    bad_tools: &AtomicBool,
) -> (io::Result<(String, Option<Tokens>)>, Usage) {
    let (output, usage) = control_ai::json_lines_live(
        reader,
        decoder,
        |event| match event {
            control_ai::Event::Activity(activity) => update(activity),
            control_ai::Event::RejectedTools => bad_tools.store(true, Ordering::Relaxed),
        },
        progress,
    );
    let reply = output.and_then(|output| {
        Ok((
            output
                .text
                .ok_or_else(|| fail(&format!("{name} did not return a final result")))?,
            output.tokens,
        ))
    });
    (reply, usage)
}

/// The CLI's own ending, when the stream reported one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ending {
    pub text: String,
    /// It listed or called a tool outside Control's.
    pub rejected_tools: bool,
}
impl Ending {
    /// The ending as the user reads it.
    pub fn failure(self) -> Failure {
        if self.rejected_tools {
            Failure::UnexpectedTools(self.text)
        } else {
            Failure::Ended(self.text)
        }
    }
}

/// How long a provider may keep running after its handoff was accepted, so it
/// can end the turn itself and report the turn's usage (Codex reports usage
/// only when a turn completes; stopping it at once lost it for nearly every
/// persistent-agent turn). Tools are fenced after the handoff, so the turn can
/// only finish its reply; it is stopped at this bound otherwise.
pub const HANDOFF_GRACE: Duration = Duration::from_secs(20);
/// Whether a turn whose handoff was accepted at `accepted` should be stopped.
pub fn handoff_grace_over(accepted: Instant, now: Instant) -> bool {
    now.saturating_duration_since(accepted) >= HANDOFF_GRACE
}

/// How the CLI reads its prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Written to standard input once the container runs.
    Stdin,
    /// A private file the backend mounts read-only before launch.
    File,
}

/// One backend's data for the shared turn.
pub struct Cli {
    pub name: &'static str,
    pub probe: &'static Probe,
    /// The mailbox directory beside the workspace, mounted as `/workspace`.
    pub mailbox: &'static str,
    /// The refusal when the host granted no login.
    pub no_login: &'static str,
    pub delivery: Delivery,
    /// How the CLI writes back a sign-in it refreshed.
    pub keeps: crate::Keeps,
    pub capture: Capture,
    /// The failure from the CLI's own ending and its bounded stderr.
    pub failure: fn(Option<Ending>, &[u8]) -> Failure,
}
/// What a backend's launch receives once the shared steps are done.
pub struct Prepared<'a> {
    /// Beside the workspace: the attempt's private files.
    pub root: &'a Path,
    pub mailbox: &'a Path,
    /// The turn container's name.
    pub name: &'a str,
    pub login: &'a crate::Granted,
    /// The observed prompt, for [`Delivery::File`].
    pub prompt: Option<String>,
}

/// One turn: probe, mailbox, the backend's launch, then the supervised reply.
pub fn turn(
    cli: &Cli,
    turn: Turn<'_>,
    host: &mut dyn Host,
    launch: impl FnOnce(&Prepared<'_>) -> Result<RunSpec, Failure>,
) -> Outcome {
    let Turn {
        id,
        isolation,
        node,
        image,
        workspace,
        prompt,
        started_unix,
        login,
        ..
    } = turn;
    let blocked = |message: &str| Failure::Blocked(message.to_owned());
    let Ok(login) = login else {
        return blocked(cli.no_login).into();
    };
    let Some(root) = workspace.parent() else {
        return blocked("Invalid attempt workspace").into();
    };
    if let Err(error) = probe(
        host.node(),
        isolation,
        image,
        node,
        root,
        &format!("control-{id}-probe"),
        cli.probe,
    ) {
        return match error.kind() {
            io::ErrorKind::TimedOut => blocked(cli.probe.timed_out),
            _ => Failure::Blocked(format!(
                "{} tool isolation probe failed. No account credentials were mounted.",
                cli.name
            )),
        }
        .into();
    }
    let mailbox = root.join(cli.mailbox);
    if fs::create_dir_all(&mailbox).is_err() {
        return Failure::Failed(format!("Cannot prepare {} mailbox", cli.name)).into();
    }
    let (prompt, stdin) = match cli.delivery {
        Delivery::File => (Some(host.observed(prompt)), None),
        Delivery::Stdin => (None, Some(prompt)),
    };
    let name = format!("control-{id}");
    let prepared = Prepared {
        root,
        mailbox: &mailbox,
        name: &name,
        login: &login,
        prompt,
    };
    let spec = match launch(&prepared) {
        Ok(spec) => spec,
        Err(failure) => return failure.into(),
    };
    if let Err(secret) = secrets_private(&spec) {
        return Failure::Blocked(format!(
            "{} turn would mount {secret} outside its private home",
            cli.name
        ))
        .into();
    }
    if host.cancelled() {
        return Failure::Cancelled(Cancelled::BeforeLaunch(Some(cli.name))).into();
    }
    let input = match stdin {
        Some(_) => Stream::Piped,
        None => Stream::Null,
    };
    let Ok(child) = isolation.spawn(&spec, Streams::new(input, Stream::Piped, Stream::Piped)) else {
        return Failure::Failed(format!("Cannot start the {} Nix container", cli.name)).into();
    };
    let stdin = stdin.map(|prompt| host.observed(prompt));
    let outcome = supervise(cli, child, &mailbox, stdin, started_unix, host);
    // Only the sign-in outlives the turn in a folder the CLI could write.
    // Best effort: the turn's result does not depend on it, and the next
    // turn's sweep removes what this one could not.
    if let (Some(folder), crate::Keeps::Folder(kept)) = (login.folder(cli.keeps), cli.keeps) {
        let _ = crate::sweep(folder, kept.keep);
    }
    outcome
}

/// Supervises a started CLI to its reply: activity and usage are recorded as
/// they stream, and a handoff, cancellation, design request or the time limit
/// stops it.
pub fn supervise(
    cli: &Cli,
    mut child: Child,
    mailbox: &Path,
    stdin: Option<String>,
    started_unix: u64,
    host: &mut dyn Host,
) -> Outcome {
    let name = cli.name;
    let diagnostics = drain(child.stderr.take().unwrap(), 64000);
    let bad_tools = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&bad_tools);
    let stdout = child.stdout.take().unwrap();
    let events = host.events();
    let capture = cli.capture;
    let output = std::thread::spawn(move || {
        let mut stdout = stdout;
        capture(
            &mut stdout,
            &mut |activity| events.activity(activity),
            // Live totals while the turn runs; the finished attempt replaces them.
            &mut |usage| events.usage(usage),
            &flag,
        )
    });
    let sent = match stdin {
        Some(prompt) => child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(prompt.as_bytes()).is_ok()),
        None => true,
    };
    let start = Instant::now();
    let admitted = crate::admitted(started_unix);
    let wake = MailboxWake::new(mailbox);
    let mut timed_out = None;
    let mut handoff = None;
    let mut accepted = None;
    let success = loop {
        if host.design_requested() || host.cancelled() {
            break false;
        }
        // An unexpected init tool list is final: stop before the model can act.
        if !sent || bad_tools.load(Ordering::Relaxed) || host.unavailable() {
            break false;
        }
        host.pump(mailbox);
        if host.design_requested() {
            break false;
        }
        match host.poll(start.elapsed() + Duration::from_secs(admitted)) {
            // Let the turn end itself and report its totals (tools are fenced
            // after a handoff), within the grace.
            Control::Finish(summary) => {
                handoff = Some(summary);
                if handoff_grace_over(*accepted.get_or_insert_with(Instant::now), Instant::now()) {
                    break false;
                }
            }
            Control::TimedOut(limit) => {
                timed_out = Some(limit);
                break false;
            }
            Control::Continue => {}
        }
        match child.try_wait() {
            // Stops the attempt's resources below; the handoff is the result.
            Ok(Some(_)) if handoff.is_some() => break false,
            Ok(Some(status)) => break status.success(),
            Err(_) => break false,
            _ => {}
        }
        wake.wait(Duration::from_millis(100));
    };
    if !success {
        let _ = child.kill();
        let _ = child.wait();
        if let Err(message) = host.stop(Stop::Live) {
            return Outcome::Launched {
                result: Err(Failure::Interrupted(message)),
                usage: None,
            };
        }
    }
    let Ok((result, usage)) = output.join() else {
        return Outcome::Launched {
            result: Err(Failure::Failed(format!("{name} stream reader stopped"))),
            usage: None,
        };
    };
    let stderr = diagnostics.join().unwrap_or_default();
    let result = match handoff {
        Some(summary) => Ok(summary),
        None if !success => Err(if host.cancelled() {
            Failure::Cancelled(Cancelled::Running)
        } else if host.design_requested() {
            Failure::Waiting
        } else if let Some(limit) = timed_out {
            Failure::TimedOut(limit)
        } else {
            let rejected_tools = bad_tools.load(Ordering::Relaxed);
            let ended = result
                .as_ref()
                .err()
                .map(ToString::to_string)
                .filter(|e| e.starts_with(&format!("{name} ended with")) || rejected_tools)
                .map(|text| Ending { text, rejected_tools });
            (cli.failure)(ended, &stderr)
        }),
        None => result
            .map(|(text, _)| text)
            .map_err(|e| Failure::Failed(format!("{e} The previous workspace checkpoint is unchanged."))),
    };
    Outcome::Launched {
        result,
        usage: Some(usage),
    }
}

/// The login a credential canary stands in for.
#[derive(Clone, Copy, Debug)]
pub enum Canary {
    /// Read-only, as every key, cloud credential and the operator's own sign-in is.
    ReadOnly(crate::Form),
    /// A sign-in in a folder Control owns, written back as the CLI `keeps` it.
    Refreshable(crate::Keeps),
}

/// `control ID check`'s credential canary: a fake credential, private and
/// owned like a real login, beside the check's state; `launch` builds the
/// turn container with it mounted at `target` as a turn would. Prints one
/// line per assertion and fails unless every one held.
pub fn canary_check(
    isolation: &dyn Isolation,
    root: &Path,
    login: Canary,
    target: &str,
    launch: impl FnOnce(&crate::Granted) -> io::Result<RunSpec>,
) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let meta = fs::metadata(root)?;
    let owner = User {
        uid: meta.uid(),
        gid: meta.gid(),
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let canary = format!("control-canary-{}-{nanos}", std::process::id());
    // Fresh names for every case: Docker Desktop's shared-file cache keeps a
    // stale entry for a name an earlier container mounted and the host then
    // deleted, so a recreated `canary-credential` was intermittently unreadable
    // in the next case's container. A sign-in kept in a folder gets a folder of
    // its own, as an account has.
    let (file, folder) = (
        root.join(format!("canary-credential-{nanos}")),
        root.join(format!("canary-login-{nanos}")),
    );
    let (granted, keeps) = match login {
        Canary::ReadOnly(form) => (crate::Granted::located(form, file, owner), crate::Keeps::InPlace),
        Canary::Refreshable(keeps @ crate::Keeps::Folder(kept)) => {
            let _ = fs::remove_dir_all(&folder);
            fs::DirBuilder::new().mode(0o700).create(&folder)?;
            (crate::Granted::refreshable(folder.join(kept.file), owner), keeps)
        }
        Canary::Refreshable(keeps @ crate::Keeps::InPlace) => (crate::Granted::refreshable(file, owner), keeps),
    };
    let path = granted.path().to_path_buf();
    private_file(&path, &canary)?;
    let target = match granted.form() {
        crate::Form::Session => granted.session_target(target, keeps),
        crate::Form::ApiKey | crate::Form::CloudDefault => target.to_owned(),
    };
    let results = launch(&granted).and_then(|spec| canary::run(isolation, spec, &target, &canary, granted.write()));
    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir_all(&folder);
    let (lines, held) = canary::report(&results?);
    lines.iter().for_each(|line| println!("{line}"));
    if held {
        Ok(())
    } else {
        Err(fail("the credential canary failed; see the FAIL lines above"))
    }
}

#[cfg(test)]
mod tests;
