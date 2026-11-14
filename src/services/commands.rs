//! Durable commands inside an exact, credential-free service. No host paths or
//! engine options enter the request. Uncertain effects are never replayed.
use super::*;
use control_effect::sync::{CancelToken, Recv};
use std::{
    io::Read,
    process::{Command, Stdio},
    thread::JoinHandle,
    time::Instant,
};

const MAX_EXECUTIONS: usize = 16;
const MAX_RECEIPTS: usize = 1024;

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
}
impl Execution {
    /// The first line of a command still running, for operator views.
    pub(super) fn running_label(&self) -> Option<String> {
        let command = if self.display_command.is_empty() { &self.command } else { &self.display_command };
        matches!(self.status, Status::Running).then(|| command.lines().next().unwrap_or_default().chars().take(80).collect())
    }
    pub(super) fn brief(&self) -> Value {
        json!({"execution_id":self.id,"status":self.status,"exit_code":self.exit_code,"diagnostic":self.diagnostic})
    }
    fn summary(&self) -> Value {
        let mut value = self.brief();
        value["output"] = json!(self.output);
        value["output_truncated"] = json!(self.truncated);
        value["output_limit_bytes"] = json!(OUTPUT);
        value
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
    fn summary(&self) -> Value {
        json!({"execution_id":self.id,"status":self.status,"exit_code":self.exit_code,"diagnostic":self.diagnostic,"output":"","output_evicted":true,"output_truncated":true,"output_limit_bytes":OUTPUT})
    }
}
fn live_attempt(state: &State, run: &str, attempt: &str) -> bool {
    state.runtime.runs.iter().any(|r| {
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
}
fn default_timeout() -> u64 {
    120
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
        },
        |_| Ok(()),
    )
}
pub(super) fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"control_service_exec","description":"Run a command asynchronously inside an exact chat-owned running service, using its private files and approved loopback relays. Use shell commands/heredocs to create task files, configure clients or install dependencies; no host paths, mounts or engine options. Returns an execution_id; inspect control_service_exec_status. One active command per service. Commands survive model turns. Long-lived background programs must redirect their output to private files; the service foreground process must remain alive. A timed-out or interrupted command is stopped inside the service, which keeps running; the service is stopped only if that cannot be verified. Run long work such as large downloads as a watch (control_watch_start) instead. Uncertain commands are never replayed.","inputSchema":{"type":"object","properties":{"service_id":{"type":"string"},"command":{"type":"string","minLength":1,"maxLength":16000},"timeout_seconds":{"type":"integer","minimum":1,"maximum":1800,"default":120}},"required":["service_id","command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":true}}),
        json!({"name":"control_service_exec_status","description":"Read a chat-owned service command's bounded live output, exit code and durable outcome. Use a fresh execution_id only for a deliberate new command; retries with the same request identity never replay effects.","inputSchema":{"type":"object","properties":{"service_id":{"type":"string"},"execution_id":{"type":"string"}},"required":["service_id","execution_id"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
    ]
}
fn hash(value: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, value)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn owned(
    state: &State,
    run: &str,
    generation: &str,
    node: &NodeId,
    service: &str,
) -> io::Result<usize> {
    let (active, _) = caller(state, run, generation)?;
    state
        .services
        .iter()
        .position(|s| s.resource.id.as_str() == service && accessible(s, node, active))
        .ok_or_else(|| fail("Service is not owned by this chat"))
}
pub(super) fn exec(shared: &Arc<Mutex<State>>, run: &str, generation: &str, nonce: &str, node: &NodeId, input: Input) -> io::Result<Value> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    admit(&mut state, run, generation, nonce, node, input, crate::storage::save)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handle {
    service_id: String,
    execution_id: String,
}
pub(super) fn status(shared: &Arc<Mutex<State>>, run: &str, generation: &str, node: &NodeId, input: Handle) -> io::Result<Value> {
    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let index = owned(&state, run, generation, node, &input.service_id)?;
    state.services[index]
        .executions
        .iter()
        .find(|e| e.id == input.execution_id)
        .map(Execution::summary)
        .or_else(|| {
            state.services[index]
                .command_receipts
                .iter()
                .find(|r| r.id == input.execution_id)
                .map(Receipt::summary)
        })
        .ok_or_else(|| fail("Service command not found"))
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
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(fail("Invalid service command identity"));
    }
    if !(1..=1800).contains(&input.timeout_seconds) {
        return Err(fail("timeout_seconds must be 1–1800"));
    }
    let index = owned(state, run, generation, node, &input.service_id)?;
    let id = hash(
        &serde_json::to_vec(&(input.service_id.as_str(), run, generation, nonce)).map_err(fail)?,
    );
    let digest = hash(&serde_json::to_vec(&(&input.command, input.timeout_seconds)).map_err(fail)?);
    let service = &state.services[index];
    if let Some(previous) = service.executions.iter().find(|e| e.id == id) {
        return if previous.digest == digest {
            Ok(previous.summary())
        } else {
            Err(fail(
                "Service command identity was reused with different arguments",
            ))
        };
    }
    if let Some(receipt) = service
        .command_receipts
        .iter()
        .find(|receipt| receipt.id == id)
    {
        return if receipt.digest == digest {
            Ok(receipt.summary())
        } else {
            Err(fail(
                "Service command identity was reused with different arguments",
            ))
        };
    }
    if !matches!(service.resource.phase, Phase::Running(_)) || service.resource.lease.expired(now())
    {
        return Err(fail("Wait for a running service with a live lease"));
    }
    if service.executions.iter().any(|e| e.status.active()) {
        return Err(fail(
            "Service already has an active command; inspect its execution status",
        ));
    }
    let evict = if service.executions.len() >= MAX_EXECUTIONS {
        Some(
            service
                .executions
                .iter()
                .position(|e| !e.status.active())
                .ok_or_else(|| fail("Service already has an active command"))?,
        )
    } else {
        None
    };
    let receipt = evict.and_then(|i| {
        let e = &state.services[index].executions[i];
        live_attempt(state, &e.run, &e.attempt).then(|| Receipt::from_execution(e))
    });
    let old_receipts = if receipt.is_some()
        && state.services[index].command_receipts.len() >= MAX_RECEIPTS
    {
        let retained = state.services[index]
            .command_receipts
            .iter()
            .filter(|r| live_attempt(state, &r.run, &r.attempt))
            .cloned()
            .collect::<Vec<_>>();
        if retained.len() >= MAX_RECEIPTS {
            return Err(fail(
                "Active command receipt budget exhausted; finish this model turn before running more commands",
            ));
        }
        Some(std::mem::replace(
            &mut state.services[index].command_receipts,
            retained,
        ))
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
        command: input.command.0.to_string(),
        display_command: input.command.0.chars().take(2000).collect(),
        timeout_seconds: input.timeout_seconds,
        status: Status::Requested,
        output: String::new(),
        truncated: false,
        exit_code: None,
        diagnostic: String::new(),
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
    Ok(value)
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    truncated: bool,
}
impl Output {
    fn append(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(OUTPUT.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..count]);
        self.truncated |= count < bytes.len();
    }
    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
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
    command: Command,
    timeout: Duration,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&Output),
) -> io::Result<ResultOutput> {
    capture_with(command, timeout, cancel, progress, &crate::shutdown::token())
}
/// Shutdown cancellation interrupts the command like a failed `cancel` check.
fn capture_with(
    mut command: Command,
    timeout: Duration,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&Output),
    shutdown: &CancelToken,
) -> io::Result<ResultOutput> {
    if timeout.is_zero() || cancel() {
        return Err(fail("Service command is no longer eligible"));
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
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
            break exited(watched.wait(None, &CancelToken::new())).unwrap_or_else(|| Err(fail("Service command did not exit")));
        }
        if let Ok(output) = output.lock() {
            progress(&output);
        }
    };
    let _ = reader.join();
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
pub(super) fn logs(id: &str) -> io::Result<String> {
    let mut command = crate::engine::cli();
    command.args(["logs", "--tail", "80", id]);
    let result = capture(command, Duration::from_secs(5), || false, |_| {})?;
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
    let Some(index) = state.services.iter().position(|s| {
        s.resource.id == service.resource.id && s.resource.generation == service.resource.generation
    }) else {
        return Err(fail("Service generation changed"));
    };
    let execution = state.services[index]
        .executions
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or_else(|| fail("Service command missing"))?;
    let previous = persist.then(|| execution.clone());
    let previous_output = (
        execution.output.len(),
        execution.truncated,
        execution.status,
    );
    change(execution);
    let changed = previous_output
        != (
            execution.output.len(),
            execution.truncated,
            execution.status,
        );
    let run = execution.run.clone();
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
    Ok(())
}
fn stop(shared: &Arc<Mutex<State>>, service: &Service) -> io::Result<()> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    if let Some(index) = state.services.iter().position(|s| {
        s.resource.id == service.resource.id && s.resource.generation == service.resource.generation
    }) {
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
    format!(r#"m={MARKER}={execution}
members() {{ for d in /proc/[0-9]*; do read -r s 2>/dev/null < "$d/stat" || continue; s=${{s##*) }}; [ "${{s%% *}}" = Z ] && continue; while IFS= read -r -d '' v; do [ "$v" = "$m" ] && {{ echo "${{d#/proc/}}"; break; }}; done 2>/dev/null < "$d/environ"; done; }}
signal() {{ for p in $(members); do kill -"$1" "$p" 2>/dev/null; done; }}
signal TERM
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -z "$(members)" ] && exit 0; sleep 0.2; done
signal KILL
sleep 0.2
[ -z "$(members)" ]"#)
}
fn reap(service: &Service, execution: &str) -> bool {
    let Some(id) = service.resource.phase.container() else { return false };
    let command = crate::engine::exec(id.as_str(), &format!("{}:{}", service.uid, service.gid), None, &[], &["bash", "-c", &reap_script(execution)]);
    capture(command, Duration::from_secs(10), || false, |_| {}).is_ok_and(|r| r.exit_code == Some(0) && !r.timed_out)
}
fn execute(shared: Arc<Mutex<State>>, service: Service, execution: Execution) {
    let result = (|| -> io::Result<ResultOutput> {
        identity::authorize(&service.actor, Action::Task).map_err(fail)?;
        let id = service
            .resource
            .phase
            .container()
            .ok_or_else(|| fail("Service is not running"))?;
        let text = crate::engine::query(&["inspect", "--format", INSPECT, id.as_str()])?;
        let observed: Inspection = serde_json::from_str(text.trim()).map_err(fail)?;
        verify(&service, &observed)?;
        if observed.status != "running" || service.resource.lease.expired(now()) {
            return Err(fail("Service is no longer running"));
        }
        let command = crate::engine::exec(id.as_str(), &format!("{}:{}", service.uid, service.gid), Some("/workspace"), &[(MARKER, &execution.id)], &["bash", "-c", &launch(&execution.command)]);
        let checked = std::cell::Cell::new(Instant::now());
        let authorized = std::cell::Cell::new(true);
        capture(
            command,
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
                    || shared.lock().map_or(true, |s| {
                        !s.services.iter().any(|s| s.resource == service.resource)
                    })
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
        )
    })();
    let unfinished = !result
        .as_ref()
        .is_ok_and(|r| !r.interrupted && !r.timed_out && r.exit_code.is_some());
    // A command that outlived its client is stopped inside the service; only an
    // unverifiable stop falls back to stopping the whole service.
    let reaped = unfinished && result.is_ok() && reap(&service, &execution.id);
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
                    if uncertain {
                        e.diagnostic =
                            "Service stop requested because command completion was uncertain"
                                .into();
                    } else if reaped {
                        e.diagnostic = "The command was stopped inside the service, which keeps running. Run long work as a watch (control_watch_start) instead.".into();
                    }
                }
                Err(_) => {
                    e.status = Status::Interrupted;
                    e.diagnostic="Service command could not complete; inspect service diagnostics. It was not replayed.".into();
                }
            }
        },
        true,
    );
}
pub(super) fn recover(
    shared: &Arc<Mutex<State>>,
    save: impl Fn(&State) -> io::Result<()>,
) -> io::Result<()> {
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
                execution.diagnostic="Control restarted before command completion; service stop requested, no replay".into();
            }
        }
        let resource = &state.services[index].resource;
        let decision = decide(
            resource,
            resource.stamp(),
            Event::Stop(StopReason::Requested),
            now(),
        )
        .map_err(fail)?;
        state.services[index].resource.phase = decision.phase;
        if let Err(error) = save(&state) {
            state.services[index] = old;
            return Err(error);
        }
    }
    Ok(())
}
pub(super) fn dispatch(
    shared: &Arc<Mutex<State>>,
    threads: &mut Vec<JoinHandle<()>>,
) -> io::Result<()> {
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
                e.diagnostic =
                    "Service ended before the command started; no command launched".into();
            },
            true,
        )?;
    }
    let ready = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        state
            .services
            .iter()
            .filter(|s| {
                matches!(s.resource.phase, Phase::Running(_))
                    && !s.resource.lease.expired(now())
                    && !s.executions.iter().any(|e| e.status == Status::Running)
            })
            .filter_map(|s| {
                s.executions
                    .iter()
                    .find(|e| e.status == Status::Requested)
                    .map(|e| (s.binding_snapshot(), e.clone()))
            })
            .collect::<Vec<_>>()
    };
    for (service, execution) in ready {
        update(
            shared,
            &service,
            &execution.id,
            |e| e.status = Status::Running,
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
                        e.diagnostic =
                            "Could not start service command supervisor; no command launched"
                                .into();
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
