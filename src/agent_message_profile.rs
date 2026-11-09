//! Credential-free release fixture for existing downward message delivery.
//! Copy this fixture unchanged to a baseline checkout for paired profiling.
use super::*;
use std::{hint::black_box, time::Instant};

fn fixture(chats: usize) -> (State, String, String, usize) {
    let mut state = State::default();
    let lead = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &lead, "Own an independent task", "benchmark").unwrap();
    state.runtime.runs[0].status = "running".into();
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        workspace_id: Some("benchmark-attempt".into()), ..Default::default()
    });
    let session = state.chats[0].clone();
    let run = state.runtime.runs[0].clone();
    for index in 1..chats {
        let mut chat = session.clone();
        chat.id = format!("benchmark-chat-{index}");
        chat.messages.clear();
        chat.parent_chat = (index == chats - 1).then(|| lead.clone());
        let mut task = run.clone();
        task.id = format!("benchmark-run-{index}");
        task.chat_id = Some(chat.id.clone());
        task.status = "completed".into();
        task.attempts.clear();
        state.chats.push(chat);
        state.runtime.runs.push(task);
    }
    let target = state.chats.last().unwrap().id.clone();
    let sender = state.runtime.runs[0].id.clone();
    let runs = state.runtime.runs.len();
    (state, sender, target, runs)
}

#[test]
#[ignore = "explicit synthetic release timing/flamegraph; no models, personal state or credentials"]
fn routing_profile() {
    println!("{}", json!({"workload":"coordination_input_bytes", "room_policy_bytes":control_context::agent_policy::ROOM_COMMUNICATION_GUIDANCE.len(), "message_schema_bytes":Tools.definitions()[0].to_string().len()}));
    let rounds = std::env::var("CONTROL_PROFILE_ROUNDS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(10_000).clamp(1, 2_000_000);
    let selected = std::env::var("CONTROL_PROFILE_CHATS").ok().and_then(|v| v.parse::<usize>().ok());
    for chats in [16, 64, 1024].into_iter().filter(|n| selected.is_none_or(|value| *n == value)) {
        let (mut state, sender, target, runs) = fixture(chats);
        let mut samples = Vec::new();
        for batch in 0..10 {
            let start = Instant::now();
            for _ in 0..rounds {
                black_box(handle(&mut state,
                    &AttemptRef { run: &sender, generation: "benchmark-attempt", nonce: "benchmark" },
                    MessageCall { chat_id: target.clone(), text: "A dependency changed; proceed with your assignment".into(), services: vec![] },
                    |_| Ok(())).unwrap());
                state.runtime.runs.truncate(runs);
                let session = state.chats.last_mut().unwrap();
                session.messages.clear();
                state.agent_messages.clear();
            }
            if batch > 0 { samples.push(start.elapsed().as_nanos() / rounds as u128); }
        }
        samples.sort_unstable();
        println!("{}", json!({"workload":"agent_message_downward_cpu", "chats":chats,
            "rounds_per_batch":rounds,"samples":samples,"median_ns":samples[4],"min_ns":samples[0],
            "note":"Includes queue/evidence construction and in-memory rollback reset; excludes filesystem saves, model execution and fixture setup."}));
    }
}
