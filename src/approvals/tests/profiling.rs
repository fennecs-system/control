use super::*;

#[test]
#[ignore = "release approval admission benchmark/perf fixture; synthetic state, no models"]
fn profile_approval_resume() {
    use std::{hint::black_box, time::Instant};
    let rounds = std::env::var("CONTROL_APPROVAL_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 100_000);
    for retained in [16, 1024, 20_001] {
        let (mut state, chat, _) = requested();
        let mut template = state.runtime.runs.pop().unwrap();
        template.status = "completed".into();
        state.runtime.runs = (0..retained)
            .map(|index| {
                let mut run = template.clone();
                run.id = format!("run-{index}");
                run.queue_order = index as u64;
                run.kind = if index == 0 {
                    RunKind::Task
                } else {
                    RunKind::BotWake
                };
                run
            })
            .collect();
        let revision = state.chats[0].revision;
        let mut samples = Vec::new();
        for batch in 0..11 {
            let start = Instant::now();
            for _ in 0..rounds {
                let result = decide(black_box(&mut state), &chat, "run-0");
                assert!(matches!(result, Outcome::Resumed(_)));
                // Synthetic successful acknowledgement: the driver now owns the
                // wake. Retain one notification per measured admission.
                crate::worker::wake();
                black_box(result);
                black_box(state.runtime.runs.pop().unwrap());
                black_box(state.chats[0].messages.pop().unwrap());
                state.chats[0].revision = revision;
            }
            if batch != 0 {
                samples.push(start.elapsed().as_nanos() / rounds as u128);
            }
        }
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"case":"approval_resume", "retained":retained,
            "median_ns":(samples[4]+samples[5])/2, "min_ns":samples[0], "max_ns":samples[9], "rounds":rounds})
        );
    }
}
