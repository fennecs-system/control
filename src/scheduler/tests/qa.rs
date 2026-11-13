//! Compatibility oracle and synthetic timing; never reads personal node state.
use crate::runtime::RunKind;
use super::*;
use std::{collections::HashSet, hint::black_box, time::Instant};
fn reference_admit(runtime: &Runtime) -> Result<(), &'static str> {
    if runtime
        .runs
        .iter()
        .filter(|r| {
            matches!(
                r.status.as_str(),
                "queued" | "running" | "cancelling" | "waiting"
            )
        })
        .count()
        >= MAX_PENDING
    {
        Err("Run queue is full (32 pending tasks). Wait for work to finish or cancel a task.")
    } else {
        Ok(())
    }
}

fn reference_next(runtime: &Runtime) -> Option<usize> {
    let active = runtime
        .runs
        .iter()
        .filter(|r| matches!(r.status.as_str(), "running" | "cancelling"))
        .count();
    if runtime.paused.is_some() || active >= runtime.execution.max_parallel_runs.min(MAX_PARALLEL) {
        return None;
    }
    let blocked = runtime
        .runs
        .iter()
        .filter(|r| {
            matches!(r.status.as_str(), "running" | "cancelling")
                || (r.status == "waiting"
                    && (r.delegation.is_some()
                        || r.question.as_ref().is_some_and(|q| q.answer.is_none())))
        })
        .filter_map(|r| r.chat_id.as_deref())
        .collect::<HashSet<_>>();
    runtime
        .runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.status == "queued")
        .filter(|(_, r)| {
            r.chat_id
                .as_deref()
                .is_none_or(|chat| !blocked.contains(chat))
        })
        .min_by_key(|(index, r)| (r.queue_order, *index))
        .map(|(index, _)| index)
}

fn seed() -> crate::runtime::Run {
    let mut runtime = Runtime::default();
    runtime.enqueue("Synthetic".into(), RunKind::Chat, "test");
    runtime.runs.pop().unwrap()
}
#[test]
fn adapter_matches_original_scheduler_over_mixed_saved_states() {
    let template = seed();
    let mut random = 0x6c37_u64;
    let mut draw = || {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        random
    };
    let states = [
        "queued",
        "running",
        "cancelling",
        "waiting",
        "completed",
        "failed",
        "interrupted",
        "continued",
        "unknown",
    ];
    for _ in 0..2000 {
        let mut runtime = Runtime::default();
        runtime.execution.max_parallel_runs = (draw() % 11) as usize;
        runtime.paused = (draw() % 5 == 0).then(|| "paused".into());
        for index in 0..draw() % 65 {
            let mut run = template.clone();
            run.id = format!("fixture-{index}");
            run.status = states[(draw() % states.len() as u64) as usize].into();
            run.chat_id = (draw() % 4 != 0).then(|| format!("chat-{}", draw() % 4));
            run.queue_order = draw() % 8;
            if draw() % 3 == 0 {
                run.question = Some(crate::design::Question {
                    id: "q".into(),
                    generation: "g".into(),
                    nonce: "n".into(),
                    input: crate::design::Input {
                        question: "Choose".into(),
                        options: vec!["a".into(), "b".into()],
                        recommended: None,
                    },
                    answer: (draw() % 2 == 0).then(|| "a".into()),
                    continuation: None,
                });
            }
            if draw() % 5 == 0 {
                run.delegation = Some(crate::delegation::Delegation {
                    input: crate::delegation::Input {
                        objective: "Review".into(),
                        role: crate::delegation::Role::Worker,
                    },
                    nonce: "n".into(),
                    generation: "g".into(),
                    backend: crate::backend::Backend::Codex,
                    model: "test".into(),
                    child: None,
                    continuation: None,
                });
            }
            // Optimized adapter queries must agree with the typed defaults.
            use control_core::scheduling::RunView;
            let view = scheduling::Run {
                phase: phase(&run),
                chat: run.chat_id.as_deref(),
                queue_order: run.queue_order,
            };
            assert_eq!(run.occupies_slot(), view.occupies_slot());
            assert_eq!(run.is_queued(), view.is_queued());
            assert_eq!(run.is_pending(), view.is_pending());
            assert_eq!(run.reserves_chat(), view.reserves_chat());
            runtime.runs.push(run);
        }
        assert_eq!(next(&runtime), reference_next(&runtime));
        assert_eq!(admit(&runtime), reference_admit(&runtime));
    }
}
#[test]
#[ignore = "release synthetic scheduler comparison; no models, credentials, Docker or personal state"]
fn scheduling_profile() {
    let template = seed();
    for workload in [
        "small-open",
        "open",
        "occupied",
        "waiting",
        "paused",
        "slots-full",
        "admission-full",
        "admission-open",
    ] {
        let mut runtime = Runtime::default();
        for index in 0..5032 {
            let mut run = template.clone();
            run.id = format!("r-{index}");
            run.chat_id = Some(format!("chat-{}", index % 8));
            run.queue_order = index as u64;
            run.status = if index >= 5000 { "queued" } else { "completed" }.into();
            runtime.runs.push(run);
        }
        if workload == "paused" {
            runtime.paused = Some("paused".into());
        }
        if workload == "slots-full" {
            for run in &mut runtime.runs[..2] {
                run.status = "running".into();
            }
        }
        if workload == "admission-full" {
            runtime.runs.rotate_right(32);
        }
        if workload == "small-open" {
            runtime.runs.drain(..5000);
        }
        if workload == "occupied" {
            runtime.runs[0].status = "running".into();
        }
        if workload == "waiting" {
            for run in &mut runtime.runs[..8] {
                run.status = "waiting".into();
                run.question = Some(crate::design::Question {
                    id: "q".into(),
                    nonce: "n".into(),
                    generation: "g".into(),
                    input: crate::design::Input {
                        question: "Pick".into(),
                        options: vec!["a".into(), "b".into()],
                        recommended: None,
                    },
                    answer: None,
                    continuation: None,
                });
            }
        }
        if workload == "admission-open" {
            runtime.runs.last_mut().unwrap().status = "completed".into();
        }
        let admission = workload.starts_with("admission-");
        let measure = |reference| {
            let mut samples = Vec::new();
            for _ in 0..9 {
                let start = Instant::now();
                for _ in 0..200 {
                    if admission {
                        black_box(if reference {
                            reference_admit(black_box(&runtime))
                        } else {
                            admit(black_box(&runtime))
                        })
                        .ok();
                    } else {
                        black_box(if reference {
                            reference_next(black_box(&runtime))
                        } else {
                            next(black_box(&runtime))
                        });
                    }
                }
                samples.push(start.elapsed().as_nanos() / 200);
            }
            samples.sort_unstable();
            samples[4]
        };
        let before = measure(true);
        let after = measure(false);
        println!(
            "{workload}: legacy={before} ns core={after} ns per call; {} runs, 9x200 calls",
            runtime.runs.len()
        );
    }
}
