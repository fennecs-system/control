//! Durable commands inside an exact, credential-free service. No host paths or
//! engine options enter the request. Uncertain effects are never replayed.
use super::*;
use control_effect::sync::{CancelToken, Changes, Recv};
use std::{io::Read, process::Child, thread::JoinHandle, time::Instant};
mod routine;

/// Finished command outputs retained per service. Active commands are bounded
/// separately by [`Concurrency`], so they never evict each other.
const MAX_EXECUTIONS: usize = 16;
const MAX_RECEIPTS: usize = 1024;

/// Bumped when a request is admitted and whenever a request changes status
/// (launched, finished or cancelled). The supervisor dispatches on it instead
/// of waiting for its reconciliation tick, and inline waiters wake on it.
/// Output progress alone never bumps it.
pub(super) fn changes() -> &'static Arc<Changes> {
    static CHANGES: std::sync::OnceLock<Arc<Changes>> = std::sync::OnceLock::new();
    CHANGES.get_or_init(Changes::new)
}
/// Inline waits are bounded below the worker MCP bridge's 35 s host-reply
/// timeout (tools/room-mcp), leaving margin for admission and engine work:
/// a longer wait would turn a successful submission into a transport error.
pub(super) const MAX_WAIT_SECONDS: u64 = 30;
/// Most game, build-step and client commands finish within seconds; waiting
/// that long inline saves the status round trip. A longer command costs the
/// caller at most this long before it gets the usual handle to poll.
const DEFAULT_WAIT_SECONDS: u64 = 15;
/// Cancellation, attempt changes and turn deadlines are rechecked at least
/// this often while waiting; completion itself wakes waiters immediately.
const RECHECK: Duration = Duration::from_secs(1);
/// Reply this long before the attempt's deadline so the result still arrives.
const DEADLINE_MARGIN: u64 = 3;
pub(super) fn default_wait() -> u64 {
    DEFAULT_WAIT_SECONDS
}
pub(super) fn wait_bound(seconds: u64) -> io::Result<Duration> {
    if seconds > MAX_WAIT_SECONDS {
        return Err(fail(format!("wait_seconds must be 0–{MAX_WAIT_SECONDS}")));
    }
    Ok(Duration::from_secs(seconds))
}

/// Safety ceiling on commands one service runs at once. The service's own
/// resources are the real bound: every command is at least one process in its
/// pids cgroup (which scales with RAM) and shares its memory cgroup, so a
/// service never admits more commands than its process limit. The ceiling only
/// bounds Control's side: per running command one supervisor thread, one engine
/// client and up to ~42 KB of persisted record. Node setting
/// `service-commands-max` (1–1024); invalid or absent values keep the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Concurrency(pub usize);
impl Concurrency {
    pub(super) const DEFAULT: Self = Self(64);
    pub(super) fn from_settings(setting: impl Fn(&str) -> Option<String>) -> Self {
        setting("service-commands-max")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| (1..=1024).contains(value))
            .map_or(Self::DEFAULT, Self)
    }
    /// The bound for one service: the ceiling or its process limit, if lower.
    pub(super) fn for_service(self, service: &Service) -> usize {
        self.0.min(usize::try_from(service.pids_limit()).unwrap_or(usize::MAX))
    }
}
#[cfg(not(test))]
fn concurrency() -> Concurrency {
    let directory = crate::config::directory().ok();
    Concurrency::from_settings(|name| std::fs::read_to_string(directory.as_ref()?.join(name)).ok())
}
/// Unit tests never read the operator's node configuration.
#[cfg(test)]
fn concurrency() -> Concurrency {
    Concurrency::DEFAULT
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Requested,
    Running,
    Completed,
    Failed,
    Interrupted,
    TimedOut,
}
impl Status {
    fn active(self) -> bool {
        matches!(self, Self::Requested | Self::Running)
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Execution {
    id: String,
    digest: String,
    run: String,
    attempt: String,
    command: String,
    #[serde(default)]
    display_command: String,
    timeout_seconds: u64,
    status: Status,
    output: String,
    truncated: bool,
    exit_code: Option<i32>,
    diagnostic: String,
    /// A file copy from the requesting attempt's workspace. It takes the
    /// service exclusively and shares receipts and recovery with commands;
    /// `command` stays empty, so an older Control that ignores this field
    /// would launch nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copy: Option<copy::Transfer>,
}
impl Execution {
    /// Command, operator label and copy request, for sibling test suites.
    #[cfg(test)]
    pub(super) fn request(&self) -> (&str, &str, Option<&copy::Transfer>) {
        (&self.command, &self.display_command, self.copy.as_ref())
    }
    /// The run that requested it.
    pub(super) fn run(&self) -> &str {
        &self.run
    }
    /// The first line of a command still running, for operator views.
    pub(super) fn running_label(&self) -> Option<String> {
        let command = if self.display_command.is_empty() {
            &self.command
        } else {
            &self.display_command
        };
        matches!(self.status, Status::Running)
            .then(|| command.lines().next().unwrap_or_default().chars().take(80).collect())
    }
    /// One line naming an active request for a refusal: its handle, status,
    /// owning chat (or run) and command.
    fn holder(&self, state: &State) -> String {
        let owner = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == self.run)
            .and_then(|r| r.chat_id.as_deref())
            .map_or_else(|| format!("run {}", self.run), |chat| format!("chat {chat}"));
        let command = if self.display_command.is_empty() {
            &self.command
        } else {
            &self.display_command
        };
        let label: String = command.lines().next().unwrap_or_default().chars().take(60).collect();
        format!(
            "{} ({}, {owner}: {label})",
            handle(&self.id),
            if self.status == Status::Running {
                "running"
            } else {
                "waiting"
            }
        )
    }
    /// Status fields that are set, under the short execution handle.
    pub(super) fn brief(&self) -> Value {
        let mut value = json!({"execution_id":handle(&self.id),"status":self.status});
        if let Some(code) = self.exit_code {
            value["exit_code"] = json!(code);
        }
        if !self.diagnostic.is_empty() {
            value["diagnostic"] = json!(self.diagnostic);
        }
        value
    }
    fn summary(&self) -> Value {
        let mut value = self.brief();
        if !self.output.is_empty() {
            value["output"] = json!(self.output);
        }
        if self.truncated {
            value["output_truncated"] = json!(true);
        }
        value
    }
}

/// Characters of an execution ID shown to agents. Within one service's
/// retained executions and receipts, 48 bits keep handles unique; a handle
/// that ever matched two of them is refused, never guessed.
pub(super) const HANDLE: usize = 12;
/// The short handle tools show for the durable execution ID `id`.
pub(super) fn handle(id: &str) -> &str {
    id.get(..HANDLE).unwrap_or(id)
}
/// Whether `given`, a full execution ID or its handle, names `id`.
fn names(id: &str, given: &str) -> bool {
    id == given || (given.len() >= HANDLE && id.starts_with(given))
}
/// The one retained execution or receipt `given` names on `service`.
fn lookup<'a>(service: &'a Service, given: &str) -> io::Result<Option<Recorded<'a>>> {
    let mut found = service
        .executions
        .iter()
        .filter(|e| names(&e.id, given))
        .map(Recorded::Execution)
        .chain(
            service
                .command_receipts
                .iter()
                .filter(|r| names(&r.id, given))
                .map(Recorded::Receipt),
        );
    let first = found.next();
    if found.next().is_some() {
        return Err(fail("Execution handle is ambiguous; pass the full execution_id"));
    }
    Ok(first)
}
enum Recorded<'a> {
    Execution(&'a Execution),
    Receipt(&'a Receipt),
}
impl Recorded<'_> {
    fn summary(&self) -> Value {
        match self {
            Self::Execution(execution) => execution.summary(),
            Self::Receipt(receipt) => receipt.summary(),
        }
    }
}

/// Keep effect identity and outcome after evicting the much larger output.
/// An acknowledged nonce can never turn into another command execution.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    id: String,
    digest: String,
    run: String,
    attempt: String,
    status: Status,
    exit_code: Option<i32>,
    diagnostic: String,
}
impl Receipt {
    fn from_execution(execution: &Execution) -> Self {
        Self {
            id: execution.id.clone(),
            digest: execution.digest.clone(),
            run: execution.run.clone(),
            attempt: execution.attempt.clone(),
            status: execution.status,
            exit_code: execution.exit_code,
            diagnostic: execution.diagnostic.clone(),
        }
    }
    /// The retained outcome; its output was evicted.
    fn summary(&self) -> Value {
        let mut value = json!({"execution_id":handle(&self.id),"status":self.status,"output_evicted":true});
        if let Some(code) = self.exit_code {
            value["exit_code"] = json!(code);
        }
        if !self.diagnostic.is_empty() {
            value["diagnostic"] = json!(self.diagnostic);
        }
        value
    }
}
pub(super) fn live_attempt(runs: &[crate::runtime::Run], run: &str, attempt: &str) -> bool {
    runs.iter().any(|r| {
        r.id == run
            && r.status == "running"
            && r.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt)
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    service_id: String,
    command: CommandText,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    /// How long to wait for the result inline; not part of request identity.
    #[serde(default = "default_wait")]
    pub(super) wait_seconds: u64,
}
fn default_timeout() -> u64 {
    120
}

/// One retained, finished command as pathology facts see it: who ran it, its
/// stable ID, the command text and whether it failed. Copies are skipped.
pub(crate) struct Finished<'a> {
    pub(crate) run: &'a str,
    pub(crate) id: &'a str,
    pub(crate) command: &'a str,
    pub(crate) failed: bool,
}
/// Every service's retained finished commands (at most `MAX_EXECUTIONS` per
/// service), oldest first per service. No engine calls or log reads.
pub(crate) fn finished(state: &State) -> impl Iterator<Item = Finished<'_>> {
    state
        .services
        .iter()
        .flat_map(|service| &service.executions)
        .filter(|execution| execution.copy.is_none() && !execution.status.active())
        .map(|execution| Finished {
            run: &execution.run,
            id: &execution.id,
            // A launched command keeps only its display text (`command` is cleared).
            command: if execution.display_command.is_empty() {
                &execution.command
            } else {
                &execution.display_command
            },
            failed: !matches!(execution.status, Status::Completed) || execution.exit_code.is_some_and(|code| code != 0),
        })
}

/// Project retained command evidence for one run; no engine calls or log reads.
pub(crate) fn activity<'a>(
    state: &'a State,
    run: &'a str,
) -> impl DoubleEndedIterator<Item = crate::usage::Activity> + 'a {
    state
        .services
        .iter()
        .flat_map(|service| &service.executions)
        .filter(move |execution| execution.run == run)
        .map(|execution| {
            let clean = |text: &str| {
                text.chars()
                    .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                    .collect::<String>()
            };
            let mut output = clean(&execution.output);
            if execution.truncated {
                output.push_str("\n[Output truncated]");
            }
            if let Some(code) = execution.exit_code {
                output.push_str(&format!("\nExit {code}"));
            }
            if !execution.diagnostic.is_empty() {
                output.push_str(&format!("\n{}", clean(&execution.diagnostic)));
            }
            crate::usage::Activity {
                id: format!("service-command-{}", execution.id),
                kind: "command_execution".into(),
                text: clean(if !execution.display_command.is_empty() {
                    &execution.display_command
                } else if !execution.command.is_empty() {
                    &execution.command
                } else {
                    "Service command"
                }),
                output,
                status: match execution.status {
                    Status::Requested => "queued",
                    Status::Running => "running",
                    Status::Completed => "completed",
                    Status::Failed => "failed",
                    Status::Interrupted => "interrupted",
                    Status::TimedOut => "timed_out",
                }
                .into(),
            }
        })
}
#[cfg(test)]
pub(crate) fn enqueue_test(
    state: &mut State,
    run: &str,
    nonce: &str,
    service_id: &str,
    command: &str,
    timeout_seconds: u64,
) -> io::Result<Value> {
    let node = state
        .services
        .iter()
        .find(|s| s.resource.id.as_str() == service_id)
        .ok_or_else(|| fail("Missing test service"))?
        .resource
        .owner
        .node
        .clone();
    admit(
        state,
        run,
        "attempt-a",
        nonce,
        &node,
        Input {
            service_id: service_id.into(),
            command: command.to_owned().try_into()?,
            timeout_seconds,
            wait_seconds: 0,
        },
        |_| Ok(()),
    )
}
pub(super) fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "control_service_exec",
            "description": concat!(
                "Run a one-off command inside an exact chat-owned running service, using its private files and ",
                "approved loopback relays: setup, inspection, or installing and editing a routine. For anything ",
                "repeated or polled, write a routine (control_watch_start) instead. Use shell commands/heredocs to ",
                "create small task files, configure clients or install dependencies; bring files or trees from your ",
                "task workspace with control_service_copy instead. No host paths, mounts or engine options. Results ",
                "return inline when the command finishes within wait_seconds (default 15, max 30): final status, exit ",
                "code and output, marked final=true. Call control_service_exec_status only when the result says ",
                "running and gives an execution_id. Use wait_seconds=0 to return at once. Commands from every agent ",
                "sharing the service run concurrently, each with its own execution_id, output and timeout, up to the ",
                "service process limit (a larger shape allows more), capped by a node ceiling (default 64); at the ",
                "limit, or while a control_service_copy holds the service, the request is refused with the active ",
                "execution_ids and nothing is submitted. Commands are bounded by timeout_seconds; they survive model ",
                "turns only until then. Run routines and other long-running work with control_watch_start: ",
                "restart=transient runs finite work such as large downloads once; restart=permanent restarts ",
                "persistent clients, servers or daemons after exit. Both survive model turns without a task deadline. ",
                "Use exec for bounded commands: a timed-out or interrupted command is stopped inside the service with ",
                "every process it started, detached ones included. The service keeps running; it is stopped only if ",
                "that cannot be verified. Background programs must redirect their output to private files; the ",
                "service foreground process must remain alive. Uncertain commands are never replayed."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "service_id": {"type": "string"},
                    "command": {"type": "string", "minLength": 1, "maxLength": 16000},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 1800, "default": 120},
                    "wait_seconds": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_WAIT_SECONDS,
                        "default": DEFAULT_WAIT_SECONDS
                    }
                },
                "required": ["service_id", "command"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": true}
        }),
        json!({
            "name": "control_service_exec_status",
            "description": concat!(
                "Read a chat-owned service command's bounded live output, exit code and durable outcome. Only needed ",
                "when control_service_exec or control_service_copy said running and gave an execution_id; a result ",
                "marked final=true is already complete. Optional wait_seconds (max 30, default 0) waits for an active ",
                "command to finish and returns as soon as it does. execution_id is the short handle results show (the ",
                "full ID also works). Long output keeps its first and last bytes around one omission marker; unset ",
                "fields (exit code, diagnostic, output) are omitted. Use a fresh execution_id only for a deliberate ",
                "new command; retries with the same request identity never replay effects."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "service_id": {"type": "string"},
                    "execution_id": {"type": "string"},
                    "wait_seconds": {"type": "integer", "minimum": 0, "maximum": MAX_WAIT_SECONDS, "default": 0}
                },
                "required": ["service_id", "execution_id"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }),
    ]
}
pub(super) fn hash(value: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, value)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn owned(
    authority: ServiceAuthority<'_>,
    run: &str,
    generation: &str,
    node: &NodeId,
    service: &str,
) -> io::Result<usize> {
    let (active, _) = caller(authority.runs, run, generation)?;
    target(authority, node, active, service)
}
pub(super) fn exec(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    nonce: &str,
    node: &NodeId,
    input: Input,
) -> io::Result<Value> {
    // An invalid wait refuses the request before anything is submitted.
    let wait = wait_bound(input.wait_seconds)?;
    let service_id = input.service_id.clone();
    let admitted = {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let mut admitted = admit(&mut state, run, generation, nonce, node, input, crate::storage::save)?;
        routine::hint(&state, run, generation, &service_id, &mut admitted);
        admitted
    };
    let execution_id = admitted["execution_id"].as_str().unwrap_or_default().to_owned();
    // settle keeps admitted fields, so inline results also carry the hint.
    Ok(settle(
        shared,
        Waiter {
            run,
            generation,
            node,
            service_id: &service_id,
        },
        &execution_id,
        admitted,
        wait,
        &crate::shutdown::token(),
    ))
}
/// The caller whose access is rechecked on every wake while waiting.
#[derive(Clone, Copy)]
pub(super) struct Waiter<'a> {
    pub(super) run: &'a str,
    pub(super) generation: &'a str,
    pub(super) node: &'a NodeId,
    pub(super) service_id: &'a str,
}
/// Waits without the state lock for an admitted request to finish, for at most
/// `wait`, the caller's turn deadline, or until its attempt ends or Control
/// stops, and returns its latest summary. Fields of `admitted` that the
/// summary lacks (a copy's tally) are kept. Waiting never creates or replays
/// an effect, so a retried request simply waits on the same execution.
pub(super) fn settle(
    shared: &Arc<Mutex<State>>,
    waiter: Waiter<'_>,
    execution_id: &str,
    admitted: Value,
    wait: Duration,
    token: &CancelToken,
) -> Value {
    let active = |value: &Value| {
        value["status"]
            .as_str()
            .is_some_and(|s| matches!(s, "requested" | "running"))
    };
    let service_id = waiter.service_id;
    let mut value = if wait.is_zero() || !active(&admitted) {
        admitted
    } else {
        let mut value = wait_until(
            shared,
            waiter,
            execution_id,
            admitted.clone(),
            Instant::now() + wait,
            token,
        );
        if let (Some(value), Some(admitted)) = (value.as_object_mut(), admitted.as_object()) {
            for (key, field) in admitted {
                value.entry(key.clone()).or_insert_with(|| field.clone());
            }
        }
        value
    };
    // Say which result is final, so a finished command is not polled again.
    let finished = !active(&value);
    if let Some(object) = value.as_object_mut() {
        if finished {
            object.insert("final".into(), json!(true));
        } else {
            object
                .entry("wait")
                .or_insert_with(|| json!(still_running(service_id, execution_id)));
        }
    }
    value
}
/// The callable next step for a request still running after its wait.
fn still_running(service_id: &str, execution_id: &str) -> String {
    format!(
        "Still running: call control_service_exec_status {{\"service_id\":\"{service_id}\",\"execution_id\":\"{}\",\"wait_seconds\":30}} for its result.",
        handle(execution_id)
    )
}
/// When the caller's attempt must have its reply, as a local instant.
fn turn_end(state: &State, run: &str) -> Option<Instant> {
    let run = state.runtime.runs.iter().find(|r| r.id == run)?;
    let attempt = run.attempts.last()?;
    let limit = crate::attempt_control::deadline(state, run, attempt)?;
    let end = attempt
        .started_unix
        .saturating_add(limit)
        .saturating_sub(DEADLINE_MARGIN);
    Some(Instant::now() + Duration::from_secs(end.saturating_sub(now())))
}
fn wait_until(
    shared: &Arc<Mutex<State>>,
    waiter: Waiter<'_>,
    execution_id: &str,
    mut last: Value,
    until: Instant,
    token: &CancelToken,
) -> Value {
    let signal = changes();
    loop {
        // Read the generation before the state so no completion is missed.
        let seen = signal.generation();
        let limit = {
            let Ok(state) = shared.lock() else { return last };
            // An ended run or replaced attempt stops waiting with what it saw.
            let Ok(index) = owned(
                ServiceAuthority::of(&state),
                waiter.run,
                waiter.generation,
                waiter.node,
                waiter.service_id,
            ) else {
                return last;
            };
            let service = &state.services[index];
            match lookup(service, execution_id) {
                Ok(Some(Recorded::Execution(execution))) => {
                    last = execution.summary();
                    if !execution.status.active() {
                        return last;
                    }
                }
                Ok(Some(receipt)) => return receipt.summary(),
                Ok(None) | Err(_) => return last,
            }
            turn_end(&state, waiter.run).map_or(until, |end| end.min(until))
        };
        let now = Instant::now();
        if now >= limit || token.is_cancelled() {
            last["wait"] = json!(still_running(waiter.service_id, execution_id));
            return last;
        }
        signal.wait_newer(seen, Some(limit.min(now + RECHECK)), token);
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handle {
    service_id: String,
    execution_id: String,
    /// Optional long poll; zero (the default) reads the current status.
    #[serde(default)]
    wait_seconds: u64,
}
/// Reads a request's status, waiting up to `wait_seconds` for it to finish.
pub(super) fn status_wait(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    node: &NodeId,
    input: Handle,
) -> io::Result<Value> {
    let wait = wait_bound(input.wait_seconds)?;
    let (service_id, execution_id) = (input.service_id.clone(), input.execution_id.clone());
    let value = status(shared, run, generation, node, input)?;
    Ok(settle(
        shared,
        Waiter {
            run,
            generation,
            node,
            service_id: &service_id,
        },
        &execution_id,
        value,
        wait,
        &crate::shutdown::token(),
    ))
}
pub(super) fn status(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    node: &NodeId,
    input: Handle,
) -> io::Result<Value> {
    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let index = owned(ServiceAuthority::of(&state), run, generation, node, &input.service_id)?;
    lookup(&state.services[index], &input.execution_id)?
        .map(|recorded| recorded.summary())
        .ok_or_else(|| fail("Service command not found"))
}
/// One request on a service: a shell command or an exclusive copy.
pub(super) struct Work {
    pub(super) service_id: String,
    /// Argument digest; a nonce reused with different arguments is refused.
    pub(super) digest: String,
    pub(super) command: String,
    pub(super) display: String,
    pub(super) timeout_seconds: u64,
    pub(super) copy: Option<copy::Transfer>,
}
fn admit(
    state: &mut State,
    run: &str,
    generation: &str,
    nonce: &str,
    node: &NodeId,
    input: Input,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<Value> {
    let digest = hash(&serde_json::to_vec(&(&input.command, input.timeout_seconds)).map_err(fail)?);
    let work = Work {
        service_id: input.service_id,
        digest,
        command: input.command.0.to_string(),
        display: input.command.0.chars().take(2000).collect(),
        timeout_seconds: input.timeout_seconds,
        copy: None,
    };
    admit_work(state, run, generation, nonce, node, work, save)
}
fn valid_nonce(nonce: &str) -> io::Result<()> {
    if nonce.is_empty() || nonce.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(fail("Invalid service command identity"));
    }
    Ok(())
}
/// The durable execution ID of one tool request; shared by commands and copies.
fn identity(service_id: &str, run: &str, generation: &str, nonce: &str) -> io::Result<String> {
    Ok(hash(
        &serde_json::to_vec(&(service_id, run, generation, nonce)).map_err(fail)?,
    ))
}
/// The recorded outcome of an already-admitted request, never a new effect.
pub(super) fn previous(
    service: &Service,
    run: &str,
    generation: &str,
    nonce: &str,
    digest: &str,
) -> io::Result<Option<Value>> {
    valid_nonce(nonce)?;
    let id = identity(service.resource.id.as_str(), run, generation, nonce)?;
    let found = service
        .executions
        .iter()
        .find(|e| e.id == id)
        .map(|e| (e.digest == digest, e.summary()))
        .or_else(|| {
            service
                .command_receipts
                .iter()
                .find(|r| r.id == id)
                .map(|r| (r.digest == digest, r.summary()))
        });
    match found {
        Some((true, value)) => Ok(Some(value)),
        Some((false, _)) => Err(fail("Service command identity was reused with different arguments")),
        None => Ok(None),
    }
}
pub(super) fn admit_work(
    state: &mut State,
    run: &str,
    generation: &str,
    nonce: &str,
    node: &NodeId,
    work: Work,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<Value> {
    valid_nonce(nonce)?;
    if !(1..=1800).contains(&work.timeout_seconds) {
        return Err(fail("timeout_seconds must be 1–1800"));
    }
    let index = owned(ServiceAuthority::of(state), run, generation, node, &work.service_id)?;
    let id = identity(&work.service_id, run, generation, nonce)?;
    let digest = work.digest;
    let service = &state.services[index];
    if let Some(value) = previous(service, run, generation, nonce, &digest)? {
        return Ok(value);
    }
    if !matches!(service.resource.phase, Phase::Running(_)) || service.resource.lease.expired(now()) {
        return Err(fail("Wait for a running service with a live lease"));
    }
    // Commands run concurrently up to the node limit. A copy takes the service
    // exclusively: it waits for running commands, and nothing new is admitted
    // until it finishes, so no command observes a partial copy.
    let active = service
        .executions
        .iter()
        .filter(|e| e.status.active())
        .collect::<Vec<_>>();
    let service_id = service.resource.id.as_str();
    if let Some(copy) = active.iter().find(|e| e.copy.is_some()) {
        return Err(fail(format!(
            "Service {service_id} is reserved for copy {}; nothing was submitted. Call control_service_exec_status {{\"service_id\":\"{service_id}\",\"execution_id\":\"{}\",\"wait_seconds\":30}} and retry after it finishes.",
            copy.holder(state),
            handle(&copy.id)
        )));
    }
    let ceiling = concurrency();
    let limit = ceiling.for_service(service);
    if work.copy.is_none() && active.len() >= limit {
        let holders = active.iter().map(|e| e.holder(state)).collect::<Vec<_>>().join("; ");
        let bound = if limit < ceiling.0 {
            format!(
                "the service's process limit of {}; replace it with a larger shape for more",
                service.pids_limit()
            )
        } else {
            format!("node setting service-commands-max={limit}")
        };
        return Err(fail(format!(
            "Service {service_id} is running {} of {limit} concurrent commands ({bound}); nothing was submitted. Active: {holders}. Call control_service_exec_status {{\"service_id\":\"{service_id}\",\"execution_id\":\"{}\",\"wait_seconds\":30}} until one finishes, then retry; run long-lived programs as watches (control_watch_start), or use another service.",
            active.len(),
            active.first().map_or("", |e| handle(&e.id))
        )));
    }
    let waiting = if work.copy.is_some() { active.len() } else { 0 };
    let evict = (service.executions.len() - active.len() >= MAX_EXECUTIONS)
        .then(|| service.executions.iter().position(|e| !e.status.active()))
        .flatten();
    let receipt = evict.and_then(|i| {
        let e = &state.services[index].executions[i];
        live_attempt(&state.runtime.runs, &e.run, &e.attempt).then(|| Receipt::from_execution(e))
    });
    let old_receipts = if receipt.is_some() && state.services[index].command_receipts.len() >= MAX_RECEIPTS {
        let retained = state.services[index]
            .command_receipts
            .iter()
            .filter(|r| live_attempt(&state.runtime.runs, &r.run, &r.attempt))
            .cloned()
            .collect::<Vec<_>>();
        if retained.len() >= MAX_RECEIPTS {
            return Err(fail(
                "Active command receipt budget exhausted; finish this model turn before running more commands",
            ));
        }
        Some(std::mem::replace(&mut state.services[index].command_receipts, retained))
    } else {
        None
    };
    let previous = evict.map(|i| (i, state.services[index].executions.remove(i)));
    let receipt_len = state.services[index].command_receipts.len();
    if let Some(receipt) = receipt {
        state.services[index].command_receipts.push(receipt);
    }
    let execution = Execution {
        id,
        digest,
        run: run.into(),
        attempt: generation.into(),
        command: work.command,
        display_command: work.display,
        timeout_seconds: work.timeout_seconds,
        status: Status::Requested,
        output: String::new(),
        truncated: false,
        exit_code: None,
        diagnostic: if waiting > 0 {
            format!(
                "Waiting for {waiting} running command(s) to finish; new commands are refused until this copy completes."
            )
        } else {
            String::new()
        },
        copy: work.copy,
    };
    let value = execution.summary();
    state.services[index].executions.push(execution);
    if let Err(error) = save(state) {
        state.services[index].executions.pop();
        state.services[index].command_receipts.truncate(receipt_len);
        if let Some(old) = old_receipts {
            state.services[index].command_receipts = old;
        }
        if let Some((i, old)) = previous {
            state.services[index].executions.insert(i, old);
        }
        return Err(error);
    }
    // Wake the supervisor: the request launches without waiting for its tick.
    changes().bump();
    Ok(value)
}

/// Bounded command output: the first and the latest bytes, so both how a
/// command started and how it ended survive a long run. Between them, one
/// marker counts the bytes left out.
#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    /// The latest bytes once the head is full, at most `TAIL` of them.
    tail: std::collections::VecDeque<u8>,
    /// Bytes dropped between the head and the tail.
    omitted: u64,
    truncated: bool,
}
/// Head and tail each keep half the budget, less room for the marker.
const HEAD: usize = OUTPUT / 2;
const TAIL: usize = OUTPUT / 2 - 64;
impl Output {
    fn append(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(HEAD.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..count]);
        let rest = &bytes[count..];
        if rest.is_empty() {
            return;
        }
        self.tail.extend(rest);
        let excess = self.tail.len().saturating_sub(TAIL);
        if excess > 0 {
            self.tail.drain(..excess);
            self.omitted += excess as u64;
            self.truncated = true;
        }
    }
    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.omitted > 0 {
            text.push_str(&format!("\n[… {} bytes omitted …]\n", self.omitted));
        }
        let (front, back) = self.tail.as_slices();
        text.push_str(&String::from_utf8_lossy(&[front, back].concat()));
        text.truncate(text.floor_char_boundary(OUTPUT));
        text
    }
}
struct ResultOutput {
    output: Output,
    exit_code: Option<i32>,
    interrupted: bool,
    timed_out: bool,
}
fn capture(
    start: impl FnOnce(Streams) -> io::Result<Child>,
    timeout: Duration,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&Output),
) -> io::Result<ResultOutput> {
    capture_with(start, timeout, cancel, progress, &crate::shutdown::token())
}
/// Shutdown cancellation interrupts the command like a failed `cancel` check.
fn capture_with(
    start: impl FnOnce(Streams) -> io::Result<Child>,
    timeout: Duration,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&Output),
    shutdown: &CancelToken,
) -> io::Result<ResultOutput> {
    capture_fed(start, timeout, cancel, progress, shutdown, None)
}
/// Writes a command's stdin on its own thread; dropping the pipe ends input.
/// Killing the command breaks the pipe, so the writer cannot outlive it.
pub(super) type Feed = Box<dyn FnOnce(std::process::ChildStdin) + Send>;
fn capture_fed(
    start: impl FnOnce(Streams) -> io::Result<Child>,
    timeout: Duration,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&Output),
    shutdown: &CancelToken,
    feed: Option<Feed>,
) -> io::Result<ResultOutput> {
    if timeout.is_zero() || cancel() {
        return Err(fail("Service command is no longer eligible"));
    }
    let mut child = start(Streams::new(
        if feed.is_some() { Stream::Piped } else { Stream::Null },
        Stream::Piped,
        Stream::Null,
    ))?;
    let writer = match (feed, child.stdin.take()) {
        (Some(feed), Some(stdin)) => Some(std::thread::spawn(move || feed(stdin))),
        _ => None,
    };
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| fail("Missing service command output"))?;
    let output = Arc::new(Mutex::new(Output::default()));
    let reader_output = output.clone();
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut output) = reader_output.lock() {
                        output.append(&chunk[..n]);
                    }
                }
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let watched = match crate::child::watch(child) {
        Ok(watched) => watched,
        Err(error) => {
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            return Err(error);
        }
    };
    let mut interrupted = false;
    let mut timed_out = false;
    let exited = |result: Recv<io::Result<std::process::ExitStatus>>| match result {
        Recv::Item(status) => Some(status),
        Recv::Closed => Some(Err(fail("Service command waiter stopped"))),
        Recv::Cancelled | Recv::Elapsed => None,
    };
    let status = loop {
        let tick = (Instant::now() + Duration::from_millis(250)).min(deadline);
        if let Some(status) = exited(watched.wait(Some(tick), shutdown)) {
            break status;
        }
        if shutdown.is_cancelled() || cancel() || Instant::now() >= deadline {
            timed_out = Instant::now() >= deadline;
            interrupted = !timed_out;
            watched.kill();
            break exited(watched.wait(None, &CancelToken::new()))
                .unwrap_or_else(|| Err(fail("Service command did not exit")));
        }
        if let Ok(output) = output.lock() {
            progress(&output);
        }
    };
    let _ = reader.join();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let status = status?;
    let output = Arc::try_unwrap(output)
        .map_err(|_| fail("Output reader did not finish"))?
        .into_inner()
        .map_err(|_| fail("Output unavailable"))?;
    Ok(ResultOutput {
        output,
        exit_code: status.code(),
        interrupted,
        timed_out,
    })
}
/// Output of a host probe command.
pub(crate) struct ProbeOutput {
    pub(crate) text: String,
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
}
impl ProbeOutput {
    /// The exit status as shown to operators and agents: the code, or
    /// "unknown" when the command ended without one.
    pub(crate) fn status(&self) -> String {
        self.exit_code.map_or_else(|| "unknown".into(), |code| code.to_string())
    }
}
/// Runs a short host probe command (a chat's fresh observation or a score
/// sample) in a verified service as the service user, outside any attempt,
/// without an execution record or exec slot, bounded in time and output. A
/// command still running at `timeout` is stopped inside the service under a
/// marker derived from `owner`, which should be unique to concurrent probes.
/// Queries the engine: call without the state lock.
pub(crate) fn probe(
    isolation: &dyn Isolation,
    service: &Service,
    command: &str,
    owner: &str,
    timeout: Duration,
) -> io::Result<ProbeOutput> {
    let (id, user) = super::exec_target(isolation, service)?;
    let marker = format!("probe-{}", &hash(owner.as_bytes())[..32]);
    let run = in_service(
        id,
        user,
        &[(MARKER, marker.as_str())],
        control_isolation::Input::Closed,
        &launch(command),
    );
    let result = capture(|streams| isolation.exec(&run, streams), timeout, || false, |_| {})?;
    if result.timed_out || result.interrupted || result.exit_code.is_none() {
        reap(isolation, service, &marker);
    }
    Ok(ProbeOutput {
        text: result.output.text(),
        exit_code: result.exit_code,
        timed_out: result.timed_out,
    })
}
/// A snapshot of service `id` that `usable` accepts, to probe without the
/// state lock. Observations and score probes check the service each time
/// they run, not when they were declared.
pub(crate) fn probe_target(services: &[Service], id: &str, usable: impl Fn(&Service) -> bool) -> Option<Service> {
    services
        .iter()
        .find(|service| service.resource.id.as_str() == id && usable(service))
        .map(Service::binding_snapshot)
}
pub(super) fn logs(isolation: &dyn Isolation, id: &ContainerId) -> io::Result<String> {
    let result = capture(
        |_| isolation.logs(&id.as_str().into(), 80),
        Duration::from_secs(5),
        || false,
        |_| {},
    )?;
    if result.exit_code != Some(0) || result.timed_out {
        return Err(fail("Could not retain service logs"));
    }
    Ok(result.output.text())
}
fn update(
    shared: &Arc<Mutex<State>>,
    service: &Service,
    id: &str,
    change: impl FnOnce(&mut Execution),
    persist: bool,
) -> io::Result<()> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let Some(index) = state
        .services
        .iter()
        .position(|s| s.resource.id == service.resource.id && s.resource.generation == service.resource.generation)
    else {
        return Err(fail("Service generation changed"));
    };
    let execution = state.services[index]
        .executions
        .iter_mut()
        .find(|e| names(&e.id, id))
        .ok_or_else(|| fail("Service command missing"))?;
    let previous = persist.then(|| execution.clone());
    let previous_status = execution.status;
    let previous_output = (execution.output.len(), execution.truncated, execution.status);
    change(execution);
    let changed = previous_output != (execution.output.len(), execution.truncated, execution.status);
    let run = execution.run.clone();
    let transitioned = execution.status != previous_status;
    if persist && let Err(error) = crate::storage::save(&state) {
        if let Some(previous) = previous {
            *state.services[index]
                .executions
                .iter_mut()
                .find(|e| e.id == id)
                .unwrap() = previous;
        }
        return Err(error);
    }
    if changed {
        let chat = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == run)
            .and_then(|r| r.chat_id.clone());
        if let Some(chat) = chat {
            crate::chat::activity_changed(&mut state, &chat);
        }
    }
    drop(state);
    // A finished command may free a slot for queued work and has a result
    // for its waiters; a launch tells waiters it started.
    if transitioned {
        changes().bump();
    }
    Ok(())
}
fn stop(shared: &Arc<Mutex<State>>, service: &Service) -> io::Result<()> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    if let Some(index) = state
        .services
        .iter()
        .position(|s| s.resource.id == service.resource.id && s.resource.generation == service.resource.generation)
    {
        transition(
            &mut state,
            index,
            Event::Stop(StopReason::Requested),
            now(),
            crate::storage::save,
        )?;
    }
    Ok(())
}
/// The command runs with `CONTROL_EXEC=<execution id>` set at exec, so it and
/// every process it starts, detached ones included, can be found inside the
/// service if it outlives its engine client. Execution IDs are hex digests.
const MARKER: &str = "CONTROL_EXEC";
fn launch(command: &str) -> String {
    format!("exec 2>&1\n{command}")
}
/// Terminates, then kills, every running process carrying the command's
/// marker, and succeeds only once none remains. Exited processes awaiting
/// their reaper do not count, and no marker means nothing is running.
fn reap_script(execution: &str) -> String {
    format!(
        r#"m={MARKER}={execution}
members() {{ for d in /proc/[0-9]*; do read -r s 2>/dev/null < "$d/stat" || continue; s=${{s##*) }}; [ "${{s%% *}}" = Z ] && continue; while IFS= read -r -d '' v; do [ "$v" = "$m" ] && {{ echo "${{d#/proc/}}"; break; }}; done 2>/dev/null < "$d/environ"; done; }}
signal() {{ for p in $(members); do kill -"$1" "$p" 2>/dev/null; done; }}
signal TERM
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -z "$(members)" ] && exit 0; sleep 0.2; done
signal KILL
sleep 0.2
[ -z "$(members)" ]"#
    )
}
/// A `bash` script in a verified service as `user` from `/workspace`, with `env` set.
fn in_service(
    container: control_isolation::ContainerId,
    user: User,
    env: &[(&str, &str)],
    input: control_isolation::Input,
    script: &str,
) -> ExecSpec {
    ExecSpec {
        container,
        user: Some(user),
        workdir: Some("/workspace".into()),
        env: env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        input,
        argv: ["bash", "-c", script].map(str::to_owned).to_vec(),
    }
}
fn reap(isolation: &dyn Isolation, service: &Service, execution: &str) -> bool {
    let Some(id) = service.resource.phase.container() else {
        return false;
    };
    let command = ExecSpec {
        container: id.as_str().into(),
        user: Some(service.user()),
        argv: ["bash", "-c", &reap_script(execution)].map(str::to_owned).to_vec(),
        ..ExecSpec::default()
    };
    capture(
        |streams| isolation.exec(&command, streams),
        Duration::from_secs(10),
        || false,
        |_| {},
    )
    .is_ok_and(|r| r.exit_code == Some(0) && !r.timed_out)
}
fn execute(shared: Arc<Mutex<State>>, service: Service, execution: Execution) {
    let isolation = crate::engine::isolation();
    // A copy reads its requesting attempt's live workspace. A source that is
    // gone, unsafe or over its bounds fails before anything enters the service.
    let copy = match execution
        .copy
        .as_ref()
        .map(|transfer| copy::Job::prepare(&shared, &service, &execution.run, &execution.attempt, transfer))
    {
        None => None,
        Some(Ok(job)) => Some(job),
        Some(Err(error)) => {
            let _ = update(
                &shared,
                &service,
                &execution.id,
                |e| {
                    e.status = Status::Failed;
                    e.diagnostic = format!("Nothing was copied: {error}");
                },
                true,
            );
            return;
        }
    };
    let result = (|| -> io::Result<ResultOutput> {
        identity::authorize(&service.actor, Action::Task).map_err(fail)?;
        let id = service
            .resource
            .phase
            .container()
            .ok_or_else(|| fail("Service is not running"))?;
        let observed = super::inspect_one(isolation, id)?;
        verify(&service, &observed)?;
        if observed.status != "running" || service.resource.lease.expired(now()) {
            return Err(fail("Service is no longer running"));
        }
        let user = service.user();
        // Approved egress also brings the service's HTTPS_PROXY settings.
        let proxy = shared
            .lock()
            .map(|state| crate::ports::proxy_env(&state.ports, &service))
            .unwrap_or_default();
        let marker = [(MARKER, execution.id.as_str())]
            .into_iter()
            .chain(proxy.iter().copied())
            .collect::<Vec<_>>();
        // Both run as the service user under the same marker, so a copy that
        // outlives its client is reaped exactly like a command.
        let (command, feed) = match &copy {
            Some(job) => (
                in_service(
                    id.as_str().into(),
                    user,
                    &marker,
                    control_isolation::Input::Attached,
                    copy::EXTRACT,
                ),
                Some(job.feed()),
            ),
            None => (
                in_service(
                    id.as_str().into(),
                    user,
                    &marker,
                    control_isolation::Input::Closed,
                    &launch(&execution.command),
                ),
                None,
            ),
        };
        let checked = std::cell::Cell::new(Instant::now());
        let authorized = std::cell::Cell::new(true);
        capture_fed(
            |streams| isolation.exec(&command, streams),
            Duration::from_secs(
                execution
                    .timeout_seconds
                    .min(service.resource.lease.expires().saturating_sub(now())),
            ),
            || {
                if checked.get().elapsed() >= Duration::from_secs(1) {
                    authorized.set(identity::authorize(&service.actor, Action::Task).is_ok());
                    checked.set(Instant::now());
                }
                crate::shutdown::requested()
                    || !authorized.get()
                    || shared
                        .lock()
                        .map_or(true, |s| !s.services.iter().any(|s| s.resource == service.resource))
            },
            |output| {
                let _ = update(
                    &shared,
                    &service,
                    &execution.id,
                    |e| {
                        e.output = output.text();
                        e.truncated = output.truncated;
                    },
                    false,
                );
            },
            &crate::shutdown::token(),
            feed,
        )
    })();
    // A copy completes only when the whole archive was streamed and extracted.
    let streamed = copy.as_ref().map(copy::Job::outcome);
    let unfinished = !result
        .as_ref()
        .is_ok_and(|r| !r.interrupted && !r.timed_out && r.exit_code.is_some());
    // A command that outlived its client is stopped inside the service; only an
    // unverifiable stop falls back to stopping the whole service.
    let reaped = unfinished && result.is_ok() && reap(isolation, &service, &execution.id);
    let uncertain = unfinished && !reaped;
    // Stop intent is durable before releasing an uncertain remote writer. Killing
    // a Docker client alone does not prove the command inside the service exited.
    if uncertain && stop(&shared, &service).is_err() {
        return;
    }
    let _ = update(
        &shared,
        &service,
        &execution.id,
        |e| {
            e.command.clear();
            match result {
                Ok(result) => {
                    e.status = if result.timed_out {
                        Status::TimedOut
                    } else if uncertain {
                        Status::Interrupted
                    } else if result.exit_code != Some(0) {
                        Status::Failed
                    } else {
                        Status::Completed
                    };
                    e.output = result.output.text();
                    e.truncated = result.output.truncated;
                    e.exit_code = result.exit_code;
                    match streamed {
                        Some(Some(Ok(report))) if e.status == Status::Completed => {
                            let mut text = report + &e.output;
                            text.truncate(text.floor_char_boundary(OUTPUT));
                            e.output = text;
                        }
                        Some(Some(Ok(_))) => {
                            e.diagnostic =
                                "Extraction in the service failed; see output. Files already extracted remain.".into();
                        }
                        Some(stopped) => {
                            if e.status == Status::Completed {
                                e.status = Status::Failed;
                            }
                            e.diagnostic = format!(
                                "Copy stopped: {}. Files already extracted remain in the service.",
                                stopped
                                    .and_then(Result::err)
                                    .unwrap_or_else(|| "the archive was not streamed".into())
                            );
                        }
                        None => {}
                    }
                    if uncertain {
                        e.diagnostic = "Service stop requested because command completion was uncertain".into();
                    } else if reaped {
                        e.diagnostic = "The command was stopped inside the service, which keeps running. Run long work as a watch (control_watch_start) instead.".into();
                    }
                }
                Err(_) => {
                    e.status = Status::Interrupted;
                    e.diagnostic =
                        "Service command could not complete; inspect service diagnostics. It was not replayed.".into();
                }
            }
        },
        true,
    );
}
pub(super) fn recover(shared: &Arc<Mutex<State>>, save: impl Fn(&State) -> io::Result<()>) -> io::Result<()> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    for index in 0..state.services.len() {
        if !state.services[index]
            .executions
            .iter()
            .any(|e| e.status == Status::Running)
        {
            continue;
        }
        let old = state.services[index].clone();
        for execution in &mut state.services[index].executions {
            if execution.status == Status::Running {
                execution.status = Status::Interrupted;
                execution.command.clear();
                execution.diagnostic =
                    "Control restarted before command completion; service stop requested, no replay".into();
            }
        }
        let resource = &state.services[index].resource;
        let decision = decide(resource, resource.stamp(), Event::Stop(StopReason::Requested), now()).map_err(fail)?;
        state.services[index].resource.phase = decision.phase;
        if let Err(error) = save(&state) {
            state.services[index] = old;
            return Err(error);
        }
    }
    Ok(())
}
/// Requests a running service can launch now, in admission order, in one
/// pass. Admission bounds how many commands are active and admits nothing
/// after an active copy, so every request a copy waits for precedes it. A
/// copy launches alone once nothing runs, and nothing launches beside it.
fn launchable(executions: &[Execution]) -> Vec<&Execution> {
    let (mut running, mut ready) = (0, Vec::new());
    for e in executions {
        match (e.status, e.copy.is_some()) {
            (Status::Running, true) => return Vec::new(),
            (Status::Running, false) => running += 1,
            (Status::Requested, true) => {
                if running == 0 && ready.is_empty() {
                    ready.push(e);
                }
                break;
            }
            (Status::Requested, false) => ready.push(e),
            _ => {}
        }
    }
    ready
}
pub(super) fn dispatch(shared: &Arc<Mutex<State>>, threads: &mut Vec<JoinHandle<()>>) -> io::Result<()> {
    let mut index = 0;
    while index < threads.len() {
        if threads[index].is_finished() {
            let _ = threads.swap_remove(index).join();
        } else {
            index += 1;
        }
    }
    let cancelled = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        state
            .services
            .iter()
            .filter(|s| !matches!(s.resource.phase, Phase::Running(_)))
            .flat_map(|s| {
                s.executions
                    .iter()
                    .filter(|e| e.status == Status::Requested)
                    .map(|e| (s.binding_snapshot(), e.id.clone()))
            })
            .collect::<Vec<_>>()
    };
    for (service, id) in cancelled {
        update(
            shared,
            &service,
            &id,
            |e| {
                e.status = Status::Interrupted;
                e.command.clear();
                e.diagnostic = "Service ended before the command started; no command launched".into();
            },
            true,
        )?;
    }
    let ready = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        state
            .services
            .iter()
            .filter(|s| matches!(s.resource.phase, Phase::Running(_)) && !s.resource.lease.expired(now()))
            .flat_map(|s| {
                launchable(&s.executions)
                    .into_iter()
                    .map(move |e| (s.binding_snapshot(), e.clone()))
            })
            .collect::<Vec<_>>()
    };
    for (service, execution) in ready {
        update(
            shared,
            &service,
            &execution.id,
            |e| {
                e.status = Status::Running;
                e.diagnostic.clear();
                // The launched snapshot carries the text; a restart never
                // replays a running command, so state keeps only its label.
                e.command.clear();
            },
            true,
        )?;
        let worker = shared.clone();
        let snapshot = service.clone();
        let request = execution.clone();
        match std::thread::Builder::new()
            .name("control-service-command".into())
            .spawn(move || execute(worker, snapshot, request))
        {
            Ok(thread) => threads.push(thread),
            Err(error) => {
                update(
                    shared,
                    &service,
                    &execution.id,
                    |e| {
                        e.status = Status::Interrupted;
                        e.diagnostic = "Could not start service command supervisor; no command launched".into();
                    },
                    true,
                )?;
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
