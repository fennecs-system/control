//! Adapter policy for named compute requests; independent of image capabilities
//! and the pure resource lifecycle. Admission runs under the State lock.
use super::*;

pub(super) const GIB: u64 = 1024 * 1024 * 1024;
pub(super) const CAPACITY: Limits = Limits { memory_bytes: 20 * GIB, cpus: 8 };
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
    let used = reserved(services);
    if used.memory_bytes.saturating_add(requested.memory_bytes) > CAPACITY.memory_bytes
        || used.cpus.saturating_add(requested.cpus) > CAPACITY.cpus
    {
        return Err(fail(
            "Service compute capacity reached (20 GiB / 8 CPUs per node); stop a service and wait for Stopped, or request a smaller shape",
        ));
    }
    Ok(())
}

pub(super) fn status(services: &[Service]) -> Value {
    let used = reserved(services);
    json!({"shapes":SHAPES.map(|shape|json!({"shape":shape,"limits":shape.limits()})),
        "capacity":{"limit":CAPACITY,"reserved":used,
            "available":{"memory_bytes":CAPACITY.memory_bytes.saturating_sub(used.memory_bytes),"cpus":CAPACITY.cpus.saturating_sub(used.cpus)}}})
}

#[cfg(test)]
mod tests;
