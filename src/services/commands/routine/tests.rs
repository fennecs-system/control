use super::*;

fn fixture() -> (State, String, NodeId, String) {
    let (mut state, run, _, time) = crate::services::tests::fixture();
    crate::services::tests::admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    state.services[0].resource.phase = Phase::Running("b".repeat(64).try_into().unwrap());
    let id = state.services[0].resource.id.as_str().to_owned();
    (state, run, "node-a".to_owned().try_into().unwrap(), id)
}
/// Makes `attempt` the run's live attempt, as a retry or continuation would.
fn next_attempt(state: &mut State, attempt: &str) {
    let mut next = state.runtime.runs[0].attempts.last().unwrap().clone();
    next.workspace_id = Some(attempt.into());
    state.runtime.runs[0].attempts.push(next);
}
/// Admits command `n` of an attempt and returns its result as an exec would.
fn exec(state: &mut State, run: &str, attempt: &str, node: &NodeId, service: &str, n: usize) -> Value {
    let input = Input {
        service_id: service.into(),
        command: format!("echo {n}").try_into().unwrap(),
        timeout_seconds: 60,
        wait_seconds: 0,
    };
    let mut value = admit(
        state,
        run,
        attempt,
        &format!("nonce-{attempt}-{n}"),
        node,
        input,
        |_| Ok(()),
    )
    .unwrap();
    hint(state, run, attempt, service, &mut value);
    value
}
fn hinted(value: &Value) -> bool {
    value.get("hint").and_then(Value::as_str) == Some(HINT)
}

#[test]
fn hint_appears_once_at_the_threshold_per_attempt_and_service() {
    let (mut state, run, node, id) = fixture();
    let mut shown: Vec<_> = (1..=HINT_AFTER)
        .map(|n| hinted(&exec(&mut state, &run, "attempt-a", &node, &id, n)))
        .collect();
    // A retry of the threshold request returns the recorded result and the same advice.
    assert!(hinted(&exec(&mut state, &run, "attempt-a", &node, &id, HINT_AFTER)));
    shown
        .extend((HINT_AFTER + 1..=HINT_AFTER + 3).map(|n| hinted(&exec(&mut state, &run, "attempt-a", &node, &id, n))));
    assert_eq!(shown.iter().filter(|h| **h).count(), 1);
    assert!(shown[HINT_AFTER - 1]);
    // A later attempt counts only its own commands.
    next_attempt(&mut state, "attempt-b");
    let other: Vec<_> = (1..=HINT_AFTER)
        .map(|n| hinted(&exec(&mut state, &run, "attempt-b", &node, &id, n)))
        .collect();
    assert_eq!(other.iter().filter(|h| **h).count(), 1);
    assert!(other[HINT_AFTER - 1]);
}

#[test]
fn a_running_routine_on_the_service_suppresses_the_hint() {
    let (mut state, run, node, id) = fixture();
    let watch: crate::watches::Watch = serde_json::from_value(json!({
        "id":"w-1","name":"routine","chat":"chat","service":id,"generation":1,"permanent":true,"command":"exec /workspace/routine","stopped":false
    }))
    .unwrap();
    state.watches.push(watch);
    assert!(!(1..=HINT_AFTER + 1).any(|n| hinted(&exec(&mut state, &run, "attempt-a", &node, &id, n))));
    // A stopped routine no longer suppresses it.
    state.watches[0].stopped = true;
    next_attempt(&mut state, "attempt-b");
    assert!(!(1..HINT_AFTER).any(|n| hinted(&exec(&mut state, &run, "attempt-b", &node, &id, n))));
    assert!(hinted(&exec(&mut state, &run, "attempt-b", &node, &id, HINT_AFTER)));
}

#[test]
fn an_inline_result_keeps_the_hint() {
    let (mut state, run, node, id) = fixture();
    state.runtime.runs[0].attempts.last_mut().unwrap().started_unix = now();
    let admitted = (1..=HINT_AFTER)
        .map(|n| exec(&mut state, &run, "attempt-a", &node, &id, n))
        .last()
        .unwrap();
    assert!(hinted(&admitted));
    let snapshot = state.services[0].clone();
    let shared = crate::store::Store::new(state);
    let execution = admitted["execution_id"].as_str().unwrap().to_owned();
    update(&shared, &snapshot, &execution, |e| e.status = Status::Running, false).unwrap();
    update(
        &shared,
        &snapshot,
        &execution,
        |e| {
            e.status = Status::Completed;
            e.output = "done".into();
            e.exit_code = Some(0);
        },
        false,
    )
    .unwrap();
    let waiter = Waiter {
        run: &run,
        generation: "attempt-a",
        node: &node,
        service_id: &id,
    };
    let inline = settle(
        &shared,
        waiter,
        &execution,
        admitted,
        Duration::from_secs(5),
        &CancelToken::new(),
    );
    assert_eq!(
        (inline["status"].as_str(), inline["output"].as_str()),
        (Some("completed"), Some("done"))
    );
    assert!(hinted(&inline));
}

#[test]
fn hint_is_absent_below_the_threshold_and_for_unknown_services() {
    let (mut state, run, node, id) = fixture();
    assert!(!(1..HINT_AFTER).any(|n| hinted(&exec(&mut state, &run, "attempt-a", &node, &id, n))));
    let mut value = json!({});
    hint(&state, &run, "attempt-a", "svc-missing", &mut value);
    assert!(value.get("hint").is_none());
}
