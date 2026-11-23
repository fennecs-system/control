//! One supervisor dispatch pass over many services with retained command
//! history and nothing new to launch: the steady-state cost paid every 2 s.
//! Synthetic state only; no engine, credentials or model calls.
use super::*;
use std::hint::black_box;

fn execution(service: usize, n: usize, status: Status) -> Execution {
    Execution {
        id: format!("{service:032x}{n:032x}"),
        digest: String::new(),
        run: "run-profile".into(),
        attempt: "attempt-a".into(),
        command: String::new(),
        display_command: format!("status check {n}"),
        timeout_seconds: 60,
        status,
        output: "x".repeat(256),
        truncated: false,
        exit_code: (status == Status::Completed).then_some(0),
        diagnostic: String::new(),
        copy: None,
    }
}

fn state(services: usize, running: usize) -> State {
    let (mut state, ..) = fixture();
    let template = state.services[0].clone();
    state.services.clear();
    for s in 0..services {
        let mut service = template.clone();
        service.resource.id = format!("svc-{s:032x}").try_into().unwrap();
        // `running` active records plus a full retention of finished ones.
        service.executions = (0..running + MAX_EXECUTIONS)
            .map(|n| {
                execution(
                    s,
                    n,
                    if n < running {
                        Status::Running
                    } else {
                        Status::Completed
                    },
                )
            })
            .collect();
        state.services.push(service);
    }
    state
}

#[test]
#[ignore = "profile: cargo test --release --locked --bin control services::commands::tests::dispatch_profile -- --ignored --nocapture"]
fn dispatch_profile() {
    let rounds = std::env::var("CONTROL_PROFILE_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2000)
        .clamp(1, 1_000_000);
    for services in [8, 128] {
        for running in [0, 1, 8, 64] {
            let shared = Arc::new(Mutex::new(state(services, running)));
            let mut threads = Vec::new();
            let mut samples = Vec::new();
            for batch in 0..11 {
                let start = Instant::now();
                for _ in 0..rounds {
                    dispatch(black_box(&shared), &mut threads).unwrap();
                }
                if batch > 0 {
                    samples.push(start.elapsed().as_nanos() / rounds as u128);
                }
            }
            assert!(threads.is_empty(), "the profile launches nothing");
            samples.sort_unstable();
            println!(
                "{}",
                json!({"workload":"services_dispatch","services":services,"executions_per_service":running + MAX_EXECUTIONS,"running_per_service":running,"rounds_per_batch":rounds,"median_ns":samples[5],"min_ns":samples[0],"max_ns":samples[9]})
            );
        }
    }
}
