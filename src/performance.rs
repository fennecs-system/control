//! Synthetic workloads: never load personal state or make provider calls.
use crate::runtime::RunKind;
use crate::{
    chat,
    usage::{Activity, Attempt},
    State,
};
use std::{hint::black_box, time::Instant};

fn fixture(messages: usize, unrelated: usize) -> State {
    let mut state = State::default();
    chat::create(&mut state);
    state.chats[0].id = "benchmark-chat".into();
    state.chats[0].title = "Synthetic chat".into();
    state.runtime.enqueue("Fixture".into(), RunKind::Chat, "test");
    let template = state.runtime.runs.pop().unwrap();
    for index in 0..messages + unrelated {
        let mut run = template.clone();
        run.id = format!("run-{index}");
        run.chat_id = Some(
            if index < messages {
                "benchmark-chat"
            } else {
                "unrelated"
            }
            .into(),
        );
        run.status = ["completed", "running", "failed", "queued", "cancelled"][index % 5].into();
        run.kind = if index % 7 == 0 { RunKind::Steering } else { RunKind::Chat };
        run.result = format!("Answer {index}: Unicode λ & <code> \"text\".");
        run.attempts.push(Attempt {
            omitted_history: index % 3,
            activity: vec![
                Activity {
                    id: "command-1".into(),
                    kind: "command_execution".into(),
                    text: "cargo test".into(),
                    output: "Synthetic output <&> λ\n".repeat(48),
                    status: "completed".into(),
                },
                Activity {
                    id: "workspace-diff".into(),
                    kind: "file_change".into(),
                    text: "1 file · +1 −1".into(),
                    output: "--- a/demo.rs\n+++ b/demo.rs\n@@ -1 +1 @@\n-old\n+new".into(),
                    status: "completed".into(),
                },
            ],
            ..Default::default()
        });
        if index < messages {
            state.chats[0].messages.push(chat::Message {
                text: format!("Question {index}: <&> λ"),
                run_id: Some(run.id.clone()),
                reply: String::new(),
            });
        }
        state.runtime.runs.push(run);
    }
    state.runtime.runs.rotate_right(unrelated);
    state
}
fn fingerprint(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[test]
fn transcript_fixtures_only_change_by_removing_the_unsolicited_folder_picker() {
    for (messages, unrelated, expected) in [
        (
            10,
            0,
            "f52d426a0b476425d117f6d93c99958ab14244eec9a748a52cb163dacb5ba88e",
        ),
        (
            100,
            1000,
            "cb30ca991ab4e4ae7c4db9e6869d687dd8ec9ca21fb7a382449ea3e5982e1051",
        ),
        (
            500,
            5000,
            "f9e21edf6e6e78b585b4d29d3bba6d6ca01bd19905f53cd9599d58afc1fece37",
        ),
    ] {
        let state = fixture(messages, unrelated);
        let html = chat::transcript(&state, "benchmark-chat").unwrap();
        assert!(!html.contains("project-picker"));
        // Restore only the intentionally removed picker to compare every other
        // byte against the original performance regression fixtures.
        let previous = html.replace("<div id='latest'>", &format!("{}<div id='latest'>", crate::projects::panel(&state, "benchmark-chat")));
        assert_eq!(fingerprint(&previous), expected);
    }
}
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn hot_paths() {
    for (messages, unrelated) in [(10, 0), (100, 1000), (500, 5000)] {
        let state = fixture(messages, unrelated);
        let html = chat::transcript(&state, "benchmark-chat").unwrap();
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..10 {
                black_box(chat::transcript(black_box(&state), "benchmark-chat").unwrap());
            }
            samples.push(start.elapsed().as_nanos() / 10);
        }
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"workload":"chat_transcript","messages":messages,"unrelated_runs":unrelated,"median_ns":samples[4],"min_ns":samples[0],"html_bytes":html.len(),"sha256":fingerprint(&html)})
        );
    }
}
#[test]
#[ignore = "explicit release-mode identity snapshot timing; no credentials or container calls"]
fn ownership_snapshots() {
    let mut state = fixture(500, 0);
    for run in &mut state.runtime.runs {
        run.attempts[0].command_jobs.push(crate::command_jobs::Job {
            id: run.id.clone(),
            generation: format!("{}-attempt-1", run.id),
            image: format!("sha256:{}", "a".repeat(64)),
            node: "synthetic-node".into(),
            command: "command ".repeat(500),
            read_only: false,
            build_environment: true,
            review_source: false,
            profile: crate::worker_profile::Profile::Lite,
            deadline_unix: 0,
            container_id: Some("synthetic-container".into()),
            status: "completed".into(),
            output: "x".repeat(16000),
            exit_code: Some(0),
        });
    }
    let mut samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..10 {
            black_box(crate::lifecycle::owners(black_box(&state.runtime.runs)));
        }
        samples.push(start.elapsed().as_nanos() / 10);
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"workload":"ownership_snapshot","attempts":500,"median_ns":samples[4],"min_ns":samples[0]})
    );
}
