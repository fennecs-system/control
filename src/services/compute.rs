//! Adapter policy for named compute requests; independent of image capabilities
//! and the pure resource lifecycle. Admission runs under the State lock.
use super::*;

pub(super) const GIB: u64 = 1024 * 1024 * 1024;
/// The service admission budget. Memory is a strict sum of container limits.
/// CPU limits are caps, not reservations: idle services use little, and excess
/// demand is throttled rather than killed, so CPU is admitted against
/// `cpus * cpu_oversubscription` while every container keeps its hard cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Capacity {
    pub memory_bytes: u64,
    pub cpus: u64,
    pub cpu_oversubscription: u64,
}
impl Capacity {
    pub(super) const DEFAULT: Self = Self { memory_bytes: 20 * GIB, cpus: 8, cpu_oversubscription: 2 };
    /// Node settings `service-memory-gib`, `service-cpus` and
    /// `service-cpu-oversubscription`; invalid or absent values keep defaults.
    pub(super) fn from_settings(setting: impl Fn(&str) -> Option<String>) -> Self {
        let read = |name: &str, range: std::ops::RangeInclusive<u64>| {
            setting(name).and_then(|value| value.trim().parse::<u64>().ok()).filter(|value| range.contains(value))
        };
        Self {
            memory_bytes: read("service-memory-gib", 1..=4096).map_or(Self::DEFAULT.memory_bytes, |gib| gib * GIB),
            cpus: read("service-cpus", 1..=1024).unwrap_or(Self::DEFAULT.cpus),
            cpu_oversubscription: read("service-cpu-oversubscription", 1..=8).unwrap_or(Self::DEFAULT.cpu_oversubscription),
        }
    }
    pub(super) fn admitted_cpus(self) -> u64 {
        self.cpus.saturating_mul(self.cpu_oversubscription)
    }
}
#[cfg(not(test))]
pub(super) fn capacity() -> Capacity {
    let directory = crate::config::directory().ok();
    Capacity::from_settings(|name| std::fs::read_to_string(directory.as_ref()?.join(name)).ok())
}
/// Unit tests never read the operator's node configuration.
#[cfg(test)]
pub(super) fn capacity() -> Capacity {
    Capacity::DEFAULT
}
pub(super) const SHAPES: [Shape; 3] = [Shape::Small, Shape::Medium, Shape::Large];

/// These identifiers have immutable limits; a future preset needs a new name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Shape {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Limits {
    pub memory_bytes: u64,
    pub cpus: u64,
}

impl Shape {
    pub(super) fn limits(self) -> Limits {
        match self {
            Self::Small => Limits {
                memory_bytes: 2 * GIB,
                cpus: 1,
            },
            Self::Medium => Limits {
                memory_bytes: 4 * GIB,
                cpus: 2,
            },
            Self::Large => Limits {
                memory_bytes: 8 * GIB,
                cpus: 2,
            },
        }
    }
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }
}

pub(super) fn limits(shape: Option<Shape>, profile: crate::worker_profile::Profile) -> Limits {
    shape.map(Shape::limits).unwrap_or(Limits {
        memory_bytes: profile.service_memory(),
        cpus: 1,
    })
}

pub(super) fn reserved(services: &[Service]) -> Limits {
    services
        .iter()
        .filter(|s| s.resource.phase.reserves_capacity())
        .fold(
            Limits {
                memory_bytes: 0,
                cpus: 0,
            },
            |total, service| {
                let limits = service.limits();
                Limits {
                    memory_bytes: total.memory_bytes.saturating_add(limits.memory_bytes),
                    cpus: total.cpus.saturating_add(limits.cpus),
                }
            },
        )
}

/// Stopping and fenced resources keep capacity until cleanup releases it.
pub(super) fn admit(services: &[Service], requested: Limits) -> io::Result<()> {
    admit_within(services, requested, capacity())
}
pub(super) fn admit_within(services: &[Service], requested: Limits, capacity: Capacity) -> io::Result<()> {
    let used = reserved(services);
    if used.memory_bytes.saturating_add(requested.memory_bytes) > capacity.memory_bytes
        || used.cpus.saturating_add(requested.cpus) > capacity.admitted_cpus()
    {
        return Err(fail(format!(
            "Service compute capacity reached ({} GiB, {} CPUs x{} oversubscription per node); stop a service and wait for Stopped, or request a smaller shape",
            capacity.memory_bytes / GIB, capacity.cpus, capacity.cpu_oversubscription,
        )));
    }
    Ok(())
}

pub(super) fn status(services: &[Service]) -> Value {
    let (used, capacity) = (reserved(services), capacity());
    json!({"shapes":SHAPES.map(|shape|json!({"shape":shape,"limits":shape.limits()})),
        "capacity":{"limit":{"memory_bytes":capacity.memory_bytes,"cpus":capacity.cpus},
            "cpu_oversubscription":capacity.cpu_oversubscription,"admitted_cpus":capacity.admitted_cpus(),"reserved":used,
            "available":{"memory_bytes":capacity.memory_bytes.saturating_sub(used.memory_bytes),"cpus":capacity.admitted_cpus().saturating_sub(used.cpus)}}})
}

#[cfg(test)]
mod tests;
