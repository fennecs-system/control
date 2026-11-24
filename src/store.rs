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
//! A panic while the lock is held poisons it. Every method then returns
//! `Unavailable` and the caller keeps its own answer (503, an error, a skipped
//! tick); `Store` never clears poison.
use crate::State;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

/// A cheap-to-clone handle to the shared application state.
#[derive(Clone)]
pub(crate) struct Store(Arc<Mutex<State>>);

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

impl Store {
    pub(crate) fn new(state: State) -> Self {
        Self(Arc::new(Mutex::new(state)))
    }

    /// Run `view` with shared access and return what it copied out.
    pub(crate) fn read<T>(&self, view: impl FnOnce(&State) -> T) -> Result<T, Unavailable> {
        self.0.lock().map(|state| view(&state)).map_err(|_| Unavailable)
    }

    /// Run `change` with exclusive access and return its result.
    pub(crate) fn write<T>(&self, change: impl FnOnce(&mut State) -> T) -> Result<T, Unavailable> {
        self.0
            .lock()
            .map(|mut state| change(&mut state))
            .map_err(|_| Unavailable)
    }

    /// Hold the lock across statements. Prefer `read` or `write`.
    pub(crate) fn guard(&self) -> Result<Guard<'_>, Unavailable> {
        self.0.lock().map(Guard).map_err(|_| Unavailable)
    }

    /// The guard if the lock is free now, for tests that check a path does
    /// not hold it.
    #[cfg(test)]
    pub(crate) fn try_guard(&self) -> Option<Guard<'_>> {
        self.0.try_lock().ok().map(Guard)
    }

    /// The state, if this is the last handle, for tests that take it back.
    #[cfg(test)]
    pub(crate) fn into_state(self) -> Option<State> {
        Arc::try_unwrap(self.0).ok()?.into_inner().ok()
    }

    /// Whether a panic poisoned the lock; does not wait for it.
    pub(crate) fn is_unavailable(&self) -> bool {
        self.0.is_poisoned()
    }
}
