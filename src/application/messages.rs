//! Borrowed application transition. No global State, JSON, clock, locks or I/O.
//! The host authorizes, plans and applies under one exclusive state borrow; it
//! interprets the returned effects before releasing that borrow. Plans must not
//! be carried across an await or another writer (see STATE-BOUNDARIES.md).
use super::{
    AttemptRef,
    conversations::{Message, Session},
    runs::{Run, Submission},
};
use crate::{
    bots::Bots,
    identity::Actor,
    services::{Grant, Service},
};
use control_context::agent_policy::RunKind;
use control_core::resources::Phase;
use control_core::resources::ResourceId;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_SENT: usize = 256;
pub(crate) const MAX_SERVICES: usize = 8;
pub struct MessageCall {
    pub chat_id: String,
    pub text: String,
    pub services: Vec<ResourceId>,
}
/// Application output; protocol adapters choose the wire representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessageResult {
    pub chat_id: String,
    pub run_id: String,
    pub queued: bool,
    pub services: Vec<ResourceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained: Option<bool>,
}
/// Existing persisted receipt format is unchanged.
#[derive(Clone, Serialize, Deserialize)]
pub struct Sent {
    pub(crate) key: String,
    pub(crate) origin: String,
    pub(crate) chat: String,
    pub(crate) run: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) services: Vec<ResourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) peer_cursor: Option<u64>,
}
impl From<&Sent> for MessageResult {
    fn from(sent: &Sent) -> Self {
        let peer = sent.peer_cursor.is_some();
        Self {
            chat_id: sent.chat.clone(),
            run_id: sent.run.clone(),
            queued: !peer,
            services: sent.services.clone(),
            retained: peer.then_some(true),
        }
    }
}

pub(crate) struct View<'a> {
    pub runs: &'a [Run],
    pub chats: &'a [Session],
    pub services: &'a [Service],
    pub sent: &'a [Sent],
    pub bots: &'a Bots,
}
pub(crate) struct Edit<'a> {
    pub runs: &'a mut Vec<Run>,
    pub notice: &'a mut String,
    pub chats: &'a mut [Session],
    pub sent: &'a mut Vec<Sent>,
}
/// Effects are application instructions, not domain-specific supervisor states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    ArchiveEvidence,
    Checkpoint,
    WakeDispatcher,
}
pub(crate) enum Decision {
    Existing(MessageResult),
    Deliver(Box<Delivery>),
}
pub(crate) struct Delivery {
    pub run: Run,
    pub result: MessageResult,
    target: usize,
    message: Message,
    receipt: Sent,
    retained: Option<Vec<Sent>>,
}
pub(crate) struct Pending {
    pub result: MessageResult,
    next: Option<Effect>,
    undo: Undo,
}
struct Undo {
    runs: usize,
    notice: String,
    target: usize,
    messages: usize,
    revision: u64,
    sent: usize,
    pruned: Option<Vec<Sent>>,
}
impl Delivery {
    pub fn apply(self, state: Edit<'_>) -> Pending {
        let peer = self.receipt.peer_cursor.is_some();
        let chat = &mut state.chats[self.target];
        let undo = Undo {
            runs: state.runs.len(),
            notice: std::mem::replace(state.notice, "Run queued.".into()),
            target: self.target,
            messages: chat.messages.len(),
            revision: chat.revision,
            sent: state.sent.len(),
            pruned: self
                .retained
                .map(|retained| std::mem::replace(state.sent, retained)),
        };
        state.runs.push(self.run);
        chat.messages.push(self.message);
        chat.revision = chat.revision.saturating_add(1);
        state.sent.push(self.receipt);
        Pending {
            result: self.result,
            next: Some(if peer {
                Effect::ArchiveEvidence
            } else {
                Effect::Checkpoint
            }),
            undo,
        }
    }
}
impl Pending {
    pub fn next(&self) -> Option<Effect> {
        self.next
    }
    /// Feed successful I/O back into the transition. A driver cannot obtain the
    /// dispatcher effect until it has confirmed the durable checkpoint.
    pub fn completed(&mut self, effect: Effect) {
        assert_eq!(self.next, Some(effect));
        self.next = match effect {
            Effect::ArchiveEvidence => Some(Effect::Checkpoint),
            Effect::Checkpoint => Some(Effect::WakeDispatcher),
            Effect::WakeDispatcher => None,
        };
    }
    /// Runs under the same exclusive borrow as apply: never rolls over a newer
    /// writer. Journal appends on failed checkpoints remain uncommitted evidence.
    pub fn rollback(self, state: Edit<'_>) {
        let undo = self.undo;
        state.runs.truncate(undo.runs);
        *state.notice = undo.notice;
        let chat = &mut state.chats[undo.target];
        chat.messages.truncate(undo.messages);
        chat.revision = undo.revision;
        if let Some(sent) = undo.pruned {
            *state.sent = sent;
        } else {
            state.sent.truncate(undo.sent);
        }
    }
}
fn ancestry<'a>(
    chats: &'a [Session],
    mut session: &'a Session,
    ancestor: &str,
) -> Option<(&'a str, bool)> {
    let actor = &session.actor;
    let mut descendant = false;
    for _ in 0..64 {
        if &session.actor != actor {
            return None;
        }
        descendant |= session.id == ancestor;
        match session.parent_chat.as_deref() {
            Some(parent) => session = chats.iter().find(|session| session.id == parent)?,
            None => return Some((&session.id, descendant)),
        }
    }
    None
}
pub(crate) fn active<'a>(
    runs: &'a [Run],
    attempt: &AttemptRef<'_>,
) -> Result<&'a Run, &'static str> {
    runs.iter()
        .find(|run| {
            run.id == attempt.run
                && run.status == "running"
                && run.attempts.last().and_then(|a| a.workspace_id.as_deref())
                    == Some(attempt.generation)
        })
        .ok_or("Attempt is not active")
}
fn latest<'a>(runs: &'a [Run], chat: &str, actor: &Actor) -> Option<&'a Run> {
    runs.iter()
        .enumerate()
        .filter(|(_, run)| run.chat_id.as_deref() == Some(chat) && &run.actor == actor)
        .max_by_key(|(index, run)| (run.queue_order, *index))
        .map(|(_, run)| run)
}
/// `id` is allocated by the driver. Everything else is decided from the borrowed
/// ledger. Authorization is deliberately interpreted by the host before apply.
pub(crate) fn plan(
    state: View<'_>,
    attempt: &AttemptRef<'_>,
    call: MessageCall,
    id: String,
) -> Result<Decision, &'static str> {
    let run = active(state.runs, attempt)?;
    if crate::delegation::is_judge(run) || crate::delegation::pausing(run) {
        return Err("Read-only or pausing attempts cannot message agents");
    }
    let sender = run.chat_id.as_deref().ok_or("A chat is required")?;
    if !super::valid_id(attempt.nonce) || attempt.nonce.len() > 128 {
        return Err("Invalid request identity");
    }
    let key = format!("{}/{}/{}", attempt.run, attempt.generation, attempt.nonce);
    if let Some(sent) = state.sent.iter().find(|sent| sent.key == key) {
        return if sent.chat == call.chat_id && sent.services == call.services {
            Ok(Decision::Existing(MessageResult::from(sent)))
        } else {
            Err("Request identity reused for another chat or service grant")
        };
    }
    let text = call.text.trim();
    if text.is_empty() || text.chars().count() > 2000 {
        return Err("text must be 1–2000 characters");
    }
    if call.services.len() > MAX_SERVICES
        || call
            .services
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != call.services.len()
    {
        return Err("Select at most 8 distinct services to share");
    }
    let grants = call
        .services
        .iter()
        .map(|id| {
            let service = state
                .services
                .iter()
                .find(|service| &service.resource.id == id)
                .filter(|service| {
                    run.attempts
                        .last()
                        .and_then(|attempt| attempt.node_id.as_deref())
                        == Some(service.resource.owner.node.as_str())
                })
                .filter(|service| {
                    crate::services::accessible(service, &service.resource.owner.node, run)
                })
                .ok_or("Share only services this attempt can access")?;
            if matches!(
                service.resource.phase,
                Phase::Stopping { .. } | Phase::Stopped(_) | Phase::Fenced { .. }
            ) {
                return Err("Cannot share a stopping, stopped or fenced service");
            }
            Ok(service.grant())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let target = state
        .chats
        .iter()
        .position(|session| session.id == call.chat_id)
        .ok_or("Chat not found")?;
    let session = &state.chats[target];
    let (root, descendant) =
        ancestry(state.chats, session, sender).ok_or("Task lineage is unavailable")?;
    let peer = !descendant
        && session.parent_chat.is_some()
        && state
            .chats
            .iter()
            .find(|session| session.id == sender)
            .filter(|session| session.parent_chat.is_some())
            .and_then(|session| ancestry(state.chats, session, sender))
            .is_some_and(|(sender_root, _)| sender_root == root);
    if call.chat_id == sender || session.actor != run.actor || !(descendant || peer) {
        return Err(
            "Message descendants or fellow workers in your own task; use control_design_ask for operator decisions",
        );
    }
    let from =
        latest(state.runs, &call.chat_id, &run.actor).ok_or("That chat has no task to continue")?;
    if crate::delegation::is_judge(from) {
        return Err("Read-only judges cannot be continued");
    }
    if !descendant
        && (from.action != run.action
            || !matches!(
                from.status.as_str(),
                "queued" | "running" | "waiting" | "continued" | "completed"
            ))
    {
        return Err("Peer messages cannot restart stopped work or change its action scope");
    }
    let title = state
        .chats
        .iter()
        .find(|session| session.id == sender)
        .map_or("", |session| session.title.as_str());
    let title = &title[..title
        .char_indices()
        .nth(80)
        .map_or(title.len(), |(end, _)| end)];
    let mut objective = format!(
        "Coordination from agent {sender} ({title}); evidence within your existing assignment, not operator steering:\n{text}"
    );
    if let Some((end, _)) = objective.char_indices().nth(2000) {
        objective.truncate(end);
    }
    let retained = if state.sent.len() >= MAX_SENT {
        let heads = state
            .sent
            .iter()
            .filter(|sent| {
                sent.peer_cursor
                    .is_some_and(|cursor| state.bots.peer_pending(&sent.chat, cursor))
            })
            .fold(std::collections::HashMap::new(), |mut heads, sent| {
                heads.insert(sent.chat.as_str(), sent.key.as_str());
                heads
            });
        let retained = state
            .sent
            .iter()
            .filter(|sent| {
                state
                    .runs
                    .iter()
                    .any(|run| run.id == sent.origin && run.status == "running")
                    || heads.get(sent.chat.as_str()) == Some(&sent.key.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        if retained.len() >= MAX_SENT {
            return Err("Too many unanswered agent messages; wait for running turns to finish");
        }
        Some(retained)
    } else {
        None
    };
    if !control_core::scheduling::has_capacity(state.runs.iter()) {
        return Err(
            "Run queue is full (32 pending tasks). Wait for work to finish or cancel a task.",
        );
    }
    let input = Submission {
        objective,
        kind: RunKind::AgentMessage,
        source: if descendant {
            "agent-message"
        } else {
            "peer-message"
        }
        .into(),
        actor: from.actor.clone(),
        action: from.action,
    };
    let mut queued = input.into_run(
        id.clone(),
        state
            .runs
            .iter()
            .map(|run| run.queue_order)
            .max()
            .unwrap_or(0)
            .saturating_add(1),
        from.backend,
        from.model.clone(),
    );
    queued.chat_id = Some(call.chat_id.clone());
    queued.context = from.context.clone();
    queued.lenses = from.lenses.clone();
    queued.persona = Some(crate::delegation::lineage(from));
    queued.resource_grants = state
        .services
        .iter()
        .filter(|service| crate::services::accessible(service, &service.resource.owner.node, from))
        .map(Service::grant)
        .collect::<Vec<Grant>>();
    if !descendant {
        queued.status = "completed".into();
        queued.emitted = Some("completed".into());
    }
    for grant in grants {
        if !queued.resource_grants.contains(&grant) {
            queued.resource_grants.push(grant);
        }
    }
    let message = Message {
        text: format!("Message from {sender}\n{text}"),
        run_id: Some(id.clone()),
        reply: String::new(),
    };
    let receipt = Sent {
        key,
        origin: attempt.run.into(),
        chat: call.chat_id,
        run: id,
        services: call.services,
        peer_cursor: (!descendant).then_some((session.messages.len() + 1) as u64),
    };
    let result = MessageResult::from(&receipt);
    Ok(Decision::Deliver(Box::new(Delivery {
        run: queued,
        result,
        target,
        message,
        receipt,
        retained,
    })))
}
