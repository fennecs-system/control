//! The worker dispatch table: every tool name a worker may send, with the
//! fences it passes and its typed handler. Unknown names get the
//! not-allowed entry, after every fence.
use super::{
    authority::Caller,
    deferred,
    kinds::{Call, External, Handler, Read, Refusal, Save, Shared},
};
use crate::{State, identity};
use control_mcp::{data, failure, text};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::OnceLock};

/// The run-level checks a tool passes after the staged-handoff check every
/// call gets. They run in this order, and each variant adds the next one:
/// the run's standing, then the delegation pause, then this attempt's design
/// question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fence {
    /// Reads of the run's own captured context and callable schemas.
    Open,
    /// Standing only: the call that itself waits for delegated work.
    Standing,
    /// Standing and the delegation pause: spawning, and asking the design
    /// question itself.
    Delegation,
    /// Every check.
    Design,
}
/// Whether a managed session (one reading posts through its turn context)
/// may make the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Session {
    Any,
    Unmanaged,
}
pub(super) struct Entry {
    pub fence: Fence,
    pub session: Session,
    pub handler: Handler,
}
const fn entry(fence: Fence, handler: Handler) -> Entry {
    Entry {
        fence,
        session: Session::Any,
        handler,
    }
}

static NOT_ALLOWED: Entry = entry(
    Fence::Design,
    Handler::Pure(|_| {
        failure(
            "Tool is not allowed for this worker. Next: call control_tool_search {} to list the tools this session may call.",
        )
    }),
);
/// The entry for a tool name; an unknown name gets the not-allowed entry.
pub(super) fn lookup(name: &str) -> &'static Entry {
    static TABLE: OnceLock<HashMap<&'static str, Entry>> = OnceLock::new();
    TABLE.get_or_init(table).get(name).unwrap_or(&NOT_ALLOWED)
}
/// Where two sources name the same tool, the earlier wins: guidance is also
/// an attempt family, but answers before standing as a read of the run.
fn table() -> HashMap<&'static str, Entry> {
    let fixed = [
        ("control_guidance", entry(Fence::Open, Handler::Read(&GUIDANCE))),
        (deferred::SEARCH, entry(Fence::Open, Handler::Read(&SEARCH))),
        (
            "control_agent_spawn_status",
            entry(Fence::Delegation, Handler::Write(spawn_manage)),
        ),
        (
            "control_agent_spawn_cancel",
            entry(Fence::Delegation, Handler::Write(spawn_manage)),
        ),
        ("control_agent_spawn", entry(Fence::Delegation, Handler::Write(spawn))),
        (
            "control_agent_spawn_blocking",
            entry(Fence::Standing, Handler::Write(delegate)),
        ),
        (
            "control_design_ask",
            entry(Fence::Delegation, Handler::Write(design_ask)),
        ),
        (
            "control_github_open_pr",
            entry(Fence::Design, Handler::External(&OPEN_PR)),
        ),
        (
            "control_preview_start",
            entry(Fence::Design, Handler::External(&PREVIEW_START)),
        ),
        (
            "control_preview_stop",
            entry(Fence::Design, Handler::External(&PREVIEW_STOP)),
        ),
        (
            "control_project_request",
            entry(Fence::Design, Handler::Write(project_request)),
        ),
        (
            "control_agent_result",
            entry(Fence::Design, Handler::Read(&AGENT_RESULT)),
        ),
        (
            "control_workspace_run",
            entry(Fence::Design, Handler::External(&WORKSPACE_RUN)),
        ),
        ("control_web_fetch", entry(Fence::Design, Handler::External(&WEB_FETCH))),
        (
            "control_browser_capture",
            entry(Fence::Design, Handler::Pure(browser_capture)),
        ),
    ];
    let knowledge = [
        "control.knowledge.search",
        "control.knowledge.read",
        "control.knowledge.propose",
        "control_knowledge_search",
        "control_knowledge_read",
        "control_knowledge_propose",
    ]
    .map(|name| (name, entry(Fence::Design, Handler::Write(knowledge))));
    let families = crate::tool_registry::attempt_families().iter().flat_map(|&family| {
        family
            .names()
            .iter()
            .map(move |&name| (name, entry(Fence::Design, Handler::Family(family))))
    });
    // Toolbox tools answer to their registered (dotted) names and to their
    // worker spellings; generation also observes the attempt's cancellation.
    let toolbox = crate::tool_registry::entries()
        .iter()
        .filter(|e| e.route == crate::tool_registry::Route::Toolbox)
        .flat_map(|registered| {
            let dotted = registered.name();
            let handler = if dotted == crate::model_calls::tool::NAME {
                Handler::External(&MODEL_GENERATE)
            } else {
                Handler::Pure(toolbox)
            };
            std::iter::once(dotted)
                .chain(control_context::worker_name(dotted))
                .map(move |name| (name, entry(Fence::Design, handler)))
        });
    let rooms = [
        "control_room_list",
        "control_room_read",
        "control_room_post",
        "control_room_watch",
        "control_room_notifications",
        "control_room_ack",
        "control_room_claim",
        "control_room_plan",
    ]
    .map(|name| {
        let session = if matches!(name, "control_room_ack" | "control_room_notifications") {
            Session::Unmanaged
        } else {
            Session::Any
        };
        let rooms = Entry {
            session,
            ..entry(Fence::Design, Handler::Write(rooms))
        };
        (name, rooms)
    });
    fixed
        .into_iter()
        .chain(knowledge)
        .chain(families)
        .chain(toolbox)
        .chain(rooms)
        .fold(HashMap::new(), |mut table, (name, entry)| {
            table.entry(name).or_insert(entry);
            table
        })
}

/// Pure read of the run's captured context, for every role including judges.
static GUIDANCE: Read<std::io::Result<Value>> = Read {
    view: |state, caller, call| crate::guidance::answer(caller.run(state), call.arguments),
    reply: crate::toolbox::result,
};
/// Schemas of the tools this session may call; a pure read for every role.
static SEARCH: Read<std::io::Result<Value>> = Read {
    view: |_, caller, call| deferred::search(caller.facts(), call.arguments),
    reply: crate::toolbox::result,
};
/// The child's result is located under the lock and read from its files
/// after release.
static AGENT_RESULT: Read<Result<crate::delegated_results::Plan, &'static str>> = Read {
    view: |state, caller, call| crate::delegated_results::prepare(state, caller.id(), call.arguments.clone()),
    reply: |plan| match plan.and_then(crate::delegated_results::execute) {
        Ok(value) => text(&value.to_string()),
        Err(message) => failure(message),
    },
};

fn outcome(result: Result<Value, impl AsRef<str>>, ok: fn(&Value) -> Value) -> Value {
    match result {
        Ok(value) => ok(&value),
        Err(message) => failure(message.as_ref()),
    }
}
fn as_text(value: &Value) -> Value {
    text(&value.to_string())
}

/// Spawn status and cancellation wait for any pending design answer,
/// including one from an earlier attempt.
fn spawn_manage(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    if caller.run(state).question.as_ref().is_some_and(|q| q.pending()) {
        return failure("This attempt is pausing for a design answer.");
    }
    outcome(
        crate::spawning::manage(state, caller.id(), call.name, call.arguments, save),
        data,
    )
}

/// `control_agent_spawn` with the identities it is admitted under, checked
/// in this order.
struct Spawn<'a> {
    generation: String,
    nonce: &'a str,
    input: crate::spawning::Input,
}
impl<'a> Spawn<'a> {
    fn parse(state: &State, caller: &Caller<'_>, call: &Call<'a>) -> Result<Self, &'static str> {
        let generation = caller.generation(state).ok_or("Missing attempt identity")?.to_owned();
        let nonce = call.nonce.ok_or("Missing request identity")?;
        let input = serde_json::from_value(call.arguments.clone())
            .map_err(|_| "Expected a bounded list of agent objectives")?;
        Ok(Self {
            generation,
            nonce,
            input,
        })
    }
}
fn spawn(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    let spawn = match Spawn::parse(state, caller, call) {
        Ok(spawn) => spawn,
        Err(message) => return failure(message),
    };
    let chat = caller.chat(state).map(str::to_owned);
    match crate::spawning::request(state, caller.id(), &spawn.generation, spawn.nonce, spawn.input, save) {
        Ok(value) => data(&value),
        Err(message) => failure(&crate::delegation::refusal(state, chat.as_deref(), message)),
    }
}

/// `control_agent_spawn_blocking`.
struct Delegate<'a> {
    nonce: &'a str,
    input: crate::delegation::Input,
}
impl<'a> Delegate<'a> {
    fn parse(call: &Call<'a>) -> Result<Self, &'static str> {
        let nonce = call.nonce.ok_or("Missing request identity")?;
        let input =
            serde_json::from_value(call.arguments.clone()).map_err(|_| "Expected a delegation objective and role")?;
        Ok(Self { nonce, input })
    }
}
fn delegate(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    let delegate = match Delegate::parse(call) {
        Ok(delegate) => delegate,
        Err(message) => return failure(message),
    };
    let chat = caller.chat(state).map(str::to_owned);
    match crate::delegation::request_with_save(state, caller.id(), delegate.nonce, delegate.input, save) {
        Ok(value) => as_text(&value),
        Err(message) => failure(&crate::delegation::refusal(state, chat.as_deref(), message)),
    }
}

/// `control_design_ask`.
struct Ask<'a> {
    nonce: &'a str,
    input: crate::design::Input,
}
impl<'a> Ask<'a> {
    fn parse(call: &Call<'a>) -> Result<Self, &'static str> {
        let nonce = call.nonce.ok_or("Missing request identity")?;
        let input = crate::design::parse_input(call.arguments.clone())?;
        Ok(Self { nonce, input })
    }
}
fn design_ask(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    let result =
        Ask::parse(call).and_then(|ask| crate::design::ask_with_save(state, caller.id(), ask.nonce, ask.input, save));
    outcome(result, as_text)
}

/// `control_project_request`. Missing or mistyped fields read as empty, so
/// the project rules give the refusal.
struct Project<'a> {
    path: &'a str,
    name: Option<&'a str>,
}
impl<'a> Project<'a> {
    fn parse(call: &Call<'a>) -> Self {
        Self {
            path: call.arguments["path"].as_str().unwrap_or(""),
            name: call.arguments["name"].as_str(),
        }
    }
}
fn project_request(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    let project = Project::parse(call);
    let chat = caller.chat(state).unwrap_or_default().to_owned();
    let Some(index) = state.chats.iter().position(|c| c.id == chat) else {
        return failure("Chat not found");
    };
    let previous = state.chats[index].clone();
    let requester = crate::projects::Requester::Agent { run: Some(caller.id()) };
    match crate::projects::request_named(state, &chat, project.path, project.name, requester) {
        Ok(value) if save(state).is_ok() => as_text(&value),
        Ok(_) => {
            state.chats[index] = previous;
            failure("Could not save project request")
        }
        Err(message) => failure(message),
    }
}

fn knowledge(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    outcome(
        crate::knowledge_tools::worker(
            state,
            caller.id(),
            call.name,
            call.arguments,
            call.nonce.unwrap_or(""),
            save,
        ),
        data,
    )
}

/// Room writes, cursors and wake budgets share one checkpoint. The room
/// broker answers every `tools/call` with a result, never a protocol error.
fn rooms(state: &mut State, caller: &Caller<'_>, call: &Call<'_>, save: Save) -> Value {
    let mut response = crate::rooms::checkpoint(state, caller.id(), call.envelope, save);
    debug_assert!(response.get("error").is_none(), "{response}");
    response.get_mut("result").map(Value::take).unwrap_or_default()
}

/// Network requests and compilation run without the lock.
fn toolbox(call: &Call<'_>) -> Value {
    crate::toolbox::result(crate::toolbox::call(toolbox_name(call.name), call.arguments))
}
/// A worker spelling of a toolbox tool resolves to its registered name.
fn toolbox_name(name: &str) -> &str {
    control_context::dotted_name(name)
        .filter(|dotted| {
            crate::tool_registry::lookup(dotted).is_some_and(|e| e.route == crate::tool_registry::Route::Toolbox)
        })
        .unwrap_or(name)
}
fn browser_capture(call: &Call<'_>) -> Value {
    crate::web_access::capture_tool(call.arguments).unwrap_or_else(|error| crate::toolbox::result(Err(error)))
}

/// Inference runs on the host, outside the state lock; cancelling the
/// attempt closes the endpoint connection so the server stops generating.
static MODEL_GENERATE: External<String> = External {
    prepare: |_, caller, _| Ok(caller.id().to_owned()),
    run: |run, shared, call| {
        crate::toolbox::result(crate::model_calls::tool::call(call.arguments, &|| {
            crate::lifecycle::cancelled(Some((shared, &run)))
        }))
    },
};
static WEB_FETCH: External<String> = External {
    prepare: |_, caller, _| Ok(caller.id().to_owned()),
    run: |run, shared, call| crate::toolbox::result(crate::web_access::worker(shared, &run, call.arguments)),
};

/// `control_github_open_pr`: only an improvement run publishes.
struct Publication {
    chat: String,
    input: crate::github::Input,
}
static OPEN_PR: External<Publication> = External {
    prepare: |state, caller, call| {
        if caller.run(state).action != identity::Action::Improve {
            return Err("GitHub publication requires an improvement run".into());
        }
        let chat = caller.chat(state).unwrap_or_default().to_owned();
        let input = serde_json::from_value(call.arguments.clone())
            .map_err(|_| Refusal::from("Expected run_id, title and optional body"))?;
        Ok(Publication { chat, input })
    },
    run: |publication, shared, _| {
        outcome(
            crate::github::open(shared, &publication.chat, publication.input).map_err(|e| e.to_string()),
            as_text,
        )
    },
};

struct PreviewStart {
    run: String,
    input: crate::previews::Input,
}
static PREVIEW_START: External<PreviewStart> = External {
    prepare: |_, caller, call| {
        let input = serde_json::from_value(call.arguments.clone())
            .map_err(|_| Refusal::from("Expected a preview command and port"))?;
        Ok(PreviewStart {
            run: caller.id().to_owned(),
            input,
        })
    },
    run: |start, shared, _| {
        outcome(
            crate::previews::start(shared, &start.run, start.input).map_err(|e| e.to_string()),
            as_text,
        )
    },
};
/// The chat and this attempt's preview container.
struct PreviewStop {
    chat: String,
    preview: String,
}
static PREVIEW_STOP: External<PreviewStop> = External {
    prepare: |state, caller, _| {
        Ok(PreviewStop {
            chat: caller.chat(state).unwrap_or_default().to_owned(),
            preview: caller
                .generation(state)
                .map(|id| format!("control-{id}-preview"))
                .unwrap_or_default(),
        })
    },
    run: |stop, shared, _| {
        outcome(
            crate::previews::stop(shared, &stop.chat, &stop.preview)
                .map(|()| json!({"stopped":true}))
                .map_err(|e| e.to_string()),
            as_text,
        )
    },
};

/// An admitted `control_workspace_run`: everything the command needs after
/// the lock is released.
struct WorkspaceCommand {
    run: String,
    generation: String,
    image: String,
    node: String,
    workspace: std::path::PathBuf,
    container: String,
    review_only: bool,
    command: String,
    nonce: String,
}
static WORKSPACE_RUN: External<WorkspaceCommand> = External {
    prepare: |state, caller, call| {
        if !caller.facts().shell {
            return Err("Workspace tool is not enabled for this backend".into());
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            command: String,
        }
        let input = serde_json::from_value::<Input>(call.arguments.clone()).map_err(|_| "Expected a command")?;
        let attempt = caller.run(state).attempts.last().ok_or("No active attempt")?;
        if attempt.command_jobs.iter().any(|job| job.status.is_active()) {
            return Err(
                "An asynchronous workspace command owns this attempt; await or cancel it before running another command"
                    .into(),
            );
        }
        let (Some(image), Some(node), Some(workspace)) = (&attempt.image, &attempt.node_id, call.workspace) else {
            return Err("Attempt workspace is unavailable".into());
        };
        let expected = attempt
            .workspace_id
            .as_deref()
            .and_then(|id| crate::runtime::workspace(id).ok())
            .ok_or("No captured workspace identity")?;
        if expected.canonicalize().ok().as_deref() != Some(workspace) {
            return Err("Workspace does not match the active attempt".into());
        }
        let nonce = call
            .nonce
            .filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .ok_or("Invalid tool request nonce")?;
        Ok(WorkspaceCommand {
            run: caller.id().to_owned(),
            generation: attempt.workspace_id.clone().unwrap_or_default(),
            image: image.clone(),
            node: node.clone(),
            workspace: workspace.to_owned(),
            container: format!(
                "control-{}-tool",
                attempt.workspace_id.as_deref().unwrap_or(caller.id())
            ),
            review_only: attempt.review_only,
            command: input.command,
            nonce: nonce.to_owned(),
        })
    },
    run: workspace_run,
};
/// Persists intent outside both model-visible mounts before the command, so
/// a retry after an ambiguous acknowledgement never repeats the effect. The
/// fsync runs without the state lock (this mailbox is served serially), and
/// the run is re-fenced before anything executes.
fn workspace_run(command: WorkspaceCommand, shared: &Shared, _: &Call<'_>) -> Value {
    if crate::host_shell::claim(&command.workspace, &command.nonce).is_err() {
        return failure(
            "Workspace command already admitted or intent could not be saved; inspect the attempt before retrying",
        );
    }
    let activity = |output: String, status: &str| crate::usage::Activity {
        id: command.nonce.clone(),
        kind: "command_execution".into(),
        text: command.command.chars().take(2000).collect(),
        output,
        status: status.into(),
    };
    let Ok(mut state) = shared.lock() else {
        return failure("State unavailable; tools are disabled until Control is checked and restarted");
    };
    // Records only while the same attempt is still running (cancellation fences here).
    if !crate::worker::record_activity_for(
        &mut state,
        &command.run,
        &command.generation,
        activity(String::new(), "running"),
    ) {
        return failure("Run is not active");
    }
    drop(state);
    let result = crate::host_shell::shell_scoped(
        crate::engine::isolation(),
        &command.image,
        &command.container,
        &command.node,
        &command.workspace,
        &command.command,
        command.review_only,
    );
    if !command.review_only {
        crate::worker::observe_workspace_diff(
            shared,
            &command.run,
            &command.generation,
            &command.workspace,
            "observed",
        );
    }
    if let Ok(mut state) = shared.lock() {
        let output = result
            .as_ref()
            .ok()
            .map(|v| v.to_string())
            .unwrap_or_default()
            .chars()
            .take(8000)
            .collect();
        let status = if result.as_ref().is_ok_and(|v| v["exit_code"] == 0) {
            "completed"
        } else {
            "failed"
        };
        crate::worker::record_activity_for(&mut state, &command.run, &command.generation, activity(output, status));
    }
    crate::toolbox::result(result)
}
