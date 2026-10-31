//! Nonblocking admission of a bounded batch. Source is a captured immutable
//! baseline; a running parent never lends its writable workspace to children.
use crate::{
    State, chat,
    delegation::{self, Persona, Role},
    identity, runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
}
impl Batch {
    fn summary(&self, state: &State) -> Value {
        json!({"status":"spawned","source":"captured baseline; live edits excluded","children":self.children.iter().filter_map(|id|state.runtime.runs.iter().find(|run|run.id==*id)).map(|run|json!({"run_id":run.id,"chat_id":run.chat_id,"status":run.status})).collect::<Vec<_>>()})
    }
}
pub fn definition() -> Value {
    json!({"name":"control_agent_spawn","description":"Spawn 1–8 independent worker tasks and continue working immediately. Returns child run/chat handles, not results; queued children share bounded node model slots. Each inherits your captured provider/model, lenses, actor and immutable attempt baseline, NOT your live edits. A first Improve turn without a published baseline must use control_agent_delegate instead. All children and delegated tasks share the existing depth-2 / 8-child root budget. Read their status and immutable output with control_agent_result; changes never apply automatically. Cancelling the parent cancels its active descendants.","inputSchema":{"type":"object","properties":{"objectives":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":2000}}},"required":["objectives"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
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
        || input.objectives.len() > delegation::MAX_CHILDREN
        || input
            .objectives
            .iter()
            .any(|s| s.trim().is_empty() || s.chars().count() > 2000)
        || !chat::valid_id(nonce)
        || nonce.len() > 128
    {
        return Err("Expected 1–8 objectives of 1–2000 characters and a valid request identity.");
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
            Ok(batch.summary(state))
        } else {
            Err("Spawn request identity already has different objectives.")
        };
    }
    delegation::budget(state, run, Role::Worker)?;
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
    let context = run.context.clone();
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
    let result = (|| {
        let mut children = Vec::with_capacity(input.objectives.len());
        for objective in &input.objectives {
            delegation::budget(state, &state.runtime.runs[index], Role::Worker)?;
            crate::scheduler::admit(&state.runtime)?;
            let child_id = chat::create(state);
            let child = state.chats.last_mut().unwrap();
            child.actor = actor.clone();
            child.parent_chat = Some(parent_chat.clone());
            child.lenses = lenses.clone();
            child.lens = lenses
                .as_ref()
                .filter(|l| l.len() == 1)
                .map(|l| l[0].clone());
            child.room = room.clone();
            child.design_context = design.clone();
            child.project = project.clone();
            child.title = objective.chars().take(60).collect();
            if action == identity::Action::Improve {
                child.improve_mode = true;
                child.improve_workspace = baseline.clone();
            } else if project.is_none() {
                child.workspace = baseline.clone();
            }
            state.runtime.enqueue_for(
                objective.clone(),
                "spawned",
                "agent-spawn",
                actor.clone(),
                action,
            )?;
            let run = state.runtime.runs.last_mut().unwrap();
            run.chat_id = Some(child_id);
            run.backend = backend;
            run.model = model.clone();
            run.context = context.clone();
            run.lenses = lenses.clone();
            run.persona = Some(Persona {
                role: Role::Worker,
                root: lineage.root.clone(),
                depth: lineage.depth + 1,
                parent: Some(id.to_owned()),
            });
            children.push(run.id.clone());
            state
                .chats
                .last_mut()
                .unwrap()
                .messages
                .push(chat::Message {
                    text: objective.clone(),
                    run_id: Some(run.id.clone()),
                    reply: String::new(),
                });
        }
        let batch = Batch {
            generation: generation.into(),
            nonce: nonce.into(),
            input,
            children,
        };
        let value = batch.summary(state);
        state.runtime.runs[index].spawns.push(batch);
        if save(state).is_err() {
            state.runtime.runs[index].spawns.pop();
            return Err("Could not persist spawned agents; no child was admitted.");
        }
        Ok(value)
    })();
    if result.is_err() {
        state.chats.truncate(before.0);
        state.runtime.runs.truncate(before.1);
        state.runtime.notice = previous_notice;
    }
    result
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
                        && run.kind == "spawned"
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
    fn active() -> (State, String) {
        let mut s = State::default();
        let chat = chat::create(&mut s);
        s.runtime
            .enqueue_for(
                "Build a page".into(),
                "chat",
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
    fn input(n: usize) -> Input {
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
                "chat",
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
    fn failed_save_and_partial_queue_admission_roll_back_all_children() {
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
                    "chat",
                    "test",
                    Actor::LocalOperator,
                    Action::Task,
                )
                .unwrap();
        }
        let original = serde_json::to_vec(&s).unwrap();
        assert!(
            request(
                &mut s,
                &id,
                "parent-attempt",
                "nonce",
                input(3),
                |_| panic!("Partial batch cannot be persisted")
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&s).unwrap(), original);
    }
    #[test]
    fn child_budget_authority_and_cancellation_are_shared_with_delegation() {
        let (mut s, id) = active();
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
}
