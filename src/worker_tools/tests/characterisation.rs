//! Pins what `dispatch_raw` answers for every worker tool name under every
//! authority situation, byte for byte, with the state each call changed. The
//! table drives the same entry the worker mailbox and Control's own loop use,
//! so a restructuring of the dispatcher must leave the fixture unchanged:
//! refusal texts, which check answers first when several fail, and which
//! calls checkpoint. Tools whose valid call starts a container or reaches the
//! network are driven with valid arguments only where a fence refuses them
//! first. Regenerate the fixture only for an intended behaviour change.
use super::*;
use control_context::{
    turn_mailbox::{Disposition, Handoff as Staged, Mailbox},
    working_state::Payload,
};

const FIXTURE: &str = include_str!("../../../tests/fixtures/worker_dispatch.txt");
const GENERATION: &str = "generation-1";

/// One thing about the run a request arrives for. A scenario combines them,
/// so the table also pins which check answers first when several fail.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Condition {
    /// A backend with its own sandbox shell (no `control_workspace_run`).
    Codex,
    /// No run with this id.
    Missing,
    Inactive,
    /// The mailbox names a workspace other than the attempt's.
    StaleWorkspace,
    /// The mailbox names the attempt's own workspace.
    Workspace,
    /// The attempt records its image and node, as a started attempt does.
    Image,
    /// The run has no attempt yet (no attempt identity).
    NoAttempt,
    Judge,
    /// A continuity chat whose attempt has a turn mailbox.
    Managed,
    /// A handoff staged in that mailbox (implies `Managed`).
    Handoff,
    /// Paused for delegated work on this attempt.
    Pausing,
    /// A design question pending on this attempt.
    DesignPending,
    /// A design question pending from an earlier attempt.
    DesignEarlier,
}
use Condition::*;
/// Each starts from a running task on a backend that uses Control's shell.
const SCENARIOS: [(&str, &[Condition]); 20] = [
    ("Active", &[]),
    ("Codex", &[Codex]),
    ("Missing", &[Missing]),
    ("Inactive", &[Inactive]),
    ("StaleWorkspace", &[StaleWorkspace]),
    ("Workspace", &[Workspace]),
    ("WorkspaceCodex", &[Workspace, Codex]),
    ("WorkspaceImage", &[Workspace, Image]),
    ("NoAttempt", &[NoAttempt]),
    ("Judge", &[Judge]),
    ("Managed", &[Managed]),
    ("Handoff", &[Handoff]),
    ("Pausing", &[Pausing]),
    ("DesignPending", &[DesignPending]),
    ("DesignEarlier", &[DesignEarlier]),
    ("InactiveStale", &[Inactive, StaleWorkspace]),
    ("HandoffJudge", &[Handoff, Judge]),
    ("JudgePausing", &[Judge, Pausing]),
    ("PausingDesign", &[Pausing, DesignPending]),
    ("ManagedJudge", &[Managed, Judge]),
];

struct Fixture {
    shared: Store,
    run: String,
    chat: String,
    /// The chat's callsign: briefs name teams by it, and it hashes the id.
    callsign: String,
    workspace: Option<std::path::PathBuf>,
}
fn fixture(conditions: &[Condition]) -> Fixture {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Work on the fixture", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = RunStatus::Running;
    run.target.backend = crate::backend::Backend::Claude;
    let mut attempt = crate::usage::Attempt::new();
    attempt.workspace_id = Some(GENERATION.into());
    run.attempts.push(attempt);
    let id = run.id().to_string();
    let mut workspace = None;
    for condition in conditions {
        match condition {
            Missing => {}
            Codex => state.runtime.runs[0].target.backend = crate::backend::Backend::Codex,
            Inactive => state.runtime.runs[0].status = RunStatus::Queued,
            StaleWorkspace => workspace = Some(std::path::PathBuf::from("/nonexistent/control-stale-workspace")),
            NoAttempt => state.runtime.runs[0].attempts.clear(),
            Image => {
                let attempt = &mut state.runtime.runs[0].attempts[0];
                attempt.image = Some("control-worker:fixture".into());
                attempt.node_id = Some("node-fixture".into());
            }
            Workspace => {
                let path = crate::runtime::workspace(GENERATION).unwrap();
                std::fs::create_dir_all(&path).unwrap();
                workspace = Some(path.canonicalize().unwrap());
            }
            Judge => {
                state.runtime.runs[0].persona = Some(crate::delegation::Persona {
                    role: crate::delegation::Role::Judge,
                    root: "root".into(),
                    depth: 1,
                    parent: Some("parent".into()),
                    bundle: None,
                })
            }
            Managed | Handoff => {
                state.chats[0].continuity = Some(Box::new(crate::turn_context::Managed::default()));
                let mut mailbox = Mailbox::default();
                if *condition == Handoff {
                    mailbox
                        .stage(
                            "draft".into(),
                            Staged {
                                base_revision: 0,
                                payload: Payload {
                                    subgoal: "Keep the goal".into(),
                                    ..Default::default()
                                },
                                processed: vec![],
                                resynchronised: vec![],
                                disposition: Disposition::Continue,
                                progress: None,
                            },
                            0,
                        )
                        .unwrap();
                }
                state.runtime.runs[0].attempts[0].turn_mailbox = Some(Box::new(mailbox)).into();
            }
            Pausing => {
                let run = &mut state.runtime.runs[0];
                run.delegation = Some(crate::delegation::Delegation {
                    input: crate::delegation::Input {
                        model: None,
                        objective: "Inspect".into(),
                        role: crate::delegation::Role::Worker,
                    },
                    nonce: "delegate-fixture".into(),
                    generation: GENERATION.into(),
                    target: crate::providers::Target {
                        backend: run.target.backend,
                        model: Some("fixture".into()),
                        ..Default::default()
                    },
                    child: None,
                    continuation: None,
                });
            }
            DesignPending | DesignEarlier => {
                crate::design::tests::ask_without_io(&mut state, &id, "nonce-1", crate::design::tests::input())
                    .unwrap();
                if *condition == DesignEarlier {
                    state.runtime.runs[0].attempts[0].workspace_id = Some("generation-2".into());
                }
            }
        }
    }
    let callsign = crate::callsigns::name(&state.chats, &chat).unwrap().into_owned();
    Fixture {
        callsign,
        shared: crate::store::Store::new(state),
        run: if conditions.contains(&Missing) {
            "missing-run".into()
        } else {
            id
        },
        chat,
        workspace,
    }
}

/// One tool name with a representative valid call. `external`: its valid
/// call would start a container, reach the network or a model endpoint.
struct Tool {
    name: &'static str,
    valid: Value,
    external: bool,
}
fn tool(name: &'static str, valid: Value) -> Tool {
    Tool {
        name,
        valid,
        external: false,
    }
}
fn external(name: &'static str, valid: Value) -> Tool {
    Tool {
        name,
        valid,
        external: true,
    }
}
/// Every worker catalogue name (worker spellings, plus a dotted spelling of
/// each toolbox route), the loaders and an unknown name.
fn tools() -> Vec<Tool> {
    vec![
        external("control_rust_bench", json!({"code":"fn main() {}"})),
        external("control_solve", json!({"solver":"z3","model":"(check-sat)"})),
        external("control_model_generate", json!({"prompt":"Say hi"})),
        tool(
            "control_search_path",
            json!({"start":"a","goal":"b","edges":[{"from":"a","to":"b","cost":1}]}),
        ),
        external("control_web_search", json!({"query":"rust"})),
        external("control_rust_run", json!({"code":"fn main() {}"})),
        external("control.rust.run", json!({"code":"fn main() {}"})),
        tool(
            "control_design_ask",
            json!({"question":"Which format?","options":["Text","JSON"],"recommended":1}),
        ),
        tool("control_knowledge_search", json!({"query":"parser"})),
        tool("control.knowledge.search", json!({"query":"parser"})),
        tool("control_knowledge_read", json!({"id":"doc:missing"})),
        tool(
            "control_knowledge_propose",
            json!({"claim":"Parsers need tests","evidence":"Seen in review","scope":"project","validity":"until changed"}),
        ),
        external("control_web_fetch", json!({"url":"https://example.com/"})),
        external("control_browser_capture", json!({"html":"<p>hi</p>"})),
        tool(
            "control_project_request",
            json!({"path":"/tmp/control-fixture-project"}),
        ),
        external("control_preview_start", json!({"command":"true","port":8000})),
        external("control_preview_stop", json!({})),
        external(
            "control_github_open_pr",
            json!({"run_id":"r-other","title":"Must not publish"}),
        ),
        tool(
            "control_agent_spawn_blocking",
            json!({"objective":"Inspect the parser","role":"worker"}),
        ),
        tool("control_agent_spawn", json!({"objectives":["Inspect the parser"]})),
        tool("control_agent_spawn_status", json!({"batch":"batch-missing"})),
        tool("control_agent_spawn_cancel", json!({"batch":"batch-missing"})),
        external("control_github_read", json!({"pull_request":1})),
        external("control_github_write", json!({"kind":"comment","body":"x"})),
        tool("control_history_read", json!({"query":"parser"})),
        tool("control_recovery", json!({})),
        tool("control_turn_context", json!({})),
        tool(
            "control_turn_handoff",
            json!({"disposition":{"kind":"continue"},"payload":{"subgoal":"Keep going"}}),
        ),
        external("control_workspace_start", json!({"command":"true"})),
        tool("control_workspace_status", json!({"job_id":"job-missing"})),
        tool("control_workspace_cancel", json!({"job_id":"job-missing"})),
        external("control_service_start", json!({"command":"sleep 1"})),
        tool("control_service_attach", json!({"service_id":"svc-missing"})),
        tool("control_service_status", json!({})),
        tool("control_service_stop", json!({"service_id":"svc-missing"})),
        tool(
            "control_service_exec",
            json!({"service_id":"svc-missing","command":"true"}),
        ),
        tool(
            "control_service_exec_status",
            json!({"service_id":"svc-missing","execution_id":"exec-missing"}),
        ),
        tool(
            "control_service_copy",
            json!({"service_id":"svc-missing","from":"/workspace/a","to":"/workspace/b"}),
        ),
        tool(
            "control_port_request",
            json!({"service_id":"svc-missing","name":"web","direction":"ingress","service_port":8080}),
        ),
        tool("control_port_status", json!({})),
        tool("control_port_stop", json!({"port_id":"port-missing"})),
        tool("control_bot_pause", json!({})),
        tool("control_bot_request", json!({"chat_id":"chat-missing"})),
        tool("control_bot_status", json!({})),
        tool(
            "control_agent_message",
            json!({"chat_id":"chat-missing","text":"Hello"}),
        ),
        tool(
            "control_watch_start",
            json!({"service_id":"svc-missing","name":"log","command":"tail -f /tmp/x"}),
        ),
        tool("control_watch_events", json!({"watch_id":"watch-missing"})),
        tool("control_watch_status", json!({})),
        tool("control_watch_wake", json!({})),
        tool("control_watch_stop", json!({"watch_id":"watch-missing"})),
        tool("control_observe_set", json!({"clear":true})),
        tool("control_packages_search", json!({"query":"parser"})),
        tool("control_package_read", json!({"name":"missing-package"})),
        tool("control_guidance", json!({"topic":"session"})),
        tool("control_score_probe", json!({"clear":true})),
        tool("control_intel_brief", json!({})),
        tool("control_operator_notify", json!({"text":"Fixture notice"})),
        tool("control_agent_result", json!({"child":"child-missing"})),
        tool("control_room_list", json!({})),
        tool("control_room_read", json!({})),
        tool("control_room_post", json!({"message":"Fixture update"})),
        tool("control_room_claim", json!({"key":"parser","action":"acquire"})),
        tool("control_room_plan", json!({})),
        tool("control_room_watch", json!({"enabled":true})),
        tool("control_room_notifications", json!({})),
        tool("control_room_ack", json!({"through":0})),
        tool("control_workspace_run", json!({"command":"true"})),
        tool("control_tool_search", json!({"names":["room_post"]})),
        tool("control_call", json!({"tool":"control_guidance","arguments":{}})),
        tool("control_chat", json!({"action":"list"})),
        tool("control_nonexistent", json!({})),
    ]
}

/// Whether a valid call of `tool` under `conditions` would run something
/// outside the process: a workspace command needs the attempt's workspace,
/// image and node, on a backend that uses Control's shell.
fn outside(tool: &Tool, conditions: &[Condition]) -> bool {
    tool.external
        || (tool.name == "control_workspace_run"
            && conditions.contains(&Workspace)
            && conditions.contains(&Image)
            && !conditions.contains(&Codex))
}
/// Whether a valid call of `tool` under `conditions` is refused before it
/// could run anything outside the process.
fn fenced(tool: &Tool, conditions: &[Condition]) -> bool {
    conditions.iter().any(|condition| match condition {
        Missing | Inactive | StaleWorkspace | Pausing => true,
        DesignPending => tool.name != "control_design_ask",
        Handoff => !control_context::tool_policy::after_handoff(tool.name),
        Codex | Judge | Managed | DesignEarlier | Workspace | Image | NoAttempt => false,
    })
}

/// Paths (at most three levels deep) whose serialized value differs.
fn changed(before: &Value, after: &Value, path: &str, depth: usize, out: &mut Vec<String>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(a), Value::Object(b)) if depth < 3 => {
            let mut keys = a.keys().chain(b.keys()).collect::<Vec<_>>();
            keys.sort();
            keys.dedup();
            for key in keys {
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                changed(
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                    &next,
                    depth + 1,
                    out,
                );
            }
        }
        _ => out.push(path.to_owned()),
    }
}

/// One line: the response with the fixture's own ids replaced, then the
/// state paths the call changed.
fn case((scenario, conditions): (&str, &[Condition]), label: &str, request: Value, nonce: Option<&str>) -> String {
    let fixture = fixture(conditions);
    let envelope = match nonce {
        Some(nonce) => json!({"nonce":nonce,"request":request}),
        None => json!({"request":request}),
    };
    let before = serde_json::to_value(&*fixture.shared.guard().unwrap()).unwrap();
    // The clock reads the attempt's own start, so a time budget always
    // reads zero seconds spent, however long the table takes.
    let now = crate::design::UnixSeconds(
        fixture
            .shared
            .read(|state| {
                state
                    .runtime
                    .runs
                    .get(&fixture.run)
                    .and_then(|run| run.attempts.last())
                    .map(|a| a.started_unix)
            })
            .unwrap()
            .unwrap_or_default(),
    );
    let response = dispatch_raw_at(
        &fixture.shared,
        &fixture.run,
        &envelope,
        fixture.workspace.as_deref(),
        now,
    );
    let after = serde_json::to_value(&*fixture.shared.guard().unwrap()).unwrap();
    let mut paths = Vec::new();
    changed(&before, &after, "", 0, &mut paths);
    let response = if request["method"] == "tools/list" {
        json!({"tools":response["result"]["tools"].as_array().map(|tools| tools.iter().map(|t| t["name"].clone()).collect::<Vec<_>>())})
    } else {
        response
    };
    // A tool refusal (the common answer) is written as its text alone; the
    // check is exact, so any other shape prints in full.
    let refused = crate::toolbox::failure_text(&response)
        .filter(|text| response == control_mcp::tool_error(&request["id"], text))
        .map(|text| format!("refused {}", json!(text)));
    let text = abbreviated(scrub(&refused.unwrap_or_else(|| response.to_string()), &fixture));
    format!("{scenario} {label}: {text} changed={paths:?}")
}
/// Replaces what differs between runs of the table (the fixture's run and
/// chat ids, the node id, creation stamps and dates) so lines compare.
fn scrub(text: &str, fixture: &Fixture) -> String {
    let node = crate::config::node_id().expect("the table initializes the node");
    let state_directory = crate::config::state_directory().unwrap().display().to_string();
    let text = text
        .replace(&fixture.run, "<run>")
        .replace(&fixture.chat, "<chat>")
        .replace(&state_directory, "<state>")
        .replace(&node, "<node>")
        .replace(&fixture.callsign, "<callsign>");
    let text = hex(&dates(&text));
    // Spawned runs and chats carry fresh creation stamps; keep only their shape.
    let mut out = String::with_capacity(text.len());
    let mut digits = String::new();
    for c in text.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        if digits.len() >= 9 {
            out.push_str("<n>");
        } else {
            out.push_str(&digits);
        }
        digits.clear();
        if c != '\0' {
            out.push(c);
        }
    }
    out
}

fn call(name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

fn lines() -> Vec<String> {
    let invalid = json!({"unexpected_field":true});
    let mut lines = Vec::new();
    for scenario in SCENARIOS {
        for (label, request) in [
            ("ping", json!({"jsonrpc":"2.0","id":1,"method":"ping"})),
            (
                "initialize",
                json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            ),
            ("tools/list", json!({"jsonrpc":"2.0","id":3,"method":"tools/list"})),
            ("prompts/list", json!({"jsonrpc":"2.0","id":4,"method":"prompts/list"})),
            (
                "notification",
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            ),
        ] {
            lines.push(case(scenario, label, request, Some("1-1")));
        }
        for tool in tools() {
            if !outside(&tool, scenario.1) || fenced(&tool, scenario.1) {
                lines.push(case(
                    scenario,
                    &format!("{} valid", tool.name),
                    call(tool.name, &tool.valid),
                    Some("1-1"),
                ));
            }
            lines.push(case(
                scenario,
                &format!("{} invalid", tool.name),
                call(tool.name, &invalid),
                Some("1-1"),
            ));
            // Without a nonce, a workspace command is refused before it runs.
            if matches!(scenario.0, "Active" | "WorkspaceImage" | "NoAttempt") && !tool.external {
                lines.push(case(
                    scenario,
                    &format!("{} no-nonce", tool.name),
                    call(tool.name, &tool.valid),
                    None,
                ));
            }
        }
    }
    lines
}

#[test]
fn dispatch_answers_match_the_pinned_table() {
    // Saves and node-scoped tools need the (per-process test) state directory.
    crate::storage::initialize().unwrap();
    let actual = lines().join("\n") + "\n";
    if actual != FIXTURE {
        let path = std::env::temp_dir().join("control-worker-dispatch.txt");
        std::fs::write(&path, &actual).unwrap();
        let (line, (want, got)) = FIXTURE
            .lines()
            .chain(std::iter::repeat(""))
            .zip(actual.lines().chain(std::iter::repeat("")))
            .enumerate()
            .find(|(_, (want, got))| want != got)
            .unwrap();
        panic!(
            "Worker dispatch changed at line {}:\n  pinned: {want}\n  actual: {got}\nFull output: {}\n\
             If the change is intended: cp {} tests/fixtures/worker_dispatch.txt",
            line + 1,
            path.display(),
            path.display()
        );
    }
}
/// The UTC day a brief was written (today, or yesterday across midnight)
/// becomes `<date>`.
fn dates(text: &str) -> String {
    let now = crate::config::unix_seconds();
    [now, now.saturating_sub(86_400)]
        .map(crate::intel::date)
        .iter()
        .fold(text.to_owned(), |text, day| text.replace(day.as_str(), "<date>"))
}
/// Lowercase hex runs of twelve or more characters with a digit (creation
/// stamps, request and question ids) become `<hex>`.
fn hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 12 && run.bytes().any(|b| b.is_ascii_digit()) {
            out.push_str("<hex>");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_digit() || ('a'..='f').contains(&c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}
/// A long response keeps its start for readers and pins the rest by an
/// FNV-1a digest of the whole line, so the fixture stays small.
fn abbreviated(text: String) -> String {
    const SHOWN: usize = 240;
    if text.len() <= SHOWN + 40 {
        return text;
    }
    let digest = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    });
    let cut = (0..=SHOWN).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
    format!("{}… [{} bytes, fnv {digest:016x}]", &text[..cut], text.len())
}
