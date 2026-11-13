
use super::*;
pub fn resource() -> Resource {
    Resource {
        id: ResourceId::try_from("service-a".to_owned()).unwrap(),
        owner: Owner {
            node: NodeId::try_from("node-a".to_owned()).unwrap(),
            chat: ChatId::try_from("chat-a".to_owned()).unwrap(),
        },
        generation: Generation::INITIAL,
        image: ImageId::try_from(format!("sha256:{}", "a".repeat(64))).unwrap(),
        scope: Scope::PRIVATE_OFFLINE,
        lease: Lease::new(10, 60).unwrap(),
        phase: Phase::Requested,
    }
}
fn id() -> ContainerId {
    ContainerId::try_from("b".repeat(64)).unwrap()
}
fn apply(r: &mut Resource, event: Event, now: u64) -> Option<Effect> {
    let d = decide(r, r.stamp(), event, now).unwrap();
    r.phase = d.phase;
    d.effect
}
#[test]
fn creation_is_durable_before_execution_and_exit_is_never_replayed() {
    let mut r = resource();
    assert_eq!(apply(&mut r, Event::Tick, 11), Some(Effect::Create));
    assert_eq!(apply(&mut r, Event::Created(id()), 11), None);
    assert!(matches!(r.phase, Phase::Starting(_)));
    assert_eq!(
        apply(
            &mut r,
            Event::Observed {
                container: id(),
                state: EngineState::Dormant
            },
            11
        ),
        Some(Effect::Start)
    );
    apply(
        &mut r,
        Event::Observed {
            container: id(),
            state: EngineState::Running,
        },
        12,
    );
    assert!(matches!(r.phase, Phase::Running(_)));
    assert_eq!(
        apply(
            &mut r,
            Event::Observed {
                container: id(),
                state: EngineState::Exited
            },
            13
        ),
        Some(Effect::Remove)
    );
    apply(&mut r, Event::Missing, 13);
    assert_eq!(r.phase, Phase::Stopped(StopReason::Exited));
    assert_eq!(apply(&mut r, Event::Tick, 14), None);
}
#[test]
fn stop_during_create_binds_late_result_but_never_starts_it() {
    let mut r = resource();
    apply(&mut r, Event::Tick, 11);
    apply(&mut r, Event::Stop(StopReason::Requested), 11);
    apply(&mut r, Event::Created(id()), 11);
    assert_eq!(
        apply(
            &mut r,
            Event::Observed {
                container: id(),
                state: EngineState::Dormant
            },
            12
        ),
        Some(Effect::Remove)
    );
    apply(&mut r, Event::Missing, 12);
    assert_eq!(r.phase, Phase::Stopped(StopReason::Requested));
}
#[test]
fn stale_reports_unknown_execution_and_expiry_fail_closed() {
    let mut r = resource();
    assert!(
        decide(
            &r,
            Stamp {
                id: &r.id,
                generation: r.generation.checked_next().unwrap()
            },
            Event::Tick,
            11
        )
        .is_err()
    );
    let other = ResourceId::try_from("other".to_owned()).unwrap();
    assert!(
        decide(
            &r,
            Stamp {
                id: &other,
                generation: r.generation
            },
            Event::Tick,
            11
        )
        .is_err()
    );
    apply(&mut r, Event::Tick, 11);
    apply(
        &mut r,
        Event::Observed {
            container: id(),
            state: EngineState::Running,
        },
        12,
    );
    assert_eq!(
        r.phase,
        Phase::Fenced {
            fault: Fault::UnrecordedExecution,
            container: None
        }
    );
    let mut r = resource();
    apply(&mut r, Event::Tick, 70);
    assert_eq!(r.phase, Phase::Stopped(StopReason::Expired));
    assert!(Lease::new(u64::MAX, 1).is_err());
    assert!(Lease::new(0, 0).is_err());
    assert!(Lease::new(0, 3601).is_err());
    assert!(Lease::new(0, u64::MAX).is_err());
    let lifetime = Lease::until_stopped(10).unwrap();
    assert!(!lifetime.expired(10 + 86400 * 365));
    assert!(lifetime.expired(9));
    assert_eq!(
        Lease::try_from(<[u64; 2]>::from(lifetime)).unwrap(),
        lifetime
    );
    assert!(Lease::new(10, 10).unwrap().expired(9));
    assert!(ResourceId::try_from("../../host".to_owned()).is_err());
    assert!(ImageId::try_from("latest".to_owned()).is_err());
}
#[test]
fn fenced_stop_requires_observed_cleanup_and_never_restarts() {
    for container in [None, Some(id())] {
        let mut r = resource();
        r.phase = Phase::Fenced {
            fault: Fault::UnexpectedEngineState,
            container: container.clone(),
        };
        assert_eq!(apply(&mut r, Event::Tick, 11), None);
        assert_eq!(apply(&mut r, Event::Missing, 11), None);
        assert!(matches!(r.phase, Phase::Fenced { .. }));
        assert_eq!(apply(&mut r, Event::Stop(StopReason::Requested), 11), None);
        assert_eq!(
            r.phase,
            Phase::Stopping {
                container,
                reason: StopReason::Requested
            }
        );
        assert!(r.phase.reserves_capacity());
        assert_eq!(apply(&mut r, Event::Tick, 11), Some(Effect::Inspect));
        assert_eq!(apply(&mut r, Event::Missing, 11), None);
        assert_eq!(r.phase, Phase::Stopped(StopReason::Requested));
        assert!(!r.phase.reserves_capacity());
    }
}
#[test]
fn lifecycle_matrix_never_restarts_terminal_or_stopping_work() {
    let phases = [
        Phase::Requested,
        Phase::Creating,
        Phase::Starting(id()),
        Phase::Running(id()),
        Phase::Stopping {
            container: None,
            reason: StopReason::Requested,
        },
        Phase::Stopping {
            container: Some(id()),
            reason: StopReason::Expired,
        },
        Phase::Stopped(StopReason::Exited),
        Phase::Fenced {
            fault: Fault::IdentityMismatch,
            container: Some(id()),
        },
    ];
    for phase in phases {
        for now in [9, 11, 70] {
            for event in [
                Event::Tick,
                Event::Missing,
                Event::Created(id()),
                Event::Stop(StopReason::Revoked),
                Event::Fence(Fault::IdentityMismatch),
                Event::Observed {
                    container: id(),
                    state: EngineState::Dormant,
                },
                Event::Observed {
                    container: id(),
                    state: EngineState::Running,
                },
                Event::Observed {
                    container: id(),
                    state: EngineState::Exited,
                },
                Event::Observed {
                    container: id(),
                    state: EngineState::Other,
                },
            ] {
                let mut r = resource();
                r.phase = phase.clone();
                if let Ok(d) = decide(&r, r.stamp(), event, now) {
                    if matches!(phase, Phase::Stopped(_)) {
                        assert_eq!(d.phase, phase, "Terminal state cannot reserve new work");
                    }
                    if matches!(
                        phase,
                        Phase::Stopping { .. } | Phase::Stopped(_) | Phase::Fenced { .. }
                    ) {
                        assert!(!matches!(d.effect, Some(Effect::Create | Effect::Start)));
                    }
                    if d.effect == Some(Effect::Start) {
                        assert!(matches!(phase, Phase::Starting(_)));
                        assert!(!r.lease.expired(now));
                    }
                }
            }
        }
    }
}
