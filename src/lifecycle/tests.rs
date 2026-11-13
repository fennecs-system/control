
use super::*;
use crate::runtime::RunKind;
#[test]
#[ignore = "requires the pinned Nix worker image and local Linux engine; offline, no credentials or model calls"]
fn live_owned_resource_stop_and_mismatch_preserves_unrelated_writer() {
    let image = crate::runtime::worker_image().expect("pinned worker image");
    let name = format!("control-lifecycle-fixture-{}", std::process::id());
    let workspace = std::env::temp_dir().join(&name);
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let mount = format!("type=bind,src={},dst=/workspace", workspace.display());
    let id = crate::engine::query(&[
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
    let still_running =
        crate::engine::query(&["ps", "--quiet", "--filter", &format!("id={}", id.trim())]).unwrap();
    let stopped = stop_resource(&name, "lifecycle-fixture", &image, &workspace);
    let absent =
        crate::engine::query(&["ps", "--quiet", "--filter", &format!("id={}", id.trim())]).unwrap();
    std::fs::remove_dir(&workspace).unwrap();
    assert!(wrong.is_err());
    assert!(!still_running.trim().is_empty());
    assert!(stopped.is_ok(), "{stopped:?}");
    assert!(absent.trim().is_empty());
}
#[test]
fn cancel_authority_queue_and_completion_generation() {
    let mut rt = Runtime::default();
    rt.enqueue("task".into(), RunKind::Task, "test");
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
    rt.enqueue("task".into(), RunKind::Task, "test");
    for n in 1..=2 {
        let mut a = Attempt::new();
        a.workspace_id = Some(format!("old-attempt-{n}"));
        a.activity.push(crate::usage::Activity {
            id: "x".into(),
            kind: "command_execution".into(),
            text: "t".into(),
            output: "o".repeat(100),
            status: "completed".into(),
        });
        rt.runs[0].attempts.push(a);
    }
    rt.runs[0].status = "failed".into();
    let owners = owners(&rt.runs);
    assert!(owners[0].attempts.iter().all(|a| a.activity.is_empty()));
    let row = |name: &str, purpose: &str| (name.to_owned(), "node".to_owned(), purpose.to_owned());
    let mut stopped = Vec::new();
    assert!(
        reconcile_with(
            &owners,
            "node",
            vec![
                row("control-old-attempt-1", "run"),
                row("control-auth-check-1", "check")
            ],
            |_, name| {
                stopped.push(name.to_owned());
                Ok(())
            }
        )
        .is_ok()
    );
    assert_eq!(stopped, ["control-old-attempt-1"]);
    let blocked = reconcile_with(
        &owners,
        "node",
        vec![row("control-stranger", "")],
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(blocked.contains("control-stranger") && blocked.contains("docker rm"));
    assert!(
        reconcile_with(
            &owners,
            "node",
            vec![(
                "control-stranger".into(),
                "other-node".into(),
                String::new()
            )],
            |_, _| Ok(())
        )
        .is_ok()
    );
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
    assert!(
        stop_named_with(&a, name, |args| {
            calls.push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            Ok(if args[0] == "inspect" {
                inspect.clone()
            } else {
                String::new()
            })
        })
        .is_ok()
    );
    assert_eq!(calls[1], vec!["kill".to_owned(), "a".repeat(64)]);
    assert!(
        stop_named_with(&a, name, |args| if args[0] == "inspect" {
            Ok(inspect.clone())
        } else {
            Err(std::io::Error::other("injected stop failure"))
        })
        .is_err()
    );
    assert!(
        stop_named_with(&a, name, |args| Ok(match args[0] {
            "inspect" => inspect.clone(),
            "kill" => String::new(),
            _ => "still-running".into(),
        }))
        .is_err()
    );
    let mut wrong: serde_json::Value = serde_json::from_str(&inspect).unwrap();
    wrong["node"] = serde_json::json!("other");
    let mut killed = false;
    assert!(
        stop_named_with(&a, name, |args| {
            if args[0] == "kill" {
                killed = true;
            }
            Ok(wrong.to_string())
        })
        .is_err()
    );
    assert!(!killed);
    // --rm removed the worker between listing and inspection; a fresh all-container listing proves absence.
    assert!(
        stop_named_with(&a, name, |args| if args[0] == "inspect" {
            Err(std::io::Error::other("gone"))
        } else {
            Ok("unrelated\n".into())
        })
        .is_ok()
    );
    assert!(
        stop_named_with(&a, name, |args| if args[0] == "inspect" {
            Err(std::io::Error::other("uncertain"))
        } else {
            Ok(name.into())
        })
        .is_err()
    );
}
