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
mod tests {
    use super::*;
    fn run(phase: Phase, chat: Option<&str>, order: u64) -> Run<'_> {
        Run {
            phase,
            chat,
            queue_order: order,
        }
    }
    #[test]
    fn independent_chats_share_slots_but_waits_and_cancellation_reserve_their_chat() {
        let capacity = Capacity::new(2).unwrap();
        for blocker in [
            Phase::Running,
            Phase::Cancelling,
            Phase::Waiting {
                reserves_chat: true,
            },
        ] {
            let runs = [
                run(blocker, Some("a"), 0),
                run(Phase::Queued, Some("a"), 1),
                run(Phase::Queued, Some("b"), 2),
            ];
            assert_eq!(next(runs.iter(), capacity, false), Some(2));
        }
        let runs = [
            run(
                Phase::Waiting {
                    reserves_chat: false,
                },
                Some("a"),
                0,
            ),
            run(Phase::Queued, Some("a"), 1),
        ];
        assert_eq!(next(runs.iter(), capacity, false), Some(1));
    }
    #[test]
    fn queue_order_and_stable_ties_apply_to_anonymous_work_too() {
        let runs = [
            run(Phase::Inactive, None, 0),
            run(Phase::Queued, None, 8),
            run(Phase::Queued, None, 2),
            run(Phase::Queued, None, 2),
        ];
        assert_eq!(next(runs.iter(), Capacity::new(1).unwrap(), false), Some(2));
        assert!(Capacity::new(0).is_none());
        assert!(Capacity::new(MAX_PARALLEL + 1).is_none());
        assert!(Capacity::new(usize::MAX).is_none());
    }
    #[test]
    fn pause_does_not_inspect_history_and_capacity_limits_remain_bounded() {
        let capacity = Capacity::new(1).unwrap();
        let poison = std::iter::from_fn(|| -> Option<&'static Run<'static>> {
            panic!("paused queue inspected")
        });
        assert_eq!(next(poison, capacity, true), None);
        static BUSY: Run<'static> = Run {
            phase: Phase::Running,
            chat: None,
            queue_order: 0,
        };
        assert_eq!(next(std::iter::once(&BUSY), capacity, false), None);
        let queued = run(Phase::Queued, None, 0);
        assert!(!has_capacity(std::iter::repeat_n(&queued, MAX_PENDING)));
        assert!(!has_capacity(std::iter::repeat_n(&queued, MAX_PENDING + 1)));
        assert!(has_capacity(std::iter::repeat_n(&queued, MAX_PENDING - 1)));
        let inactive = run(Phase::Inactive, None, 0);
        let waiting = run(
            Phase::Waiting {
                reserves_chat: false,
            },
            None,
            0,
        );
        assert!(has_capacity(
            std::iter::repeat_n(&inactive, 100).chain(std::iter::once(&waiting))
        ));
    }
    #[test]
    fn current_generation_requires_an_active_phase_and_matching_latest_attempt() {
        let stamp = AttemptStamp {
            workspace: Some("attempt-a"),
            started_unix: 1,
        };
        assert!(current_attempt(Phase::Running, Some(stamp), stamp));
        assert!(current_attempt(Phase::Cancelling, Some(stamp), stamp));
        for phase in [
            Phase::Inactive,
            Phase::Queued,
            Phase::Waiting {
                reserves_chat: true,
            },
        ] {
            assert!(!current_attempt(phase, Some(stamp), stamp));
        }
        assert!(!current_attempt(Phase::Running, None, stamp));
        assert!(!current_attempt(
            Phase::Running,
            Some(AttemptStamp {
                workspace: Some("attempt-b"),
                ..stamp
            }),
            stamp
        ));
        assert!(!current_attempt(
            Phase::Running,
            Some(AttemptStamp {
                started_unix: 2,
                ..stamp
            }),
            stamp
        ));
    }
}
