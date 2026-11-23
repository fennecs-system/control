//! Submit-to-start and submit-to-result latency of service commands against a
//! real engine, with the supervisor dispatching on its 2 s tick only (before)
//! and woken by admission with an inline wait (after). Its own fixture
//! container and isolated state; no models or credentials.
use super::*;
use std::path::PathBuf;

fn percentile(samples: &mut [f64], p: f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[((samples.len() - 1) as f64 * p).round() as usize]
}

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE, an isolated temporary CONTROL_STATE_DIR and a local Linux engine; no credentials/models"]
fn live_exec_latency_before_and_after_wake_and_inline_wait() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let rounds = std::env::var("CONTROL_QA_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(2, 500);
    let state_dir = PathBuf::from(std::env::var("CONTROL_STATE_DIR").expect("set an isolated CONTROL_STATE_DIR"));
    assert!(
        state_dir.starts_with("/private/tmp")
            || state_dir.starts_with("/tmp")
            || state_dir.starts_with(std::env::temp_dir()),
        "CONTROL_STATE_DIR must be an isolated temporary directory"
    );
    std::fs::create_dir_all(&state_dir).unwrap();
    let profile = crate::worker_profile::Profile::inspect(crate::engine::isolation(), &image).unwrap();
    let (mut state, run, _, time) = super::super::super::tests::fixture();
    state.runtime.runs[0].attempts.last_mut().unwrap().started_unix = now();
    let node: NodeId = crate::test_support::unique("latency-qa").try_into().unwrap();
    super::super::super::admit_profile(
        &mut state,
        super::super::super::Request {
            run: &run,
            generation: "attempt-a",
            nonce: "latency-probe",
        },
        super::super::super::Input {
            command: "sleep 600".to_owned().try_into().unwrap(),
            lease_seconds: 600,
            storage_limit_mb: None,
            shape: None,
        },
        node.clone(),
        time,
        profile,
        |_| Ok(()),
    )
    .unwrap();
    let mut service = state.services.remove(0);
    service.resource.image = image.try_into().unwrap();
    service.resource.phase = Phase::Creating;
    struct Cleanup(Option<ContainerId>, Option<String>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(id) = &self.0 {
                let _ = crate::engine::query(&["rm", "-f", id.as_str()]);
            }
            if let Some(volume) = &self.1 {
                let _ = crate::engine::query(&["volume", "rm", volume]);
            }
        }
    }
    let mut cleanup = Cleanup(
        None,
        super::super::super::workspace::Workspace::of(&service).map(|w| w.name().to_owned()),
    );
    let id = create_container(crate::engine::isolation(), &service).unwrap();
    cleanup.0 = Some(id.clone());
    service.resource.phase = Phase::Starting(id.clone());
    let observations = inventory(crate::engine::isolation(), node.as_str()).unwrap();
    start_container(
        crate::engine::isolation(),
        &verify(&service, &observations[id.as_str()]).unwrap(),
    )
    .unwrap();
    service.resource.phase = Phase::Running(id.clone());
    state.services.push(service.clone());
    let shared = Arc::new(Mutex::new(state));
    let service_id = service.resource.id.as_str().to_owned();
    let waiter = Waiter {
        run: &run,
        generation: "attempt-a",
        node: &node,
        service_id: &service_id,
    };

    let mut report = Vec::new();
    for (mode, woken) in [("tick-only-poll", false), ("wake-inline-wait", true)] {
        // Before: nothing wakes the dispatcher between 2 s ticks.
        let idle = Changes::new();
        let signal = if woken { changes() } else { &idle };
        let token = CancelToken::new();
        let (mut start, mut result, mut passes) = (Vec::new(), Vec::new(), 0usize);
        std::thread::scope(|scope| {
            let dispatcher = scope.spawn(|| {
                let mut threads = Vec::new();
                let mut count = 0usize;
                super::super::super::serve(
                    signal,
                    super::super::super::TICK,
                    &token,
                    || {},
                    |threads: &mut Vec<_>| {
                        count += 1;
                        dispatch(&shared, threads).unwrap();
                    },
                    &mut threads,
                );
                threads.into_iter().for_each(|t| t.join().unwrap());
                count
            });
            // Let the first (empty) pass run, then submit spread over the tick
            // phase, like model turns that end at arbitrary times.
            std::thread::sleep(Duration::from_millis(100));
            for n in 0..rounds {
                std::thread::sleep(Duration::from_millis((n as u64 * 733) % 2000));
                let nonce = format!("{mode}-{n}");
                let submitted = Instant::now();
                let admitted = admit(
                    &mut shared.lock().unwrap(),
                    &run,
                    "attempt-a",
                    &nonce,
                    &node,
                    Input {
                        service_id: service_id.clone(),
                        command: "echo ok".to_owned().try_into().unwrap(),
                        timeout_seconds: 30,
                        wait_seconds: 0,
                    },
                    crate::storage::save,
                )
                .unwrap();
                let execution = admitted["execution_id"].as_str().unwrap().to_owned();
                // Observe the launch through the same signal the waiters use.
                let observer = scope.spawn({
                    let (shared, execution) = (&shared, execution.clone());
                    move || {
                        let mut started = None;
                        loop {
                            let seen = changes().generation();
                            let status = shared.lock().unwrap().services[0]
                                .executions
                                .iter()
                                .find(|e| e.id == execution)
                                .map(|e| e.status)
                                .unwrap();
                            if started.is_none() && status != Status::Requested {
                                started = Some(submitted.elapsed());
                            }
                            if !status.active() {
                                return (started.unwrap(), submitted.elapsed());
                            }
                            changes().wait_newer(
                                seen,
                                Some(Instant::now() + Duration::from_millis(100)),
                                &CancelToken::new(),
                            );
                        }
                    }
                });
                // After: the exec call itself returns the result. Before, the
                // earliest a status call could see it is when it turns terminal.
                let inline = woken.then(|| {
                    let done = settle(
                        &shared,
                        waiter,
                        &execution,
                        admitted,
                        Duration::from_secs(20),
                        &CancelToken::new(),
                    );
                    assert_eq!(
                        (done["status"].as_str(), done["output"].as_str()),
                        (Some("completed"), Some("ok\n")),
                        "{done}"
                    );
                    submitted.elapsed()
                });
                let (started, terminal) = observer.join().unwrap();
                start.push(started.as_secs_f64() * 1000.0);
                result.push(inline.unwrap_or(terminal).as_secs_f64() * 1000.0);
            }
            token.cancel();
            passes = dispatcher.join().unwrap();
        });
        let row = json!({"mode":mode,"rounds":rounds,"dispatch_passes":passes,
            "submit_to_start_ms":{"p50":percentile(&mut start, 0.5),"p95":percentile(&mut start, 0.95)},
            "submit_to_result_ms":{"p50":percentile(&mut result, 0.5),"p95":percentile(&mut result, 0.95)}});
        println!("{row}");
        report.push(row);
    }
    drop(cleanup);
    assert!(report[1]["submit_to_start_ms"]["p50"].as_f64() < report[0]["submit_to_start_ms"]["p50"].as_f64());
}
