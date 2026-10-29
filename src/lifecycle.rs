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
        "queued" => {
            run.status = "cancelled".into();
            run.result = "Cancelled before execution.".into();
        }
        "running" => {
            run.status = "cancelling".into();
            run.result = "Stopping the owned worker…".into();
        }
        "cancelling" | "cancelled" => {}
        _ => return Err("Run is already finished."),
    }
    Ok(())
}

pub fn cancelled(bridge: Option<(&Arc<Mutex<State>>, &str)>) -> bool {
    bridge.is_some_and(|(state, id)| {
        state.lock().map_or(true, |s| {
            !s.runtime
                .runs
                .iter()
                .any(|r| r.id == id && r.status == "running")
        })
    })
}

pub fn current(run: &crate::runtime::Run, attempt: &Attempt) -> bool {
    ["running", "cancelling"].contains(&run.status.as_str())
        && run.attempts.last().is_some_and(|last| {
            last.workspace_id == attempt.workspace_id && last.started_unix == attempt.started_unix
        })
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
fn names() -> Result<Vec<(String, String)>, &'static str> {
    crate::runtime::docker_query(&[
        "ps",
        "--all",
        "--format",
        "{{.Names}}|{{.Label \"dev.control.node\"}}",
    ])
    .and_then(|s| {
        s.lines()
            .map(|line| {
                line.split_once('|')
                    .map(|(name, node)| (name.to_owned(), node.to_owned()))
                    .ok_or_else(|| std::io::Error::other("Invalid container listing"))
            })
            .collect()
    })
    .map_err(|_| "Container inspection unavailable; affected work remains fenced.")
}
fn stop_named(attempt: &Attempt, name: &str) -> Result<(), &'static str> {
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
    Ok(())
}

pub fn stop_resource(
    name: &str,
    node: &str,
    image: &str,
    workspace: &std::path::Path,
) -> Result<(), &'static str> {
    let text = crate::runtime::docker_query(&["inspect", "--format", INSPECT, name]);
    let text = match text {
        Ok(s) => s,
        Err(_) => {
            let listed = names()?;
            return if listed.iter().any(|(n, _)| n == name) {
                Err("Resource ownership unavailable; work remains fenced.")
            } else {
                Ok(())
            };
        }
    };
    let container: Container =
        serde_json::from_str(text.trim()).map_err(|_| "Invalid resource inspection.")?;
    verified_resource(&container, Some(node), Some(image), workspace, name)?;
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
    for (name, _) in names()? {
        if name == prefix || name == format!("{prefix}-tool") || name == format!("{prefix}-probe") {
            stop_named(attempt, &name)?;
        }
    }
    Ok(())
}

/// Reconcile after restart outside the state lock. Active attempts belong to
/// execution supervisors; only interrupted attempts are eligible for cleanup.
pub fn reconcile(runs: &[crate::runtime::Run]) -> Result<(), &'static str> {
    let node = crate::config::node_id()?;
    for (name, owner_node) in names()? {
        let owner = runs
            .iter()
            .filter_map(|r| r.attempts.last().map(|a| (r, a)))
            .find(|(_, a)| expected(a, &name).is_ok());
        if owner.is_none() && owner_node != node {
            continue;
        }
        match owner {
            Some((run, attempt)) if !["running", "cancelling"].contains(&run.status.as_str()) => {
                stop_named(attempt, &name)?
            }
            Some((run, _)) if ["running", "cancelling"].contains(&run.status.as_str()) => {}
            _ => {
                return Err("Unrecognized node container requires inspection; dispatch is paused.")
            }
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
