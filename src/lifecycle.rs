//! Fail-closed lifecycle operations. Docker names discover intent; verified IDs
//! authorize termination. Never stop an arbitrary container from a label alone.
use crate::{State, identity::Actor, runtime::Runtime, usage::Attempt};
use control_isolation::{ContainerId, Isolation, Observed, Select, States, Target};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub fn request_cancel(runtime: &mut Runtime, id: &str, actor: &Actor) -> Result<(), &'static str> {
    let run = runtime.runs.iter_mut().find(|r| r.id == id).ok_or("Run not found.")?;
    if actor != &Actor::LocalOperator && actor != &run.actor {
        return Err("Cannot cancel another actor's run.");
    }
    crate::identity::authorize(actor, run.action)?;
    match run.status.as_str() {
        "queued" | "waiting" => {
            let was_waiting = run.status == "waiting";
            run.status = "cancelled".into();
            run.result = if was_waiting {
                "Cancelled while waiting. The previous published workspace is unchanged."
            } else {
                "Cancelled before execution."
            }
            .into();
        }
        "running" => {
            run.status = "cancelling".into();
            run.result = "Stopping the owned worker…".into();
        }
        "cancelling" | "cancelled" => {}
        _ => return Err("Run is already finished."),
    }
    // Cancelling a paused supervisor also cancels its bounded descendants;
    // independent human-created chats are not part of this lineage.
    let children: Vec<_> = runtime
        .runs
        .iter()
        .filter(|r| {
            r.persona.as_ref().is_some_and(|p| p.parent.as_deref() == Some(id))
                && matches!(r.status.as_str(), "queued" | "waiting" | "running" | "cancelling")
        })
        .map(|r| r.id.clone())
        .collect();
    for child in children {
        request_cancel(runtime, &child, actor)?;
    }
    Ok(())
}

pub fn cancelled(bridge: Option<(&Arc<Mutex<State>>, &str)>) -> bool {
    crate::shutdown::requested()
        || bridge.is_some_and(|(state, id)| {
            state.lock().map_or(true, |s| {
                !s.runtime.runs.iter().any(|r| r.id == id && r.status == "running")
            })
        })
}

pub fn current(run: &crate::runtime::Run, attempt: &Attempt) -> bool {
    control_core::scheduling::current_attempt(
        crate::scheduler::phase(run),
        run.attempts.last().map(crate::scheduler::stamp),
        crate::scheduler::stamp(attempt),
    )
}

/// The ownership facts of one inspected container.
#[cfg_attr(test, derive(Clone))]
struct Container {
    id: ContainerId,
    name: String,
    node: Option<String>,
    attempt: Option<String>,
    image: String,
    source: String,
    running: bool,
}
/// `None` when it has no `/workspace` mount, which no owned resource lacks.
fn container(observed: Observed) -> Option<Container> {
    Some(Container {
        source: observed.mount("/workspace")?.source.clone(),
        node: observed.label("dev.control.node").map(str::to_owned),
        attempt: observed.label("dev.control.attempt").map(str::to_owned),
        id: observed.id,
        name: observed.name,
        image: observed.image_ref,
        running: observed.running,
    })
}
/// The single container `name` inspects as; `None` if the engine reported another shape.
fn inspected(isolation: &dyn Isolation, name: &str) -> std::io::Result<Option<Container>> {
    Ok(<[Observed; 1]>::try_from(isolation.inspect(&[Target::Name(name)])?)
        .ok()
        .and_then(|[observed]| container(observed)))
}

fn expected(attempt: &Attempt, name: &str) -> Result<PathBuf, &'static str> {
    let path = expected_path(attempt, name)?;
    Ok(path.canonicalize().unwrap_or(path))
}
fn expected_path(attempt: &Attempt, name: &str) -> Result<PathBuf, &'static str> {
    let id = attempt
        .workspace_id
        .as_deref()
        .ok_or("Missing persisted workspace identity.")?;
    let main = format!("control-{id}");
    let workspace = crate::runtime::workspace(id)?;
    if let Some(job) = attempt.command_jobs.iter().find(|job| job.name() == name) {
        return job.workspace().map_err(|_| "Missing command workspace");
    }
    if name == main {
        return Ok(match attempt.backend.agent().mailbox() {
            Some(mailbox) => workspace.parent().unwrap().join(mailbox),
            None => workspace,
        });
    }
    if name == format!("{main}-tool") {
        return Ok(workspace);
    }
    if name == format!("{main}-preview") && attempt.project.is_some() {
        return Ok(workspace);
    }
    if name == format!("{main}-probe") {
        return Ok(workspace
            .parent()
            .unwrap()
            .join(attempt.backend.agent().probe_workspace()));
    }
    Err("Container is not an exact persisted attempt resource.")
}
fn verified(container: &Container, attempt: &Attempt, name: &str) -> Result<(), &'static str> {
    verified_resource(
        container,
        attempt.node_id.as_deref(),
        attempt.image.as_deref(),
        &expected(attempt, name)?,
        name,
    )
}
fn verified_resource(
    container: &Container,
    node: Option<&str>,
    image: Option<&str>,
    source: &std::path::Path,
    name: &str,
) -> Result<(), &'static str> {
    if container.name != format!("/{name}")
        || container.node.as_deref() != node
        || container.node.is_none()
        || container.attempt.as_deref() != Some(name)
        || image != Some(container.image.as_str())
        || std::path::Path::new(&container.source) != source
    {
        return Err("Container ownership, image or workspace does not match durable intent.");
    }
    Ok(())
}
/// Every container's name with its node and purpose labels (empty when absent).
fn names(isolation: &dyn Isolation) -> Result<Vec<(String, String, String)>, &'static str> {
    let label =
        |labels: &std::collections::BTreeMap<String, String>, key: &str| labels.get(key).cloned().unwrap_or_default();
    isolation
        .list(States::All, &Select::Any)
        .map(|listed| {
            listed
                .into_iter()
                .map(|c| {
                    (
                        label(&c.labels, "dev.control.node"),
                        label(&c.labels, "dev.control.purpose"),
                        c.name,
                    )
                })
                .map(|(node, purpose, name)| (name, node, purpose))
                .collect()
        })
        .map_err(|_| "Container inspection unavailable; affected work remains fenced.")
}
fn stop_named(isolation: &dyn Isolation, attempt: &Attempt, name: &str) -> Result<(), &'static str> {
    if let Some(job) = attempt.command_jobs.iter().find(|job| job.name() == name) {
        return crate::command_jobs::stop(isolation, job)
            .map_err(|_| "Could not stop exact command job; work remains fenced.");
    }
    stop_named_with(isolation, attempt, name)
}
fn stop_named_with(isolation: &dyn Isolation, attempt: &Attempt, name: &str) -> Result<(), &'static str> {
    let container = match inspected(isolation, name) {
        Ok(container) => container,
        Err(_) => {
            let remaining = isolation
                .list(States::All, &Select::Any)
                .map_err(|_| "Could not inspect owned container; work remains fenced.")?;
            return if remaining.iter().any(|c| c.name == name) {
                Err("Owned container could not be inspected; work remains fenced.")
            } else {
                Ok(())
            };
        }
    };
    let container = container.ok_or("Invalid container inspection response.")?;
    verified(&container, attempt, name)?;
    stop_container(isolation, &container)
}
fn stop_container(isolation: &dyn Isolation, container: &Container) -> Result<(), &'static str> {
    if container.running {
        isolation
            .kill(&container.id)
            .map_err(|_| "Could not stop the verified worker; work remains fenced.")?;
    }
    // Query by immutable ID. Empty output proves no running writer, including --rm.
    let running = || {
        isolation
            .list(States::Running, &Select::Id(container.id.clone()))
            .map(|active| !active.is_empty())
            .map_err(|_| "Could not confirm worker exit; work remains fenced.")
    };
    await_exit(running, || std::thread::sleep(EXIT_POLL))?;
    // Best effort: --rm usually removed it already; an exited leftover is removed by verified ID.
    let _ = isolation.remove(Target::Id(&container.id));
    Ok(())
}
const EXIT_POLLS: usize = 100;
const EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(50);
/// The engine can acknowledge a kill while it still lists the container as
/// running, so a single check would fence a writer that is about to exit.
/// Polls `running` (pausing between checks) for a bounded time instead.
fn await_exit(mut running: impl FnMut() -> Result<bool, &'static str>, pause: impl Fn()) -> Result<(), &'static str> {
    let still_running = (0..EXIT_POLLS)
        .map(|poll| {
            if poll > 0 {
                pause();
            }
            running()
        })
        .find(|checked| checked != &Ok(true))
        .unwrap_or(Ok(true))?;
    if still_running {
        return Err("Worker is still running; work remains fenced.");
    }
    Ok(())
}

pub fn stop_resource(
    isolation: &dyn Isolation,
    name: &str,
    node: &str,
    image: &str,
    workspace: &std::path::Path,
) -> Result<(), &'static str> {
    stop_resource_id(isolation, name, node, image, workspace, None)
}

pub fn stop_resource_id(
    isolation: &dyn Isolation,
    name: &str,
    node: &str,
    image: &str,
    workspace: &std::path::Path,
    expected_id: Option<&str>,
) -> Result<(), &'static str> {
    let container = match inspected(isolation, name) {
        Ok(container) => container,
        Err(_) => {
            let listed = names(isolation)?;
            return if listed.iter().any(|(n, _, _)| n == name) {
                Err("Resource ownership unavailable; work remains fenced.")
            } else {
                Ok(())
            };
        }
    };
    let container = container.ok_or("Invalid resource inspection.")?;
    verified_resource(&container, Some(node), Some(image), workspace, name)?;
    if expected_id.is_some_and(|id| container.id.as_str() != id) {
        return Err("Resource identity changed; replacement was not stopped.");
    }
    stop_container(isolation, &container)
}

pub fn stop_attempt(isolation: &dyn Isolation, attempt: &Attempt) -> Result<(), &'static str> {
    let _node = attempt.node_id.as_deref().ok_or("Missing persisted node identity.")?;
    let id = attempt
        .workspace_id
        .as_deref()
        .ok_or("Missing persisted workspace identity.")?;
    let prefix = format!("control-{id}");
    for (name, _, _) in names(isolation)? {
        if name == prefix
            || name == format!("{prefix}-tool")
            || name == format!("{prefix}-probe")
            || (attempt.project.is_some() && name == format!("{prefix}-preview"))
            || attempt.command_jobs.iter().any(|job| job.name() == name)
        {
            stop_named(isolation, attempt, &name)?;
        }
    }
    Ok(())
}

/// Containers started by `control check`, `control claude check` or login carry
/// this label; they are supervised by their command, never by run reconciliation.
pub const CHECK_LABEL: &str = "dev.control.purpose=check";
/// Labels a check supervised by its own command, or a run's container, which
/// run reconciliation owns.
pub(crate) fn label_purpose(spec: control_isolation::RunSpec, check: bool) -> control_isolation::RunSpec {
    let (name, value) = if check { CHECK_LABEL } else { "dev.control.purpose=run" }
        .split_once('=')
        .unwrap();
    spec.label(name, value)
}
/// Identity-only snapshot of every attempt (no activity), taken under the state lock.
pub struct Owner {
    pub active: bool,
    pub attempts: Vec<Attempt>,
}
pub fn owners(runs: &[crate::runtime::Run]) -> Vec<Owner> {
    runs.iter()
        .filter(|r| !r.attempts.is_empty())
        .map(|r| Owner {
            active: ["running", "cancelling"].contains(&r.status.as_str()),
            attempts: r
                .attempts
                .iter()
                .map(|a| {
                    let mut identity = Attempt::new();
                    identity.backend = a.backend;
                    identity.image.clone_from(&a.image);
                    identity.node_id.clone_from(&a.node_id);
                    identity.workspace_id.clone_from(&a.workspace_id);
                    identity.started_unix = a.started_unix;
                    identity.project.clone_from(&a.project);
                    identity.command_jobs = a.command_jobs.iter().map(crate::command_jobs::Job::identity).collect();
                    identity
                })
                .collect(),
        })
        .collect()
}
/// Reconcile after restart outside the state lock. Active attempts belong to
/// execution supervisors; any attempt of a finished run is eligible for cleanup.
pub fn reconcile(
    isolation: &dyn Isolation,
    owners: &[Owner],
    services: &[crate::services::Service],
) -> Result<(), String> {
    let node = crate::config::node_id()?;
    let mut listed = names(isolation)?;
    if listed
        .iter()
        .any(|(_, owner, purpose)| owner == &node && purpose == "service")
    {
        let verified = crate::services::verified_names(isolation, services, &node).map_err(|e| e.to_string())?;
        listed.retain(|(name, owner, purpose)| !(owner == &node && purpose == "service" && verified.contains(name)));
    }
    reconcile_with(
        owners,
        &node,
        listed,
        |attempt, name| stop_named(isolation, attempt, name),
        |name, attempt| crate::previews::verify(isolation, name, attempt),
    )
}
fn reconcile_with(
    owners: &[Owner],
    node: &str,
    listed: Vec<(String, String, String)>,
    mut stop: impl FnMut(&Attempt, &str) -> Result<(), &'static str>,
    preview: impl Fn(&str, &Attempt) -> std::io::Result<()>,
) -> Result<(), String> {
    // Every resource name embeds its generation. Index attempts by generation
    // once, so a container checks its own candidates rather than all history.
    let mut generations = std::collections::HashMap::<&str, Vec<(usize, usize)>>::new();
    for (o, owner) in owners.iter().enumerate() {
        for (a, attempt) in owner.attempts.iter().enumerate() {
            for generation in attempt
                .workspace_id
                .iter()
                .chain(attempt.command_jobs.iter().map(|job| &job.generation))
            {
                let candidates = generations.entry(generation).or_default();
                if candidates.last() != Some(&(o, a)) {
                    candidates.push((o, a));
                }
            }
        }
    }
    for (name, owner_node, purpose) in listed {
        if format!("dev.control.purpose={purpose}") == CHECK_LABEL {
            continue;
        }
        // Candidate generations for each name shape `expected_path` accepts;
        // it still decides, in the original owner/attempt order.
        let rest = name.strip_prefix("control-").unwrap_or_default();
        let owner = std::iter::once(rest)
            .chain(
                ["-tool", "-probe", "-preview"]
                    .into_iter()
                    .filter_map(|suffix| rest.strip_suffix(suffix)),
            )
            .chain(rest.match_indices("-job-").map(|(end, _)| &rest[..end]))
            .filter_map(|generation| generations.get(generation))
            .flatten()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|(o, a)| (&owners[o], &owners[o].attempts[a]))
            .find(|(_, attempt)| expected_path(attempt, &name).is_ok());
        if owner.is_none() && owner_node != node {
            continue;
        }
        match owner {
            Some((_, attempt)) if purpose == "preview" => preview(&name, attempt).map_err(|_| {
                format!("Preview {name} does not match its persisted attempt; inspect it before resuming.")
            })?,
            Some((owner, attempt)) if !owner.active => stop(attempt, &name).map_err(|m| {
                format!("{m} Container {name}: inspect it, then run `docker rm -f {name}` to resume dispatch.")
            })?,
            Some(_) => {}
            None => {
                return Err(format!(
                    "Unrecognized Control container {name} on this node; dispatch is paused. Inspect it, then run `docker rm -f {name}` to resume."
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
