//! Synthetic workloads: never load personal state or make provider calls.
use crate::runtime::RunKind;
use crate::runtime::RunStatus;
use crate::{
    State, chat,
    usage::{Activity, Attempt},
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
        run = run.with_id(format!("run-{index}"));
        run.chat_id = Some(
            if index < messages {
                "benchmark-chat"
            } else {
                "unrelated"
            }
            .into(),
        );
        run.status = [
            RunStatus::Completed,
            RunStatus::Running,
            RunStatus::Failed,
            RunStatus::Queued,
            RunStatus::Cancelled,
        ][index % 5]
            .clone();
        run.kind = if index % 7 == 0 {
            RunKind::Steering
        } else {
            RunKind::Chat
        };
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
            state.chats[0].messages.push(chat::Message {
                speaker: Default::default(),
                text: format!("Question {index}: <&> λ"),
                run_id: Some(run.id().as_str().to_owned()),
                reply: String::new(),
            });
        }
        state.runtime.runs.push(run);
    }
    state.runtime.runs.rebuild(|runs| runs.rotate_right(unrelated));
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
            "6d01a3e11bf93a5ca187c58e1ca9c3073b86f9cd2c1ed4f90741d1de09832d1a",
        ),
        (
            100,
            1000,
            "e1c6f73abee8b58b7e65946a9bdbd40c609318dea72cf803d414194e02f5d32c",
        ),
        (
            500,
            5000,
            "8e7bfd2537bc77d9affbaf21ebce8f032f0f35824cd13eed0497159c6c6bbc1d",
        ),
    ] {
        let state = fixture(messages, unrelated);
        let html = chat::transcript(&state, "benchmark-chat").unwrap().render();
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
        let html = chat::transcript(&state, "benchmark-chat").unwrap().render();
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..10 {
                black_box(chat::transcript(black_box(&state), "benchmark-chat").unwrap().render());
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
    for run in state.runtime.runs.iter_mut() {
        let id = run.id().as_str().to_owned();
        let run = &mut **run;
        run.attempts[0].command_jobs.push(crate::command_jobs::Job {
            generation: format!("{id}-attempt-1"),
            id,
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
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    for (messages, unrelated, questions) in [(10, 0, 0), (100, 1000, 10), (500, 5000, 50)] {
        let mut state = fixture(messages, unrelated);
        for (index, run) in state
            .runtime
            .runs
            .iter_mut()
            .filter(|run| run.chat_id.as_deref() == Some("benchmark-chat"))
            .take(questions)
            .enumerate()
        {
            run.status = RunStatus::Waiting;
            run.question = Some(crate::design::Question {
                id: format!("q-{index}").into(),
                generation: format!("g-{index}"),
                nonce: format!("n-{index}"),
                input: crate::design::parse_input(
                    serde_json::json!({"question":"Choose a route <&> λ","options":["Relay","Stop"]}),
                )
                .unwrap(),
                status: crate::design::Status::Pending { asked_at: None },
                continuation: None,
            });
        }
        let workloads: [(&str, &View); 6] = [
            ("web_chat_page", &|s| chat::render(s, "benchmark-chat").unwrap().len()),
            ("web_requests_panel", &|s| {
                chat::requests(s, "benchmark-chat").unwrap().len()
            }),
            ("web_bots_page", &|s| crate::bots::overview(s, "").len()),
            ("chat_read", &|s| {
                chat::detail(s, "benchmark-chat").unwrap().to_string().len()
            }),
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
            println!(
                "{}",
                serde_json::json!({"workload":name,"messages":messages,"unrelated_runs":unrelated,"questions":questions,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"bytes":workload(&state)})
            );
        }
    }
}
/// The DNA page (gene list, editor and resolved context) as the library grows.
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn dna_page() {
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    for genes in [10, 100, 1000] {
        let mut state = State::default();
        crate::ui_previews::genes(&mut state, genes);
        let selected = format!("gene-{:04}", genes / 2);
        let mut samples = Vec::new();
        for _ in 0..rounds {
            let start = Instant::now();
            for _ in 0..10 {
                black_box(crate::lens_library::render(
                    black_box(crate::lens_library::Sources::of(&state)),
                    &selected,
                    None,
                    None,
                    false,
                ));
            }
            samples.push(start.elapsed().as_nanos() / 10);
        }
        samples.sort_unstable();
        let bytes =
            crate::lens_library::render(crate::lens_library::Sources::of(&state), &selected, None, None, false).len();
        println!(
            "{}",
            serde_json::json!({"workload":"web_dna_page","genes":genes,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"bytes":bytes})
        );
    }
}
/// The chat list/tree every client renders: `control.chats.list` (TUI picker,
/// Discord `!chats`), the web conversation tree and one chat's heading, over
/// task trees of eight chats.
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn chat_lists() {
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
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
            ("web_conversation_tree", &move |s| {
                crate::navigation::Picker::of(&s.chats, active).render().len()
            }),
            ("chat_read_heading", &move |s| {
                chat::detail(s, active).unwrap()["title"].to_string().len()
            }),
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
            println!(
                "{}",
                serde_json::json!({"workload":name,"chats":chats,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"bytes":workload(&state)})
            );
        }
    }
}
/// A node with `live` running bots spawned by one steering chat (which also
/// has a live agent) beside `dead` stopped bots (one turn each, varied stop
/// reasons), for the Bots page.
pub(crate) fn dead_bots(live: usize, dead: usize) -> State {
    let mut state = State::default();
    let root = chat::create(&mut state);
    let template = state.chats[0].clone();
    state.runtime.enqueue("Steer".into(), RunKind::Chat, "test");
    let run = state.runtime.runs.pop().unwrap();
    let id = |index: usize| format!("c-{:x}-{index}", 0x1870_0000_0000_u64 + index as u64 * 7919);
    for index in 0..dead {
        let mut chat = template.clone();
        chat.id = id(index);
        chat.title = format!("Survey tunnel {index} and report ore counts <&> λ");
        chat.activity_order = index as u64 + 2;
        state.chats.push(chat);
        let mut turn = run.clone();
        turn = turn.with_id(format!("run-{index}"));
        turn.chat_id = Some(id(index));
        turn.status = [
            RunStatus::Completed,
            RunStatus::Cancelled,
            RunStatus::Failed,
            RunStatus::Completed,
        ][index % 4]
            .clone();
        turn.bot_turn = Some(Box::new(crate::bots::Turn {
            bot: id(index),
            turn: 1,
            reasons: Vec::new(),
            recovery: None,
        }));
        turn.attempts.push(Attempt {
            started_unix: 1_700_000_000 + index as u64 * 37,
            elapsed_ms: 90_000,
            ..Default::default()
        });
        state.runtime.runs.push(turn);
    }
    for index in 0..live {
        let mut chat = template.clone();
        chat.id = format!("c-live-bot-{index}");
        chat.title = format!("Patrol tunnel ridge {index} for intruders");
        chat.parent_chat = Some(root.clone());
        state.chats.push(chat);
        let mut turn = run.clone();
        turn = turn.with_id(format!("run-live-bot-{index}"));
        turn.chat_id = Some(format!("c-live-bot-{index}"));
        turn.status = RunStatus::Completed;
        state.runtime.runs.push(turn);
        crate::bots::command(&mut state, &format!("c-live-bot-{index}"), "on every 10").unwrap();
    }
    let mut agent = template.clone();
    agent.id = "c-live-agent".into();
    agent.parent_chat = Some(root);
    state.chats.push(agent);
    let mut live = run.clone();
    live = live.with_id("run-live");
    live.chat_id = Some("c-live-agent".into());
    live.status = RunStatus::Running;
    state.runtime.runs.push(live);
    state
}
/// The Bots page (`GET /?view=agents`) with many dead bots on the node, the
/// graveyard's next scroll page and a search.
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn bots_page() {
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    for (live, dead) in [(0, 10), (100, 1000), (100, 10_000)] {
        let state = dead_bots(live, dead);
        let next = crate::graveyard::dead(&state, "")
            .get(crate::graveyard::PAGE.min(dead) - 1)
            .map(|grave| grave.chat.id.clone())
            .unwrap_or_default();
        let next: &'static str = next.leak();
        let workloads: [(&str, &View); 4] = [
            ("web_bots_page", &|s| {
                crate::public_render(s, &crate::fields("view=agents"), "none").len()
            }),
            ("web_bots_page_search", &|s| {
                crate::public_render(s, &crate::fields("view=agents&q=tunnel+9"), "none").len()
            }),
            ("web_bots_page_search_miss", &|s| {
                crate::public_render(s, &crate::fields("view=agents&q=zzzz"), "none").len()
            }),
            ("graveyard_next_page", &move |s| {
                let query = crate::graveyard::Query {
                    older: Some(next.into()),
                    ..Default::default()
                };
                crate::graveyard::rows(&crate::graveyard::dead(s, ""), &query).len()
            }),
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
            println!(
                "{}",
                serde_json::json!({"workload":name,"live_bots":live,"dead_bots":dead,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"bytes":workload(&state)})
            );
        }
    }
}
/// A node with `dms` DMs sharing `turns` turns (one in twenty failed), for
/// Bots / History. Built directly: no callbacks, saves or model calls.
pub(crate) fn history_fixture(dms: usize, turns: usize) -> State {
    let mut state = State::default();
    chat::create(&mut state);
    let template = state.chats.pop().unwrap();
    state.runtime.enqueue("Turn".into(), RunKind::Chat, "test");
    let run = state.runtime.runs.pop().unwrap();
    let id = |index: usize| format!("c-{:x}-{index}", 0x1870_0000_0000_u64 + index as u64 * 7919);
    for index in 0..dms {
        let mut chat = template.clone();
        chat.id = id(index);
        chat.title = format!("Survey tunnel {index} and report ore counts");
        chat.activity_order = index as u64 + 1;
        state.chats.push(chat);
    }
    for index in 0..turns {
        let mut turn = run.clone();
        turn = turn.with_id(format!(
            "{:x}-{index}",
            0x1790_0000_0000_0000_u64 + index as u64 * 1_000_000_000
        ));
        turn.chat_id = Some(id(index % dms));
        turn.kind = [RunKind::Chat, RunKind::BotWake, RunKind::PortDecision][index % 3];
        turn.status = if index % 20 == 0 {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        };
        turn.target.model = Some(["gpt-5", "claude"][index % 2].into());
        *turn.objective = format!("Turn {index}: check the ore counts <&>");
        turn.attempts.push(Attempt {
            started_unix: 1_700_000_000 + index as u64 * 13,
            elapsed_ms: 30_000,
            tokens: Some(crate::usage::Tokens {
                input_tokens: 9_000,
                cached_input_tokens: 6_000,
                output_tokens: 700,
            }),
            ..Default::default()
        });
        state.runtime.runs.push(turn);
    }
    state
}
/// Bots / History requests through the HTTP handler at 10, 1,000 and 10,000
/// DMs with 100,000 turns. Targets a revision does not serve report 0 bytes.
#[test]
#[ignore = "explicit release-mode synthetic timing; no credentials, models or source execution"]
fn history_page() {
    let rounds = std::env::var("CONTROL_TIMING_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    let turns = std::env::var("CONTROL_HISTORY_TURNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    for dms in [10, 1000, 10_000] {
        let mut state = history_fixture(dms, turns);
        let dm = state.chats[0].id.clone();
        let targets = [
            ("history_page", "/bots/history".to_owned()),
            ("history_runs", "/bots/history/runs".to_owned()),
            ("history_failed", "/bots/history?status=failed".to_owned()),
            ("history_expand", "/bots/history?expand=1".to_owned()),
            ("history_search", "/bots/history?q=tunnel+9".to_owned()),
            ("history_turns", format!("/bots/history/turns?dm={dm}")),
        ];
        for (name, target) in targets {
            let request = |state: &mut State| {
                let mut response = Vec::new();
                crate::session_http::handle(&mut response, state, "GET", &target, "none", "", &[]);
                response.len()
            };
            let mut samples = Vec::new();
            for _ in 0..rounds {
                let start = Instant::now();
                for _ in 0..5 {
                    black_box(request(black_box(&mut state)));
                }
                samples.push(start.elapsed().as_nanos() / 5);
            }
            samples.sort_unstable();
            println!(
                "{}",
                serde_json::json!({"workload":name,"dms":dms,"turns":turns,"median_ns":samples[samples.len() / 2],"min_ns":samples[0],"max_ns":samples[samples.len() - 1],"bytes":request(&mut state)})
            );
        }
    }
}
