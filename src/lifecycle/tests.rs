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
    let wrong = stop_resource(crate::engine::isolation(), &name, "wrong-node", &image, &workspace);
    let still_running = crate::engine::query(&["ps", "--quiet", "--filter", &format!("id={}", id.trim())]).unwrap();
    let stopped = stop_resource(
        crate::engine::isolation(),
        &name,
        "lifecycle-fixture",
        &image,
        &workspace,
    );
    let absent = crate::engine::query(&["ps", "--quiet", "--filter", &format!("id={}", id.trim())]).unwrap();
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
            },
            |_, _| Ok(())
        )
        .is_ok()
    );
    assert_eq!(stopped, ["control-old-attempt-1"]);
    let blocked = reconcile_with(
        &owners,
        "node",
        vec![row("control-stranger", "")],
        |_, _| Ok(()),
        |_, _| Ok(()),
    )
    .unwrap_err();
    assert!(blocked.contains("control-stranger") && blocked.contains("docker rm"));
    assert!(
        reconcile_with(
            &owners,
            "node",
            vec![("control-stranger".into(), "other-node".into(), String::new())],
            |_, _| Ok(()),
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
        id: "a".repeat(64).into(),
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
    use crate::engine::fake::{Call, Fake, Reply};
    use control_isolation::{Listed, MountKind, ObservedMount};
    let mut a = Attempt::new();
    a.workspace_id = Some("test-attempt-1".into());
    a.node_id = Some("node".into());
    a.image = Some("sha256:image".into());
    let name = "control-test-attempt-1";
    let id = ContainerId::from("a".repeat(64));
    let observed = |node: &str| Observed {
        id: id.clone(),
        name: format!("/{name}"),
        image_ref: "sha256:image".into(),
        labels: [("dev.control.node", node), ("dev.control.attempt", name)]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into(),
        running: true,
        mounts: vec![ObservedMount {
            kind: MountKind::Bind,
            name: None,
            driver: None,
            source: expected(&a, name).unwrap().to_string_lossy().into_owned(),
            destination: "/workspace".into(),
            writable: true,
        }],
        ..Observed::default()
    };
    let listed = |name: &str| {
        Reply::Listed(vec![Listed {
            id: id.clone(),
            name: name.into(),
            labels: Default::default(),
        }])
    };
    let calls = std::sync::Mutex::new(Vec::new());
    let engine = Fake(|call: Call| {
        calls.lock().unwrap().push(call.clone());
        Ok(match call {
            Call::Inspect(_) => Reply::Observed(vec![observed("node")]),
            Call::List(..) => Reply::Listed(Vec::new()),
            _ => Reply::Done,
        })
    });
    assert!(stop_named_with(&engine, &a, name).is_ok());
    assert_eq!(calls.lock().unwrap()[1], Call::Kill(id.clone()));
    let failing = Fake(|call| match call {
        Call::Inspect(_) => Ok(Reply::Observed(vec![observed("node")])),
        _ => Err(std::io::Error::other("injected stop failure")),
    });
    assert!(stop_named_with(&failing, &a, name).is_err());
    let running = Fake(|call| {
        Ok(match call {
            Call::Inspect(_) => Reply::Observed(vec![observed("node")]),
            Call::List(..) => listed(name),
            _ => Reply::Done,
        })
    });
    assert!(stop_named_with(&running, &a, name).is_err());
    let killed = std::sync::atomic::AtomicBool::new(false);
    let foreign = Fake(|call| {
        killed.fetch_or(matches!(call, Call::Kill(_)), std::sync::atomic::Ordering::Relaxed);
        Ok(match call {
            Call::List(..) => Reply::Listed(Vec::new()),
            _ => Reply::Observed(vec![observed("other")]),
        })
    });
    assert!(stop_named_with(&foreign, &a, name).is_err());
    assert!(!killed.load(std::sync::atomic::Ordering::Relaxed));
    // --rm removed the worker between listing and inspection; a fresh all-container listing proves absence.
    let gone = Fake(|call| match call {
        Call::Inspect(_) => Err(std::io::Error::other("gone")),
        _ => Ok(listed("unrelated")),
    });
    assert!(stop_named_with(&gone, &a, name).is_ok());
    let uncertain = Fake(|call| match call {
        Call::Inspect(_) => Err(std::io::Error::other("uncertain")),
        _ => Ok(listed(name)),
    });
    assert!(stop_named_with(&uncertain, &a, name).is_err());
}

fn owner(generation: &str, active: bool, jobs: &[&str]) -> Owner {
    let mut attempt = Attempt::new();
    attempt.workspace_id = Some(generation.into());
    attempt.command_jobs = jobs.iter().map(|id| serde_json::from_value(serde_json::json!({"id":id,"generation":generation,"image":"i","node":"node",
        "command":"","read_only":false,"build_environment":false,"review_source":false,"deadline_unix":0,"container_id":null,
        "status":"running","output":"","exit_code":null})).unwrap()).collect();
    Owner {
        active,
        attempts: vec![attempt],
    }
}
#[test]
fn reconcile_matches_each_resource_shape_to_its_first_owner() {
    // `a-attempt-1` is a prefix of `a-attempt-10`; the latter's resources
    // must not be attributed to the former, and the earliest owner wins.
    let owners = [
        owner("a-attempt-1", true, &[]),
        owner("a-attempt-10", false, &["7"]),
        owner("a-attempt-10", true, &[]),
    ];
    let row = |name: &str| (name.to_owned(), "node".to_owned(), "run".to_owned());
    let mut stopped = Vec::new();
    reconcile_with(
        &owners,
        "node",
        [
            "control-a-attempt-1",
            "control-a-attempt-1-tool",
            "control-a-attempt-10-probe",
            "control-a-attempt-10-job-7",
            "control-a-attempt-10-tool",
        ]
        .map(row)
        .to_vec(),
        |attempt, name| {
            stopped.push((attempt.workspace_id.clone().unwrap(), name.to_owned()));
            Ok(())
        },
        |_, _| Ok(()),
    )
    .unwrap();
    assert_eq!(
        stopped,
        [
            ("a-attempt-10".to_owned(), "control-a-attempt-10-probe".to_owned()),
            ("a-attempt-10".into(), "control-a-attempt-10-job-7".into()),
            ("a-attempt-10".into(), "control-a-attempt-10-tool".into())
        ]
    );
    // A preview needs a project; an unknown job is not an owned resource.
    for name in ["control-a-attempt-1-preview", "control-a-attempt-10-job-8"] {
        assert!(
            reconcile_with(&owners, "node", vec![row(name)], |_, _| Ok(()), |_, _| Ok(()))
                .unwrap_err()
                .contains(name)
        );
    }
}

#[test]
#[ignore = "explicit synthetic release timing; no engine, credentials or models"]
fn reconcile_profile() {
    use std::time::Instant;
    for history in [500, 3_000, 12_000] {
        let owners = (0..history)
            .map(|n| owner(&format!("run-{n}-attempt-1"), n + 24 >= history, &[]))
            .collect::<Vec<_>>();
        // The newest attempts' containers, as during a live swarm or its cancellation.
        let listed = (history - 24..history)
            .flat_map(|n| {
                [
                    format!("control-run-{n}-attempt-1"),
                    format!("control-run-{n}-attempt-1-tool"),
                ]
            })
            .map(|name| (name, "node".to_owned(), "run".to_owned()))
            .collect::<Vec<_>>();
        let mut samples = (0..9)
            .map(|_| {
                let start = Instant::now();
                reconcile_with(
                    &owners,
                    "node",
                    std::hint::black_box(listed.clone()),
                    |_, _| Ok(()),
                    |_, _| Ok(()),
                )
                .unwrap();
                start.elapsed().as_micros()
            })
            .collect::<Vec<_>>();
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"workload":"lifecycle_reconcile","attempts":history,"containers":listed.len(),"median_us":samples[4],"min_us":samples[0],"max_us":samples[8]})
        );
    }
}
