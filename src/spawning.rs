//! Nonblocking admission of a bounded batch. Source is a captured immutable
//! baseline; a running parent never lends its writable workspace to children.
use crate::runtime::RunKind;
use crate::{
    State, chat,
    delegation::{self, Persona, Role},
    identity, runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
mod completions;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    objectives: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub generation: String,
    pub nonce: String,
    input: Input,
    pub children: Vec<String>,
    #[serde(default)]
    seed: Option<Seed>,
    #[serde(default)]
    cancelled: bool,
    // Old saved batches retain their original polling behavior on upgrade.
    #[serde(default)]
    notify: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    completions: Vec<completions::Receipt>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seed {
    #[serde(default)]
    resource_grants: Vec<crate::services::Grant>,
    actor: identity::Actor,
    action: identity::Action,
    backend: crate::backend::Backend,
    model: Option<String>,
    context: Option<control_context::Resolved>,
    lenses: Option<Vec<String>>,
    lineage: Persona,
    baseline: Option<String>,
    project: Option<crate::projects::Project>,
    parent_chat: String,
    room: Option<String>,
    design: Option<String>,
}
impl Batch {
    /// A missing handoff must not turn a normal asynchronous dependency into
    /// repeated parent admissions. Explicit runnable handoffs remain separate.
    pub(crate) fn awaits_callback(&self,state:&State)->bool {
        !self.cancelled && self.notify && (self.pending()>0 || self.children.iter().any(|id|
            delegation::leaf(state,id).is_ok_and(|run|!matches!(run.status.as_str(),"completed"|"failed"|"interrupted"|"blocked"|"cancelled"))))
    }
    pub(crate) fn reserved(&self) -> usize {
        self.input.objectives.len()
    }
    pub(crate) fn pending(&self) -> usize {
        if self.cancelled {
            0
        } else {
            self.input
                .objectives
                .len()
                .saturating_sub(self.children.len())
        }
    }
    fn id(&self, parent: &str) -> String {
        format!("{parent}:{}:{}", self.generation, self.nonce)
    }
    fn summary(&self, state: &State) -> Value {
        self.page(state, 0, 10)
    }
    fn page(&self, state: &State, offset: usize, limit: usize) -> Value {
        json!({"status":if self.cancelled {"cancelled"} else if self.pending()>0 {"accepted"} else {"spawned"},"requested":self.reserved(),"pending":self.pending(),"materialized":self.children.len(),"source":"captured baseline; live edits excluded","children":self.children.iter().skip(offset).take(limit).filter_map(|id|state.runtime.runs.iter().find(|run|run.id==*id)).map(|run|json!({"run_id":run.id,"chat_id":run.chat_id,"status":run.status})).collect::<Vec<_>>(),"next_offset":(offset.saturating_add(limit)<self.children.len()).then_some(offset.saturating_add(limit))})
    }
}
pub fn definition() -> Value {
    json!({"name":"control_agent_spawn","description":"Start independent or background worker objectives without pausing this conversation. Give each worker ownership of its goal and dependencies, including ongoing infrastructure when needed. Workers coordinate directly with same-task peers; keep the root chat for steering and consequential decisions rather than relaying routine updates. Continue useful work in this session; spawning does not require ending it. Control queues completion updates automatically. Yield when genuinely waiting with no independent work remaining. Use control_agent_spawn_status only when a current status check is needed, never in a polling loop. Configured team budget (default 64), pending-intent budget and 24000-byte payload apply independently of model slots. Children inherit captured model, lenses, actor, linked room and immutable baseline, never live edits. Mentioning a room in an objective does not link it. First Improve turns without a published baseline must checkpoint via control_agent_spawn_blocking or finish first. Depth limit 2; children and blocking spawns share one root budget. Cancellation fences backlog and descendants; later originating-chat turns can inspect or cancel retained batches.","inputSchema":{"type":"object","properties":{"objectives":{"type":"array","description":"One concise task per child, 1–2000 characters each and 24000 UTF-8 bytes total. Example: [\"Inspect the API\",\"Review the tests\"]. Request identity comes from the transport; do not supply nonce or identity fields.","minItems":1,"items":{"type":"string","minLength":1,"maxLength":2000}}},"required":["objectives"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
pub fn management_definitions() -> Vec<Value> {
    vec![
        json!({"name":"control_agent_spawn_status","description":"Read a retained spawn batch in your originating chat, including after its parent finishes. At most 20 materialized handles per page; pending counts are independent of running slots.","inputSchema":{"type":"object","properties":{"batch":{"type":"string","maxLength":512},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["batch"],"additionalProperties":false},"annotations":{"readOnlyHint":true}}),
        json!({"name":"control_agent_spawn_cancel","description":"Cancel a retained spawn batch owned by your originating chat. Fences unmaterialized backlog and cancels active descendants; completed results remain retained.","inputSchema":{"type":"object","properties":{"batch":{"type":"string","maxLength":512}},"required":["batch"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false}}),
    ]
}

pub fn request(
    state: &mut State,
    id: &str,
    generation: &str,
    nonce: &str,
    input: Input,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    if input.objectives.is_empty()
        || input
            .objectives
            .iter()
            .map(String::len)
            .try_fold(0usize, |sum, len| sum.checked_add(len))
            .is_none_or(|bytes| bytes > 24000)
        || input
            .objectives
            .iter()
            .any(|s| s.trim().is_empty() || s.chars().count() > 2000)
        || !chat::valid_id(nonce)
        || nonce.len() > 128
    {
        return Err(
            "Expected objectives within the configured team budget, 1–2000 characters each, 24000 total bytes, and a valid request identity.",
        );
    }
    let index = state
        .runtime
        .runs
        .iter()
        .position(|r| r.id == id && r.status == "running")
        .ok_or("Run is not active.")?;
    let run = &state.runtime.runs[index];
    identity::authorize(&run.actor, run.action)?;
    let attempt = run
        .attempts
        .last()
        .filter(|a| a.workspace_id.as_deref() == Some(generation))
        .ok_or("Spawn request belongs to an inactive attempt.")?;
    if delegation::is_judge(run)
        || delegation::pausing(run)
        || run.question.as_ref().is_some_and(|q| q.answer.is_none())
    {
        return Err("Read-only or pausing attempts cannot spawn agents.");
    }
    if let Some(batch) = run
        .spawns
        .iter()
        .find(|batch| batch.generation == generation && batch.nonce == nonce)
    {
        return if batch.input == input {
            let mut value = batch.summary(state);
            value["batch"] = json!(batch.id(id));
            Ok(value)
        } else {
            Err("Spawn request identity already has different objectives.")
        };
    }
    delegation::budget_for(state, run, Role::Worker, input.objectives.len(), false)?;
    let pending = state
        .runtime
        .runs
        .iter()
        .flat_map(|run| run.spawns.iter())
        .map(Batch::pending)
        .sum::<usize>();
    if pending.saturating_add(input.objectives.len())
        > state.runtime.execution.max_pending_agent_intents
    {
        return Err(
            "Node pending-agent intent budget is full; wait for backlog or cancel a batch.",
        );
    }
    let parent = state
        .chats
        .iter()
        .find(|c| Some(&c.id) == run.chat_id.as_ref() && c.actor == run.actor)
        .ok_or("Conversation actor mismatch.")?;
    let project = if run.action == identity::Action::Task {
        parent.project.as_ref().and_then(|p| p.approved())
    } else {
        None
    };
    if project.as_ref().and_then(|p| p.grant.as_ref()) != attempt.project.as_ref()
        || project.as_ref().and_then(|p| p.head.as_ref()) != attempt.project_base.as_ref()
    {
        return Err("Project changed during this attempt; start a new turn before spawning.");
    }
    if run.action == identity::Action::Improve && attempt.parent_artifact.is_none() {
        return Err(
            "No immutable Improve baseline yet. Delegate after checkpointing, or finish this turn first.",
        );
    }
    // Clone only captured inputs needed by children, never attempts/history.
    let actor = run.actor.clone();
    let action = run.action;
    let backend = run.backend;
    let model = run.model.clone();
    let context = (*run.context).clone();
    let lenses = run.lenses.clone();
    let lineage = delegation::lineage(run);
    let baseline = attempt.parent_artifact.clone();
    let mut project = project;
    if let Some(project) = &mut project {
        project.head = attempt.project_parent.clone();
    }
    let parent_chat = parent.id.clone();
    let room = parent.room.clone();
    let design = parent.design_context.clone();
    let before = (state.chats.len(), state.runtime.runs.len());
    let previous_notice = state.runtime.notice.clone();
    let seed = Seed {
        resource_grants: crate::services::grants(state,run),
        actor,
        action,
        backend,
        model,
        context,
        lenses,
        lineage,
        baseline,
        project,
        parent_chat,
        room,
        design,
    };
    let result = (|| {
        let batch = Batch {
            generation: generation.into(),
            nonce: nonce.into(),
            input,
            children: Vec::new(),
            seed: Some(seed),
            cancelled: false,
            notify: true,
            completions: Vec::new(),
        };
        state.runtime.runs[index].spawns.push(batch);
        let batch_index = state.runtime.runs[index].spawns.len() - 1;
        if let Err(message) = materialize(state, index, batch_index) {
            state.runtime.runs[index].spawns.pop();
            return Err(message);
        }
        if save(state).is_err() {
            state.runtime.runs[index].spawns.pop();
            return Err("Could not persist spawned agents; no child was admitted.");
        }
        let batch = &state.runtime.runs[index].spawns[batch_index];
        let mut value = batch.summary(state);
        value["batch"] = json!(batch.id(id));
        Ok(value)
    })();
    if result.is_err() {
        state.chats.truncate(before.0);
        state.runtime.runs.truncate(before.1);
        state.runtime.notice = previous_notice;
    }
    result
}

fn materialize(state: &mut State, index: usize, batch_index: usize) -> Result<(), &'static str> {
    let parent_id = state.runtime.runs[index].id.clone();
    let batch = &state.runtime.runs[index].spawns[batch_index];
    if batch.cancelled || batch.pending() == 0 {
        return Ok(());
    }
    let seed = batch
        .seed
        .clone()
        .ok_or("Spawn batch has no frozen source seed.")?;
    let offset = batch.children.len();
    let capacity = control_core::scheduling::MAX_PENDING.saturating_sub(
        state
            .runtime
            .runs
            .iter()
            .filter(|run| crate::scheduler::phase(run).is_pending())
            .count(),
    );
    let objectives = batch
        .input
        .objectives
        .iter()
        .skip(offset)
        .take(capacity)
        .cloned()
        .collect::<Vec<_>>();
    for objective in objectives {
        crate::scheduler::admit(&state.runtime)?;
        let child_id = chat::create(state);
        let child = state.chats.last_mut().unwrap();
        child.actor = seed.actor.clone();
        child.parent_chat = Some(seed.parent_chat.clone());
        child.lenses = seed.lenses.clone();
        child.lens = seed
            .lenses
            .as_ref()
            .filter(|l| l.len() == 1)
            .map(|l| l[0].clone());
        child.room = seed.room.clone();
        child.design_context = seed.design.clone();
        child.project = seed.project.clone();
        child.title = objective.chars().take(60).collect();
        if seed.action == identity::Action::Improve {
            child.improve_mode = true;
            child.improve_workspace = seed.baseline.clone();
        } else if seed.project.is_none() {
            child.workspace = seed.baseline.clone();
        }
        state.runtime.enqueue_for(
            objective.clone(),
            RunKind::Spawned,
            "agent-spawn",
            seed.actor.clone(),
            seed.action,
        )?;
        let run = state.runtime.runs.last_mut().unwrap();
        run.chat_id = Some(child_id);
        run.resource_grants = seed.resource_grants.clone();
        run.backend = seed.backend;
        run.model = seed.model.clone();
        *run.context = seed.context.clone();
        run.lenses = seed.lenses.clone();
        run.persona = Some(Persona {
            role: Role::Worker,
            root: seed.lineage.root.clone(),
            depth: seed.lineage.depth + 1,
            parent: Some(parent_id.clone()),
        });
        let child_run = run.id.clone();
        state
            .chats
            .last_mut()
            .unwrap()
            .messages
            .push(chat::Message {
                text: objective,
                run_id: Some(child_run.clone()),
                reply: String::new(),
            });
        state.runtime.runs[index].spawns[batch_index]
            .children
            .push(child_run);
    }
    Ok(())
}

fn fenced(state: &State, index: usize, batch_index: usize, authorized: bool) -> bool {
    let parent = &state.runtime.runs[index];
    let batch = &parent.spawns[batch_index];
    let Some(seed) = &batch.seed else {
        return batch.pending() > 0;
    };
    let lineage = delegation::lineage(parent);
    if seed.actor != parent.actor
        || seed.action != parent.action
        || seed.lineage.root != lineage.root
        || seed.lineage.depth != lineage.depth
        || !authorized
        || !parent
            .attempts
            .iter()
            .any(|a| a.workspace_id.as_deref() == Some(&batch.generation))
    {
        return true;
    }
    let Some(chat) = state.chats.iter().find(|chat| {
        chat.id == seed.parent_chat
            && Some(&chat.id) == parent.chat_id.as_ref()
            && chat.actor == parent.actor
    }) else {
        return true;
    };
    if seed
        .project
        .as_ref()
        .and_then(|project| project.grant.as_ref())
        != chat
            .project
            .as_ref()
            .and_then(|project| project.grant.as_ref())
        && parent.action == identity::Action::Task
    {
        return true;
    }
    let mut current = parent;
    for _ in 0..=delegation::MAX_DEPTH + 1 {
        if matches!(
            current.status.as_str(),
            "cancelled" | "cancelling" | "blocked"
        ) {
            return true;
        }
        let Some(ancestor_id) = current.persona.as_ref().and_then(|p| p.parent.as_ref()) else {
            return false;
        };
        let Some(ancestor) = state.runtime.runs.iter().find(|run| {
            &run.id == ancestor_id && run.actor == parent.actor && run.action == parent.action
        }) else {
            return true;
        };
        if ancestor
            .spawns
            .iter()
            .any(|batch| batch.cancelled && batch.children.contains(&current.id))
        {
            return true;
        }
        current = ancestor;
    }
    true
}

/// Cancellation is a fail-closed supervisor decision, including after authority
/// revocation. It never grants tools or executes a model; worker teardown follows
/// the ordinary persisted cancelling state.
fn owned_ids(state: &State, index: usize, batch_index: usize) -> std::collections::HashSet<String> {
    let owner = &state.runtime.runs[index];
    let actor = owner.actor.clone();
    let action = owner.action;
    let mut selected = owner.spawns[batch_index]
        .children
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    loop {
        let before = selected.len();
        for run in &state.runtime.runs {
            if selected.contains(&run.id) && run.actor == actor && run.action == action {
                for next in [
                    run.delegation
                        .as_ref()
                        .and_then(|d| d.continuation.as_ref()),
                    run.question.as_ref().and_then(|q| q.continuation.as_ref()),
                ]
                .into_iter()
                .flatten()
                {
                    if state.runtime.runs.iter().any(|candidate| {
                        candidate.id == *next
                            && candidate.actor == actor
                            && candidate.action == action
                            && candidate.chat_id == run.chat_id
                    }) {
                        selected.insert(next.clone());
                    }
                }
            }
            if run.actor == actor
                && run.action == action
                && run
                    .persona
                    .as_ref()
                    .and_then(|p| p.parent.as_ref())
                    .is_some_and(|parent| selected.contains(parent))
            {
                selected.insert(run.id.clone());
            }
        }
        if selected.len() == before {
            break;
        }
    }
    selected
}
fn cancel_owned(
    state: &mut State,
    index: usize,
    batch_index: usize,
    selected: &std::collections::HashSet<String>,
) {
    let actor = state.runtime.runs[index].actor.clone();
    let action = state.runtime.runs[index].action;
    for run in &mut state.runtime.runs {
        if run.actor == actor && run.action == action && selected.contains(&run.id) {
            for batch in &mut run.spawns {
                batch.cancelled = true;
            }
            match run.status.as_str() {
                "running" | "cancelling" => run.status = "cancelling".into(),
                "queued" | "waiting" => run.status = "cancelled".into(),
                _ => continue,
            }
            run.result = "Spawn batch cancelled; pending workers will not start.".into();
        }
    }
    state.runtime.runs[index].spawns[batch_index].cancelled = true;
}

fn transact_batch(
    state: &mut State,
    index: usize,
    batch_index: usize,
    cancel: bool,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    let sizes = (state.runtime.runs.len(), state.chats.len());
    let children = state.runtime.runs[index].spawns[batch_index].children.len();
    let cancelled = state.runtime.runs[index].spawns[batch_index].cancelled;
    let notice = state.runtime.notice.clone();
    let ids = if cancel {
        owned_ids(state, index, batch_index)
    } else {
        std::collections::HashSet::new()
    };
    let flags = if cancel {
        state
            .runtime
            .runs
            .iter()
            .enumerate()
            .filter(|(_, run)| ids.contains(&run.id))
            .flat_map(|(run_index, run)| {
                run.spawns
                    .iter()
                    .enumerate()
                    .map(move |(batch_index, batch)| (run_index, batch_index, batch.cancelled))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let statuses = cancel.then(|| {
        runtime::RunStatusCheckpoint::capture(&state.runtime, |run| ids.contains(&run.id))
    });
    let result = if cancel {
        cancel_owned(state, index, batch_index, &ids);
        Ok(())
    } else {
        materialize(state, index, batch_index)
    };
    if result.is_err() || save(state).is_err() {
        state.runtime.runs.truncate(sizes.0);
        state.chats.truncate(sizes.1);
        state.runtime.runs[index].spawns[batch_index]
            .children
            .truncate(children);
        state.runtime.runs[index].spawns[batch_index].cancelled = cancelled;
        for (run_index, batch_index, cancelled) in flags {
            state.runtime.runs[run_index].spawns[batch_index].cancelled = cancelled;
        }
        if let Some(statuses) = statuses {
            statuses.restore(&mut state.runtime);
        }
        state.runtime.notice = notice;
        return result.and(Err(
            "Could not persist spawn backlog transition; no work dispatched.",
        ));
    }
    Ok(())
}

/// Called at the existing dispatch boundary; queue capacity and model slots stay
/// separate. One bounded batch transaction per pass, with no external effects.
pub fn reconcile(
    state: &mut State,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    let mut authorize = identity::authorizer();
    let mut capacity = None;
    let candidate = state
        .runtime
        .runs
        .iter()
        .enumerate()
        .find_map(|(index, run)| {
            run.spawns
                .iter()
                .enumerate()
                .find_map(|(batch_index, batch)| {
                    if batch.pending() == 0 {
                        return None;
                    }
                    let cancel = fenced(
                        state,
                        index,
                        batch_index,
                        authorize(&run.actor, run.action).is_ok(),
                    );
                    if cancel
                        || *capacity
                            .get_or_insert_with(|| crate::scheduler::admit(&state.runtime).is_ok())
                    {
                        Some((index, batch_index, cancel))
                    } else {
                        None
                    }
                })
        });
    if let Some((index, batch_index, cancel)) = candidate {
        transact_batch(state, index, batch_index, cancel, save)
    } else {
        completions::reconcile(state, save)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Management {
    batch: String,
    offset: Option<usize>,
    limit: Option<usize>,
}
pub fn manage(
    state: &mut State,
    caller: &str,
    name: &str,
    args: &Value,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    let input: Management = serde_json::from_value(args.clone())
        .map_err(|_| "Expected batch ID and optional status page offset/limit.")?;
    if input.batch.len() > 512
        || input.offset.unwrap_or(0) > 65536
        || !(1..=20).contains(&input.limit.unwrap_or(10))
    {
        return Err("Use a batch ID and status page limit of 1–20.");
    }
    let current = state
        .runtime
        .runs
        .iter()
        .find(|run| run.id == caller && run.status == "running")
        .ok_or("Run is not active.")?;
    identity::authorize(&current.actor, current.action)?;
    if delegation::is_judge(current) {
        return Err("Judges cannot manage spawn batches.");
    }
    let (index, batch_index) = state
        .runtime
        .runs
        .iter()
        .enumerate()
        .filter(|(_, run)| {
            run.actor == current.actor
                && run.action == current.action
                && run.chat_id.is_some()
                && run.chat_id == current.chat_id
        })
        .find_map(|(index, run)| {
            run.spawns
                .iter()
                .enumerate()
                .find(|(_, batch)| batch.id(&run.id) == input.batch)
                .map(|(batch_index, _)| (index, batch_index))
        })
        .ok_or("Spawn batch not found in this originating chat.")?;
    match name {
        "control_agent_spawn_cancel" => {
            if !state.runtime.runs[index].spawns[batch_index].cancelled {
                transact_batch(state, index, batch_index, true, save)?;
            }
        }
        "control_agent_spawn_status" => {}
        _ => return Err("Unknown spawn management tool."),
    }
    let batch = &state.runtime.runs[index].spawns[batch_index];
    let mut value = batch.page(state, input.offset.unwrap_or(0), input.limit.unwrap_or(10));
    value["batch"] = json!(input.batch);
    Ok(value)
}

/// Provenance edges are receipts, not a shared-root or parent_chat guess.
pub fn children<'a>(
    state: &'a State,
    parent: &'a runtime::Run,
) -> Result<Vec<&'a runtime::Run>, &'static str> {
    let lineage = delegation::lineage(parent);
    parent
        .spawns
        .iter()
        .flat_map(|batch| batch.children.iter().map(move |id| (batch, id)))
        .map(|(batch, id)| {
            if !parent
                .attempts
                .iter()
                .any(|a| a.workspace_id.as_deref() == Some(&batch.generation))
            {
                return Err("Spawn source generation mismatch.");
            }
            state
                .runtime
                .runs
                .iter()
                .find(|run| {
                    run.id == *id
                        && run.kind == RunKind::Spawned
                        && run.actor == parent.actor
                        && run.action == parent.action
                        && run.persona.as_ref().is_some_and(|p| {
                            p.parent.as_deref() == Some(&parent.id)
                                && p.root == lineage.root
                                && p.depth == lineage.depth + 1
                                && p.role == Role::Worker
                        })
                })
                .ok_or("Spawned child provenance mismatch.")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::Backend,
        identity::{Action, Actor},
        usage::Attempt,
    };
    pub(super) fn active() -> (State, String) {
        let mut s = State::default();
        let chat = chat::create(&mut s);
        s.runtime
            .enqueue_for(
                "Build a page".into(),
                RunKind::Chat,
                "web",
                Actor::LocalOperator,
                Action::Task,
            )
            .unwrap();
        let run = s.runtime.runs.last_mut().unwrap();
        run.chat_id = Some(chat);
        run.status = "running".into();
        run.backend = Backend::Codex;
        run.model = Some("captured-model".into());
        run.attempts.push(Attempt {
            workspace_id: Some("parent-attempt".into()),
            parent_artifact: Some("immutable-baseline".into()),
            ..Default::default()
        });
        let id = run.id.clone();
        (s, id)
    }
    pub(super) fn input(n: usize) -> Input {
        Input {
            objectives: (0..n).map(|n| format!("Inspect page {n}")).collect(),
        }
    }
    #[test]
    fn batch_is_nonblocking_isolated_deduplicated_and_available_after_restore() {
        let (mut s, id) = active();
        let first = request(&mut s, &id, "parent-attempt", "nonce", input(3), |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs[0].status, "running");
        assert!(!delegation::pausing(&s.runtime.runs[0]));
        assert_eq!(s.runtime.runs.len(), 4);
        assert_eq!(s.chats.len(), 4);
        for child in &s.runtime.runs[1..] {
            assert_eq!(child.status, "queued");
            assert_eq!(child.model.as_deref(), Some("captured-model"));
            assert_eq!(child.backend, Backend::Codex);
            assert_eq!(
                child.persona.as_ref().unwrap().parent.as_deref(),
                Some(id.as_str())
            );
            let chat = s
                .chats
                .iter()
                .find(|c| Some(&c.id) == child.chat_id.as_ref())
                .unwrap();
            assert_eq!(chat.workspace.as_deref(), Some("immutable-baseline"));
            assert_eq!(chat.parent_chat, s.runtime.runs[0].chat_id);
            let status = crate::delegated_results::execute(
                crate::delegated_results::prepare(
                    &s,
                    &id,
                    json!({"child":child.id,"section":"status"}),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(status["status"], "queued");
        }
        let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(
            first,
            request(
                &mut restored,
                &id,
                "parent-attempt",
                "nonce",
                input(3),
                |_| panic!("No second admission save")
            )
            .unwrap()
        );
        assert!(
            request(
                &mut restored,
                &id,
                "parent-attempt",
                "nonce",
                input(2),
                |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(restored.runtime.runs.len(), 4);
        // A later human turn in the same chat can inspect the durable receipt.
        restored.runtime.runs[0].status = "completed".into();
        let parent_chat = restored.runtime.runs[0].chat_id.clone();
        restored
            .runtime
            .enqueue_for(
                "Check the agents".into(),
                RunKind::Chat,
                "web",
                Actor::LocalOperator,
                Action::Task,
            )
            .unwrap();
        let next = restored.runtime.runs.last_mut().unwrap();
        next.chat_id = parent_chat;
        next.status = "running".into();
        let next_id = next.id.clone();
        assert!(
            crate::delegated_results::prepare(
                &restored,
                &next_id,
                json!({"child":restored.runtime.runs[1].id,"section":"status"})
            )
            .is_ok()
        );
        restored.runtime.runs[1].status = "running".into();
        assert!(
            crate::delegated_results::prepare(
                &restored,
                &restored.runtime.runs[1].id,
                json!({"child":restored.runtime.runs[2].id,"section":"status"})
            )
            .is_err()
        );
    }
    #[test]
    fn failed_save_rolls_back_and_full_queue_retains_backlog() {
        let (mut s, id) = active();
        let original = serde_json::to_vec(&s).unwrap();
        assert!(
            request(&mut s, &id, "parent-attempt", "nonce", input(3), |_| Err(
                std::io::Error::other("disk full")
            ))
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&s).unwrap(), original);
        for _ in 0..30 {
            s.runtime
                .enqueue_for(
                    "Pending".into(),
                    RunKind::Chat,
                    "test",
                    Actor::LocalOperator,
                    Action::Task,
                )
                .unwrap();
        }
        let accepted =
            request(&mut s, &id, "parent-attempt", "nonce", input(3), |_| Ok(())).unwrap();
        assert_eq!(accepted["requested"], 3);
        assert_eq!(accepted["materialized"], 1);
        assert_eq!(accepted["pending"], 2);
        reconcile(&mut s, |_| panic!("Full queue must not checkpoint a no-op")).unwrap();
    }
    #[test]
    fn child_budget_authority_and_cancellation_are_shared_with_delegation() {
        let (mut s, id) = active();
        s.runtime.execution.max_agents_per_root = 8;
        assert!(request(&mut s, &id, "stale", "nonce", input(1), |_| Ok(())).is_err());
        s.runtime.runs[0].actor = Actor::Unknown;
        assert!(request(&mut s, &id, "parent-attempt", "nonce", input(1), |_| Ok(())).is_err());
        s.runtime.runs[0].actor = Actor::LocalOperator;
        request(&mut s, &id, "parent-attempt", "nonce", input(8), |_| Ok(())).unwrap();
        assert!(delegation::budget(&s, &s.runtime.runs[0], Role::Worker).is_err());
        assert!(
            request(&mut s, &id, "parent-attempt", "another", input(1), |_| Ok(
                ()
            ))
            .is_err()
        );
        crate::lifecycle::request_cancel(&mut s.runtime, &id, &Actor::LocalOperator).unwrap();
        assert!(
            s.runtime.runs[1..]
                .iter()
                .all(|run| run.status == "cancelled")
        );
        let (mut s, id) = active();
        s.runtime.runs[0].action = Action::Improve;
        s.runtime.runs[0].attempts[0].parent_artifact = None;
        assert!(request(&mut s, &id, "parent-attempt", "nonce", input(1), |_| Ok(())).is_err());
    }
    #[test]
    fn fifty_agents_reserve_once_and_materialize_gradually_after_restart() {
        let (mut s, id) = active();
        let accepted = request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(accepted["requested"], 50);
        assert_eq!(accepted["materialized"], 31);
        assert_eq!(accepted["pending"], 19);
        assert_eq!(accepted["children"].as_array().unwrap().len(), 10);
        assert_eq!(accepted["next_offset"], 10);
        // Fifty total reservations, not fifty plus the materialized children.
        assert!(delegation::budget_for(&s, &s.runtime.runs[0], Role::Worker, 14, false).is_ok());
        assert!(delegation::budget_for(&s, &s.runtime.runs[1], Role::Worker, 15, false).is_err());
        let snapshot = serde_json::to_vec(&s).unwrap();
        assert_eq!(
            request(
                &mut s,
                &id,
                "parent-attempt",
                "fifty",
                input(50),
                |_| panic!("Retry must not write")
            )
            .unwrap(),
            accepted
        );
        let mut s: State = serde_json::from_slice(&snapshot).unwrap();
        for child in &mut s.runtime.runs[1..11] {
            child.status = "completed".into();
        }
        s.runtime.runs[0].model = Some("new live model must not leak".into());
        let before = serde_json::to_vec(&s).unwrap();
        assert!(reconcile(&mut s, |_| Err(std::io::Error::other("disk full"))).is_err());
        assert_eq!(serde_json::to_vec(&s).unwrap(), before);
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs.len(), 42);
        assert_eq!(s.runtime.runs[0].spawns[0].pending(), 9);
        for child in &s.runtime.runs[32..] {
            assert_eq!(child.model.as_deref(), Some("captured-model"));
            let chat = s
                .chats
                .iter()
                .find(|c| Some(&c.id) == child.chat_id.as_ref())
                .unwrap();
            assert_eq!(chat.workspace.as_deref(), Some("immutable-baseline"));
        }
        for run in &mut s.runtime.runs {
            run.status = "completed".into();
        }
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs.len(), 51);
        assert_eq!(s.runtime.runs[0].spawns[0].pending(), 0);
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs.len(), 52);
        assert_eq!(s.runtime.runs[51].kind, RunKind::AgentCompletion);
        reconcile(&mut s, |_| panic!("No completed backlog effects")).unwrap();
    }
    #[test]
    fn later_originating_turn_pages_and_cancels_backlog_atomically() {
        let (mut s, id) = active();
        let accepted = request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| Ok(()),
        )
        .unwrap();
        let batch = accepted["batch"].clone();
        s.runtime.runs[0].status = "completed".into();
        let originating = s.runtime.runs[0].chat_id.clone();
        s.runtime
            .enqueue_for(
                "Stop the batch".into(),
                RunKind::Chat,
                "test",
                identity::Actor::LocalOperator,
                identity::Action::Task,
            )
            .unwrap();
        let caller = s.runtime.runs.last_mut().unwrap();
        caller.status = "running".into();
        caller.chat_id = originating;
        let caller = caller.id.clone();
        let args = json!({"batch":batch,"offset":10,"limit":20});
        let page = manage(&mut s, &caller, "control_agent_spawn_status", &args, |_| {
            panic!("Read cannot save")
        })
        .unwrap();
        assert_eq!(page["children"].as_array().unwrap().len(), 20);
        assert_eq!(page["next_offset"], 30);
        let before = serde_json::to_vec(&s).unwrap();
        assert!(
            manage(
                &mut s,
                &caller,
                "control_agent_spawn_cancel",
                &json!({"batch":batch}),
                |_| Err(std::io::Error::other("full"))
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&s).unwrap(), before);
        manage(
            &mut s,
            &caller,
            "control_agent_spawn_cancel",
            &json!({"batch":batch}),
            |_| Ok(()),
        )
        .unwrap();
        assert!(s.runtime.runs[0].spawns[0].cancelled);
        assert!(
            s.runtime.runs[1..32]
                .iter()
                .all(|run| run.status == "cancelled")
        );
        reconcile(&mut s, |_| panic!("Cancelled backlog cannot spawn")).unwrap();
        assert_eq!(s.runtime.runs.len(), 33);
        s.runtime.runs.last_mut().unwrap().chat_id = Some("unrelated-chat".into());
        assert!(
            manage(
                &mut s,
                &caller,
                "control_agent_spawn_status",
                &args,
                |_| Ok(())
            )
            .is_err()
        );
    }
    #[test]
    fn cancellation_revocation_and_intent_limits_fence_unmaterialized_work() {
        let (mut s, id) = active();
        s.runtime.execution.max_pending_agent_intents = 40;
        assert!(
            request(
                &mut s,
                &id,
                "parent-attempt",
                "fifty",
                input(50),
                |_| panic!("Over budget cannot save")
            )
            .is_err()
        );
        s.runtime.execution.max_pending_agent_intents = 256;
        request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| Ok(()),
        )
        .unwrap();
        crate::lifecycle::request_cancel(&mut s.runtime, &id, &identity::Actor::LocalOperator)
            .unwrap();
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert!(s.runtime.runs[0].spawns[0].cancelled);
        assert_eq!(s.runtime.runs.len(), 32);
        let (mut s, id) = active();
        request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| Ok(()),
        )
        .unwrap();
        s.runtime.runs[0].actor = identity::Actor::Unknown;
        reconcile(&mut s, |_| Ok(())).unwrap();
        assert!(s.runtime.runs[0].spawns[0].cancelled);
        assert_eq!(s.runtime.runs.len(), 32);
    }
    #[test]
    fn node_backlog_budget_is_shared_across_independent_roots() {
        let (mut s, id) = active();
        request(
            &mut s,
            &id,
            "parent-attempt",
            "fifty",
            input(50),
            |_| Ok(()),
        )
        .unwrap();
        s.runtime.runs[0].status = "completed".into();
        s.runtime.execution.max_pending_agent_intents = 20;
        let next_chat = chat::create(&mut s);
        s.runtime
            .enqueue_for(
                "Another team".into(),
                RunKind::Chat,
                "test",
                identity::Actor::LocalOperator,
                identity::Action::Task,
            )
            .unwrap();
        let root = s.runtime.runs.last_mut().unwrap();
        root.status = "running".into();
        root.chat_id = Some(next_chat);
        root.attempts.push(crate::usage::Attempt {
            workspace_id: Some("other-attempt".into()),
            ..Default::default()
        });
        let root = root.id.clone();
        assert!(
            request(&mut s, &root, "other-attempt", "two", input(2), |_| panic!(
                "Node backlog must apply across roots"
            ))
            .is_err()
        );
        assert_eq!(s.runtime.runs[0].spawns[0].pending(), 19);
        assert!(s.runtime.runs.last().unwrap().spawns.is_empty());
    }
    #[test]
    fn batch_cancel_follows_retained_continuations_and_fences_their_backlog() {
        let (mut s, id) = active();
        let root = request(
            &mut s,
            &id,
            "parent-attempt",
            "root-batch",
            input(1),
            |_| Ok(()),
        )
        .unwrap();
        let mut next = s.runtime.runs[1].clone();
        next.id = "continued-child".into();
        next.kind = RunKind::DelegationResult;
        next.status = "running".into();
        next.attempts.push(crate::usage::Attempt {
            workspace_id: Some("continued-attempt".into()),
            parent_artifact: Some("child-baseline".into()),
            ..Default::default()
        });
        s.runtime.runs[1].status = "continued".into();
        s.runtime.runs[1].delegation = Some(delegation::Delegation {
            input: delegation::Input {
                objective: "Review".into(),
                role: Role::Judge,
            },
            nonce: "judge".into(),
            generation: "old-attempt".into(),
            backend: next.backend,
            model: "captured-model".into(),
            child: Some("completed-review".into()),
            continuation: Some(next.id.clone()),
        });
        s.runtime.runs.push(next);
        request(
            &mut s,
            "continued-child",
            "continued-attempt",
            "more",
            input(50),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(s.runtime.runs[2].spawns[0].pending(), 20);
        let before = serde_json::to_vec(&s).unwrap();
        let args = json!({"batch":root["batch"]});
        assert!(
            manage(&mut s, &id, "control_agent_spawn_cancel", &args, |_| Err(
                std::io::Error::other("full")
            ))
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&s).unwrap(), before);
        manage(&mut s, &id, "control_agent_spawn_cancel", &args, |_| Ok(())).unwrap();
        assert_eq!(s.runtime.runs[2].status, "cancelling");
        assert!(s.runtime.runs[2].spawns[0].cancelled);
        assert!(
            s.runtime.runs[3..]
                .iter()
                .all(|run| run.status == "cancelled")
        );
        reconcile(&mut s, |_| panic!("Descendant backlog must remain fenced")).unwrap();
    }
    #[test]
    fn configured_team_above_legacy_graph_limit_remains_inspectable() {
        let (mut s, id) = active();
        s.runtime.execution.max_agents_per_root = 1100;
        s.runtime.execution.max_pending_agent_intents = 1100;
        request(
            &mut s,
            &id,
            "parent-attempt",
            "large-team",
            input(1030),
            |_| Ok(()),
        )
        .unwrap();
        while s.runtime.runs[0].spawns[0].pending() > 0 {
            for child in &mut s.runtime.runs[1..] {
                child.status = "completed".into();
            }
            reconcile(&mut s, |_| Ok(())).unwrap();
        }
        assert_eq!(s.runtime.runs.len(), 1031);
        let last = s.runtime.runs.last().unwrap().id.clone();
        let plan =
            crate::delegated_results::prepare(&s, &id, json!({"child":last,"section":"status"}))
                .unwrap();
        assert_eq!(
            crate::delegated_results::execute(plan).unwrap()["status"],
            "queued"
        );
    }
    #[test]
    #[ignore = "manual optimized profile of the no-backlog reconciliation path"]
    fn profile_idle_backlog_reconciliation() {
        let (mut s, _) = active();
        s.runtime.runs[0].status = "completed".into();
        let template = s.runtime.runs[0].clone();
        for index in 0..20000 {
            let mut run = template.clone();
            run.id = format!("history-{index}");
            s.runtime.runs.push(run);
        }
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            reconcile(std::hint::black_box(&mut s), |_| {
                panic!("No backlog must not checkpoint")
            })
            .unwrap();
        }
        eprintln!(
            "idle spawn reconciliation over {} retained runs: {:?}/pass",
            s.runtime.runs.len(),
            start.elapsed() / 1000
        );
    }
}
