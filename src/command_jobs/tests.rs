
#[test]
fn command_deadline_respects_owner_without_limiting_persistent_service_lifetime() {
    assert_eq!(super::effective_deadline(300, 600, Some(600)).unwrap(), 600);
    assert_eq!(super::effective_deadline(300, 600, None).unwrap(), 900);
    assert!(super::effective_deadline(600, 600, Some(600)).is_err());
}
use super::*;
use crate::tool_registry::{AttemptCall, AttemptTools, LocalHost};
fn call(
    shared: &Arc<Mutex<State>>,
    run: &str,
    generation: &str,
    nonce: &str,
    name: &str,
    args: &Value,
) -> io::Result<Value> {
    Tools.call(
        &AttemptCall {
            attempt: AttemptRef {
                run,
                generation,
                nonce,
            },
            host: &LocalHost(shared),
        },
        name,
        args,
    )
}
#[test]
fn output_is_bounded_utf8_and_strips_terminal_controls() {
    let output = bounded_output(&format!("\x1b{}\0", "é".repeat(20000)));
    assert!(output.len() <= OUTPUT_BYTES + 30);
    assert!(output.starts_with("[earlier output omitted]"));
    assert!(!output.contains(['\x1b', '\0']));
}
#[test]
fn unknown_command_options_cannot_set_mounts_or_images() {
    assert!(serde_json::from_value::<Input>(json!({"command":"true","image":"ubuntu"})).is_err());
    assert!(serde_json::from_value::<Input>(json!({"command":"true","mount":"/"})).is_err());
}
fn fixture() -> (State, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "fixture", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    let generation = format!("{}-attempt-1", run.id);
    run.attempts.push(Attempt {
        workspace_id: Some(generation.clone()),
        ..Default::default()
    });
    let id = run.id.clone();
    (state, id, generation)
}
#[test]
fn status_cannot_cross_attempt_or_job_boundaries() {
    let (state, run, generation) = fixture();
    let state = Arc::new(Mutex::new(state));
    assert!(
        call(
            &state,
            &run,
            "stale",
            "nonce",
            "control_workspace_status",
            &json!({"job_id":"anything"})
        )
        .unwrap_err()
        .to_string()
        .contains("inactive attempt")
    );
    assert!(
        call(
            &state,
            &run,
            &generation,
            "nonce",
            "control_workspace_status",
            &json!({"job_id":"anything"})
        )
        .unwrap_err()
        .to_string()
        .contains("Unknown job")
    );
    state.lock().unwrap().runtime.runs[0].status = "cancelled".into();
    assert!(
        call(
            &state,
            &run,
            &generation,
            "nonce",
            "control_workspace_status",
            &json!({"job_id":"anything"})
        )
        .unwrap_err()
        .to_string()
        .contains("not active")
    );
}
#[test]
fn checkpoint_barrier_fails_closed_for_unconfirmed_writer() {
    let (mut state, run, generation) = fixture();
    state.runtime.runs[0].attempts[0].command_jobs.push(Job {
        id: "fixture".into(),
        generation: generation.clone(),
        image: "fixture".into(),
        node: "fixture".into(),
        command: "sleep".into(),
        read_only: false,
        build_environment: false,
        review_source: false,
        profile: crate::worker_profile::Profile::Lite,
        deadline_unix: now() + 100,
        container_id: None,
        status: "preparing".into(),
        output: String::new(),
        exit_code: None,
    });
    let state = Arc::new(Mutex::new(state));
    assert!(
        quiesce_until(&state, &run, &generation, Instant::now())
            .unwrap_err()
            .to_string()
            .contains("checkpoint remains unchanged")
    );
    assert_eq!(
        state.lock().unwrap().runtime.runs[0].attempts[0].command_jobs[0].status,
        "stopping"
    );
}
#[test]
fn immutable_job_identity_and_isolation_are_checked_before_adoption() {
    let job = Job {
        id: "fixture".into(),
        generation: "generation".into(),
        image: "pinned-image".into(),
        node: "node".into(),
        command: "true".into(),
        read_only: true,
        build_environment: false,
        review_source: false,
        profile: crate::worker_profile::Profile::Lite,
        deadline_unix: 0,
        container_id: Some("b".repeat(64)),
        status: "running".into(),
        output: String::new(),
        exit_code: None,
    };
    let inspection = Inspection {
        id: "b".repeat(64),
        name: format!("/{}", job.name()),
        node: job.node.clone(),
        attempt: job.name(),
        image: job.image.clone(),
        source: "/fixture".into(),
        writable: false,
        network: "none".into(),
        readonly_root: true,
        running: true,
        exit_code: 0,
    };
    let expected = std::path::Path::new("/fixture");
    let mut retained = job.clone();
    retained.output = "diagnostic".repeat(2000);
    let identity = retained.identity();
    assert!(identity.command.is_empty() && identity.output.is_empty());
    assert_eq!(identity.command.capacity() + identity.output.capacity(), 0);
    assert!(verify_identity(&identity, &inspection, expected).is_ok());
    let mut changed = inspection.clone();
    changed.id = "a".repeat(64);
    assert!(verify_identity(&identity, &changed, expected).is_err());
    assert!(verify_identity(&job, &changed, expected).is_err());
    let mut changed = inspection.clone();
    changed.source = "/host".into();
    assert!(verify_identity(&job, &changed, expected).is_err());
    let mut changed = inspection.clone();
    changed.image = "other-image".into();
    assert!(verify_identity(&job, &changed, expected).is_err());
    let mut changed = inspection.clone();
    changed.writable = true;
    assert!(verify_identity(&job, &changed, expected).is_err());
    let mut changed = inspection.clone();
    changed.network = "bridge".into();
    assert!(verify_identity(&job, &changed, expected).is_err());
    let mut changed = inspection.clone();
    changed.readonly_root = false;
    assert!(verify_identity(&job, &changed, expected).is_err());
    assert!(serde_json::to_value(&job).unwrap().get("profile").is_none());
    let mut full = job.clone();
    full.profile = crate::worker_profile::Profile::Full;
    assert!(verify_identity(&full, &changed, expected).is_ok());
    assert!(verify_identity(&full, &inspection, expected).is_err());
    let restored: Job = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
    assert_eq!(
        restored.identity().profile,
        crate::worker_profile::Profile::Full
    );
    assert!(verify_identity(&restored, &changed, expected).is_ok());
}
#[test]
#[ignore = "requires isolated CONTROL_STATE_DIR/CONTROL_CONFIG_DIR under /private/tmp/control-command-qa and CONTROL_QA_IMAGE; no credentials/model calls"]
fn live_commands_stream_past_short_deadline_cancel_and_reconcile_exact_restart_identity() {
    let state_dir = crate::config::state_directory().unwrap();
    assert!(state_dir.parent().is_some_and(|p| {
        p.parent() == Some(std::path::Path::new("/private/tmp"))
            && p.file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with("control-command-qa."))
    }));
    crate::storage::initialize().unwrap();
    let image = std::env::var("CONTROL_QA_IMAGE").unwrap();
    assert!(crate::runtime::pinned_image(&image));
    let (mut state, run, generation) = fixture();
    let workspace = crate::runtime::workspace(&generation).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(workspace.parent().unwrap().join("baseline")).unwrap();
    let attempt = &mut state.runtime.runs[0].attempts[0];
    attempt.image = Some(image);
    attempt.node_id = Some(crate::config::node_id().unwrap());
    let state = Arc::new(Mutex::new(state));
    struct Cleanup(Arc<Mutex<State>>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let jobs = self
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .runtime
                .runs[0]
                .attempts
                .iter()
                .flat_map(|a| a.command_jobs.clone())
                .collect::<Vec<_>>();
            for job in jobs {
                let _ = stop(&job);
            }
        }
    }
    let _cleanup = Cleanup(state.clone());
    let command = "set -eu; test ! -e /home/agent/.codex/auth.json; test ! -e /home/agent/.claude/.credentials.json; test ! -S /var/run/docker.sock; test ! -S /nix/var/nix/daemon-socket/socket; test -z \"${OPENAI_API_KEY-}${ANTHROPIC_API_KEY-}\"; if touch /nix/store/forbidden-write 2>/dev/null; then exit 42; fi; if timeout 1 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then exit 43; fi; printf 'early-output\n'; sleep 27; printf late-output; printf verified > result";
    let input = Input {
        command: command.into(),
        timeout_seconds: 60,
        environment: "worker".into(),
    };
    let initial = start(&state, &run, &generation, "long-fixture", input).unwrap();
    let id = initial["job_id"].as_str().unwrap();
    let until = Instant::now() + Duration::from_secs(45);
    let mut saw_live = false;
    loop {
        let value = call(
            &state,
            &run,
            &generation,
            "status",
            "control_workspace_status",
            &json!({"job_id":id}),
        )
        .unwrap();
        if value["status"] == "running"
            && value["output"].as_str().unwrap().contains("early-output")
        {
            saw_live = true;
            let saw_activity = state.lock().unwrap().runtime.runs[0].attempts[0]
                .activity
                .iter()
                .any(|a| a.output.contains("early-output"));
            assert!(saw_activity);
        }
        if value["status"] == "completed" {
            assert_eq!(value["exit_code"], 0);
            assert!(value["output"].as_str().unwrap().contains("late-output"));
            break;
        }
        assert!(Instant::now() < until, "command did not finish: {value}");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(saw_live);
    assert_eq!(fs::read(workspace.join("result")).unwrap(), b"verified");
    let command = "printf cancelling; sleep 100; printf bad > after-cancel";
    let handle = start(
        &state,
        &run,
        &generation,
        "cancel-fixture",
        Input {
            command: command.into(),
            timeout_seconds: 180,
            environment: "worker".into(),
        },
    )
    .unwrap();
    assert!(
        start(
            &state,
            &run,
            &generation,
            "other-writer",
            Input {
                command: "true".into(),
                timeout_seconds: 10,
                environment: "worker".into()
            }
        )
        .is_err()
    );
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let value = call(
            &state,
            &run,
            &generation,
            "status",
            "control_workspace_status",
            &json!({"job_id":handle["job_id"]}),
        )
        .unwrap();
        if value["status"] == "running" {
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(50));
    }
    call(
        &state,
        &run,
        &generation,
        "cancel",
        "control_workspace_cancel",
        &json!({"job_id":handle["job_id"]}),
    )
    .unwrap();
    quiesce(&state, &run, &generation).unwrap();
    assert!(!workspace.join("after-cancel").exists());
    let handle = start(
        &state,
        &run,
        &generation,
        "deadline-fixture",
        Input {
            command: "printf 'deadline-output\n'; sleep 100".into(),
            timeout_seconds: 1,
            environment: "worker".into(),
        },
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let value = call(
            &state,
            &run,
            &generation,
            "status",
            "control_workspace_status",
            &json!({"job_id":handle["job_id"]}),
        )
        .unwrap();
        if value["status"] == "timed_out" {
            break;
        }
        assert!(Instant::now() < until, "deadline was not enforced: {value}");
        std::thread::sleep(Duration::from_millis(50));
    }
    {
        let mut locked = state.lock().unwrap();
        let attempt = &mut locked.runtime.runs[0].attempts[0];
        attempt.review_only = true;
        attempt.backend = crate::backend::Backend::Claude;
    }
    let handle=start(&state,&run,&generation,"readonly-fixture",Input{command:"set -eu; if touch /workspace/forbidden-judge-write 2>/dev/null; then exit 42; fi; test \"$CARGO_TARGET_DIR\" = /build-target; printf scratch > /build-target/allowed-judge-write; printf 'readonly-verified\n'".into(),timeout_seconds:10,environment:"worker".into()}).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let value = call(
            &state,
            &run,
            &generation,
            "status",
            "control_workspace_status",
            &json!({"job_id":handle["job_id"]}),
        )
        .unwrap();
        if value["status"] == "completed" {
            assert_eq!(value["exit_code"], 0);
            assert!(
                value["output"]
                    .as_str()
                    .unwrap()
                    .contains("readonly-verified")
            );
            break;
        }
        assert!(
            Instant::now() < until,
            "read-only verification failed: {value}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!workspace.join("forbidden-judge-write").exists());
    state.lock().unwrap().runtime.runs[0].attempts[0].review_only = false;
    let handle = start(
        &state,
        &run,
        &generation,
        "restart-fixture",
        Input {
            command: "printf restart-output; sleep 100".into(),
            timeout_seconds: 180,
            environment: "worker".into(),
        },
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let job = loop {
        let locked = state.lock().unwrap();
        let job = locked.runtime.runs[0].attempts[0]
            .command_jobs
            .iter()
            .find(|j| j.id == handle["job_id"])
            .unwrap()
            .clone();
        if job.container_id.is_some() {
            break job;
        }
        assert!(Instant::now() < until);
        drop(locked);
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut replacement = job.clone();
    replacement.container_id = Some("a".repeat(64));
    assert!(
        stop(&replacement)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    assert!(inspect(&job).unwrap().running);
    // Reconstruct owner records exactly as a restarted process would. Inactive attempts
    // retain their independent captured image and immutable container ID.
    let recovered: State =
        serde_json::from_slice(&serde_json::to_vec(&*state.lock().unwrap()).unwrap()).unwrap();
    let mut recovered = recovered;
    recovered.runtime.runs[0].status = "interrupted".into();
    crate::lifecycle::reconcile(
        &crate::lifecycle::owners(&recovered.runtime.runs),
        &recovered.services,
    )
    .unwrap();
    assert!(inspect(&job).is_err());
    // Let the original supervisor observe loss and release its final state fence.
    quiesce(&state, &run, &generation).unwrap();
    let handle = start(
        &state,
        &run,
        &generation,
        "stale-fixture",
        Input {
            command: "printf 'stale-start\n'; sleep 100; touch after-stale".into(),
            timeout_seconds: 180,
            environment: "worker".into(),
        },
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let value = call(
            &state,
            &run,
            &generation,
            "status",
            "control_workspace_status",
            &json!({"job_id":handle["job_id"]}),
        )
        .unwrap();
        if value["status"] == "running" {
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(50));
    }
    state.lock().unwrap().runtime.runs[0]
        .attempts
        .push(Attempt {
            workspace_id: Some(format!("{generation}-retry")),
            ..Default::default()
        });
    assert!(
        call(
            &state,
            &run,
            &generation,
            "stale-status",
            "control_workspace_status",
            &json!({"job_id":handle["job_id"]})
        )
        .is_err()
    );
    quiesce(&state, &run, &generation).unwrap();
    let locked = state.lock().unwrap();
    assert!(locked.runtime.runs[0].attempts[1].activity.is_empty());
    assert_eq!(
        locked.runtime.runs[0].attempts[0]
            .command_jobs
            .last()
            .unwrap()
            .status,
        "cancelled"
    );
    drop(locked);
    assert!(!workspace.join("after-stale").exists());
}
