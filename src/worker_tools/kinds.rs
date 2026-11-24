//! What a worker tool's handler may do with Control's state, as its type.
//! The kind decides how long the state lock is held, so a handler cannot
//! choose: a `Pure` handler never sees state, a `Read` handler sees it only
//! through `&State` and builds its reply from an owned copy after release,
//! a `Write` handler mutates under the lock and checkpoints through the
//! dispatcher's interpreter, and `External` and `Family` handlers work
//! (containers, network, waits) only after the lock is released.
use super::authority::Caller;
use crate::store::{Guard, Store};
use crate::{State, tool_registry::AttemptTools};
use serde_json::Value;
use std::{borrow::Cow, path::Path};

/// A refusal the model reads as a failed tool result.
pub(super) type Refusal = Cow<'static, str>;
/// The checkpoint interpreter a write persists through.
pub(super) type Save = fn(&State) -> std::io::Result<()>;
pub(super) type Shared = Store;

/// One `tools/call` request after the protocol and authority checks. It
/// borrows the request, never the state, so later steps may keep it.
pub(super) struct Call<'a> {
    /// The whole mailbox envelope, for brokers that read it themselves.
    pub envelope: &'a Value,
    pub name: &'a str,
    pub arguments: &'a Value,
    /// The delivery's idempotency nonce, when it has one.
    pub nonce: Option<&'a str>,
    /// The mailbox's workspace, already checked to be the attempt's.
    pub workspace: Option<&'a Path>,
}
impl<'a> Call<'a> {
    pub(super) fn of(envelope: &'a Value, workspace: Option<&'a Path>) -> Self {
        let params = &envelope["request"]["params"];
        Self {
            envelope,
            name: params["name"].as_str().unwrap_or(""),
            arguments: &params["arguments"],
            nonce: envelope["nonce"].as_str(),
            workspace,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Handler {
    /// Needs no state; runs after the lock is released.
    Pure(fn(&Call<'_>) -> Value),
    /// Copies a view in one critical section; the reply is built after release.
    Read(&'static dyn ReadTool),
    /// Changes state under the lock and persists through `Save`.
    Write(fn(&mut State, &Caller<'_>, &Call<'_>, Save) -> Value),
    /// Prepares an owned plan under the lock, then works without it.
    External(&'static dyn ExternalTool),
    /// A family behind the `AttemptTools` host interface, which takes the
    /// lock itself when it needs it.
    Family(&'static dyn AttemptTools),
}
impl Handler {
    /// Answers `call` with a tool result, releasing `state` as the kind
    /// requires. The guard is moved in so no caller can keep it.
    pub(super) fn answer(
        &self,
        mut state: Guard<'_>,
        shared: &Shared,
        caller: &Caller<'_>,
        call: &Call<'_>,
        save: Save,
    ) -> Value {
        match self {
            Handler::Pure(run) => {
                drop(state);
                run(call)
            }
            Handler::Read(tool) => tool.answer(state, caller, call),
            Handler::Write(apply) => apply(&mut state, caller, call, save),
            Handler::External(tool) => tool.answer(state, shared, caller, call),
            Handler::Family(tools) => {
                let generation = caller.generation(&state).unwrap_or_default().to_owned();
                drop(state);
                let host = crate::tool_registry::LocalHost(shared);
                let cx = crate::tool_registry::AttemptCall {
                    attempt: crate::tool_registry::AttemptRef {
                        run: caller.id(),
                        generation: &generation,
                        nonce: call.nonce.unwrap_or(""),
                    },
                    host: &host,
                };
                crate::toolbox::result(tools.call(&cx, call.name, call.arguments))
            }
        }
    }
}

mod sealed {
    pub trait Sealed {}
}
/// Implemented only by [`Read`]: the release between view and reply is its.
pub(super) trait ReadTool: sealed::Sealed + Sync {
    fn answer(&self, state: Guard<'_>, caller: &Caller<'_>, call: &Call<'_>) -> Value;
}
/// Implemented only by [`External`].
pub(super) trait ExternalTool: sealed::Sealed + Sync {
    fn answer(&self, state: Guard<'_>, shared: &Shared, caller: &Caller<'_>, call: &Call<'_>) -> Value;
}

/// `view` reads under the lock; `reply` gets only that `'static` copy, so
/// it cannot borrow state, and runs after release.
pub(super) struct Read<V: 'static> {
    pub view: fn(&State, &Caller<'_>, &Call<'_>) -> V,
    pub reply: fn(V) -> Value,
}
impl<V: 'static> sealed::Sealed for Read<V> {}
impl<V: 'static> ReadTool for Read<V> {
    fn answer(&self, state: Guard<'_>, caller: &Caller<'_>, call: &Call<'_>) -> Value {
        let view = (self.view)(&state, caller, call);
        drop(state);
        (self.reply)(view)
    }
}

/// `prepare` checks and parses under the lock into an owned plan; `run`
/// gets the plan and the shared handle after release. A refusal from
/// `prepare` ends the call.
pub(super) struct External<P: 'static> {
    pub prepare: fn(&State, &Caller<'_>, &Call<'_>) -> Result<P, Refusal>,
    pub run: fn(P, &Shared, &Call<'_>) -> Value,
}
impl<P: 'static> sealed::Sealed for External<P> {}
impl<P: 'static> ExternalTool for External<P> {
    fn answer(&self, state: Guard<'_>, shared: &Shared, caller: &Caller<'_>, call: &Call<'_>) -> Value {
        let plan = (self.prepare)(&state, caller, call);
        drop(state);
        match plan {
            Ok(plan) => (self.run)(plan, shared, call),
            Err(refusal) => control_mcp::failure(&refusal),
        }
    }
}
