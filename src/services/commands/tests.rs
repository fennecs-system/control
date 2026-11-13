
use super::*;
#[test]
fn live_command_evidence_reaches_parent_and_terminal_clients_without_reordering() {
    let (mut state, run, node, service_id) = fixture();
    let admitted = admit(
        &mut state,
        &run,
        "attempt-a",
        "visible-command",
        &node,
        input(&service_id, "printf '<progress>'"),
        |_| Ok(()),
    )
    .unwrap();
    let execution_id = admitted["execution_id"].as_str().unwrap();
    let parent = state.chats[0].id.clone();
    let child = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(parent.clone());
    state.runtime.runs[0].chat_id = Some(child.clone());
    let unrelated = crate::chat::create(&mut state);
    let before: Vec<_> = state
        .chats
        .iter()
        .map(|chat| (chat.revision, chat.activity_order))
        .collect();
    let service = state.services[0].clone();
    let shared = Arc::new(Mutex::new(state));
    update(
        &shared,
        &service,
        execution_id,
        |e| {
            e.status = Status::Running;
            e.output = "first <progress>\n".into();
        },
        false,
    )
    .unwrap();
    {
        let state = shared.lock().unwrap();
        assert!(state.chats[0].revision > before[0].0);
        assert!(state.chats[1].revision > before[1].0);
        assert_eq!(state.chats[2].revision, before[2].0);
        for (chat, (_, order)) in state.chats.iter().zip(&before) {
            assert_eq!(chat.activity_order, *order);
        }
        let detail = crate::chat::detail(&state, &parent).unwrap();
        assert_eq!(
            detail["agent_activity"][0]["activity"]["output"],
            "first <progress>\n"
        );
        let html = crate::chat::transcript(&state, &parent).unwrap();
        assert!(html.contains("first &lt;progress&gt;"));
        assert!(
            html.contains("printf &#39;&lt;progress&gt;&#39;")
                || html.contains("printf '&lt;progress&gt;'")
        );
        assert!(
            !crate::chat::transcript(&state, &unrelated)
                .unwrap()
                .contains("progress")
        );
        assert!(
            crate::chat::detail(&state, &unrelated).unwrap()["agent_activity"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    update(
        &shared,
        &service,
        execution_id,
        |e| {
            e.status = Status::Completed;
            e.output.push_str("second\n");
            e.exit_code = Some(0);
            e.command.clear();
        },
        false,
    )
    .unwrap();
    let state = shared.lock().unwrap();
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&*state).unwrap()).unwrap();
    let event = activity(&restored, &run).next().unwrap();
    assert_eq!(event.text, "printf '<progress>'");
    assert_eq!(event.status, "completed");
    assert!(event.output.contains("second\n\nExit 0"));
    assert!(activity(&restored, "other-run").next().is_none());
    let mut isolated = restored;
    isolated.chats[1].actor = Actor::Unknown;
    assert!(
        crate::chat::detail(&isolated, &parent).unwrap()["agent_activity"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let revision = isolated.chats[0].revision;
    crate::chat::activity_changed(&mut isolated, &child);
    assert_eq!(isolated.chats[0].revision, revision);
}

#[test]
fn unchanged_samples_do_not_refresh_chat_and_parent_cycles_terminate() {
    let (mut state, run, node, id) = fixture();
    let result = admit(
        &mut state,
        &run,
        "attempt-a",
        "sample",
        &node,
        input(&id, "echo hi"),
        |_| Ok(()),
    )
    .unwrap();
    let service = state.services[0].clone();
    let chat = state.chats[0].id.clone();
    state.chats[0].parent_chat = Some(chat);
    let revision = state.chats[0].revision;
    let shared = Arc::new(Mutex::new(state));
    update(
        &shared,
        &service,
        result["execution_id"].as_str().unwrap(),
        |_| {},
        false,
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().chats[0].revision, revision);
    update(
        &shared,
        &service,
        result["execution_id"].as_str().unwrap(),
        |e| e.status = Status::Running,
        false,
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().chats[0].revision, revision + 1);
}
fn fixture() -> (State, String, NodeId, String) {
    let (mut state, run, _, time) = super::super::tests::fixture();
    super::super::tests::admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    state.services[0].resource.phase = Phase::Running("b".repeat(64).try_into().unwrap());
    let id = state.services[0].resource.id.as_str().to_owned();
    (state, run, "node-a".to_owned().try_into().unwrap(), id)
}
#[test]
fn output_eviction_keeps_active_attempt_retry_receipts_and_rolls_back() {
    let (mut state, run, node, id) = fixture();
    for n in 0..MAX_EXECUTIONS + 8 {
        let nonce = format!("command-{n}");
        let before = serde_json::to_vec(&state).unwrap();
        if n == MAX_EXECUTIONS {
            assert!(
                admit(
                    &mut state,
                    &run,
                    "attempt-a",
                    &nonce,
                    &node,
                    input(&id, "echo test"),
                    |_| Err(fail("disk full"))
                )
                .is_err()
            );
            assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        }
        admit(
            &mut state,
            &run,
            "attempt-a",
            &nonce,
            &node,
            input(&id, "echo test"),
            |_| Ok(()),
        )
        .unwrap();
        let execution = state.services[0].executions.last_mut().unwrap();
        execution.status = Status::Completed;
        execution.exit_code = Some(0);
        execution.output = "retained output".into();
    }
    assert_eq!(state.services[0].executions.len(), MAX_EXECUTIONS);
    assert_eq!(state.services[0].command_receipts.len(), 8);
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let receipt = admit(
        &mut restored,
        &run,
        "attempt-a",
        "command-0",
        &node,
        input(&id, "echo test"),
        |_| panic!("Eviction must not replay effects"),
    )
    .unwrap();
    assert_eq!(receipt["output_evicted"], true);
    assert_eq!(receipt["status"], "completed");
    assert_eq!(receipt["exit_code"], 0);
    assert!(
        admit(
            &mut restored,
            &run,
            "attempt-a",
            "command-0",
            &node,
            input(&id, "different command"),
            |_| panic!("Changed identity must fail")
        )
        .is_err()
    );
}
#[test]
fn receipt_pruning_never_discards_live_attempt_identity() {
    let (mut state, run, node, id) = fixture();
    for n in 0..MAX_EXECUTIONS + 1 {
        admit(
            &mut state,
            &run,
            "attempt-a",
            &format!("command-{n}"),
            &node,
            input(&id, "true"),
            |_| Ok(()),
        )
        .unwrap();
        state.services[0].executions.last_mut().unwrap().status = Status::Completed;
    }
    let receipt = state.services[0].command_receipts[0].clone();
    state.services[0].command_receipts = vec![receipt; MAX_RECEIPTS];
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "next",
            &node,
            input(&id, "true"),
            |_| panic!("Full live receipt budget must fail before effects")
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    for receipt in &mut state.services[0].command_receipts {
        receipt.run = "finished-run".into();
    }
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "next",
            &node,
            input(&id, "true"),
            |_| Err(fail("disk full"))
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    admit(
        &mut state,
        &run,
        "attempt-a",
        "next",
        &node,
        input(&id, "true"),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.services[0].command_receipts.len(), 1);
    assert_eq!(state.services[0].command_receipts[0].run, run);
}
fn input(service: &str, command: &str) -> Input {
    Input {
        service_id: service.into(),
        command: command.to_owned().try_into().unwrap(),
        timeout_seconds: 60,
    }
}
#[test]
fn admission_is_durable_scoped_idempotent_and_single_writer() {
    let (mut state, run, node, id) = fixture();
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "nonce-a",
            &node,
            input(&id, "printf hello"),
            |_| Err(fail("disk full"))
        )
        .is_err()
    );
    assert!(state.services[0].executions.is_empty());
    let receipt = admit(
        &mut state,
        &run,
        "attempt-a",
        "nonce-a",
        &node,
        input(&id, "printf hello"),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        receipt,
        admit(
            &mut state,
            &run,
            "attempt-a",
            "nonce-a",
            &node,
            input(&id, "printf hello"),
            |_| panic!("Replay must not save")
        )
        .unwrap()
    );
    for (generation, nonce, command) in [
        ("attempt-old", "nonce-a", "printf hello"),
        ("attempt-a", "nonce-a", "printf different"),
        ("attempt-a", "nonce-b", "printf hello"),
    ] {
        assert!(
            admit(
                &mut state,
                &run,
                generation,
                nonce,
                &node,
                input(&id, command),
                |_| panic!("Invalid request must not persist")
            )
            .is_err()
        );
    }
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "nonce-b",
            &"foreign-node".to_owned().try_into().unwrap(),
            input(&id, "true"),
            |_| Ok(())
        )
        .is_err()
    );
    state.services[0].executions[0].status = Status::Completed;
    state.runtime.runs[0].actor = Actor::Unknown;
    assert!(
        admit(
            &mut state,
            &run,
            "attempt-a",
            "nonce-b",
            &node,
            input(&id, "true"),
            |_| Ok(())
        )
        .is_err()
    );
}
#[test]
fn restart_never_replays_uncertain_execution_and_save_failure_rolls_back() {
    let (mut state, run, node, id) = fixture();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "nonce-a",
        &node,
        input(&id, "printf hello"),
        |_| Ok(()),
    )
    .unwrap();
    state.services[0].executions[0].status = Status::Running;
    let shared = Arc::new(Mutex::new(state));
    let before = serde_json::to_vec(&*shared.lock().unwrap()).unwrap();
    assert!(recover(&shared, |_| Err(fail("disk full"))).is_err());
    assert_eq!(
        serde_json::to_vec(&*shared.lock().unwrap()).unwrap(),
        before
    );
    recover(&shared, |_| Ok(())).unwrap();
    let restored: State =
        serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    assert!(matches!(
        restored.services[0].resource.phase,
        Phase::Stopping { .. }
    ));
    assert_eq!(
        restored.services[0].executions[0].status,
        Status::Interrupted
    );
    assert!(restored.services[0].executions[0].command.is_empty());
    recover(&shared, |_| panic!("Recovery must not repeat")).unwrap();
}
#[test]
fn capture_drains_large_output_without_unbounded_retention() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'early\\n'; head -c 100000 /dev/zero; exit 7"]);
    let result = capture(command, Duration::from_secs(5), || false, |_| {}).unwrap();
    assert_eq!(result.exit_code, Some(7));
    assert!(result.output.truncated);
    assert_eq!(result.output.bytes.len(), OUTPUT);
    assert!(result.output.text().starts_with("early\n"));
    let mut command = Command::new("sh");
    command.args(["-c", "exec sleep 10"]);
    let start = Instant::now();
    let result = capture(command, Duration::from_millis(50), || false, |_| {}).unwrap();
    assert!(result.timed_out);
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "reads /proc")]
fn a_command_that_outlives_its_client_is_reaped_with_detached_descendants() {
    let id = format!("{:064x}", std::process::id());
    let other = format!("{:064x}", u64::from(std::process::id()) + 1);
    // Like a killed engine client: the launcher dies, the command does not.
    let mut client = Command::new("bash")
        .args(["-c", &launch("setsid sleep 30 & sleep 30")])
        .env(MARKER, &id)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut bystander = Command::new("bash")
        .args(["-c", &launch("sleep 30")])
        .env(MARKER, &other)
        .spawn()
        .unwrap();
    let members = |id: &str| {
        Command::new("bash")
            .args([
                "-c",
                &format!(
                    "{}\nmembers | wc -l",
                    reap_script(id)
                        .lines()
                        .take(2)
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ])
            .output()
            .unwrap()
    };
    let count = |id: &str| {
        String::from_utf8_lossy(&members(id).stdout)
            .trim()
            .parse::<usize>()
            .unwrap()
    };
    let start = Instant::now();
    while count(&id) < 2 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    client.kill().unwrap();
    client.wait().unwrap();
    assert!(
        count(&id) >= 2,
        "killing the client leaves the command and its detached child running"
    );
    assert!(
        Command::new("bash")
            .args(["-c", &reap_script(&id)])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        count(&id),
        0,
        "every marked process is gone, setsid included"
    );
    assert_eq!(count(&other), 1, "another execution is untouched");
    bystander.kill().unwrap();
    let _ = bystander.wait();
    assert!(
        Command::new("bash")
            .args(["-c", &reap_script(&other)])
            .status()
            .unwrap()
            .success()
    );
    let finished = Command::new("bash")
        .args(["-c", &launch("exit 3")])
        .env(MARKER, &id)
        .status()
        .unwrap();
    assert_eq!(finished.code(), Some(3), "the exit status passes through");
}
#[test]
fn shutdown_interrupts_capture_without_spinning_or_waiting_for_the_deadline() {
    let mut command = Command::new("sh");
    command.args(["-c", "exec sleep 30"]);
    let shutdown = CancelToken::new();
    shutdown.cancel();
    let samples = std::sync::atomic::AtomicUsize::new(0);
    let start = Instant::now();
    let result = capture_with(
        command,
        Duration::from_secs(20),
        || false,
        |_| {
            samples.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        },
        &shutdown,
    )
    .unwrap();
    assert!(result.interrupted && !result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(samples.load(std::sync::atomic::Ordering::SeqCst), 0);
    let mut command = Command::new("sh");
    command.args(["-c", "exit 3"]);
    let result = capture_with(
        command,
        Duration::from_secs(5),
        || false,
        |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(
        (result.exit_code, result.interrupted, result.timed_out),
        (Some(3), false, false)
    );
}
#[test]
fn capture_publishes_output_before_command_exit() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "printf 'early output\\n'; sleep 1; printf 'final output\\n'",
    ]);
    let samples = Mutex::new(Vec::new());
    let result = capture(
        command,
        Duration::from_secs(5),
        || false,
        |output| samples.lock().unwrap().push(output.text()),
    )
    .unwrap();
    assert!(
        samples
            .into_inner()
            .unwrap()
            .iter()
            .any(|text| text.contains("early output") && !text.contains("final output"))
    );
    assert!(result.output.text().contains("final output"));
    assert_eq!(result.exit_code, Some(0));
}
