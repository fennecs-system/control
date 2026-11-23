//! Inline waits and signal-driven dispatch. Completion is simulated with
//! unsaved updates; nothing here writes node state or calls an engine.
use super::*;
use crate::runtime::RunStatus;
use std::sync::mpsc;

struct Fixture {
    shared: Arc<Mutex<State>>,
    run: String,
    node: NodeId,
    service: String,
    snapshot: Service,
}
fn waiting_fixture() -> Fixture {
    let (mut state, run, node, service) = fixture();
    // The attempt started now, so its default turn deadline is minutes away.
    state.runtime.runs[0].attempts.last_mut().unwrap().started_unix = now();
    let snapshot = state.services[0].clone();
    Fixture {
        shared: Arc::new(Mutex::new(state)),
        run,
        node,
        service,
        snapshot,
    }
}
impl Fixture {
    fn submit(&self, nonce: &str, command: &str) -> Value {
        admit(
            &mut self.shared.lock().unwrap(),
            &self.run,
            "attempt-a",
            nonce,
            &self.node,
            input(&self.service, command),
            |_| Ok(()),
        )
        .unwrap()
    }
    fn wait(&self, admitted: Value, wait: Duration, token: &CancelToken) -> Value {
        let id = admitted["execution_id"].as_str().unwrap().to_owned();
        settle(
            &self.shared,
            Waiter {
                run: &self.run,
                generation: "attempt-a",
                node: &self.node,
                service_id: &self.service,
            },
            &id,
            admitted,
            wait,
            token,
        )
    }
    fn finish(&self, id: &str, output: &str) {
        update(&self.shared, &self.snapshot, id, |e| e.status = Status::Running, false).unwrap();
        update(
            &self.shared,
            &self.snapshot,
            id,
            |e| {
                e.status = Status::Completed;
                e.output = output.into();
                e.exit_code = Some(0);
            },
            false,
        )
        .unwrap();
    }
}
fn id(value: &Value) -> String {
    value["execution_id"].as_str().unwrap().to_owned()
}

#[test]
fn a_fast_command_returns_its_result_inline() {
    let f = waiting_fixture();
    let admitted = f.submit("fast", "echo ok");
    assert_eq!(admitted["status"], "requested");
    let started = Instant::now();
    let done = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| f.wait(admitted.clone(), Duration::from_secs(20), &CancelToken::new()));
        std::thread::sleep(Duration::from_millis(50));
        f.finish(&id(&admitted), "ok\n");
        waiter.join().unwrap()
    });
    // Completion wakes the waiter; it never sleeps out the wait or a recheck slice.
    assert!(started.elapsed() < RECHECK, "{:?}", started.elapsed());
    assert_eq!(
        (
            done["status"].as_str(),
            done["output"].as_str(),
            done["exit_code"].as_i64()
        ),
        (Some("completed"), Some("ok\n"), Some(0))
    );
    assert_eq!(done["execution_id"], admitted["execution_id"]);
    assert!(done.get("wait").is_none());
    // An inline finished result says so; no status call is needed.
    assert_eq!(done["final"], true);
}

#[test]
fn a_slow_command_falls_back_to_its_handle_and_zero_waits_return_at_once() {
    let f = waiting_fixture();
    let admitted = f.submit("slow", "sleep 600");
    let started = Instant::now();
    let pending = f.wait(admitted.clone(), Duration::from_millis(300), &CancelToken::new());
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert_eq!(pending["execution_id"], admitted["execution_id"]);
    assert_eq!(pending["status"], "requested");
    assert!(
        pending["wait"]
            .as_str()
            .unwrap()
            .contains("control_service_exec_status")
    );
    let started = Instant::now();
    let immediate = f.wait(admitted.clone(), Duration::ZERO, &CancelToken::new());
    assert!(started.elapsed() < Duration::from_millis(50));
    // Still running: the handle names the exact status call; it is not final.
    assert_eq!(
        (&immediate["execution_id"], &immediate["status"]),
        (&admitted["execution_id"], &admitted["status"])
    );
    assert!(immediate["wait"].as_str().unwrap().contains(&format!(
        "\"execution_id\":\"{}\"",
        admitted["execution_id"].as_str().unwrap()
    )));
    assert!(immediate.get("final").is_none());
    // Bounds are checked before any submission.
    assert!(wait_bound(MAX_WAIT_SECONDS).is_ok());
    assert!(
        wait_bound(MAX_WAIT_SECONDS + 1)
            .unwrap_err()
            .to_string()
            .contains("wait_seconds")
    );
    let request: Input =
        serde_json::from_value(json!({"service_id":f.service,"command":"true","wait_seconds":31})).unwrap();
    let before = f.shared.lock().unwrap().services[0].executions.len();
    assert!(exec(&f.shared, &f.run, "attempt-a", "too-long", &f.node, request).is_err());
    assert_eq!(f.shared.lock().unwrap().services[0].executions.len(), before);
    let default: Input = serde_json::from_value(json!({"service_id":f.service,"command":"true"})).unwrap();
    assert_eq!(default.wait_seconds, DEFAULT_WAIT_SECONDS);
}

#[test]
fn shutdown_run_cancellation_and_the_turn_deadline_end_a_wait() {
    let f = waiting_fixture();
    let admitted = f.submit("cancel", "sleep 600");
    // Shutdown wakes the waiter at once.
    let token = CancelToken::new();
    let started = Instant::now();
    let value = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| f.wait(admitted.clone(), Duration::from_secs(20), &token));
        std::thread::sleep(Duration::from_millis(50));
        token.cancel();
        waiter.join().unwrap()
    });
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(value["execution_id"], admitted["execution_id"]);

    // A run cancelled mid-wait is noticed within one recheck slice and keeps the handle.
    let started = Instant::now();
    let value = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| f.wait(admitted.clone(), Duration::from_secs(20), &CancelToken::new()));
        std::thread::sleep(Duration::from_millis(50));
        f.shared.lock().unwrap().runtime.runs[0].status = RunStatus::Cancelling;
        waiter.join().unwrap()
    });
    assert!(started.elapsed() < RECHECK * 3, "{:?}", started.elapsed());
    assert_eq!(
        (value["execution_id"].clone(), value["status"].as_str()),
        (admitted["execution_id"].clone(), Some("requested"))
    );
    f.shared.lock().unwrap().runtime.runs[0].status = RunStatus::Running;

    // A wait never outlasts the attempt's deadline (less a reply margin).
    {
        let mut state = f.shared.lock().unwrap();
        let attempt = state.runtime.runs[0].attempts.last_mut().unwrap();
        attempt.explicit_timeout_seconds = Some(60);
        attempt.started_unix = now() - 60 + DEADLINE_MARGIN + 1;
    }
    let started = Instant::now();
    let value = f.wait(admitted.clone(), Duration::from_secs(20), &CancelToken::new());
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert!(value["wait"].is_string());
}

#[test]
fn retries_during_and_after_a_wait_return_the_same_execution() {
    let f = waiting_fixture();
    let first = f.submit("retry", "echo once");
    let (first_done, retry_done) = std::thread::scope(|scope| {
        let a = scope.spawn(|| f.wait(first.clone(), Duration::from_secs(20), &CancelToken::new()));
        std::thread::sleep(Duration::from_millis(50));
        // The retry arrives while the first call still waits: same identity, no new effect.
        let retry = f.submit("retry", "echo once");
        assert_eq!(retry["execution_id"], first["execution_id"]);
        assert_eq!(f.shared.lock().unwrap().services[0].executions.len(), 1);
        let b = scope.spawn(|| f.wait(retry, Duration::from_secs(20), &CancelToken::new()));
        std::thread::sleep(Duration::from_millis(50));
        f.finish(&id(&first), "once\n");
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(first_done, retry_done);
    assert_eq!(
        (first_done["status"].as_str(), first_done["output"].as_str()),
        (Some("completed"), Some("once\n"))
    );
    // After completion a retry returns the recorded outcome without waiting.
    let started = Instant::now();
    let late = f.submit("retry", "echo once");
    assert_eq!(f.wait(late, Duration::from_secs(20), &CancelToken::new()), first_done);
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(f.shared.lock().unwrap().services[0].executions.len(), 1);
    // A wait never makes a reused identity with other arguments acceptable.
    assert!(
        admit(
            &mut f.shared.lock().unwrap(),
            &f.run,
            "attempt-a",
            "retry",
            &f.node,
            input(&f.service, "echo twice"),
            |_| Ok(())
        )
        .is_err()
    );
}

#[test]
fn many_waiters_each_receive_their_own_result_in_completion_order() {
    let f = waiting_fixture();
    let admitted = (0..8)
        .map(|n| f.submit(&format!("player-{n}"), &format!("act {n}")))
        .collect::<Vec<_>>();
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for (n, value) in admitted.iter().enumerate() {
            // Two callers wait on each command.
            for _ in 0..2 {
                let sender = sender.clone();
                let f = &f;
                scope.spawn(move || {
                    let done = f.wait(value.clone(), Duration::from_secs(20), &CancelToken::new());
                    sender.send((n, done, Instant::now())).unwrap();
                });
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        for n in (0..8).rev() {
            let finished = Instant::now();
            f.finish(&id(&admitted[n]), &format!("result {n}\n"));
            for _ in 0..2 {
                let (got, done, at) = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(got, n, "only the finished command's waiters wake with a result");
                assert_eq!(done["output"], format!("result {n}\n"));
                assert_eq!(done["execution_id"], admitted[n]["execution_id"]);
                assert!(at.duration_since(finished) < Duration::from_millis(500));
            }
        }
    });
}

#[test]
fn admission_wakes_the_dispatcher_without_waiting_for_the_tick() {
    let signal = Changes::new();
    let token = CancelToken::new();
    let (sender, receiver) = mpsc::channel();
    let reconciles = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            super::super::super::serve(
                &signal,
                Duration::from_secs(30),
                &token,
                || {
                    reconciles.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
                |_: &mut ()| sender.send(Instant::now()).unwrap(),
                &mut (),
            );
        });
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..3 {
            let admitted = Instant::now();
            signal.bump();
            let dispatched = receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("dispatch before the 30 s tick");
            assert!(dispatched.duration_since(admitted) < Duration::from_millis(200));
        }
        // Reconciliation stays on its tick; wakes run dispatch alone.
        assert_eq!(reconciles.load(std::sync::atomic::Ordering::SeqCst), 1);
        token.cancel();
    });
    // Admission and status transitions bump the shared signal.
    let f = waiting_fixture();
    let before = changes().generation();
    let admitted = f.submit("signal", "true");
    assert!(changes().generation() > before);
    let before = changes().generation();
    update(
        &f.shared,
        &f.snapshot,
        &id(&admitted),
        |e| e.output = "progress".into(),
        false,
    )
    .unwrap();
    update(
        &f.shared,
        &f.snapshot,
        &id(&admitted),
        |e| e.status = Status::Running,
        false,
    )
    .unwrap();
    assert!(changes().generation() > before);
}
