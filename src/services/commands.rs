//! Durable commands inside an exact, credential-free service. No host paths or
//! engine options enter the request. Uncertain effects are never replayed.
use super::*;
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
    timeout_seconds: u64,
    status: Status,
    output: String,
    truncated: bool,
    exit_code: Option<i32>,
    diagnostic: String,
}
impl Execution {
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
struct Input {
    service_id: String,
    command: CommandText,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    120
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
pub(super) fn known(name: &str) -> bool {
    matches!(name, "control_service_exec" | "control_service_exec_status")
}
pub(super) fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"control_service_exec","description":"Run a command asynchronously inside an exact chat-owned running service, using its private files and approved loopback relays. Use shell commands/heredocs to create task files, configure clients or install dependencies; no host paths, mounts or engine options. Returns an execution_id; inspect control_service_exec_status. One active command per service. Commands survive model turns. Long-lived background programs must redirect their output to private files; the service foreground process must remain alive. Timeout or interrupted execution stops the service to prevent an untracked writer; never automatically replay an uncertain command.","inputSchema":{"type":"object","properties":{"service_id":{"type":"string"},"command":{"type":"string","minLength":1,"maxLength":16000},"timeout_seconds":{"type":"integer","minimum":1,"maximum":1800,"default":120}},"required":["service_id","command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":true}}),
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
pub(super) fn call(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    nonce: &str,
    name: &str,
    args: &Value,
    node: &NodeId,
) -> io::Result<Value> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    if name == "control_service_exec" {
        let input: Input = serde_json::from_value(args.clone())
            .map_err(|_| fail("Expected service_id, command and optional timeout_seconds"))?;
        return admit(
            &mut state,
            run,
            generation,
            nonce,
            node,
            input,
            crate::storage::save,
        );
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Handle {
        service_id: String,
        execution_id: String,
    }
    let input: Handle = serde_json::from_value(args.clone())
        .map_err(|_| fail("Expected service_id and execution_id"))?;
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
    mut command: Command,
    timeout: Duration,
    cancel: impl Fn() -> bool,
    progress: impl Fn(&Output),
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
    let started = Instant::now();
    let mut sampled = Instant::now();
    let mut interrupted = false;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel() || started.elapsed() >= timeout {
            timed_out = started.elapsed() >= timeout;
            interrupted = !timed_out;
            let _ = child.kill();
            break child.wait()?;
        }
        if sampled.elapsed() >= Duration::from_millis(250) {
            if let Ok(output) = output.lock() {
                progress(&output);
            }
            sampled = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let _ = reader.join();
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
    let mut command = Command::new("docker");
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
    change(execution);
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
fn execute(shared: Arc<Mutex<State>>, service: Service, execution: Execution) {
    let result = (|| -> io::Result<ResultOutput> {
        identity::authorize(&service.actor, Action::Task).map_err(fail)?;
        let id = service
            .resource
            .phase
            .container()
            .ok_or_else(|| fail("Service is not running"))?;
        let text = crate::runtime::docker_query(&["inspect", "--format", INSPECT, id.as_str()])?;
        let observed: Inspection = serde_json::from_str(text.trim()).map_err(fail)?;
        verify(&service, &observed)?;
        if observed.status != "running" || service.resource.lease.expired(now()) {
            return Err(fail("Service is no longer running"));
        }
        let mut command = Command::new("docker");
        command.args([
            "exec",
            "--user",
            &format!("{}:{}", service.uid, service.gid),
            "--workdir",
            "/workspace",
            id.as_str(),
            "bash",
            "-c",
            &format!("exec 2>&1\n{}", execution.command),
        ]);
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
    let uncertain = !result
        .as_ref()
        .is_ok_and(|r| !r.interrupted && !r.timed_out && r.exit_code.is_some());
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
mod tests {
    use super::*;
    fn fixture() -> (State, String, NodeId, String) {
        let (mut state, run, _, time) = super::super::tests::fixture();
        super::super::tests::admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
        state.services[0].resource.phase = Phase::Running("b".repeat(64).try_into().unwrap());
        let id = state.services[0].resource.id.as_str().to_owned();
        (state, run, "node-a".to_owned().try_into().unwrap(), id)
    }
    #[test]
    fn output_eviction_keeps_active_attempt_retry_receipts_and_rolls_back() {
        let (mut state, run, node, id) = fixture();
        for n in 0..MAX_EXECUTIONS + 8 {
            let nonce = format!("command-{n}");
            let before = serde_json::to_vec(&state).unwrap();
            if n == MAX_EXECUTIONS {
                assert!(
                    admit(
                        &mut state,
                        &run,
                        "attempt-a",
                        &nonce,
                        &node,
                        input(&id, "echo test"),
                        |_| Err(fail("disk full"))
                    )
                    .is_err()
                );
                assert_eq!(serde_json::to_vec(&state).unwrap(), before);
            }
            admit(
                &mut state,
                &run,
                "attempt-a",
                &nonce,
                &node,
                input(&id, "echo test"),
                |_| Ok(()),
            )
            .unwrap();
            let execution = state.services[0].executions.last_mut().unwrap();
            execution.status = Status::Completed;
            execution.exit_code = Some(0);
            execution.output = "retained output".into();
        }
        assert_eq!(state.services[0].executions.len(), MAX_EXECUTIONS);
        assert_eq!(state.services[0].command_receipts.len(), 8);
        let mut restored: State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let receipt = admit(
            &mut restored,
            &run,
            "attempt-a",
            "command-0",
            &node,
            input(&id, "echo test"),
            |_| panic!("Eviction must not replay effects"),
        )
        .unwrap();
        assert_eq!(receipt["output_evicted"], true);
        assert_eq!(receipt["status"], "completed");
        assert_eq!(receipt["exit_code"], 0);
        assert!(
            admit(
                &mut restored,
                &run,
                "attempt-a",
                "command-0",
                &node,
                input(&id, "different command"),
                |_| panic!("Changed identity must fail")
            )
            .is_err()
        );
    }
    #[test]
    fn receipt_pruning_never_discards_live_attempt_identity() {
        let (mut state, run, node, id) = fixture();
        for n in 0..MAX_EXECUTIONS + 1 {
            admit(
                &mut state,
                &run,
                "attempt-a",
                &format!("command-{n}"),
                &node,
                input(&id, "true"),
                |_| Ok(()),
            )
            .unwrap();
            state.services[0].executions.last_mut().unwrap().status = Status::Completed;
        }
        let receipt = state.services[0].command_receipts[0].clone();
        state.services[0].command_receipts = vec![receipt; MAX_RECEIPTS];
        let before = serde_json::to_vec(&state).unwrap();
        assert!(
            admit(
                &mut state,
                &run,
                "attempt-a",
                "next",
                &node,
                input(&id, "true"),
                |_| panic!("Full live receipt budget must fail before effects")
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        for receipt in &mut state.services[0].command_receipts {
            receipt.run = "finished-run".into();
        }
        let before = serde_json::to_vec(&state).unwrap();
        assert!(
            admit(
                &mut state,
                &run,
                "attempt-a",
                "next",
                &node,
                input(&id, "true"),
                |_| Err(fail("disk full"))
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        admit(
            &mut state,
            &run,
            "attempt-a",
            "next",
            &node,
            input(&id, "true"),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(state.services[0].command_receipts.len(), 1);
        assert_eq!(state.services[0].command_receipts[0].run, run);
    }
    fn input(service: &str, command: &str) -> Input {
        Input {
            service_id: service.into(),
            command: command.to_owned().try_into().unwrap(),
            timeout_seconds: 60,
        }
    }
    #[test]
    fn admission_is_durable_scoped_idempotent_and_single_writer() {
        let (mut state, run, node, id) = fixture();
        assert!(
            admit(
                &mut state,
                &run,
                "attempt-a",
                "nonce-a",
                &node,
                input(&id, "printf hello"),
                |_| Err(fail("disk full"))
            )
            .is_err()
        );
        assert!(state.services[0].executions.is_empty());
        let receipt = admit(
            &mut state,
            &run,
            "attempt-a",
            "nonce-a",
            &node,
            input(&id, "printf hello"),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            receipt,
            admit(
                &mut state,
                &run,
                "attempt-a",
                "nonce-a",
                &node,
                input(&id, "printf hello"),
                |_| panic!("Replay must not save")
            )
            .unwrap()
        );
        for (generation, nonce, command) in [
            ("attempt-old", "nonce-a", "printf hello"),
            ("attempt-a", "nonce-a", "printf different"),
            ("attempt-a", "nonce-b", "printf hello"),
        ] {
            assert!(
                admit(
                    &mut state,
                    &run,
                    generation,
                    nonce,
                    &node,
                    input(&id, command),
                    |_| panic!("Invalid request must not persist")
                )
                .is_err()
            );
        }
        assert!(
            admit(
                &mut state,
                &run,
                "attempt-a",
                "nonce-b",
                &"foreign-node".to_owned().try_into().unwrap(),
                input(&id, "true"),
                |_| Ok(())
            )
            .is_err()
        );
        state.services[0].executions[0].status = Status::Completed;
        state.runtime.runs[0].actor = Actor::Unknown;
        assert!(
            admit(
                &mut state,
                &run,
                "attempt-a",
                "nonce-b",
                &node,
                input(&id, "true"),
                |_| Ok(())
            )
            .is_err()
        );
    }
    #[test]
    fn restart_never_replays_uncertain_execution_and_save_failure_rolls_back() {
        let (mut state, run, node, id) = fixture();
        admit(
            &mut state,
            &run,
            "attempt-a",
            "nonce-a",
            &node,
            input(&id, "printf hello"),
            |_| Ok(()),
        )
        .unwrap();
        state.services[0].executions[0].status = Status::Running;
        let shared = Arc::new(Mutex::new(state));
        let before = serde_json::to_vec(&*shared.lock().unwrap()).unwrap();
        assert!(recover(&shared, |_| Err(fail("disk full"))).is_err());
        assert_eq!(
            serde_json::to_vec(&*shared.lock().unwrap()).unwrap(),
            before
        );
        recover(&shared, |_| Ok(())).unwrap();
        let restored: State =
            serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
        assert!(matches!(
            restored.services[0].resource.phase,
            Phase::Stopping { .. }
        ));
        assert_eq!(
            restored.services[0].executions[0].status,
            Status::Interrupted
        );
        assert!(restored.services[0].executions[0].command.is_empty());
        recover(&shared, |_| panic!("Recovery must not repeat")).unwrap();
    }
    #[test]
    fn capture_drains_large_output_without_unbounded_retention() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'early\\n'; head -c 100000 /dev/zero; exit 7"]);
        let result = capture(command, Duration::from_secs(5), || false, |_| {}).unwrap();
        assert_eq!(result.exit_code, Some(7));
        assert!(result.output.truncated);
        assert_eq!(result.output.bytes.len(), OUTPUT);
        assert!(result.output.text().starts_with("early\n"));
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 10"]);
        let start = Instant::now();
        let result = capture(command, Duration::from_millis(50), || false, |_| {}).unwrap();
        assert!(result.timed_out);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
