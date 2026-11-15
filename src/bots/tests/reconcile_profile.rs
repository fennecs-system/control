//! Credential-free release fixture for `bots::reconcile` on a large node:
//! many chats, runs, claims and one persistent bot. Copy the `idle` and
//! `offered` cases unchanged to a baseline checkout for paired profiling.
use super::*;
use std::{hint::black_box, time::Instant};

/// One task tree under `general`: a root, `chats` helpers with `runs` turns
/// each (one in twenty failed) and the room's full claim allowance.
pub(super) fn large(chats: usize, runs: usize) -> (State, String, Vec<String>) {
    let (mut state, root) = fixture();
    let mut helpers = Vec::with_capacity(chats);
    for index in 0..chats {
        helpers.push(spawned(&mut state, &root, &format!("Worker {index}")));
    }
    let template = state.runtime.runs.last().unwrap().clone();
    for turn in 0..runs {
        for (index, chat) in helpers.iter().enumerate() {
            let mut run = template.clone();
            run.id = format!("bench-{index}-{turn}");
            run.chat_id = Some(chat.clone());
            run.status = if (index + turn) % 20 == 0 { "failed" } else { "completed" }.into();
            run.attempts = vec![crate::usage::Attempt::default()];
            state.runtime.runs.push(run);
        }
    }
    // Latest turns complete so claims stay live rather than abandoned.
    for chat in &helpers {
        let mut run = template.clone();
        run.id = format!("bench-latest-{chat}");
        run.chat_id = Some(chat.clone());
        state.runtime.runs.push(run);
    }
    for index in 0..128 {
        state.collective.claim_seq += 1;
        state.collective.claims.push(crate::rooms::claims::Claim {
            room: "general".into(), key: format!("deliverable-{index}"), owner: helpers[index % chats].clone(),
            status: crate::rooms::claims::Status::Working, blocked_on: None, note: String::new(),
            revision: state.collective.claim_seq, incarnation: state.collective.claim_seq, abandoned: false,
        });
    }
    command_at(&mut state, &helpers[0], "on", Now(0)).unwrap();
    (state, root, helpers)
}

pub(super) fn measure(label: &str, chats: usize, state: &mut State, bot: &str, offered: bool) {
    let rounds = std::env::var("CONTROL_PROFILE_ROUNDS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(200).clamp(1, 1_000_000);
    let mut now = 1_000;
    let mut samples = Vec::new();
    for batch in 0..11 {
        let start = Instant::now();
        for _ in 0..rounds {
            if offered {
                // An own-chat post passes the idle guard without admitting a turn.
                post(state, Some(bot), "status");
            }
            now += 1;
            reconcile_at(black_box(&mut *state), now, |_| Ok(())).unwrap();
        }
        if batch > 0 { samples.push(start.elapsed().as_nanos() / rounds as u128); }
    }
    samples.sort_unstable();
    assert!(bot_runs(state).is_empty(), "the profile measures reconciliation, not admitted turns");
    println!("{}", json!({"workload":"bots_reconcile","case":label,"chats":chats,"runs":state.runtime.runs.len(),
        "claims":state.collective.claims.len(),"rounds_per_batch":rounds,"median_ns":samples[5],"min_ns":samples[0],"max_ns":samples[9]}));
}

#[test]
#[ignore = "explicit synthetic release timing/flamegraph; no models, personal state or credentials"]
fn reconcile_profile() {
    let advised = std::env::var("CONTROL_PROFILE_ADVISOR").is_ok_and(|value| value == "1");
    for (chats, runs) in [(64, 16), (512, 16)] {
        for offered in [false, true] {
            let (mut state, root, helpers) = large(chats, runs);
            if advised {
                // After-only case: one advisor observes the whole tree each pass.
                super::advisor::advisor(&mut state, &root);
                reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
            }
            let label = match (offered, advised) { (false, false) => "idle", (true, false) => "offered", (false, true) => "idle_advised", (true, true) => "offered_advised" };
            measure(label, chats, &mut state, &helpers[0], offered);
        }
    }
}
