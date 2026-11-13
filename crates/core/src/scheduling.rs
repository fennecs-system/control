use std::{collections::HashSet, num::NonZeroU8};

pub const MAX_PENDING: usize = 32;
pub const MAX_PARALLEL: usize = 8;

/// An adapter projection, not a persisted status or provider event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Queued,
    Running,
    Cancelling,
    Waiting { reserves_chat: bool },
    Inactive,
}
impl Phase {
    #[inline]
    pub fn occupies_slot(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
    #[inline]
    pub fn is_pending(self) -> bool {
        !matches!(self, Self::Inactive)
    }
    #[inline]
    fn reserves_chat(self) -> bool {
        self.occupies_slot()
            || matches!(
                self,
                Self::Waiting {
                    reserves_chat: true
                }
            )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Run<'a> {
    pub phase: Phase,
    pub chat: Option<&'a str>,
    pub queue_order: u64,
}

/// Statically dispatched view: adapters need not construct or clone records for
/// completed history just to inspect a phase. No virtual dispatch or allocation.
/// Fast query overrides must agree with the phase-derived defaults.
pub trait RunView {
    fn phase(&self) -> Phase;
    fn chat(&self) -> Option<&str>;
    fn queue_order(&self) -> u64;
    fn occupies_slot(&self) -> bool {
        self.phase().occupies_slot()
    }
    fn is_queued(&self) -> bool {
        self.phase() == Phase::Queued
    }
    fn is_pending(&self) -> bool {
        self.phase().is_pending()
    }
    fn reserves_chat(&self) -> bool {
        self.phase().reserves_chat()
    }
}
impl RunView for Run<'_> {
    #[inline]
    fn phase(&self) -> Phase {
        self.phase
    }
    #[inline]
    fn chat(&self) -> Option<&str> {
        self.chat
    }
    #[inline]
    fn queue_order(&self) -> u64 {
        self.queue_order
    }
}

/// Constructed once at the application boundary; zero/unbounded capacity is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity(NonZeroU8);
impl Capacity {
    #[inline]
    pub fn new(slots: usize) -> Option<Self> {
        if (1..=MAX_PARALLEL).contains(&slots) {
            Some(Self(NonZeroU8::new(slots as u8)?))
        } else {
            None
        }
    }
    #[inline]
    pub fn slots(self) -> usize {
        self.0.get() as usize
    }
}

/// Counts borrowed observations; completed history is never copied or retained.
/// A full reduction keeps ordinary scans optimizable by the compiler.
pub fn has_capacity<'a, R: RunView + 'a>(runs: impl Iterator<Item = &'a R>) -> bool {
    runs.filter(|run| run.is_pending()).count() < MAX_PENDING
}

/// Returns an index in the caller's original sequence. Queue order wins, then
/// stable ledger order. Waiting humans/children reserve a chat, not a model slot.
/// The clone duplicates only the iterator cursor, never the ledger or its records.
pub fn next<'a, R: RunView + 'a>(
    runs: impl Iterator<Item = &'a R> + Clone,
    capacity: Capacity,
    paused: bool,
) -> Option<usize> {
    if paused {
        return None;
    }
    let active = runs.clone().filter(|run| run.occupies_slot()).count();
    if active >= capacity.slots() {
        return None;
    }
    let blocked = runs
        .clone()
        .filter(|run| run.reserves_chat())
        .filter_map(|run| run.chat())
        .collect::<HashSet<_>>();
    runs.enumerate()
        .filter(|(_, run)| run.is_queued() && run.chat().is_none_or(|chat| !blocked.contains(chat)))
        .min_by_key(|(index, run)| (run.queue_order(), *index))
        .map(|(index, _)| index)
}

/// Captured attempt identity. The caller remains responsible for authorization,
/// validated workspace IDs and verified container ownership before any effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptStamp<'a> {
    pub workspace: Option<&'a str>,
    pub started_unix: u64,
}
pub fn current_attempt(
    phase: Phase,
    latest: Option<AttemptStamp<'_>>,
    reported: AttemptStamp<'_>,
) -> bool {
    phase.occupies_slot() && latest == Some(reported)
}

#[cfg(test)]
mod tests;
