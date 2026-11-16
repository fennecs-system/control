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
        *run.result = format!("Answer {index}: Unicode λ & <code> \"text\".");
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
            state.chats[0].messages.push(chat::Message { speaker: Default::default(),
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
fn transcript_fixtures_keep_activity_collapsed_without_a_folder_picker() {
    for (messages, unrelated, expected) in [
        (
            10,
            0,
            "554970efa3a2565da043c6f012c7976a5011470e2f9c36d2d927daeb91692b39",
        ),
        (
            100,
            1000,
            "af34f02d5139219fd19d702527d9bfeaf07e828474d586cd7268b9aaae9d67a8",
        ),
        (
            500,
            5000,
            "822798200d64f130c74ce9c2e01f480d606a1abb37dbb81ee8791135e6e5780a",
        ),
    ] {
        let state = fixture(messages, unrelated);
        let html = chat::transcript(&state, "benchmark-chat").unwrap();
        assert!(!html.contains("project-picker"));
        assert!(!html.contains("' open><summary>"));
        // Snapshots include the intentional closed activity defaults and keep
        // the rest of the transcript byte-for-byte stable across fixture sizes.
        assert_eq!(fingerprint(&html), expected);
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
#[ignore = "explicit inline/archived history-read timing; no credentials or model calls"]
fn history_reads() {
    let rounds = std::env::var("CONTROL_HISTORY_PROFILE_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 1000);
    for messages in [10, 100, 500] {
        for archived in [false, true] {
            let mut state = fixture(messages, messages * 2);
            if archived {
                for run in state
                    .runtime
                    .runs
                    .iter_mut()
                    .filter(|run| run.chat_id.as_deref() == Some("benchmark-chat"))
                {
                    crate::history::archive_run(run).unwrap();
                }
            }
            let detail = chat::detail(&state, "benchmark-chat").unwrap().to_string();
            let mut samples = Vec::new();
            for batch in 0..10 {
                let start = Instant::now();
                for _ in 0..rounds {
                    black_box(chat::detail(black_box(&state), "benchmark-chat").unwrap());
                }
                if batch > 0 {
                    samples.push(start.elapsed().as_nanos() / rounds as u128);
                }
            }
            samples.sort_unstable();
            println!(
                "{}",
                serde_json::json!({"workload":"history_detail","messages":messages,"archived":archived,"median_ns":samples[4],"samples":samples,"sha256":fingerprint(&detail)})
            );
        }
    }
}
#[test]
#[ignore = "explicit release-mode identity snapshot timing; no credentials or container calls"]
fn ownership_snapshots() {
    let mut state = fixture(500, 0);
    for run in &mut state.runtime.runs {
        let run = &mut **run;
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
type View = dyn Fn(&State) -> usize;
/// Operator-facing views: the web chat page, its requests panel, the Bots page,
/// `control.chat.read`/`control.chats.list` (TUI) and the node-wide request
/// view an away-from-keyboard adapter reads on change hints.
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn operator_views() {
    // More samples for profiling: CONTROL_TIMING_ROUNDS=2000.
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(9);
    for (messages, unrelated, questions) in [(10, 0, 0), (100, 1000, 10), (500, 5000, 50)] {
        let mut state = fixture(messages, unrelated);
        for (index, run) in state.runtime.runs.iter_mut().filter(|run| run.chat_id.as_deref() == Some("benchmark-chat")).take(questions).enumerate() {
            run.status = "waiting".into();
            run.question = Some(crate::design::Question {
                id: format!("q-{index}"),
                generation: format!("g-{index}"),
                nonce: format!("n-{index}"),
                input: crate::design::parse_input(serde_json::json!({"question":"Choose a route <&> λ","options":["Relay","Stop"]})).unwrap(),
                answer: None,
                continuation: None,
            });
        }
        let workloads: [(&str, &View); 6] = [
            ("web_chat_page", &|s| chat::render(s, "benchmark-chat").unwrap().len()),
            ("web_requests_panel", &|s| chat::requests(s, "benchmark-chat").unwrap().len()),
            ("web_bots_page", &|s| crate::bots::overview(s).len()),
            ("chat_read", &|s| chat::detail(s, "benchmark-chat").unwrap().to_string().len()),
            ("chats_list", &|s| chat::list(s).to_string().len()),
            ("requests_list", &|s| chat::pending_requests(s).to_string().len()),
        ];
        for (name, workload) in workloads {
            let mut samples = Vec::new();
            for _ in 0..rounds {
                let start = Instant::now();
                for _ in 0..10 {
                    black_box(workload(black_box(&state)));
                }
                samples.push(start.elapsed().as_nanos() / 10);
            }
            samples.sort_unstable();
            println!("{}", serde_json::json!({"workload":name,"messages":messages,"unrelated_runs":unrelated,"questions":questions,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"bytes":workload(&state)}));
        }
    }
}
/// The chat list/tree every client renders: `control.chats.list` (TUI picker,
/// Discord `!chats`), the web conversation tree and one chat's heading, over
/// task trees of eight chats.
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn chat_lists() {
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(9);
    let id = |index: usize| format!("c-{:x}-{index}", 0x1870_0000_0000_u64 + index as u64 * 7919);
    for chats in [10, 100, 1000] {
        let mut state = State::default();
        for index in 0..chats {
            chat::create(&mut state);
            let chat = state.chats.last_mut().unwrap();
            chat.id = id(index);
            chat.title = format!("Own player{index} in private Minecraft civilisation. Read /world <&> λ");
            chat.activity_order = index as u64;
            chat.parent_chat = (index % 8 != 0).then(|| id(index - index % 8));
        }
        let active: &'static str = state.chats[chats / 2].id.clone().leak();
        let workloads: [(&str, &View); 3] = [
            ("chats_list", &|s| chat::list(s).to_string().len()),
            ("web_conversation_tree", &move |s| crate::navigation::conversations(s, active).len()),
            ("chat_read_heading", &move |s| chat::detail(s, active).unwrap()["title"].to_string().len()),
        ];
        for (name, workload) in workloads {
            let mut samples = Vec::new();
            for _ in 0..rounds {
                let start = Instant::now();
                for _ in 0..10 {
                    black_box(workload(black_box(&state)));
                }
                samples.push(start.elapsed().as_nanos() / 10);
            }
            samples.sort_unstable();
            println!("{}", serde_json::json!({"workload":name,"chats":chats,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"bytes":workload(&state)}));
        }
    }
}
