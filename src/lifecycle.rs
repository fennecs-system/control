//! Fail-closed lifecycle operations. Docker names discover intent; verified IDs
//! authorize termination. Never stop an arbitrary container from a label alone.
use crate::{identity::Actor, runtime::Runtime, usage::Attempt, State};
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub fn request_cancel(runtime: &mut Runtime, id: &str, actor: &Actor) -> Result<(), &'static str> {
    let run = runtime
        .runs
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or("Run not found.")?;
    if actor != &Actor::LocalOperator && actor != &run.actor {
        return Err("Cannot cancel another actor's run.");
    }
    crate::identity::authorize(actor, run.action)?;
    match run.status.as_str() {
        "queued" | "waiting" => {
            let was_waiting = run.status == "waiting";
            run.status = "cancelled".into();
            run.result = if was_waiting { "Cancelled while waiting. The previous published workspace is unchanged." } else { "Cancelled before execution." }.into();
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
    let children:Vec<_>=runtime.runs.iter().filter(|r|r.persona.as_ref().is_some_and(|p|p.parent.as_deref()==Some(id))
        && matches!(r.status.as_str(),"queued"|"waiting"|"running"|"cancelling")).map(|r|r.id.clone()).collect();
    for child in children {request_cancel(runtime,&child,actor)?;}
    Ok(())
}

pub fn cancelled(bridge: Option<(&Arc<Mutex<State>>, &str)>) -> bool {
    crate::shutdown::requested() || bridge.is_some_and(|(state, id)| {
        state.lock().map_or(true, |s| {
            !s.runtime
                .runs
                .iter()
                .any(|r| r.id == id && r.status == "running")
        })
    })
}

pub fn current(run: &crate::runtime::Run, attempt: &Attempt) -> bool {
    control_core::scheduling::current_attempt(crate::scheduler::phase(run), run.attempts.last().map(crate::scheduler::stamp), crate::scheduler::stamp(attempt))
}

#[derive(Deserialize)]
struct Container {
    id: String,
    name: String,
    node: Option<String>,
    attempt: Option<String>,
    image: String,
    source: String,
    running: bool,
}
const INSPECT: &str = r#"{"id":{{json .Id}},"name":{{json .Name}},"node":{{json (index .Config.Labels "dev.control.node")}},"attempt":{{json (index .Config.Labels "dev.control.attempt")}},"image":{{json .Config.Image}},"source":{{range .Mounts}}{{if eq .Destination "/workspace"}}{{json .Source}}{{end}}{{end}},"running":{{json .State.Running}}}"#;

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
    if let Some(job)=attempt.command_jobs.iter().find(|job|job.name()==name) {return job.workspace().map_err(|_|"Missing command workspace");}
    if name == main {
        return Ok(if attempt.backend == crate::backend::Backend::Claude {
            workspace.parent().unwrap().join("claude-mailbox")
        } else {
            workspace
        });
    }
    if name == format!("{main}-tool") {
        return Ok(workspace);
    }
    if name==format!("{main}-preview") && attempt.project.is_some() {return Ok(workspace);}
    if name == format!("{main}-probe") {
        return Ok(workspace.parent().unwrap().join(
            if attempt.backend == crate::backend::Backend::Claude {
                "claude-isolation-probe"
            } else {
                "sandbox-probe/workspace"
            },
        ));
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
    if container.id.len() != 64
        || !container.id.bytes().all(|b| b.is_ascii_hexdigit())
        || container.name != format!("/{name}")
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
fn names() -> Result<Vec<(String, String, String)>, &'static str> {
    crate::runtime::docker_query(&[
        "ps",
        "--all",
        "--format",
        "{{.Names}}|{{.Label \"dev.control.node\"}}|{{.Label \"dev.control.purpose\"}}",
    ])
    .and_then(|s| {
        s.lines()
            .map(|line| {
                let mut parts = line.splitn(3, '|');
                (|| Some((parts.next()?.to_owned(), parts.next()?.to_owned(), parts.next().unwrap_or("").to_owned())))()
                    .ok_or_else(|| std::io::Error::other("Invalid container listing"))
            })
            .collect()
    })
    .map_err(|_| "Container inspection unavailable; affected work remains fenced.")
}
fn stop_named(attempt: &Attempt, name: &str) -> Result<(), &'static str> {
    if let Some(job)=attempt.command_jobs.iter().find(|job|job.name()==name) {
        return crate::command_jobs::stop(job).map_err(|_|"Could not stop exact command job; work remains fenced.");
    }
    stop_named_with(attempt, name, crate::runtime::docker_query)
}
fn stop_named_with(
    attempt: &Attempt,
    name: &str,
    mut query: impl FnMut(&[&str]) -> std::io::Result<String>,
) -> Result<(), &'static str> {
    let text = match query(&["inspect", "--format", INSPECT, name]) {
        Ok(text) => text,
        Err(_) => {
            let remaining = query(&["ps", "--all", "--format", "{{.Names}}"])
                .map_err(|_| "Could not inspect owned container; work remains fenced.")?;
            return if remaining.lines().any(|n| n == name) {
                Err("Owned container could not be inspected; work remains fenced.")
            } else {
                Ok(())
            };
        }
    };
    let container: Container =
        serde_json::from_str(text.trim()).map_err(|_| "Invalid container inspection response.")?;
    stop_verified(&container, attempt, name, &mut query)
}
fn stop_verified(
    container: &Container,
    attempt: &Attempt,
    name: &str,
    mut query: impl FnMut(&[&str]) -> std::io::Result<String>,
) -> Result<(), &'static str> {
    verified(container, attempt, name)?;
    stop_container(container, &mut query)
}
fn stop_container(
    container: &Container,
    mut query: impl FnMut(&[&str]) -> std::io::Result<String>,
) -> Result<(), &'static str> {
    if container.running {
        query(&["kill", &container.id])
            .map_err(|_| "Could not stop the verified worker; work remains fenced.")?;
    }
    // Query by immutable ID. Empty output proves no running writer, including --rm.
    let active = query(&["ps", "--quiet", "--filter", &format!("id={}", container.id)])
        .map_err(|_| "Could not confirm worker exit; work remains fenced.")?;
    if !active.trim().is_empty() {
        return Err("Worker is still running; work remains fenced.");
    }
    // Best effort: --rm usually removed it already; an exited leftover is removed by verified ID.
    let _ = query(&["rm", &container.id]);
    Ok(())
}

pub fn stop_resource(
    name: &str,
    node: &str,
    image: &str,
    workspace: &std::path::Path,
) -> Result<(), &'static str> {
    stop_resource_id(name, node, image, workspace, None)
}

pub fn stop_resource_id(
    name: &str,
    node: &str,
    image: &str,
    workspace: &std::path::Path,
    expected_id: Option<&str>,
) -> Result<(), &'static str> {
    let text = crate::runtime::docker_query(&["inspect", "--format", INSPECT, name]);
    let text = match text {
        Ok(s) => s,
        Err(_) => {
            let listed = names()?;
            return if listed.iter().any(|(n, _, _)| n == name) {
                Err("Resource ownership unavailable; work remains fenced.")
            } else {
                Ok(())
            };
        }
    };
    let container: Container =
        serde_json::from_str(text.trim()).map_err(|_| "Invalid resource inspection.")?;
    verified_resource(&container, Some(node), Some(image), workspace, name)?;
    if expected_id.is_some_and(|id| container.id != id) {
        return Err("Resource identity changed; replacement was not stopped.");
    }
    stop_container(&container, crate::runtime::docker_query)
}

pub fn stop_attempt(attempt: &Attempt) -> Result<(), &'static str> {
    let _node = attempt
        .node_id
        .as_deref()
        .ok_or("Missing persisted node identity.")?;
    let id = attempt
        .workspace_id
        .as_deref()
        .ok_or("Missing persisted workspace identity.")?;
    let prefix = format!("control-{id}");
    for (name, _, _) in names()? {
        if name == prefix || name == format!("{prefix}-tool") || name == format!("{prefix}-probe") || attempt.command_jobs.iter().any(|job|job.name()==name) {
            stop_named(attempt, &name)?;
        }
    }
    Ok(())
}

/// Containers started by `control check`, `control claude check` or login carry
/// this label; they are supervised by their command, never by run reconciliation.
pub const CHECK_LABEL: &str = "dev.control.purpose=check";
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
            attempts: r.attempts.iter().map(|a| {
                let mut identity = Attempt::new();
                identity.backend = a.backend;
                identity.image.clone_from(&a.image);
                identity.node_id.clone_from(&a.node_id);
                identity.workspace_id.clone_from(&a.workspace_id);
                identity.started_unix = a.started_unix;
                identity.project.clone_from(&a.project);
                identity.command_jobs=a.command_jobs.iter().map(crate::command_jobs::Job::identity).collect();
                identity
            }).collect(),
        })
        .collect()
}
/// Reconcile after restart outside the state lock. Active attempts belong to
/// execution supervisors; any attempt of a finished run is eligible for cleanup.
pub fn reconcile(owners: &[Owner], services: &[crate::services::Service]) -> Result<(), String> {
    let node = crate::config::node_id()?;
    let mut listed = names()?;
    if listed.iter().any(|(_,owner,purpose)|owner==&node&&purpose=="service") {
        let verified = crate::services::verified_names(services,&node).map_err(|e|e.to_string())?;
        listed.retain(|(name,owner,purpose)|!(owner==&node&&purpose=="service"&&verified.contains(name)));
    }
    reconcile_with(owners, &node, listed, stop_named)
}
fn reconcile_with(
    owners: &[Owner],
    node: &str,
    listed: Vec<(String, String, String)>,
    mut stop: impl FnMut(&Attempt, &str) -> Result<(), &'static str>,
) -> Result<(), String> {
    for (name, owner_node, purpose) in listed {
        if format!("dev.control.purpose={purpose}") == CHECK_LABEL {
            continue;
        }
        let owner = owners
            .iter()
            .find_map(|o| o.attempts.iter().find(|a| expected_path(a, &name).is_ok()).map(|a| (o, a)));
        if owner.is_none() && owner_node != node {
            continue;
        }
        match owner {
            Some((_,attempt)) if purpose=="preview"=>crate::previews::verify(&name,attempt).map_err(|_|format!("Preview {name} does not match its persisted attempt; inspect it before resuming."))?,
            Some((owner, attempt)) if !owner.active => stop(attempt, &name)
                .map_err(|m| format!("{m} Container {name}: inspect it, then run `docker rm -f {name}` to resume dispatch."))?,
            Some(_) => {}
            None => return Err(format!("Unrecognized Control container {name} on this node; dispatch is paused. Inspect it, then run `docker rm -f {name}` to resume.")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the pinned Nix worker image and local Linux engine; offline, no credentials or model calls"]
    fn live_owned_resource_stop_and_mismatch_preserves_unrelated_writer() {
        let image = crate::runtime::worker_image().expect("pinned worker image");
        let name = format!("control-lifecycle-fixture-{}", std::process::id());
        let workspace = std::env::temp_dir().join(&name);
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let mount = format!("type=bind,src={},dst=/workspace", workspace.display());
        let id = crate::runtime::docker_query(&[
            "run",
            "--detach",
            "--rm",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--name",
            &name,
            "--label",
            "dev.control.node=lifecycle-fixture",
            "--label",
            &format!("dev.control.attempt={name}"),
            "--mount",
            &mount,
            &image,
            "bash",
            "-c",
            "sleep 120",
        ])
        .unwrap();
        let wrong = stop_resource(&name, "wrong-node", &image, &workspace);
        let still_running = crate::runtime::docker_query(&[
            "ps",
            "--quiet",
            "--filter",
            &format!("id={}", id.trim()),
        ])
        .unwrap();
        let stopped = stop_resource(&name, "lifecycle-fixture", &image, &workspace);
        let absent = crate::runtime::docker_query(&[
            "ps",
            "--quiet",
            "--filter",
            &format!("id={}", id.trim()),
        ])
        .unwrap();
        std::fs::remove_dir(&workspace).unwrap();
        assert!(wrong.is_err());
        assert!(!still_running.trim().is_empty());
        assert!(stopped.is_ok(), "{stopped:?}");
        assert!(absent.trim().is_empty());
    }
    #[test]
    fn cancel_authority_queue_and_completion_generation() {
        let mut rt = Runtime::default();
        rt.enqueue("task".into(), "task", "test");
        let id = rt.runs[0].id.clone();
        assert!(request_cancel(&mut rt, &id, &Actor::Unknown).is_err());
        request_cancel(&mut rt, &id, &Actor::LocalOperator).unwrap();
        assert_eq!(rt.runs[0].status, "cancelled");
        let mut a = Attempt::new();
        a.workspace_id = Some("one".into());
        rt.runs[0].status = "running".into();
        rt.runs[0].attempts.push(a.clone());
        assert!(current(&rt.runs[0], &a));
        request_cancel(&mut rt, &id, &Actor::LocalOperator).unwrap();
        assert_eq!(rt.runs[0].status, "cancelling");
        a.workspace_id = Some("stale".into());
        assert!(!current(&rt.runs[0], &a));
    }
    #[test]
    fn reconcile_owns_earlier_attempts_skips_checks_and_names_the_blocker() {
        let mut rt = Runtime::default();
        rt.enqueue("task".into(), "task", "test");
        for n in 1..=2 {
            let mut a = Attempt::new();
            a.workspace_id = Some(format!("old-attempt-{n}"));
            a.activity.push(crate::usage::Activity { id: "x".into(), kind: "command_execution".into(), text: "t".into(), output: "o".repeat(100), status: "completed".into() });
            rt.runs[0].attempts.push(a);
        }
        rt.runs[0].status = "failed".into();
        let owners = owners(&rt.runs);
        assert!(owners[0].attempts.iter().all(|a| a.activity.is_empty()));
        let row = |name: &str, purpose: &str| (name.to_owned(), "node".to_owned(), purpose.to_owned());
        let mut stopped = Vec::new();
        assert!(reconcile_with(&owners, "node", vec![row("control-old-attempt-1", "run"), row("control-auth-check-1", "check")], |_, name| { stopped.push(name.to_owned()); Ok(()) }).is_ok());
        assert_eq!(stopped, ["control-old-attempt-1"]);
        let blocked = reconcile_with(&owners, "node", vec![row("control-stranger", "")], |_, _| Ok(())).unwrap_err();
        assert!(blocked.contains("control-stranger") && blocked.contains("docker rm"));
        assert!(reconcile_with(&owners, "node", vec![("control-stranger".into(), "other-node".into(), String::new())], |_, _| Ok(())).is_ok());
    }
    #[test]
    fn ownership_checks_every_field_before_termination() {
        let mut a = Attempt::new();
        a.workspace_id = Some("test-attempt-1".into());
        a.node_id = Some("node".into());
        a.image = Some("sha256:image".into());
        let name = "control-test-attempt-1";
        let mut c = Container {
            id: "a".repeat(64),
            name: format!("/{name}"),
            node: a.node_id.clone(),
            attempt: Some(name.into()),
            image: a.image.clone().unwrap(),
            source: expected(&a, name).unwrap().to_string_lossy().into_owned(),
            running: true,
        };
        assert!(verified(&c, &a, name).is_ok());
        c.node = Some("other".into());
        assert!(verified(&c, &a, name).is_err());
        c.node = a.node_id.clone();
        c.source.push_str("/other");
        assert!(verified(&c, &a, name).is_err());
        c.source = expected(&a, name).unwrap().to_string_lossy().into_owned();
        c.image = "wrong".into();
        assert!(verified(&c, &a, name).is_err());
        c.image = a.image.clone().unwrap();
        c.attempt = None;
        assert!(verified(&c, &a, name).is_err());
    }
    #[test]
    fn injected_engine_races_failures_and_exact_id_termination() {
        let mut a = Attempt::new();
        a.workspace_id = Some("test-attempt-1".into());
        a.node_id = Some("node".into());
        a.image = Some("sha256:image".into());
        let name = "control-test-attempt-1";
        let inspect=serde_json::json!({"id":"a".repeat(64),"name":format!("/{name}"),"node":"node","attempt":name,"image":"sha256:image","source":expected(&a,name).unwrap(),"running":true}).to_string();
        let mut calls = Vec::new();
        assert!(stop_named_with(&a, name, |args| {
            calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            Ok(if args[0] == "inspect" {
                inspect.clone()
            } else {
                String::new()
            })
        })
        .is_ok());
        assert_eq!(calls[1], vec!["kill".to_owned(), "a".repeat(64)]);
        assert!(stop_named_with(&a, name, |args| if args[0] == "inspect" {
            Ok(inspect.clone())
        } else {
            Err(std::io::Error::other("injected stop failure"))
        })
        .is_err());
        assert!(stop_named_with(&a, name, |args| Ok(match args[0] {
            "inspect" => inspect.clone(),
            "kill" => String::new(),
            _ => "still-running".into(),
        }))
        .is_err());
        let mut wrong: serde_json::Value = serde_json::from_str(&inspect).unwrap();
        wrong["node"] = serde_json::json!("other");
        let mut killed = false;
        assert!(stop_named_with(&a, name, |args| {
            if args[0] == "kill" {
                killed = true;
            }
            Ok(wrong.to_string())
        })
        .is_err());
        assert!(!killed);
        // --rm removed the worker between listing and inspection; a fresh all-container listing proves absence.
        assert!(stop_named_with(&a, name, |args| if args[0] == "inspect" {
            Err(std::io::Error::other("gone"))
        } else {
            Ok("unrelated\n".into())
        })
        .is_ok());
        assert!(stop_named_with(&a, name, |args| if args[0] == "inspect" {
            Err(std::io::Error::other("uncertain"))
        } else {
            Ok(name.into())
        })
        .is_err());
    }
}
