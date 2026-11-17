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
    #[serde(default,skip_serializing_if="Option::is_none")]
    model:Option<String>,
    /// A judge batch starts one read-only reviewer, for example a team advisor.
    #[serde(default,skip_serializing_if="Option::is_none")]
    role:Option<Role>,
    /// A persona from the library: its purpose, DNA, standing, body and
    /// behaviours, captured for every child. Read-only personas are judges.
    #[serde(default,skip_serializing_if="Option::is_none")]
    persona:Option<String>,
    /// Explicit genes, replacing the persona's DNA (or the inherited lenses).
    #[serde(default,skip_serializing_if="Option::is_none")]
    lenses:Option<Vec<String>>,
}
impl Input {
    fn role(&self) -> Role {
        self.role.unwrap_or(Role::Worker)
    }
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
    /// The persona every child of this batch captures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    persona: Option<Box<control_context::persona::Captured>>,
}
impl Batch {
    /// A missing handoff must not turn a normal asynchronous dependency into
    /// repeated parent admissions. Explicit runnable handoffs remain separate.
    pub(crate) fn awaits_callback(&self,state:&State)->bool {
        !self.cancelled && self.notify && (self.pending()>0 || self.children.iter().any(|id|
            delegation::leaf(state,id).is_ok_and(|run|!crate::application::runs::status::finished(&run.status))))
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
}
/// Spawned judges are the root task's advisors: at most the configured
/// `max_advisors_per_root`, counting started and pending ones, separately from
/// its one blocking judge review. Like other root reservations, a started
/// advisor keeps its slot after it finishes.
fn advisor_allowance(state: &State, run: &crate::runtime::Run) -> Result<(), &'static str> {
    // Strictly opt-in: without the advisor lens on the root chat there is no
    // advisor option to use.
    if !run.chat_id.as_deref().is_some_and(|chat| crate::bots::advisor_offered_for(state, chat) || crate::bots::antibody_offered_for(state, chat)) {
        return Err(crate::bots::ADVISOR_OFF);
    }
    let root = delegation::lineage(run).root;
    let started = state.runtime.runs.iter()
        .filter(|candidate| candidate.kind == RunKind::Spawned && delegation::is_judge(candidate) && delegation::lineage(candidate).root == root)
        .count();
    let pending = state.runtime.runs.iter()
        .filter(|candidate| delegation::lineage(candidate).root == root)
        .flat_map(|candidate| &candidate.spawns)
        .filter(|batch| batch.input.role() == Role::Judge)
        .map(Batch::pending)
        .sum::<usize>();
    if started.saturating_add(pending) >= state.runtime.execution.max_advisors_per_root {
        return Err("This root task's advisor allowance (max_advisors_per_root) is used.");
    }
    Ok(())
}
impl Batch {
    fn summary(&self, state: &State) -> Value {
        self.page(state, 0, 10)
    }
    fn page(&self, state: &State, offset: usize, limit: usize) -> Value {
        json!({"status":if self.cancelled {"cancelled"} else if self.pending()>0 {"accepted"} else {"spawned"},"requested":self.reserved(),"pending":self.pending(),"materialized":self.children.len(),"source":"captured baseline; live edits excluded","children":self.children.iter().skip(offset).take(limit).filter_map(|id|state.runtime.runs.iter().find(|run|run.id==*id)).map(|run|json!({"run_id":run.id,"chat_id":run.chat_id,"status":run.status})).collect::<Vec<_>>(),"next_offset":(offset.saturating_add(limit)<self.children.len()).then_some(offset.saturating_add(limit))})
    }
}
pub fn definition() -> Value {
    let mut definition=inherited_definition();
    definition["inputSchema"]["properties"]["model"]=crate::providers::selection::schema();
    definition
}
fn inherited_definition()->Value {
    json!({"name":"control_agent_spawn","description":"Start independent or background worker objectives without pausing this conversation. Give each worker ownership of its goal and dependencies, including ongoing infrastructure when needed. Workers coordinate directly with same-task peers; keep the root chat for steering and consequential decisions rather than relaying routine updates. Continue useful work in this session; spawning does not require ending it. Control queues completion updates automatically. Yield when genuinely waiting with no independent work remaining. Use control_agent_spawn_status only when a current status check is needed, never in a polling loop. Configured team budget (default 64), pending-intent budget and 24000-byte payload apply independently of model slots. Children inherit captured model, lenses, actor and immutable baseline, never live edits, and share this chat's team room (its linked room, else its lead's), so objectives need not name a room; agents cannot create rooms. First Improve turns without a published baseline must checkpoint via control_agent_spawn_blocking or finish first. Depth limit 2; children and blocking spawns share one root budget. Cancellation fences backlog and descendants; later originating-chat turns can inspect or cancel retained batches.","inputSchema":{"type":"object","properties":{"objectives":{"type":"array","description":"One concise task per child, 1–2000 characters each and 24000 UTF-8 bytes total. Example: [\"Inspect the API\",\"Review the tests\"]. Request identity comes from the transport; do not supply nonce or identity fields.","minItems":1,"items":{"type":"string","minLength":1,"maxLength":2000}},"role":{"type":"string","enum":["worker","judge"],"description":"Default worker. judge starts exactly one isolated read-only advisor candidate with no resources, within the root task's advisor allowance (default 1), separate from its one blocking judge review. Available only when the root chat selects the advisor lens; request it as a bot to make it the team's advisor."},"persona":{"type":"string","maxLength":64,"description":"A persona from the library (control.personas.list; built-ins lead, worker, advisor, antibody, judge): children capture its purpose, DNA, standing, default model and behaviours. Read-only personas are judges and need their opt-in gene. Omit to inherit your context as before."},"lenses":{"type":"array","maxItems":32,"items":{"type":"string"},"description":"Explicit genes replacing the persona's DNA or your inherited lenses."}},"required":["objectives"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
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
    mut input: Input,
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
    // A persona is captured once per batch; its standing decides the role.
    let bundle = match input.persona.as_deref() {
        Some(persona) => Some(crate::personas::capture(state, persona).map_err(|_| "Unknown persona, or its SOUL.md cannot be captured; control.personas.list names them.")?),
        None => None,
    };
    if let Some(bundle) = &bundle {
        let role = if bundle.standing.acts.read_only() { Role::Judge } else { Role::Worker };
        if input.role.is_some_and(|given| given != role) {
            return Err("The persona's standing decides the role: read-only personas are judges, others workers.");
        }
        input.role = Some(role);
    }
    if input.lenses.as_ref().is_some_and(|lenses| lenses.len() > 32) {
        return Err("Select at most 32 genes.");
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
    if input.role() == Role::Judge {
        if input.objectives.len() != 1 {
            return Err("A judge batch starts exactly one read-only advisor candidate.");
        }
        advisor_allowance(state, run)?;
        // Observer personas keep their own opt-in gene gate.
        if let (Some(bundle), Some(chat)) = (bundle.as_ref(), run.chat_id.as_deref()) {
            use control_context::persona::{Sees, Steers};
            if bundle.standing.sees == Sees::Node && !crate::bots::antibody_offered_for(state, chat) {
                return Err(crate::bots::ANTIBODY_OFF);
            }
            if bundle.standing.steers == Steers::Lead && !crate::bots::advisor_offered_for(state, chat) {
                return Err(crate::bots::ADVISOR_OFF);
            }
        }
    }
    // A spawned judge is a team advisor candidate: it uses the root's advisor
    // allowance, not the one blocking judge review.
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
    // Judges get the task-level read-only review scope and no resources, as
    // with blocking delegation.
    let judge = input.role() == Role::Judge;
    let action = if judge { identity::Action::Task } else { run.action };
    let selected=crate::providers::selection::resolve(&state.runtime,run.backend,&run.model.clone().unwrap_or_else(||run.backend.default_model()),input.model.as_deref().or(bundle.as_ref().map(|b| b.model.as_str()).filter(|m| !m.is_empty())))?;
    let backend = selected.backend;
    let model = Some(selected.model);
    // Explicit genes, else the persona's DNA, else the parent's captured context.
    let (context, lenses) = match input.lenses.clone().or_else(|| bundle.as_ref().map(|b| b.lenses.clone())) {
        Some(genes) => (Some(crate::lens_library::resolve(state, &genes).map_err(|_| "Selected genes cannot be resolved; check the persona's DNA or the lenses given.")?), Some(genes)),
        None => ((*run.context).clone(), run.lenses.clone()),
    };
    let lineage = delegation::lineage(run);
    let baseline = attempt.parent_artifact.clone();
    let mut project = project;
    if let Some(project) = &mut project {
        project.head = attempt.project_parent.clone();
    }
    let parent_chat = parent.id.clone();
    // Children join the team room: this chat's link, else its lead's.
    let room = crate::rooms::team_room(state, &parent.id);
    let design = parent.design_context.clone();
    let before = (state.chats.len(), state.runtime.runs.len());
    let previous_notice = state.runtime.notice.clone();
    // A persona's wake behaviours file bot requests; a refused batch files none.
    let previous_bots = bundle.as_ref().filter(|b| !b.behaviours.is_empty()).map(|_| state.bots.clone());
    let seed = Seed {
        resource_grants: if judge { Vec::new() } else { crate::services::grants(state,run) },
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
        persona: bundle.map(Box::new),
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
        if let Some(bots) = previous_bots {
            state.bots = bots;
        }
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
    let role = batch.input.role();
    // Backlog admitted after the lead linked a room still joins it.
    let room = seed.room.clone().or_else(|| crate::rooms::team_room(state, &seed.parent_chat));
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
        child.model=seed.model.as_ref().map(|model|crate::providers::Model::captured(seed.backend,model.clone()));
        child.lenses = seed.lenses.clone();
        child.lens = seed
            .lenses
            .as_ref()
            .filter(|l| l.len() == 1)
            .map(|l| l[0].clone());
        child.room = room.clone();
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
        run.chat_id = Some(child_id.clone());
        run.resource_grants = seed.resource_grants.clone();
        run.backend = seed.backend;
        run.model = seed.model.clone();
        *run.context = seed.context.clone();
        run.lenses = seed.lenses.clone();
        run.persona = Some(Persona {
            role,
            root: seed.lineage.root.clone(),
            depth: seed.lineage.depth + 1,
            parent: Some(parent_id.clone()),
            bundle: seed.persona.clone(),
        });
        let child_run = run.id.clone();
        // Wake behaviours become an ordinary pending bot request.
        if let Some(bundle) = seed.persona.as_deref() {
            crate::bots::persona_request(state, &seed.parent_chat, &parent_id, &child_id, &bundle.behaviours);
        }
        state
            .chats
            .last_mut()
            .unwrap()
            .messages
            .push(chat::Message { speaker: Default::default(),
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
mod tests;
