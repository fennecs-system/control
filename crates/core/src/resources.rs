//! Persistent resource decisions. Time, authority and engine observations are
//! supplied inputs; decisions perform no effects. Default builds need only std.
use std::{fmt, num::NonZeroU64};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid;
impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid resource value")
    }
}
impl std::error::Error for Invalid {}
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn container(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
macro_rules! identity {
    ($name:ident, $valid:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize))]
        #[cfg_attr(feature = "serde", serde(transparent))]
        pub struct $name(String);
        impl TryFrom<String> for $name {
            type Error = Invalid;
            fn try_from(s: String) -> Result<Self, Invalid> {
                if ($valid)(&s) {
                    Ok(Self(s))
                } else {
                    Err(Invalid)
                }
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        #[cfg(feature = "serde")]
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                <String as serde::Deserialize>::deserialize(d)?
                    .try_into()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}
identity!(ResourceId, identifier);
identity!(NodeId, identifier);
identity!(ChatId, identifier);
identity!(RunId, identifier);
identity!(AttemptId, identifier);
identity!(ContainerId, container);
identity!(ImageId, |s: &str| s
    .strip_prefix("sha256:")
    .is_some_and(container));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Generation(NonZeroU64);
impl Generation {
    pub const INITIAL: Self = Self(NonZeroU64::MIN);
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub fn checked_next(self) -> Option<Self> {
        NonZeroU64::new(self.get().checked_add(1)?).map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "[u64;2]", into = "[u64;2]"))]
pub struct Lease {
    issued: u64,
    expires: u64,
}
impl Lease {
    pub const MAX_SECONDS: u64 = 3600;
    pub fn new(now: u64, seconds: u64) -> Result<Self, Invalid> {
        let expires = now.checked_add(seconds).ok_or(Invalid)?;
        Self::try_from([now, expires])
    }
    pub fn expired(self, now: u64) -> bool {
        now < self.issued || now >= self.expires
    }
    pub fn expires(self) -> u64 {
        self.expires
    }
    pub fn seconds(self) -> u64 {
        self.expires - self.issued
    }
}
impl TryFrom<[u64; 2]> for Lease {
    type Error = Invalid;
    fn try_from([issued, expires]: [u64; 2]) -> Result<Self, Invalid> {
        if expires
            .checked_sub(issued)
            .is_some_and(|ttl| (1..=Self::MAX_SECONDS).contains(&ttl))
        {
            Ok(Self { issued, expires })
        } else {
            Err(Invalid)
        }
    }
}
impl From<Lease> for [u64; 2] {
    fn from(lease: Lease) -> Self {
        [lease.issued, lease.expires]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NetworkScope {
    Offline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StorageScope {
    PrivateMemory,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Scope {
    pub network: NetworkScope,
    pub storage: StorageScope,
}
impl Scope {
    pub const PRIVATE_OFFLINE: Self = Self {
        network: NetworkScope::Offline,
        storage: StorageScope::PrivateMemory,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Owner {
    pub node: NodeId,
    pub chat: ChatId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StopReason {
    Requested,
    Expired,
    Revoked,
    Exited,
    Lost,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Fault {
    IdentityMismatch,
    UnrecordedExecution,
    UnexpectedEngineState,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Phase {
    Requested,
    Creating,
    Starting(ContainerId),
    Running(ContainerId),
    Stopping {
        container: Option<ContainerId>,
        reason: StopReason,
    },
    Stopped(StopReason),
    Fenced {
        fault: Fault,
        container: Option<ContainerId>,
    },
}
impl Phase {
    pub fn container(&self) -> Option<&ContainerId> {
        match self {
            Self::Starting(id) | Self::Running(id) => Some(id),
            Self::Stopping { container, .. } | Self::Fenced { container, .. } => container.as_ref(),
            _ => None,
        }
    }
    pub fn reserves_capacity(&self) -> bool {
        !matches!(self, Self::Stopped(_))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Resource {
    pub id: ResourceId,
    pub owner: Owner,
    pub generation: Generation,
    pub image: ImageId,
    pub scope: Scope,
    pub lease: Lease,
    pub phase: Phase,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp<'a> {
    pub id: &'a ResourceId,
    pub generation: Generation,
}
impl Resource {
    pub fn stamp(&self) -> Stamp<'_> {
        Stamp {
            id: &self.id,
            generation: self.generation,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineState {
    Dormant,
    Running,
    Exited,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Tick,
    Created(ContainerId),
    Observed {
        container: ContainerId,
        state: EngineState,
    },
    Missing,
    Stop(StopReason),
    Fence(Fault),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Create,
    Inspect,
    Start,
    Remove,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub phase: Phase,
    pub effect: Option<Effect>,
}
/// No record or output clones: only the small phase/identity needed for a
/// transition. Persist the decision before interpreting its effect.
pub fn decide(
    resource: &Resource,
    stamp: Stamp<'_>,
    event: Event,
    now: u64,
) -> Result<Decision, Invalid> {
    use Phase::*;
    if stamp != resource.stamp() {
        return Err(Invalid);
    }
    let phase = &resource.phase;
    let keep = || Decision {
        phase: phase.clone(),
        effect: None,
    };
    let fence = |fault| Decision {
        phase: Fenced {
            fault,
            container: phase.container().cloned(),
        },
        effect: None,
    };
    let result = match event {
        Event::Fence(_) if matches!(phase, Stopped(_) | Fenced { .. }) => keep(),
        Event::Fence(reason) => fence(reason),
        Event::Stop(reason) => match phase {
            Stopped(_) | Fenced { .. } => keep(),
            Requested => Decision {
                phase: Stopped(reason),
                effect: None,
            },
            Stopping { .. } => keep(),
            _ => Decision {
                phase: Stopping {
                    container: phase.container().cloned(),
                    reason,
                },
                effect: None,
            },
        },
        Event::Tick
            if resource.lease.expired(now)
                && !matches!(phase, Stopped(_) | Fenced { .. } | Stopping { .. }) =>
        {
            return decide(resource, stamp, Event::Stop(StopReason::Expired), now);
        }
        Event::Tick => match phase {
            Requested => Decision {
                phase: Creating,
                effect: Some(Effect::Create),
            },
            Creating | Starting(_) | Running(_) | Stopping { .. } => Decision {
                phase: phase.clone(),
                effect: Some(Effect::Inspect),
            },
            Stopped(_) | Fenced { .. } => keep(),
        },
        Event::Created(id) => match phase {
            Creating => Decision {
                phase: Starting(id),
                effect: None,
            },
            Stopping {
                container: None,
                reason,
            } => Decision {
                phase: Stopping {
                    container: Some(id),
                    reason: *reason,
                },
                effect: None,
            },
            _ => return Err(Invalid),
        },
        Event::Missing => match phase {
            Creating if !resource.lease.expired(now) => Decision {
                phase: Creating,
                effect: Some(Effect::Create),
            },
            Stopping { reason, .. } => Decision {
                phase: Stopped(*reason),
                effect: None,
            },
            Starting(_) | Running(_) | Creating => Decision {
                phase: Stopped(StopReason::Lost),
                effect: None,
            },
            _ => keep(),
        },
        Event::Observed { container, state } => {
            if matches!(phase, Stopped(_) | Fenced { .. }) {
                return Err(Invalid);
            }
            if phase
                .container()
                .is_some_and(|expected| *expected != container)
            {
                fence(Fault::IdentityMismatch)
            } else {
                match (phase, state) {
                    (Creating, EngineState::Dormant) => Decision {
                        phase: Starting(container),
                        effect: None,
                    },
                    (Creating, _)
                    | (
                        Stopping {
                            container: None, ..
                        },
                        EngineState::Running | EngineState::Exited,
                    ) => fence(Fault::UnrecordedExecution),
                    (Stopping { reason, .. }, _) => Decision {
                        phase: Stopping {
                            container: Some(container),
                            reason: *reason,
                        },
                        effect: Some(Effect::Remove),
                    },
                    (Starting(_), EngineState::Dormant) if !resource.lease.expired(now) => {
                        Decision {
                            phase: phase.clone(),
                            effect: Some(Effect::Start),
                        }
                    }
                    (Starting(_) | Running(_), EngineState::Running)
                        if !resource.lease.expired(now) =>
                    {
                        Decision {
                            phase: Running(container),
                            effect: None,
                        }
                    }
                    (Starting(_) | Running(_), EngineState::Exited) => Decision {
                        phase: Stopping {
                            container: Some(container),
                            reason: StopReason::Exited,
                        },
                        effect: Some(Effect::Remove),
                    },
                    (Starting(_) | Running(_), _) if resource.lease.expired(now) => Decision {
                        phase: Stopping {
                            container: Some(container),
                            reason: StopReason::Expired,
                        },
                        effect: None,
                    },
                    _ => fence(Fault::UnexpectedEngineState),
                }
            }
        }
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
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
        assert!(Lease::new(10, 10).unwrap().expired(9));
        assert!(ResourceId::try_from("../../host".to_owned()).is_err());
        assert!(ImageId::try_from("latest".to_owned()).is_err());
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
                ] {
                    let mut r = resource();
                    r.phase = phase.clone();
                    if let Ok(d) = decide(&r, r.stamp(), event, now) {
                        if matches!(phase, Phase::Stopped(_) | Phase::Fenced { .. }) {
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
}
