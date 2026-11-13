
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
