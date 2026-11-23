//! Whole attempts against the scripted container backend, with no engine and
//! no model. Each provider CLI is a local shell that replays a stream in the
//! shapes the decoders in `crates/ai` are tested with, and makes one real
//! Control tool call (the handoff) through the attempt's mailbox. Probes are
//! local shells that write their result file where the container would.
use super::*;
use crate::backend::Backend;
use crate::runtime::RunStatus;
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
    credential_on(backend, &crate::accounts::AccountName::implicit())
}
/// The same for a named account, in its own directory.
fn credential_on(backend: Backend, account: &crate::accounts::AccountName) -> PathBuf {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let named = |file| (crate::logins::directory(backend, account).unwrap(), file);
    let (directory, file) = match backend {
        _ if !account.is_implicit() => named(match backend {
            Backend::Codex | Backend::Grok => "auth.json",
            Backend::Claude => ".credentials.json",
            Backend::Antigravity => "gemini-api-key",
            Backend::Local => unreachable!("local endpoints have no login"),
        }),
        Backend::Codex => (crate::config::test_root().join(".codex"), "auth.json"),
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
    outcome: Result<String, (control_agent::Status, String)>,
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
    attempt_on(backend, "default", turn)
}
/// The same on `account`, which is added to the node's accounts when named.
fn attempt_on(backend: Backend, account: &str, turn: impl Fn(&RunSpec) -> Command + Sync) -> Finished {
    let name = crate::accounts::AccountName::try_from(account).unwrap();
    credential_on(backend, &name);
    attempt_as(backend, account, crate::accounts::AccountKind::Subscription, turn)
}
/// The same on `account` of `kind`, whose login the caller has saved.
fn attempt_as(
    backend: Backend,
    account: &str,
    kind: crate::accounts::AccountKind,
    turn: impl Fn(&RunSpec) -> Command + Sync,
) -> Finished {
    NODE_STATE.get_or_init(|| storage::initialize().unwrap());
    let account = crate::accounts::AccountName::try_from(account).unwrap();
    let mut state = State::default();
    if !account.is_implicit() {
        state.runtime.accounts = state
            .runtime
            .accounts
            .add(crate::accounts::Entry {
                backend,
                name: account.clone(),
                kind,
                cap: None,
                enabled: true,
                label: None,
            })
            .unwrap();
    }
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build a stone castle", "test").unwrap();
    state.runtime.runs[0].target.backend = backend;
    state.runtime.runs[0].target.account = account;
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
    let (id, objective, actor, action) = (
        run.id().to_string(),
        run.objective.to_string(),
        run.actor.clone(),
        run.action,
    );
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
/// A refreshable sign-in arrives as its own folder, writable, at `target`,
/// with the CLI pointed at it by `variable`, and nowhere else.
fn assert_folder(spec: &RunSpec, folder: &std::path::Path, name: &str, target: &str, variable: (&str, &str)) {
    let canonical = |path: &std::path::Path| fs::canonicalize(path).unwrap();
    let [secret] = spec.secrets.as_slice() else {
        panic!("one secret: {:?}", spec.secrets)
    };
    assert_eq!(
        (secret.name.as_str(), secret.target.as_str(), secret.access),
        (name, target, Access::ReadWrite)
    );
    assert_eq!(canonical(&secret.source), canonical(folder));
    assert!(
        spec.env.iter().any(|(n, v)| (n.as_str(), v.as_str()) == variable),
        "{:?}",
        spec.env
    );
    assert_eq!(
        control_agent::cli::secrets_private(spec),
        Ok(()),
        "beneath the private home"
    );
    let source = folder.display().to_string();
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
    assert_eq!(run.status, RunStatus::Completed, "{}", run.result);
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
    // Control's own default sign-in is refreshable: its folder, writable.
    assert_folder(
        turn,
        credential(Backend::Claude).parent().unwrap(),
        "claude/credentials",
        "/home/agent/.claude-login",
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR", "/home/agent/.claude-login"),
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
    assert_folder(
        turn,
        credential(Backend::Grok).parent().unwrap(),
        "grok/auth.json",
        "/home/agent/.grok-login",
        ("GROK_AUTH_PATH", "/home/agent/.grok-login/auth.json"),
    );
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
    // This account is an API key: the server starts without the exported key.
    assert_eq!(
        mcp["mcpServers"]["control"]["args"],
        json!(["-u", "GEMINI_API_KEY", "/bin/control-room-mcp"])
    );
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
            control_agent::Status::Failed,
            "The signed-in Codex account reached a usage or rate limit.".into()
        ))
    );
    assert_eq!(finished.state.runtime.runs[0].status, RunStatus::Failed);
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
        matches!(&finished.outcome, Err((control_agent::Status::Interrupted, message)) if message.contains("unfinished MCP tool call")),
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
    assert_eq!(finished.state.runtime.runs[0].status, RunStatus::Interrupted);
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
        write: control_agent::Write::ReadOnly,
    };
    let engine = Scripted::new(|call: &Call| match call {
        Call::Spawn(spec) => Answer::Run(probe(spec)),
        _ => Answer::Model,
    });
    for _ in 0..2 {
        control_codex::sandbox_probe(
            &crate::agent_host::NODE,
            &engine,
            IMAGE,
            &root,
            &auth,
            "control-probe-names",
            &policy,
            NODE,
        )
        .unwrap();
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
        backend.agent().probe(&crate::agent_host::NODE, &engine, IMAGE).unwrap();
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
        matches!(&outcome, Err((control_agent::Status::Blocked, message)) if message.contains("container engine unavailable")),
        "{outcome:?}"
    );
    assert_eq!(engine.calls(), [Call::Ready]);
    assert!(!runtime::workspace(&generation).unwrap().exists());
}

/// Accounts of one backend launch with their own logins: each turn mounts
/// only its account's own folder (or, for Codex, its own file), no account's
/// mount lies inside another's, and the backend is told nothing about
/// accounts. Named accounts and Control's own default sign-ins are writable;
/// the operator's own Codex sign-in is not.
#[test]
fn accounts_of_one_backend_each_mount_only_their_own_login() {
    let (before, after) = claude_stream();
    let turn = |spec: &RunSpec| cli(spec, &before, &after, &[(".control-result", REPLY)]);
    let names =
        ["default", "isolated-b", "isolated-c"].map(|name| crate::accounts::AccountName::try_from(name).unwrap());
    for backend in [Backend::Claude, Backend::Grok, Backend::Codex] {
        let finished: Vec<Finished> = names
            .iter()
            .map(|name| attempt_on(backend, name.as_str(), turn))
            .collect();
        let canonical = |path: &std::path::Path| fs::canonicalize(path).unwrap();
        // What each account's turn should mount: Codex rewrites its file in
        // place, Claude and Grok replace theirs inside their folder.
        let own = |name: &crate::accounts::AccountName| {
            let file = credential_on(backend, name);
            match backend {
                Backend::Codex => canonical(&file),
                _ => canonical(file.parent().unwrap()),
            }
        };
        let mounted: Vec<_> = finished
            .iter()
            .zip(&names)
            .map(|(finished, name)| {
                let spec = finished.turn();
                let [secret] = spec.secrets.as_slice() else {
                    panic!("{backend:?}: one secret: {:?}", spec.secrets)
                };
                let writable = !(backend == Backend::Codex && name.is_implicit());
                assert_eq!(
                    secret.access,
                    if writable { Access::ReadWrite } else { Access::ReadOnly },
                    "{backend:?}+{name}"
                );
                assert_eq!(control_agent::cli::secrets_private(spec), Ok(()));
                assert_eq!(canonical(&secret.source), own(name), "{backend:?}+{name}");
                assert_eq!(finished.attempt.account, *name, "the attempt records its account");
                canonical(&secret.source)
            })
            .collect();
        for (i, mine) in mounted.iter().enumerate() {
            for (j, theirs) in mounted.iter().enumerate().filter(|(j, _)| *j != i) {
                assert!(
                    !mine.starts_with(theirs) && !theirs.starts_with(mine),
                    "{backend:?}: {mine:?} and {theirs:?} overlap"
                );
                let spec = finished[i].turn();
                let other = theirs.display().to_string();
                assert!(
                    spec.mounts
                        .iter()
                        .all(|m| !matches!(&m.source, Source::Bind(p) if p.display().to_string() == other))
                        && spec.secrets.iter().all(|s| canonical(&s.source) != *theirs)
                        && !spec
                            .argv
                            .iter()
                            .chain(spec.env.iter().map(|(_, v)| v))
                            .any(|w| w.contains("isolated")),
                    "{backend:?}: account {i} sees account {j}'s login or an account's name"
                );
            }
        }
    }
    // Test binaries share the configuration folder: leave no named account behind.
    for backend in [Backend::Claude, Backend::Grok, Backend::Codex] {
        for name in &names[1..] {
            fs::remove_dir_all(crate::logins::directory(backend, name).unwrap()).unwrap();
        }
    }
}

/// A refreshable sign-in survives the turn as the CLI rewrote it, and nothing
/// else does: not what the CLI wrote beside it, and not what an earlier login
/// left there.
#[test]
fn only_the_refreshed_sign_in_outlives_a_turn_in_its_folder() {
    let (before, after) = claude_stream();
    let name = crate::accounts::AccountName::try_from("swept").unwrap();
    let folder = credential_on(Backend::Claude, &name).parent().unwrap().to_path_buf();
    // An earlier login kept its whole configuration folder here (PR #368).
    fs::create_dir_all(folder.join("projects/-workspace")).unwrap();
    fs::write(folder.join(".claude.json"), "{}").unwrap();
    // The stand-in CLI refreshes its sign-in by rename, as Claude does, and
    // writes a session and settings beside it, as a CLI pointed here might.
    const REFRESH: &str = r#"set -eu
f=$1; shift
printf refreshed > "$f/.credentials.json.tmp.0123abcd"
mv "$f/.credentials.json.tmp.0123abcd" "$f/.credentials.json"
printf session > "$f/projects/-workspace/session.jsonl"
printf '{}' > "$f/settings.json"
exec "$@""#;
    let finished = attempt_on(Backend::Claude, name.as_str(), |spec| {
        let stub = cli(spec, &before, &after, &[]);
        let source = spec.secrets[0].source.clone();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(REFRESH)
            .arg("sh")
            .arg(source)
            .arg(stub.get_program())
            .args(stub.get_args());
        command
    });
    assert_finished(&finished, (45, 30, 2));
    let mut left: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, [".credentials.json"]);
    assert_eq!(
        fs::read_to_string(folder.join(".credentials.json")).unwrap(),
        "refreshed"
    );
    // Test binaries share the configuration folder: leave no account behind.
    fs::remove_dir_all(&folder).unwrap();
}

/// A usage limit on one account cools it: the run fails with the provider's
/// reason, the account is held for new work, and a pool then picks another
/// account. A run that named the account is never moved.
#[test]
fn a_usage_limit_cools_its_account_and_a_pool_picks_another() {
    let stream = lines(&[
        json!({"type":"thread.started","thread_id":"0199a1b2-7c3d-7e10-9f00-1a2b3c4d5e6f"}),
        json!({"type":"turn.started"}),
        json!({"type":"error","message":"You\u{2019}ve hit your usage limit."}),
        json!({"type":"turn.failed","error":{"message":"You\u{2019}ve hit your usage limit."}}),
    ]);
    let mut finished = attempt_on(Backend::Codex, "second", |_| replay(&stream, 1));
    let name = crate::accounts::AccountName::try_from("second").unwrap();
    let runtime = &mut finished.state.runtime;
    assert_eq!(runtime.runs[0].status, RunStatus::Failed);
    let now = crate::providers::accounts::now();
    assert!(runtime.account_status.cooling(Backend::Codex, &name, now).is_some());
    assert!(
        runtime
            .account_status
            .cooling(Backend::Codex, &crate::accounts::AccountName::implicit(), now)
            .is_none(),
        "only the limited account cools"
    );
    let saved = serde_json::to_value(&*runtime).unwrap();
    assert_eq!(
        saved["account_status"]["cooling"][0]["name"], "second",
        "saved with the state"
    );
    // New work on the cooling account is held; a pool takes the other account.
    let mut next = runtime.runs[0].clone();
    next.status = RunStatus::Queued;
    assert_eq!(
        crate::providers::accounts::hold(runtime, &next),
        Some(crate::accounts::Hold::Cooling(None))
    );
    let pool = crate::providers::selection::choose(runtime, Backend::Codex, "codex+*:gpt-6.1-sol").unwrap();
    let picked = crate::providers::selection::admit(runtime, &pool).unwrap();
    assert!(picked.account.is_implicit(), "{picked:?}");
    crate::providers::accounts::clear(runtime, Backend::Codex, &name);
    assert_eq!(crate::providers::accounts::hold(runtime, &next), None);
}

// API-key accounts: a model can never read the key.

/// A CLI turn that first runs a command through Control's shell tool, as a
/// model trying to read the key would, then hands off. Each request waits
/// for its reply; the shell's reply is kept as `stub-shell-reply.json`.
fn cli_with_shell(spec: &RunSpec, before: &str, after: &str) -> Command {
    const SCRIPT: &str = r#"set -eu
cat >/dev/null
m=$1
call() {
  printf '%s' "$1" > "$m/.stub-request"
  mv "$m/.stub-request" "$m/.control-room-request"
  tries=0
  until [ -e "$m/.control-room-response" ]; do
    tries=$((tries + 1)); [ "$tries" -lt 400 ] || exit 3; sleep 0.05
  done
  mv "$m/.control-room-response" "$m/../$2"
}
printf '%s' "$2"
call "$3" stub-shell-reply.json
call "$4" stub-tool-reply.json
printf '%s' "$5"
"#;
    let mailbox = host(spec, "/workspace");
    let call = |nonce: &str, name: &str, arguments: Value| {
        json!({"version":1,"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}}).to_string()
    };
    let shell = call(
        "stub-shell",
        "control_workspace_run",
        json!({"command":"cat /home/agent/.control-key/* /home/agent/.claude/.credentials.json /home/agent/.grok/auth.json; env"}),
    );
    let handoff = call(
        "stub-handoff",
        "control_turn_handoff",
        json!({"disposition":{"kind":"done"},"progress":REPLY}),
    );
    sh(SCRIPT, &[mailbox.to_str().unwrap(), before, &shell, &handoff, after])
}

/// Every file under `root` whose bytes contain `canary`.
fn holding(root: &std::path::Path, canary: &str) -> Vec<PathBuf> {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .flat_map(|entry| {
                    let path = entry.path();
                    match fs::symlink_metadata(&path) {
                        Ok(meta) if meta.is_dir() => holding(&path, canary),
                        Ok(meta) if meta.is_file() => fs::read(&path)
                            .ok()
                            .filter(|bytes| bytes.windows(canary.len()).any(|w| w == canary.as_bytes()))
                            .map(|_| path)
                            .into_iter()
                            .collect(),
                        _ => Vec::new(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One attempt of `backend` on an API-key account whose key is a fresh
/// canary, with `turn` serving the CLI. The key reaches the turn only as one
/// typed read-only secret mount at `target`; it is in no argument, variable,
/// file Control wrote, record, diagnostic or published file, and no other
/// container (the probe, the model's shell) mounts any secret.
fn assert_key_unreadable(backend: Backend, target: &str, turn: impl Fn(&RunSpec) -> Command + Sync) -> Finished {
    use std::os::unix::fs::DirBuilderExt;
    let name = format!("key-{}", backend.id());
    let account = crate::accounts::AccountName::try_from(name.as_str()).unwrap();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let canary = format!("CANARY-{}-{}-{nanos}", backend.id(), std::process::id());
    let directory = crate::logins::directory(backend, &account).unwrap();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let file = crate::accounts::Backend::key_file(backend).unwrap().name;
    crate::config::write_in(&directory, file, canary.as_bytes()).unwrap();
    let key = fs::canonicalize(directory.join(file)).unwrap();
    let finished = attempt_as(backend, &name, crate::accounts::AccountKind::ApiKey, |spec| turn(spec));
    assert_eq!(finished.outcome.as_deref(), Ok(REPLY), "{backend:?}");
    let turn_name = format!("control-{}", finished.generation);
    let spawned = finished.spawned();
    let launched = spawned
        .iter()
        .find(|spec| spec.name == turn_name)
        .expect("the turn launched");
    let [secret] = launched.secrets.as_slice() else {
        panic!("one secret: {:?}", launched.secrets)
    };
    assert_eq!(
        (secret.target.as_str(), secret.access),
        (target, Access::ReadOnly),
        "{backend:?}"
    );
    assert_eq!(fs::canonicalize(&secret.source).unwrap(), key);
    let directory = fs::canonicalize(&directory).unwrap();
    // A key is never writable, in the turn or in any other container.
    assert!(
        spawned
            .iter()
            .all(|spec| spec.secrets.iter().all(|s| s.access == Access::ReadOnly)),
        "{backend:?}: an API-key account mounts nothing writable"
    );
    spawned.iter().for_each(|spec| {
        let shown = format!("{:?}{:?}", spec.argv, spec.env);
        assert!(!shown.contains(&canary), "{backend:?}: {} names the key", spec.name);
        assert!(
            spec.mounts
                .iter()
                .all(|m| !matches!(&m.source, Source::Bind(p) if p.starts_with(&directory))),
            "{backend:?}: {} binds the account directory",
            spec.name
        );
        // Codex's probe mounts an empty stand-in; nothing else but the turn has the key.
        if spec.name != turn_name {
            assert!(
                spec.secrets
                    .iter()
                    .all(|s| fs::canonicalize(&s.source).map_or(true, |p| p != key && !p.starts_with(&directory))),
                "{backend:?}: {} mounts the key",
                spec.name
            );
        }
    });
    // Files Control wrote for the turn: MCP configuration, settings, prompt,
    // mailbox, workspace and the tool replies the stub kept.
    assert_eq!(holding(&finished.root, &canary), Vec::<PathBuf>::new(), "{backend:?}");
    // What was recorded: activity, diagnostics, usage and the run itself.
    let recorded = serde_json::to_string(&(&finished.state.runtime, &finished.live, &finished.attempt)).unwrap();
    assert!(!recorded.contains(&canary), "{backend:?}: the key was recorded");
    assert!(
        !recorded.contains(&account.to_string()) || finished.attempt.account == account,
        "only the attempt's own record names its account"
    );
    let artifact = finished.attempt.artifact.clone().expect("the workspace was published");
    let manifest = crate::artifacts::manifest(&artifact).unwrap();
    manifest.files.iter().for_each(|entry| {
        let bytes = crate::artifacts::read(&manifest, &entry.path).unwrap();
        assert!(
            !bytes.windows(canary.len()).any(|w| w == canary.as_bytes()),
            "{backend:?}: {} was published with the key",
            entry.path
        );
    });
    // The key files are never published if they appear in a workspace.
    assert!(crate::artifacts::excluded(file), "{file}");
    fs::remove_dir_all(directory).unwrap();
    finished
}

/// The model's shell, for backends whose CLI uses Control's: the request
/// reached Control's host tool (the test engine runs no shell, so its reply is
/// an engine error), and the container that tool launches for this workspace
/// is offline and has no secret, whatever the account.
fn model_shell(finished: &Finished) {
    let reply = fs::read_to_string(finished.root.join("stub-shell-reply.json")).unwrap();
    assert!(reply.contains("stub-shell") && !reply.contains("CANARY"), "{reply}");
    let workspace = runtime::workspace(&finished.generation).unwrap();
    let owner = control_isolation::User { uid: 1000, gid: 1000 };
    let shell = crate::host_shell::shell_container(IMAGE, "control-shell", NODE, &workspace, "env", false, true, owner)
        .unwrap();
    assert_eq!(shell.network, Network::None);
    assert!(shell.secrets.is_empty(), "{:?}", shell.secrets);
    assert_eq!(shell.mounts.len(), 1, "only the workspace: {:?}", shell.mounts);
}

#[test]
fn a_codex_api_key_is_only_a_read_only_mount() {
    let recorded = include_str!("../../../crates/ai/tests/fixtures/codex-handoff-completed.jsonl");
    let (before, after) = recorded.split_at(recorded.match_indices('\n').nth(2).unwrap().0 + 1);
    let after = after.replace("Handed off; waiting for the build watch.", REPLY);
    // Codex runs its own shell inside its sandbox, which denies its home.
    let finished = assert_key_unreadable(Backend::Codex, "/home/agent/.codex/auth.json", |spec| {
        cli(spec, before, &after, &[(".control-result", REPLY)])
    });
    let turn = finished.turn();
    assert!(turn.argv.iter().any(|w| w.contains("\"/home/agent/.codex\"=\"deny\"")));
}

#[test]
fn a_claude_api_key_is_read_only_by_claudes_own_helper() {
    let (before, after) = claude_stream();
    let finished = assert_key_unreadable(Backend::Claude, "/home/agent/.control-key/anthropic-api-key", |spec| {
        cli_with_shell(spec, &before, &after)
    });
    model_shell(&finished);
    let turn = finished
        .spawned()
        .into_iter()
        .find(|spec| spec.argv.first().is_some_and(|w| w == "claude"))
        .unwrap();
    let at = turn.argv.iter().position(|w| w == "--settings").unwrap();
    assert_eq!(
        turn.argv[at + 1],
        r#"{"apiKeyHelper":"cat /home/agent/.control-key/anthropic-api-key"}"#
    );
    assert!(
        turn.argv.windows(2).any(|w| w == ["--tools", ""]),
        "Claude's own tools are removed"
    );
}

#[test]
fn a_grok_api_key_is_exported_only_inside_grok() {
    let before = lines(&[
        json!({"type":"available_commands","tools":["search_tool","use_tool"],"commands":[]}),
        json!({"type":"usage","messageId":"resp_1","stopReason":"tool_use","usage":{"input_tokens":400,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"reasoning_tokens":0}}),
    ]);
    let after = lines(&[
        json!({"type":"text","data":REPLY}),
        json!({"type":"end","stopReason":"end_turn","sessionId":"s","requestId":"r","usage":{"input_tokens":812,"cache_read_input_tokens":41000,"cache_creation_input_tokens":100,"output_tokens":45,"reasoning_tokens":7,"total_tokens":41957},"num_turns":2}),
    ]);
    let finished = assert_key_unreadable(Backend::Grok, "/home/agent/.control-key/xai-api-key", |spec| {
        cli_with_shell(spec, &before, &after)
    });
    model_shell(&finished);
    let turn = finished
        .spawned()
        .into_iter()
        .find(|spec| spec.argv.first().is_some_and(|w| w == "control-grok"))
        .unwrap();
    let config = fs::read_to_string(host(turn, "/home/agent/.grok/config.toml")).unwrap();
    assert!(
        config.contains(r#"args = ["-u", "XAI_API_KEY", "/bin/control-room-mcp"]"#),
        "{config}"
    );
}

#[test]
fn an_antigravity_api_key_on_a_named_account_is_only_a_read_only_mount() {
    let before = lines(&[
        json!({"event":"init","conversation_id":"c","init":{"model":"gemini-4-neon-medium","cwd":"/workspace","tools":["ask_permission","call_mcp_tool","run_command"],"permission_mode":"always-proceed"}}),
    ]);
    let after = lines(&[
        json!({"event":"result","result":{"conversation_id":"c","status":"SUCCESS","response":REPLY,"duration_seconds":6.88,"num_turns":1,"usage":{"input_tokens":10418,"output_tokens":589,"thinking_tokens":551,"cache_read_tokens":8113,"total_tokens":11007}}}),
    ]);
    let finished = assert_key_unreadable(
        Backend::Antigravity,
        "/home/agent/.control-key/gemini-api-key",
        |spec| cli_with_shell(spec, &before, &after),
    );
    model_shell(&finished);
    let turn = finished
        .spawned()
        .into_iter()
        .find(|spec| spec.argv.first().is_some_and(|w| w == "control-agy"))
        .unwrap();
    let private = host(turn, "/home/agent/.control");
    let settings: Value = serde_json::from_slice(&fs::read(private.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["modelProvider"], "gemini");
}
