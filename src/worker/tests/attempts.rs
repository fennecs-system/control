//! Whole attempts against the scripted container backend, with no engine and
//! no model. Each provider CLI is a local shell that replays a stream in the
//! shapes the decoders in `crates/ai` are tested with, and makes one real
//! Control tool call (the handoff) through the attempt's mailbox. Probes are
//! local shells that write their result file where the container would.
use super::*;
use crate::backend::Backend;
use control_isolation::{
    Access, Network, RunSpec, Source,
    scripted::{Answer, Call, Scripted},
};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

const IMAGE: &str = "sha256:5c0a4b1e2d3f40516273849a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5c6d7e8";
const NODE: &str = "0123456789abcdef0123456789abcdef";
/// The handoff's progress line is also the turn's last message, so the
/// reply is the same whichever the host reads first.
const REPLY: &str = "Castle built.";

/// The host path a spec binds at `target`.
fn host(spec: &RunSpec, target: &str) -> PathBuf {
    spec.mounts
        .iter()
        .find(|mount| mount.target == target)
        .and_then(|mount| match &mount.source {
            Source::Bind(path) => Some(path.clone()),
            Source::Volume(_) => None,
        })
        .unwrap_or_else(|| panic!("{} binds nothing at {target}", spec.name))
}
fn sh(script: &str, args: &[&str]) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script).arg("sh").args(args);
    command
}
/// Passes the probe: Codex names its result file last, the shell probes of
/// the other backends write `isolation-check`.
fn probe(spec: &RunSpec) -> Command {
    let result = match spec.argv.first().map(String::as_str) {
        Some("codex") => spec.argv.last().cloned().unwrap_or_default(),
        _ => "isolation-check".into(),
    };
    let workspace = host(spec, "/workspace");
    sh(
        r#"cat >/dev/null; printf verified > "$1/$2""#,
        &[workspace.to_str().unwrap(), &result],
    )
}
/// A CLI turn: reads its prompt, prints `before`, calls the handoff through
/// the mailbox at `/workspace` (keeping the reply beside it as
/// `stub-tool-reply.json`), writes `files` into its `/workspace` as pairs of
/// name and contents, prints `after` and exits 0.
fn cli(spec: &RunSpec, before: &str, after: &str, files: &[(&str, &str)]) -> Command {
    const SCRIPT: &str = r#"set -eu
cat >/dev/null
m=$1
printf '%s' "$2"
printf '%s' "$3" > "$m/.stub-request"
mv "$m/.stub-request" "$m/.control-room-request"
tries=0
until [ -e "$m/.control-room-response" ]; do
  tries=$((tries + 1)); [ "$tries" -lt 400 ] || exit 3; sleep 0.05
done
mv "$m/.control-room-response" "$m/../stub-tool-reply.json"
after=$4
shift 4
while [ "$#" -gt 1 ]; do printf '%s' "$2" > "$m/$1"; shift 2; done
printf '%s' "$after"
"#;
    let mailbox = host(spec, "/workspace");
    let request = json!({"version":1,"nonce":"stub-handoff","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_turn_handoff","arguments":{"disposition":{"kind":"done"},"progress":REPLY}}}}).to_string();
    let args = [mailbox.to_str().unwrap(), before, &request, after]
        .into_iter()
        .chain(files.iter().flat_map(|(name, contents)| [*name, *contents]));
    sh(SCRIPT, &args.collect::<Vec<_>>())
}
/// A CLI that prints `stream` and exits with `code`, calling no tool.
fn replay(stream: &str, code: i32) -> Command {
    sh(
        r#"cat >/dev/null; printf '%s' "$1"; exit "$2""#,
        &[stream, &code.to_string()],
    )
}
fn lines(records: &[Value]) -> String {
    records.iter().map(|record| format!("{record}\n")).collect()
}

/// A private, empty stand-in for each backend's login, located where the
/// backend looks for it in tests. Only its path and metadata are used.
fn credential(backend: Backend) -> PathBuf {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let (directory, file) = match backend {
        Backend::Codex => (crate::config::test_root().join("codex"), "auth.json"),
        Backend::Claude => (crate::config::directory().unwrap().join("claude"), ".credentials.json"),
        Backend::Grok => (crate::config::directory().unwrap().join("grok"), "auth.json"),
        Backend::Antigravity => (
            crate::config::directory().unwrap().join("antigravity"),
            "gemini-api-key",
        ),
        Backend::Local => unreachable!("local endpoints have no login"),
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let path = directory.join(file);
    let _ = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path);
    path
}

/// One finished attempt, committed like the worker commits it.
struct Finished {
    outcome: Result<String, (&'static str, String)>,
    attempt: crate::usage::Attempt,
    /// The attempt as the host saw it before the commit archived its activity.
    live: crate::usage::Attempt,
    state: State,
    generation: String,
    root: PathBuf,
    calls: Vec<Call>,
}
impl Finished {
    fn spawned(&self) -> Vec<&RunSpec> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                Call::Spawn(spec) => Some(spec),
                _ => None,
            })
            .collect()
    }
    /// The turn's container: the one spawn that is not the probe.
    fn turn(&self) -> &RunSpec {
        let spawned = self.spawned();
        let names: Vec<_> = spawned.iter().map(|spec| spec.name.as_str()).collect();
        let (probe, turn) = (
            format!("control-{}-probe", self.generation),
            format!("control-{}", self.generation),
        );
        assert_eq!(
            names,
            [probe.as_str(), turn.as_str()],
            "probe, then launch: {:?}",
            self.outcome
        );
        spawned[1]
    }
    fn activity(&self) -> Vec<(&str, &str, &str)> {
        self.live
            .activity
            .iter()
            .map(|a| (a.kind.as_str(), a.text.as_str(), a.status.as_str()))
            .collect()
    }
    /// What the Control tool answered the CLI.
    fn tool_reply(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.join("stub-tool-reply.json")).unwrap()).unwrap()
    }
}

static NODE_STATE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
/// A chat turn for `backend`, admitted, run through [`execute`] on the
/// scripted engine with `turn` serving the CLI container, and committed as
/// `run_task` commits it.
fn attempt(backend: Backend, turn: impl Fn(&RunSpec) -> Command + Sync) -> Finished {
    NODE_STATE.get_or_init(|| storage::initialize().unwrap());
    credential(backend);
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build a stone castle", "test").unwrap();
    state.runtime.runs[0].target.backend = backend;
    checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let attempt = &mut state.runtime.runs[0].attempts[0];
    attempt.image = Some(IMAGE.into());
    attempt.node_id = Some(NODE.into());
    // Run IDs come from the clock: tests running at once keep apart by a count.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    attempt.workspace_id = attempt
        .workspace_id
        .as_ref()
        .map(|id| format!("{id}-{}-{next}", backend.id()));
    let mut attempt = attempt.clone();
    let run = &state.runtime.runs[0];
    let (id, objective, actor, action) = (run.id.clone(), run.objective.to_string(), run.actor.clone(), run.action);
    let generation = attempt.workspace_id.clone().unwrap();
    let shared = Arc::new(Mutex::new(state));
    let engine = Scripted::new(|call: &Call| match call {
        Call::Spawn(spec) if spec.name.ends_with("-probe") => Answer::Run(probe(spec)),
        Call::Spawn(spec) => Answer::Run(turn(spec)),
        _ => Answer::Model,
    })
    .image(IMAGE, &[]);
    let outcome = execute(
        &engine,
        &generation,
        &objective,
        &actor,
        action,
        &mut attempt,
        Some((&shared, &id)),
    );
    let live = shared.lock().unwrap().runtime.runs[0].attempts[0].clone();
    let mut state = Arc::try_unwrap(shared)
        .ok()
        .expect("the attempt released its state")
        .into_inner()
        .unwrap();
    commit(&mut state, &id, action, attempt.clone(), outcome.clone(), false, |_| {
        Ok(())
    })
    .unwrap();
    Finished {
        outcome,
        attempt,
        live,
        state,
        root: fs::canonicalize(runtime::workspace(&generation).unwrap().parent().unwrap()).unwrap(),
        generation,
        calls: engine.calls(),
    }
}
/// The turn's login arrives as one typed read-only secret and nowhere else.
fn assert_secret(spec: &RunSpec, backend: Backend, name: &str, target: &str) {
    let canonical = |path: &std::path::Path| fs::canonicalize(path).unwrap();
    let [secret] = spec.secrets.as_slice() else {
        panic!("one secret: {:?}", spec.secrets)
    };
    assert_eq!(
        (secret.name.as_str(), secret.target.as_str(), secret.access),
        (name, target, Access::ReadOnly)
    );
    assert_eq!(canonical(&secret.source), canonical(&credential(backend)));
    let source = credential(backend).display().to_string();
    assert!(
        !spec.env.iter().any(|(_, value)| value.contains(&source))
            && !spec.argv.iter().any(|word| word.contains(&source)),
        "the login path is not passed as an argument or variable"
    );
}
/// The handoff was accepted, the reply is the turn's, its usage was recorded
/// and the workspace was published as the chat's new checkpoint.
fn assert_finished(finished: &Finished, tokens: (u64, u64, u64)) {
    assert_eq!(finished.outcome.as_deref(), Ok(REPLY));
    let reply = finished.tool_reply();
    assert_eq!(reply["nonce"], "stub-handoff");
    let result = &reply["response"]["result"];
    assert_eq!(
        (&result["isError"], &result["structuredContent"]["data"]["staged"]),
        (&json!(false), &json!(true)),
        "{reply}"
    );
    assert!(
        finished.live.turn_mailbox.as_ref().and_then(|m| m.accepted()).is_some(),
        "the handoff is staged"
    );
    assert_eq!(
        finished.attempt.tokens,
        Some(control_ai::Tokens {
            input_tokens: tokens.0,
            cached_input_tokens: tokens.1,
            output_tokens: tokens.2,
        })
    );
    assert_eq!(finished.attempt.accounting, crate::usage::Accounting::Reported);
    let run = &finished.state.runtime.runs[0];
    assert_eq!(run.status, "completed", "{}", run.result);
    let artifact = finished.attempt.artifact.clone().expect("the workspace was published");
    assert_eq!(finished.state.chats[0].workspace.as_deref(), Some(artifact.as_str()));
}

#[test]
fn a_codex_attempt_runs_from_probe_to_published_workspace() {
    let recorded = include_str!("../../../crates/ai/tests/fixtures/codex-handoff-completed.jsonl");
    let (before, after) = recorded.split_at(recorded.match_indices('\n').nth(2).unwrap().0 + 1);
    let after = after.replace("Handed off; waiting for the build watch.", REPLY);
    let finished = attempt(Backend::Codex, |spec| {
        cli(
            spec,
            before,
            &after,
            &[("castle.txt", "stone"), (".control-result", REPLY)],
        )
    });
    let turn = finished.turn();
    assert_eq!(turn.image.as_str(), IMAGE);
    assert_secret(turn, Backend::Codex, "codex/auth.json", "/home/agent/.codex/auth.json");
    let workspace = runtime::workspace(&finished.generation).unwrap();
    assert_eq!(host(turn, "/workspace"), fs::canonicalize(&workspace).unwrap());
    assert!(
        turn.mounts
            .iter()
            .any(|m| m.target == "/workspace" && m.access == Access::ReadWrite)
    );
    assert_eq!(turn.network, Network::Engine, "the provider is reachable");
    assert!(turn.seccomp.is_some());
    assert!(
        turn.argv
            .iter()
            .any(|word| word == "mcp_servers.control_rooms.command=\"/bin/control-room-mcp\""),
        "{:?}",
        turn.argv
    );
    assert_eq!(turn.argv[..2], ["codex", "exec"]);
    assert_eq!(
        finished.activity(),
        [
            ("mcp_tool_call", "control_turn_handoff", "completed"),
            ("message", REPLY, "completed"),
        ]
    );
    assert_finished(&finished, (184_233, 160_128, 2_210));
    let restored = finished.root.join("restored");
    crate::artifacts::restore(finished.attempt.artifact.as_deref().unwrap(), &restored).unwrap();
    assert_eq!(fs::read_to_string(restored.join("castle.txt")).unwrap(), "stone");
}

/// The tool call and final message every Claude-shaped stream carries, in the
/// shapes of `crates/ai/tests/fixtures/claude-stopped-mid-turn.jsonl`.
fn claude_stream() -> (String, String) {
    let session = "5f0c6c1e-1111-4b2a-9c3d-000000000001";
    let before = lines(&[
        json!({"type":"system","subtype":"init","cwd":"/workspace","session_id":session,"tools":["mcp__control__control_turn_handoff"],"mcp_servers":[{"name":"control","status":"connected"}],"model":"claude-sonnet-4-5-20250929","permissionMode":"dontAsk","apiKeySource":"none"}),
        json!({"type":"assistant","message":{"id":"msg_01A","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[{"type":"tool_use","id":"toolu_01","name":"mcp__control__control_turn_handoff","input":{"disposition":{"kind":"done"}}}],"stop_reason":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":4100,"cache_read_input_tokens":12000,"output_tokens":61}},"session_id":session}),
    ]);
    let after = lines(&[
        json!({"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01","type":"tool_result","content":"{\"accepted\":true}"}]},"session_id":session}),
        json!({"type":"assistant","message":{"id":"msg_01B","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[{"type":"text","text":REPLY}],"stop_reason":null,"usage":{"input_tokens":5,"cache_creation_input_tokens":120,"cache_read_input_tokens":16100,"output_tokens":40}},"session_id":session}),
        json!({"type":"result","subtype":"success","is_error":false,"result":REPLY,"usage":{"input_tokens":10,"cache_read_input_tokens":30,"cache_creation_input_tokens":5,"output_tokens":2}}),
    ]);
    (before, after)
}

#[test]
fn a_claude_attempt_runs_from_probe_to_published_workspace() {
    let (before, after) = claude_stream();
    let finished = attempt(Backend::Claude, |spec| cli(spec, &before, &after, &[]));
    let turn = finished.turn();
    assert_eq!(turn.image.as_str(), IMAGE);
    assert_secret(
        turn,
        Backend::Claude,
        "claude/credentials",
        "/home/agent/.claude/.credentials.json",
    );
    // The CLI sees only its mailbox; the workspace is reached through tools.
    assert_eq!(
        fs::canonicalize(host(turn, "/workspace")).unwrap(),
        finished.root.join("claude-mailbox")
    );
    assert_eq!(turn.network, Network::Engine);
    let config = turn.argv.iter().position(|word| word == "--mcp-config").unwrap();
    let config: Value = serde_json::from_str(&turn.argv[config + 1]).unwrap();
    assert_eq!(config["mcpServers"]["control"]["command"], "/bin/control-room-mcp");
    assert!(turn.argv.contains(&"--strict-mcp-config".to_owned()));
    let probe = finished.spawned()[0];
    assert_eq!(probe.network, Network::None, "the probe is offline");
    assert!(probe.secrets.is_empty(), "the probe mounts no login");
    assert_eq!(
        finished.activity(),
        [
            ("mcp_tool_call", "control_turn_handoff", "completed"),
            ("message", REPLY, "running"),
        ]
    );
    assert_finished(&finished, (45, 30, 2));
}

#[test]
fn a_grok_attempt_runs_from_probe_to_published_workspace() {
    // The shapes of `crates/ai/src/grok/tests.rs`.
    let before = lines(&[
        json!({"type":"available_commands","tools":["search_tool","use_tool"],"commands":[]}),
        json!({"type":"tool_call","toolCallId":"call_1","title":"Use tool","kind":"other","status":"in_progress","toolName":"use_tool","rawInput":{"tool_name":"control__control_turn_handoff","tool_input":{"disposition":{"kind":"done"}}},"content":[],"locations":[]}),
        json!({"type":"usage","messageId":"resp_1","stopReason":"tool_use","usage":{"input_tokens":400,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"reasoning_tokens":0}}),
    ]);
    let after = lines(&[
        json!({"type":"tool_call_update","toolCallId":"call_1","status":"completed","content":[],"rawOutput":{"accepted":true},"locations":[]}),
        json!({"type":"text","data":REPLY}),
        json!({"type":"end","stopReason":"end_turn","sessionId":"s","requestId":"r","usage":{"input_tokens":812,"cache_read_input_tokens":41000,"cache_creation_input_tokens":100,"output_tokens":45,"reasoning_tokens":7,"total_tokens":41957},"num_turns":2,"modelUsage":{"grok-4.7":{"inputTokens":812}}}),
    ]);
    let finished = attempt(Backend::Grok, |spec| cli(spec, &before, &after, &[]));
    let turn = finished.turn();
    assert_eq!(turn.image.as_str(), IMAGE);
    assert_secret(turn, Backend::Grok, "grok/auth.json", "/home/agent/.grok/auth.json");
    assert_eq!(
        fs::canonicalize(host(turn, "/workspace")).unwrap(),
        finished.root.join("grok-mailbox")
    );
    assert_eq!(turn.network, Network::Engine);
    let config = host(turn, "/home/agent/.grok/config.toml");
    assert!(
        fs::read_to_string(config)
            .unwrap()
            .contains("command = \"/bin/control-room-mcp\"")
    );
    assert!(
        turn.mounts
            .iter()
            .any(|m| m.target == "/home/agent/.grok/config.toml" && m.access == Access::ReadOnly)
    );
    assert_eq!(
        finished.activity(),
        [
            ("mcp_tool_call", "control_turn_handoff", "completed"),
            ("message", REPLY, "running"),
        ]
    );
    assert_finished(&finished, (41_912, 41_000, 45));
}

#[test]
fn an_antigravity_attempt_runs_from_probe_to_published_workspace() {
    // The shapes of `crates/ai/src/antigravity/tests.rs`, from the CLI's documentation.
    let tool = |state: &str, output: Option<&str>| {
        let mut info = json!({"name":"mcp_control_control_turn_handoff","parameters":{"disposition":{"kind":"done"}}});
        if let Some(output) = output {
            info["output"] = json!(output);
        }
        json!({"event":"step_update","step_update":{"conversation_id":"c","step_index":2,"state":state,"step_type":"tool","tool_name":"mcp_control_control_turn_handoff","tool_info":info}})
    };
    let before = lines(&[
        json!({"event":"init","conversation_id":"c","init":{"model":"gemini-4-neon-medium","cwd":"/workspace","tools":["ask_permission","call_mcp_tool","run_command"],"permission_mode":"always-proceed"}}),
        tool("ACTIVE", None),
    ]);
    let after = lines(&[
        tool("DONE", Some("{\"accepted\":true}")),
        json!({"event":"step_update","step_update":{"conversation_id":"c","step_index":3,"state":"DONE","step_type":"agent_response","text_delta":REPLY,"duration_seconds":6.2,"usage":{"input_tokens":10302,"output_tokens":582,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":10884}}}),
        json!({"event":"result","result":{"conversation_id":"c","status":"SUCCESS","response":REPLY,"duration_seconds":6.88,"num_turns":1,"usage":{"input_tokens":10418,"output_tokens":589,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":11007}}}),
    ]);
    let finished = attempt(Backend::Antigravity, |spec| cli(spec, &before, &after, &[]));
    let turn = finished.turn();
    assert_eq!(turn.image.as_str(), IMAGE);
    assert_secret(
        turn,
        Backend::Antigravity,
        "antigravity/api-key",
        "/home/agent/.control-key/gemini-api-key",
    );
    assert_eq!(
        fs::canonicalize(host(turn, "/workspace")).unwrap(),
        finished.root.join("antigravity-mailbox")
    );
    assert_eq!(turn.network, Network::Engine);
    let private = host(turn, "/home/agent/.control");
    let mcp: Value = serde_json::from_slice(&fs::read(private.join("mcp_config.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["control"]["command"], "/bin/control-room-mcp");
    assert_eq!(turn.argv[..2], ["control-agy", "turn"]);
    assert_eq!(
        finished.activity(),
        [
            ("mcp_tool_call", "control_turn_handoff", "completed"),
            ("message", REPLY, "running"),
        ]
    );
    assert_finished(&finished, (18_531, 8_113, 589));
}

// Past bugs a stub can now catch end to end.

/// 6c990d9: Codex reported its usage limit only on stdout, as `error` and
/// `turn.failed` events, and the run failed with the unknown-failure text.
#[test]
fn a_usage_limit_reported_only_on_stdout_names_the_failure() {
    let stream = lines(&[
        json!({"type":"thread.started","thread_id":"0199a1b2-7c3d-7e10-9f00-1a2b3c4d5e6f"}),
        json!({"type":"turn.started"}),
        json!({"type":"error","message":"You\u{2019}ve hit your usage limit. Visit https://example.invalid/usage or try again at Nov 26th."}),
        json!({"type":"turn.failed","error":{"message":"You\u{2019}ve hit your usage limit."}}),
    ]);
    let finished = attempt(Backend::Codex, |_| replay(&stream, 1));
    finished.turn();
    assert_eq!(
        finished.outcome,
        Err((
            "failed",
            "The signed-in Codex account reached a usage or rate limit.".into()
        ))
    );
    assert_eq!(finished.state.runtime.runs[0].status, "failed");
    assert_eq!(finished.state.chats[0].workspace, None, "nothing was published");
}

/// 9f37404: a stream that ends while a Control tool call is still running
/// must not read as success: the tool's effect is unconfirmed.
#[test]
fn a_stream_ending_inside_a_tool_call_interrupts_the_attempt() {
    let recorded = include_str!("../../../crates/ai/tests/fixtures/codex-handoff-stopped.jsonl");
    let unfinished: String = recorded.split_inclusive('\n').take(4).collect();
    assert!(unfinished.ends_with("\"status\":\"in_progress\"}}\n"));
    let finished = attempt(Backend::Codex, |spec| {
        let report = host(spec, "/workspace").join(".control-result");
        sh(
            r#"cat >/dev/null; printf done > "$1"; printf '%s' "$2""#,
            &[report.to_str().unwrap(), &unfinished],
        )
    });
    finished.turn();
    assert!(
        matches!(&finished.outcome, Err(("interrupted", message)) if message.contains("unfinished MCP tool call")),
        "{:?}",
        finished.outcome
    );
    assert_eq!(
        finished.attempt.termination,
        Some(crate::attempt_control::Termination::ProviderError)
    );
    assert_eq!(
        finished.activity(),
        [("mcp_tool_call", "control_service_exec", "interrupted")]
    );
    assert_eq!(finished.state.runtime.runs[0].status, "interrupted");
    assert_eq!(finished.state.chats[0].workspace, None, "nothing was published");
}

/// bbdff3e: probes reused one result name, and Docker Desktop's file cache
/// made every other probe fail; each probe now writes a fresh name and
/// removes it.
#[test]
fn each_sandbox_probe_writes_and_removes_a_fresh_result_name() {
    use std::os::unix::fs::MetadataExt;
    let root = crate::config::test_root().join(format!("probe-names-{}", probe_nonce().unwrap()));
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let policy = root.join("worker-seccomp.json");
    fs::write(&policy, "{}").unwrap();
    let owner = fs::metadata(&root).unwrap();
    let auth = Credentials {
        path: root.join("unused"),
        uid: owner.uid(),
        gid: owner.gid(),
    };
    let engine = Scripted::new(|call: &Call| match call {
        Call::Spawn(spec) => Answer::Run(probe(spec)),
        _ => Answer::Model,
    });
    for _ in 0..2 {
        crate::codex::sandbox_probe(&engine, IMAGE, &root, &auth, "control-probe-names", &policy, NODE).unwrap();
    }
    let results: Vec<_> = engine
        .calls()
        .iter()
        .filter_map(|call| call.spec()?.argv.last().cloned())
        .collect();
    assert_eq!(results.len(), 2);
    assert_ne!(results[0], results[1], "a fresh result name each time");
    let workspace = root.join("sandbox-probe/workspace");
    assert!(
        results.iter().all(|name| !workspace.join(name).exists()),
        "results are removed"
    );
}

/// f94d3a4: check and login containers are labelled as checks, so run
/// reconciliation never owns them; a run's containers are labelled as runs.
#[test]
fn check_containers_and_run_containers_carry_their_purpose() {
    NODE_STATE.get_or_init(|| storage::initialize().unwrap());
    let node = crate::config::node_id().unwrap();
    for backend in Backend::hosted() {
        let engine = Scripted::new(|call: &Call| match call {
            Call::Spawn(spec) => Answer::Run(probe(spec)),
            _ => Answer::Model,
        })
        .image(IMAGE, &[]);
        backend.agent().probe(&engine, IMAGE).unwrap();
        let calls = engine.calls();
        let spawned: Vec<_> = calls.iter().filter_map(Call::spec).collect();
        assert_eq!(spawned.len(), 1, "{backend:?}");
        let labels = &spawned[0].labels;
        assert!(
            labels.contains(&("dev.control.purpose".into(), "check".into())),
            "{backend:?}: {labels:?}"
        );
        assert!(
            labels.contains(&("dev.control.node".into(), node.clone())),
            "{backend:?}"
        );
    }
    let (before, after) = claude_stream();
    let finished = attempt(Backend::Claude, |spec| cli(spec, &before, &after, &[]));
    for spec in finished.spawned() {
        assert!(
            spec.labels.contains(&("dev.control.purpose".into(), "run".into())),
            "{}: {:?}",
            spec.name,
            spec.labels
        );
    }
}

/// The engine check comes first: with no engine nothing is prepared or launched.
#[test]
fn an_unready_engine_blocks_before_any_container_or_workspace() {
    let (_, run, mut attempt) = {
        credential(Backend::Codex);
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Build", "test").unwrap();
        checkpoint_next(&mut state, |_| Ok(())).unwrap();
        let mut attempt = state.runtime.runs[0].attempts[0].clone();
        attempt.image = Some(IMAGE.into());
        attempt.node_id = Some(NODE.into());
        attempt.workspace_id = attempt.workspace_id.map(|id| format!("{id}-unready"));
        let run = state.runtime.runs[0].clone();
        (state, run, attempt)
    };
    let engine = Scripted::new(|_: &Call| Answer::Model).unready();
    let generation = attempt.workspace_id.clone().unwrap();
    let outcome = execute(
        &engine,
        &generation,
        &run.objective,
        &run.actor,
        run.action,
        &mut attempt,
        None,
    );
    assert!(
        matches!(&outcome, Err(("blocked", message)) if message.contains("container engine unavailable")),
        "{outcome:?}"
    );
    assert_eq!(engine.calls(), [Call::Ready]);
    assert!(!runtime::workspace(&generation).unwrap().exists());
}
