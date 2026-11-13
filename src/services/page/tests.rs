
use super::*;

#[test]
fn cards_escape_commands_and_show_live_data_only_when_observed() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    let input = Input {
        command: "<script>alert(1)</script>\nsecond line"
            .to_owned()
            .try_into()
            .unwrap(),
        lease_seconds: -1,
        storage_limit_mb: None,
    };
    admit(
        &mut state,
        Request {
            run: &run,
            generation: "attempt-a",
            nonce: "n1",
        },
        input,
        "node-a".to_owned().try_into().unwrap(),
        time,
        |_| Ok(()),
    )
    .unwrap();
    let service = state.services[0].binding_snapshot();
    let plain = card(&service);
    assert!(plain.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !plain.contains("<script>"));
    assert!(!plain.contains("second line"));
    assert!(
        plain.contains("Requested") && plain.contains("until stopped") && plain.contains("/chat/")
    );
    assert!(plain.contains("action='/tools/services'") && plain.contains("Stop service"));
    assert!(plain.contains("/workspace retained for this chat"));
    assert!(
        !plain.contains("data-live="),
        "only running services wait for live usage"
    );
    assert!(!plain.contains("CPU") && !plain.contains("<table>"));
    let live = Live {
        stats: Some(crate::engine::Stats {
            cpu: "3.5%".into(),
            memory: "512MiB / 2GiB".into(),
            pids: "14".into(),
        }),
        processes: vec![crate::engine::Process {
            pid: "77".into(),
            user: "502".into(),
            command: "java -jar server.jar <nogui>".into(),
        }],
    };
    let observed = usage(&live);
    assert!(observed.contains("CPU 3.5% · Memory 512MiB / 2GiB · 14 processes"));
    assert!(
        observed.contains("<td>77</td>") && observed.contains("java -jar server.jar &lt;nogui&gt;")
    );
}

#[test]
fn page_snapshots_keep_escaped_diagnostics() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    let input = Input {
        command: "sleep infinity".to_owned().try_into().unwrap(),
        lease_seconds: -1,
        storage_limit_mb: None,
    };
    admit(
        &mut state,
        Request {
            run: &run,
            generation: "attempt-a",
            nonce: "n1",
        },
        input,
        "node-a".to_owned().try_into().unwrap(),
        time,
        |_| Ok(()),
    )
    .unwrap();
    state.services[0].diagnostic = "Engine said <no>".into();
    assert!(state.services[0].binding_snapshot().diagnostic.is_empty());
    let shown = card(&snapshot(&state.services[0]));
    assert!(shown.contains("Engine said &lt;no&gt;"));
}

#[test]
fn gather_keeps_results_that_arrive_before_the_deadline() {
    let jobs: Vec<(String, Job<u32>)> = vec![
        ("fast".into(), Box::new(|| 1)),
        (
            "slow".into(),
            Box::new(|| {
                std::thread::sleep(Duration::from_secs(2));
                2
            }),
        ),
        ("also-fast".into(), Box::new(|| 3)),
    ];
    let started = Instant::now();
    let results = gather(jobs, Instant::now() + Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        (
            results.get("fast"),
            results.get("also-fast"),
            results.get("slow")
        ),
        (Some(&1), Some(&3), None)
    );
    assert!(gather::<u32>(Vec::new(), Instant::now() + Duration::from_secs(5)).is_empty());
}
