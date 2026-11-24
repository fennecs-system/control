//! What an operation leaves to do once the save holding its change is
//! durable, as data. The driver releases it with the writer's proof
//! (`Durable`); nothing here performs it. A dropped value is a build error
//! (`#[must_use]` under `-D warnings`), so every path either releases it or
//! says why it never will.
use super::runs::Queued;
use control_journal::writer::Durable;

/// Work held until durability. Today only the dispatcher wake a queued run
/// needs; the live-update bump stays with the save itself.
#[must_use = "effects run only once released after durability, or are forgone with a reason"]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Effects {
    wake: bool,
}

/// Released effects, for the driver to perform.
#[must_use = "released effects are for the driver to perform"]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Released {
    pub(crate) wake: bool,
}

impl From<Queued> for Effects {
    fn from(_: Queued) -> Self {
        Self { wake: true }
    }
}

impl Effects {
    pub(crate) fn none() -> Self {
        Self::default()
    }
    /// Both sets, for an operation that queued several things.
    pub(crate) fn and(self, other: impl Into<Self>) -> Self {
        Self {
            wake: self.wake || other.into().wake,
        }
    }
    /// The effects, now that the save holding them is durable.
    pub(crate) fn release(self, _: &Durable) -> Released {
        Released { wake: self.wake }
    }
    /// For a legacy caller that saves while still holding the state lock:
    /// its change is durable exactly when that save returned `Ok`, and no
    /// one saw it before. Converted callers use [`release`](Self::release).
    pub(crate) fn after_held_save<E>(self, saved: &Result<(), E>) -> Released {
        Released {
            wake: self.wake && saved.is_ok(),
        }
    }
    /// Never released: the change was undone, or is not durable yet and
    /// whoever acts on it later finds it in state (the dispatcher scans the
    /// queue every two seconds).
    pub(crate) fn forgo(self) {}
}

/// An operation with nothing to do after durability.
impl From<()> for Effects {
    fn from((): ()) -> Self {
        Self::none()
    }
}
