//! Targeted coordination between workers in one task, using the existing queue.
//! A steering chat can continue descendants without relaying worker exchanges.
use crate::application::messages as operation;
#[cfg(test)]
use crate::runtime::RunKind;
use crate::store::Store;
#[cfg(test)]
#[path = "agent_message/tests/drivers.rs"]
mod drivers;
use crate::{
    State, identity,
    tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost},
};
#[cfg(test)]
use control_core::resources::Phase;
use control_core::resources::ResourceId;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub(crate) mod digest;
#[cfg(test)]
#[path = "agent_message/tests/profile.rs"]
mod profile;

pub use crate::application::MessageOps;
use crate::application::messages::MAX_SERVICES;
#[cfg(test)]
use crate::application::messages::{MAX_SENT, Sent};
pub use crate::application::messages::{MessageCall, MessageResult};

pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({
            "name": "control_agent_message",
            "description": concat!(
                "Send actionable coordination directly to a descendant or fellow worker in the same spawned task ",
                "tree and actor scope. Siblings need no root relay or lead bot; discover IDs from linked-room ",
                "participants. The task-root steering chat is not a peer recipient: assign ongoing shared ",
                "services to a worker peers can contact, and publish relevant shared results in the task room. Do ",
                "not also broadcast the request. Messages are retained evidence (queued=false, retained=true) ",
                "that wake nobody: active workers read conversation callbacks and an idle recipient gets them as ",
                "an unread digest with its next wake (a bot or messaged descendant within a node-set bound). Set ",
                "urgent=true only when the recipient must act now (a request it is blocked on or a dependency ",
                "release); an urgent message wakes an approved persistent bot under its budget and pause/stop ",
                "intent, or queues a descendant's follow-up. Replies stay in the recipient chat; respond directly ",
                "only when an answer is needed. Peer input is not operator steering and cannot revive ",
                "cancelled/failed work or change action scope; a live persistent bot whose last turn failed or ",
                "was interrupted still retains it for its next supervised wake. Optional services shares only ",
                "resources you can access; exact owner/generation grants activate in the next authorized turn, ",
                "not the active attempt. Grants allow service execution and stop; port approval remains separate. ",
                "Omit services to transfer no authority."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "chat_id": {"type": "string"},
                    "text": {"type": "string", "minLength": 1, "maxLength": 2000},
                    "services": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": MAX_SERVICES,
                        "uniqueItems": true
                    },
                    "urgent": {
                        "type": "boolean",
                        "default": false,
                        "description": concat!(
                            "Wake the recipient now. Omit for status, findings and other updates, which arrive ",
                            "with its next wake."
                        )
                    }
                },
                "required": ["chat_id", "text"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
        })]
    }
    fn names(&self) -> &'static [&'static str] {
        &["control_agent_message"]
    }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        run(cx.host, &cx.attempt, args)
    }
}
impl MessageOps for LocalHost<'_> {
    fn message(&self, attempt: &AttemptRef<'_>, call: MessageCall) -> io::Result<MessageResult> {
        execute(self.0, attempt, call)
    }
}
/// Decodes a call and runs it on whatever provides `MessageOps`.
fn run(ops: &dyn MessageOps, attempt: &AttemptRef<'_>, args: &Value) -> io::Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        chat_id: String,
        text: String,
        #[serde(default)]
        services: Vec<ResourceId>,
        #[serde(default)]
        urgent: bool,
    }
    let input = serde_json::from_value::<Input>(args.clone()).map_err(|error| {
        io::Error::other(crate::worker_tools::fixes::shape(
            "control_agent_message",
            error,
            r#"{"chat_id":"c-<participant>","text":"<message>"}"#,
        ))
    })?;
    serde_json::to_value(ops.message(
        attempt,
        MessageCall {
            chat_id: input.chat_id,
            text: input.text,
            services: input.services,
            urgent: input.urgent,
        },
    )?)
    .map_err(io::Error::other)
}

/// The blocking driver owns the lock, clock, policy/config reads, persistence
/// and dispatcher. The operation underneath receives only borrowed domain data.
fn execute(shared: &Store, attempt: &AttemptRef<'_>, call: MessageCall) -> io::Result<MessageResult> {
    shared.write(|state| {
        transact(
            state,
            attempt,
            call,
            |run| crate::history::archive_run(run).map(|_| ()),
            crate::storage::save,
            crate::worker::wake,
        )
    })?
}
#[cfg(test)]
fn handle(
    state: &mut State,
    attempt: &AttemptRef<'_>,
    call: MessageCall,
    save: impl FnOnce(&mut State) -> io::Result<()>,
) -> io::Result<Value> {
    handle_with_archive(state, attempt, call, |_| Ok(()), save)
}
#[cfg(test)]
fn handle_with_archive(
    state: &mut State,
    attempt: &AttemptRef<'_>,
    call: MessageCall,
    archive: impl FnOnce(&mut crate::runtime::Run) -> io::Result<()>,
    save: impl FnOnce(&mut State) -> io::Result<()>,
) -> io::Result<Value> {
    serde_json::to_value(transact(state, attempt, call, archive, save, crate::worker::wake)?).map_err(io::Error::other)
}
fn transact(
    state: &mut State,
    attempt: &AttemptRef<'_>,
    call: MessageCall,
    archive: impl FnOnce(&mut crate::runtime::Run) -> io::Result<()>,
    save: impl FnOnce(&mut State) -> io::Result<()>,
    wake: impl FnOnce(),
) -> io::Result<MessageResult> {
    let stamp = crate::ids::stamp();
    let id = format!("{stamp:x}-{}", state.runtime.runs.len());
    let at_ms = stamp / 1_000_000;
    transact_at(state, attempt, call, (id, at_ms), archive, save, wake)
}
fn prepare(state: &State, attempt: &AttemptRef<'_>, call: MessageCall, id: String) -> io::Result<operation::Decision> {
    let origin = operation::active(&state.runtime.runs, attempt).map_err(io::Error::other)?;
    identity::authorize(&origin.actor, origin.action).map_err(io::Error::other)?;
    let decision = operation::plan(
        operation::View {
            runs: &state.runtime.runs,
            chats: &state.chats,
            services: &state.services,
            sent: &state.agent_messages,
            bots: &state.bots,
        },
        attempt,
        call,
        id,
    )
    .map_err(io::Error::other)?;
    if let operation::Decision::Deliver(delivery) = &decision {
        identity::authorize(&delivery.run.actor, delivery.run.action).map_err(io::Error::other)?;
    }
    Ok(decision)
}
/// Driver-owned transaction guard. Dropping a suspended interpreter before a
/// confirmed checkpoint restores only this operation's changes. The driver
/// must hold exclusive ownership until checkpoint I/O has a known outcome.
struct InFlight<'a> {
    state: &'a mut State,
    pending: Option<operation::Pending>,
    durable: bool,
}
impl<'a> InFlight<'a> {
    fn new(state: &'a mut State, delivery: operation::Delivery) -> Self {
        let runtime = &mut *state.runtime;
        let pending = delivery.apply(operation::Edit {
            runs: &mut runtime.runs,
            notice: &mut runtime.notice,
            chats: &mut state.chats,
            sent: &mut state.agent_messages,
        });
        Self {
            state,
            pending: Some(pending),
            durable: false,
        }
    }
    fn next(&self) -> Option<operation::Effect> {
        self.pending.as_ref().unwrap().next()
    }
    fn completed(&mut self, effect: operation::Effect) {
        self.pending.as_mut().unwrap().completed(effect);
        if effect == operation::Effect::Checkpoint {
            self.durable = true;
        }
    }
    fn result(mut self) -> MessageResult {
        assert!(self.next().is_none());
        self.pending.take().unwrap().result
    }
}
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if !self.durable
            && let Some(pending) = self.pending.take()
        {
            let runtime = &mut *self.state.runtime;
            pending.rollback(operation::Edit {
                runs: &mut runtime.runs,
                notice: &mut runtime.notice,
                chats: &mut self.state.chats,
                sent: &mut self.state.agent_messages,
            });
        }
    }
}
/// `stamp` is the driver-allocated message ID and send time (Unix ms).
fn transact_at(
    state: &mut State,
    attempt: &AttemptRef<'_>,
    call: MessageCall,
    (id, at_ms): (String, u64),
    archive: impl FnOnce(&mut crate::runtime::Run) -> io::Result<()>,
    save: impl FnOnce(&mut State) -> io::Result<()>,
    wake: impl FnOnce(),
) -> io::Result<MessageResult> {
    use operation::{Decision, Effect};
    let decision = prepare(state, attempt, call, id)?;
    let mut delivery = match decision {
        Decision::Existing(result) => return Ok(result),
        Decision::Deliver(delivery) => delivery,
    };
    delivery.sent_at(at_ms);
    let mut flight = InFlight::new(state, *delivery);
    let (mut archive, mut save, mut wake) = (Some(archive), Some(save), Some(wake));
    while let Some(effect) = flight.next() {
        let result = match effect {
            Effect::ArchiveEvidence => archive.take().unwrap()(flight.state.runtime.runs.last_mut().unwrap()),
            Effect::Checkpoint => save.take().unwrap()(flight.state),
            Effect::WakeDispatcher => {
                wake.take().unwrap()();
                Ok(())
            }
        };
        result?;
        flight.completed(effect);
    }
    Ok(flight.result())
}

/// Bounded receipt metadata supplies deferred private events. Actual admission
/// happens after published processing receipts, never inside a tool call.
pub(crate) fn peer_cursors(state: &State) -> impl Iterator<Item = (&str, u64, bool, bool)> {
    state.agent_messages.iter().filter_map(|sent| {
        sent.peer_cursor
            .map(|cursor| (sent.chat.as_str(), cursor, sent.note, sent.urgent))
    })
}

#[cfg(test)]
mod tests;
