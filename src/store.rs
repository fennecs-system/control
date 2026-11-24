//! The one handle to the application state. Every lock of `State` goes through
//! here, so what sits behind the handle (today one mutex) can change in this
//! module alone.
//!
//! `read` and `write` run a closure inside the critical section. The closure's
//! result cannot borrow the state or hold the guard: its type is chosen before
//! the borrow exists. `guard` holds the lock across statements a closure cannot
//! yet wrap (dispatch moving it into a handler, a release mid-branch or inside
//! a loop); each use is a candidate for a closure later.
//!
//! `commit` is the driver for changes that release the lock before waiting
//! for the disk: lock, apply, stage, unlock, wait, then release the
//! operation's effects with the writer's proof of durability. Others can see
//! such a change before it is durable, so the store keeps the newest one's
//! mark and `read_seen`/`write_seen` hand it to replies, which wait on it
//! (`Seen`) before they are written.
//!
//! A panic while the lock is held poisons it. Every method then returns
//! `Unavailable` and the caller keeps its own answer (503, an error, a skipped
//! tick); `Store` never clears poison.
use crate::State;
use crate::application::{
    effects::{Effects, Released},
    runs::Tx,
};
use control_journal::writer::{Mark, Unsaved};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// A cheap-to-clone handle to the shared application state.
#[derive(Clone)]
pub(crate) struct Store(Arc<Shared>);

struct Shared {
    state: Mutex<State>,
    /// The newest change `commit` let others see before it was durable. Set
    /// and read only while `state` is held, so a reader's view and its mark
    /// come from one critical section.
    seen: Mutex<Option<Mark>>,
}

/// The state lock was poisoned by a panic while it was held.
#[derive(Debug)]
pub(crate) struct Unavailable;

impl From<Unavailable> for std::io::Error {
    fn from(_: Unavailable) -> Self {
        std::io::Error::other("State unavailable")
    }
}

/// Exclusive access held across statements; see the module notes.
pub(crate) struct Guard<'a>(MutexGuard<'a, State>);

impl Deref for Guard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        &self.0
    }
}

impl DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.0
    }
}

/// Permission to stage a save that others may see before it is durable;
/// only `commit` has one, so no such save skips the reply gate.
pub(crate) struct Staging(());

/// What a reply showed of changes that were not durable yet. A reply waits
/// on it before it is written; when nothing was in flight it holds nothing
/// and cost an uncontended lock and one atomic load.
#[must_use = "a reply that showed a change waits until it is durable"]
pub(crate) struct Seen(Option<Mark>);
impl Seen {
    /// Whether nothing was in flight.
    #[cfg(test)]
    pub(crate) fn is_clear(&self) -> bool {
        self.0.is_none()
    }
    /// Until every change the reply may show is durable, or storage is
    /// unavailable (those changes stay queued and are written on recovery).
    pub(crate) fn wait(self) {
        if let Some(mark) = self.0 {
            mark.wait();
        }
    }
}

/// An operation's change, applied under the lock: its answer, what to do
/// once durable, and how to take the change back if storage refuses it while
/// the lock is still held (nobody has seen it then).
pub(crate) struct Applied<T, U: FnOnce(&mut State)> {
    pub(crate) answer: T,
    pub(crate) effects: Effects,
    pub(crate) undo: U,
}

/// How a `commit` ended, by what the caller may tell its client.
#[must_use = "the caller answers its client by how the commit ended"]
pub(crate) enum Commit<T, R> {
    /// Durable; the effects were released.
    Saved(T),
    /// Applied and visible to others, but storage failed after it was
    /// staged: it stays queued and is saved when storage recovers (or never,
    /// when the writer halted and Control is stopping). Effects were forgone.
    NotYet(T, Unsaved),
    /// The operation refused; it changed nothing. A reply built under the
    /// lock may show others' changes: it waits on `Seen` first.
    Refused(R, Seen),
    /// Storage refused before anyone could see the change, so it was never
    /// applied or was undone: "not saved".
    NotSaved(std::io::Error),
}

impl Store {
    pub(crate) fn new(state: State) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(state),
            seen: Mutex::new(None),
        }))
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, Unavailable> {
        self.0.state.lock().map_err(|_| Unavailable)
    }

    /// The newest released change, if it is not durable yet. Called with
    /// the state lock held.
    fn pending(&self) -> Seen {
        let seen = self.0.seen.lock().unwrap_or_else(PoisonError::into_inner);
        Seen(seen.as_ref().filter(|mark| !mark.durable()).cloned())
    }

    /// Run `view` with shared access and return what it copied out.
    pub(crate) fn read<T>(&self, view: impl FnOnce(&State) -> T) -> Result<T, Unavailable> {
        self.lock().map(|state| view(&state))
    }

    /// `read`, with what a reply built from the copy waits for.
    pub(crate) fn read_seen<T>(&self, view: impl FnOnce(&State) -> T) -> Result<(T, Seen), Unavailable> {
        self.lock().map(|state| (view(&state), self.pending()))
    }

    /// Run `change` with exclusive access and return its result.
    pub(crate) fn write<T>(&self, change: impl FnOnce(&mut State) -> T) -> Result<T, Unavailable> {
        self.lock().map(|mut state| change(&mut state))
    }

    /// `write`, with what a reply built inside it waits for: a save made
    /// under the lock is durable already, a change `commit` released may not be.
    pub(crate) fn write_seen<T>(&self, change: impl FnOnce(&mut State) -> T) -> Result<(T, Seen), Unavailable> {
        self.lock().map(|mut state| (change(&mut state), self.pending()))
    }

    /// Lock; refuse at once while storage cannot take saves; read the clock
    /// for `Tx`; apply `operation`; stage the save; unlock; wait for the
    /// disk; release the effects with the writer's proof. A save that fails
    /// after the unlock stays queued (`Commit::NotYet`): others may already
    /// have seen and built on it. One refused while the lock is still held is
    /// undone (`Commit::NotSaved`).
    pub(crate) fn commit<T, U: FnOnce(&mut State), R>(
        &self,
        operation: impl FnOnce(&mut State, Tx) -> Result<Applied<T, U>, R>,
    ) -> Result<Commit<T, R>, Unavailable> {
        self.commit_with(operation, crate::storage::submit, crate::worker::wake)
    }

    /// `commit` with the stage and the wake passed in, so tests can stage
    /// on a writer whose disk they script and see when the wake happens.
    pub(crate) fn commit_with<T, U: FnOnce(&mut State), R>(
        &self,
        operation: impl FnOnce(&mut State, Tx) -> Result<Applied<T, U>, R>,
        submit: impl FnOnce(&State, Staging) -> std::io::Result<crate::storage::Saving>,
        wake: impl FnOnce(),
    ) -> Result<Commit<T, R>, Unavailable> {
        let mut state = self.lock()?;
        match crate::storage::health() {
            crate::storage::Health::Ready => {}
            crate::storage::Health::Degraded(unavailable) => return Ok(Commit::NotSaved(unavailable.into())),
            crate::storage::Health::Halted(halted) => return Ok(Commit::NotSaved(halted.into())),
        }
        let Applied { answer, effects, undo } = match operation(&mut state, Tx::at(crate::ids::stamp())) {
            Ok(applied) => applied,
            Err(refusal) => return Ok(Commit::Refused(refusal, self.pending())),
        };
        let saving = match submit(&state, Staging(())) {
            Ok(saving) => saving,
            Err(error) => {
                undo(&mut state);
                effects.forgo();
                return Ok(Commit::NotSaved(error));
            }
        };
        *self.0.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(saving.mark());
        drop(state);
        Ok(match saving.wait() {
            Ok(durable) => {
                if effects.release(&durable).wake {
                    wake();
                }
                Commit::Saved(answer)
            }
            Err(unsaved) => {
                effects.forgo();
                Commit::NotYet(answer, unsaved)
            }
        })
    }

    /// Hold the lock across statements. Prefer `read` or `write`.
    pub(crate) fn guard(&self) -> Result<Guard<'_>, Unavailable> {
        self.lock().map(Guard)
    }

    /// The guard if the lock is free now, for tests that check a path does
    /// not hold it.
    #[cfg(test)]
    pub(crate) fn try_guard(&self) -> Option<Guard<'_>> {
        self.0.state.try_lock().ok().map(Guard)
    }

    /// The state, if this is the last handle, for tests that take it back.
    #[cfg(test)]
    pub(crate) fn into_state(self) -> Option<State> {
        Arc::try_unwrap(self.0).ok()?.state.into_inner().ok()
    }

    /// Whether a panic poisoned the lock; does not wait for it.
    pub(crate) fn is_unavailable(&self) -> bool {
        self.0.state.is_poisoned()
    }
}

/// The clock reading for a change a legacy driver saves under the lock: the
/// read its old `enqueue_for` made, moved out of the logic to the driver.
pub(crate) fn legacy_tx() -> Tx {
    Tx::at(crate::ids::stamp())
}

/// Performs released effects for a legacy driver, after its save under the
/// lock returned (`Effects::after_held_save`).
pub(crate) fn perform(released: Released) {
    if released.wake {
        crate::worker::wake();
    }
}
