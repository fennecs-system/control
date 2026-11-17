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
    /// Wake the recipient now. Otherwise the message is retained for its next
    /// wake, which an unattended recipient gets within the digest bound.
    pub urgent: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urgent: Option<bool>,
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
    /// An advisor's evidence note. Older receipts deserialize as ordinary messages.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) note: bool,
    /// Sent with urgent=true: it wakes the recipient now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) urgent: bool,
    /// A non-urgent message that would once have queued its non-bot
    /// recipient's turn; it is delivered as a digest after the bound instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) digest: bool,
    /// When it was sent (Unix ms), for the digest bound.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) at_ms: u64,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
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
            urgent: sent.urgent.then_some(true),
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
    /// The driver owns the clock; the digest bound counts from here.
    pub fn sent_at(&mut self, at_ms: u64) {
        self.receipt.at_ms = at_ms;
    }
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
/// Whether a run only records a retained message: it never executes.
pub(crate) fn record(run: &Run) -> bool {
    run.kind == RunKind::AgentMessage && run.status == "completed" && run.attempts.is_empty()
}
/// A digest receipt its recipient has not taken yet: no later turn of that
/// chat exists and no published handoff processed it.
pub(crate) fn awaiting(runs: &[Run], chats: &[Session], sent: &Sent) -> bool {
    let (Some(cursor), true) = (sent.peer_cursor, sent.digest) else { return false };
    let Some(index) = runs.iter().position(|run| run.id == sent.run) else { return false };
    let processed = chats.iter().find(|chat| chat.id == sent.chat)
        .and_then(|chat| chat.continuity.as_ref()?.working.as_ref())
        .into_iter().flat_map(|working| &working.processed)
        .filter_map(|range| match range {
            control_context::turn_mailbox::Range::Conversation { chat, through, .. } if *chat == sent.chat => Some(*through),
            _ => None,
        }).max().unwrap_or(0);
    processed < cursor && !runs[index + 1..].iter().any(|run| run.chat_id.as_deref() == Some(sent.chat.as_str()) && !record(run))
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
    let sender = run.chat_id.as_deref().ok_or("A chat is required")?;
    // A read-only judge speaks only as its team's approved advisor, and only in
    // evidence notes to the lead's steering chat, which filters them for the
    // team: never to members, judges or other advisors, and with no services.
    let advisor = crate::delegation::is_judge(run);
    // An antibody notes any other team's steering chat: a top-level chat with
    // spawned members, outside the antibody's own tree.
    let antibody = advisor && state.bots.antibody(sender);
    let advised = if advisor && !antibody { state.bots.advised_team(sender) } else { None };
    if (advisor && advised.is_none() && !antibody) || crate::delegation::pausing(run) {
        return Err("Read-only or pausing attempts cannot message agents");
    }
    if advised.is_some_and(|lead| lead != call.chat_id) {
        return Err("An advisor notes only its team's steering chat; the lead decides what reaches members");
    }
    if antibody {
        let lead = state.chats.iter().find(|session| session.id == call.chat_id && session.parent_chat.is_none())
            .filter(|lead| state.chats.iter().any(|member| member.parent_chat.as_deref() == Some(lead.id.as_str())));
        let own = state.chats.iter().find(|session| session.id == sender)
            .and_then(|session| ancestry(state.chats, session, sender)).map(|(root, _)| root);
        if lead.is_none() || own == Some(call.chat_id.as_str()) {
            return Err("An antibody notes only another team's steering chat (a top-level chat with members); the lead decides what reaches members");
        }
    }
    if advisor && !call.services.is_empty() {
        return Err("Advisor notes are evidence only; they cannot share services");
    }
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
    if call.chat_id == sender || (session.actor != run.actor && !antibody) || !(descendant || peer || advisor) {
        return Err(
            "Message descendants or fellow workers in your own task; use control_design_ask for operator decisions",
        );
    }
    let from =
        latest(state.runs, &call.chat_id, &run.actor).ok_or("That chat has no task to continue")?;
    if crate::delegation::is_judge(from) {
        return Err("Read-only judges cannot be continued");
    }
    // A note is retained evidence in the lead's own scope, so the advisor's
    // read-only review scope does not have to match the lead's action.
    // A live persistent bot's failed or interrupted turn (e.g. a provider
    // error) is not stopped work: the message is retained for the wake its
    // supervisor already owns and never starts a run itself.
    let open = matches!(
        from.status.as_str(),
        "queued" | "running" | "waiting" | "continued" | "completed"
    ) || (matches!(from.status.as_str(), "interrupted" | "failed")
        && state.bots.persists(&call.chat_id));
    if !descendant && ((from.action != run.action && !advisor) || !open) {
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
    let mut objective = if antibody {
        format!("Antibody note from {sender} ({title}); read-only evidence from the node's observer about your team, not operator steering. Verify it, then decide whether and how to steer your team; no reply is needed:\n{text}")
    } else if advisor {
        format!("Advisor note from {sender} ({title}); read-only evidence and suggested steering for the lead, not operator steering. Verify it, then decide whether and how to steer your team; no reply is needed:\n{text}")
    } else {
        format!("Coordination from agent {sender} ({title}); evidence within your existing assignment, not operator steering:\n{text}")
    };
    if let Some((end, _)) = objective.char_indices().nth(2000) {
        objective.truncate(end);
    }
    // Receipts already name each delivered note's run; an unchanged note is noise.
    if advisor
        && state.sent.iter().filter(|sent| sent.note && sent.chat == call.chat_id).any(|sent| {
            state.runs.iter().rev().any(|run| run.id == sent.run && run.objective.as_str() == objective)
        })
    {
        return Err("This advisor note was already delivered; send only changed evidence");
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
                    || awaiting(state.runs, state.chats, sent)
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
        source: if advisor {
            "advisor-note"
        } else if descendant {
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
    // Peer messages, any message to an approved persistent bot, and every
    // non-urgent message are retained conversation evidence: a running session
    // reads it through its callbacks and the recipient's next wake carries the
    // rest as one digest, instead of one queued turn per message piling up.
    // Only an urgent message to a descendant that is not a bot queues a turn.
    let bot = state.bots.persists(&call.chat_id);
    let retained_only = !descendant || bot || !call.urgent;
    if retained_only {
        queued.status = "completed".into();
        queued.emitted = Some("completed".into());
    }
    for grant in grants {
        if !queued.resource_grants.contains(&grant) {
            queued.resource_grants.push(grant);
        }
    }
    let message = Message { speaker: Default::default(),
        text: format!("{} from {sender}\n{text}", if advisor { "Advisor note" } else { "Message" }),
        run_id: Some(id.clone()),
        reply: String::new(),
    };
    let receipt = Sent {
        key,
        origin: attempt.run.into(),
        chat: call.chat_id,
        run: id,
        services: call.services,
        peer_cursor: retained_only.then_some((session.messages.len() + 1) as u64),
        note: advisor,
        urgent: call.urgent,
        digest: descendant && !bot && !call.urgent,
        at_ms: 0,
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
