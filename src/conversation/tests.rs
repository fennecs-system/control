
use super::*;
use control_comonad::Store;

fn chat(turns: usize, text: usize) -> (State, String) {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    for turn in 0..turns {
        crate::chat::send(
            &mut state,
            &id,
            &format!("turn {turn} {}", "x".repeat(text)),
            "test",
        )
        .unwrap();
        let run = state.runtime.runs.last_mut().unwrap();
        run.status = "completed".into();
        *run.result = format!("reply {turn}");
    }
    (state, id)
}
fn session<'a>(state: &'a State, id: &str) -> &'a Session {
    state.chats.iter().find(|s| s.id == id).unwrap()
}
fn entries(window: &Window) -> Vec<usize> {
    window
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Entry(i) => Some(*i),
            Item::Summary(..) => None,
        })
        .collect()
}

#[test]
#[ignore = "explicit synthetic release benchmark; no model calls or personal state"]
fn continuity_profile() {
    use std::{hint::black_box, time::Instant};
    for turns in [15, 150] {
        let (mut state, id) = chat(turns, 900);
        for run in &mut state.runtime.runs {
            let mut attempt = crate::usage::Attempt::new();
            attempt.activity = (0..16)
                .map(|i| crate::usage::Activity {
                    id: format!("tool-{i}"),
                    kind: "mcp_tool_call".into(),
                    text: "control_service_exec_status".into(),
                    output: "Verified step output λ ".repeat(80),
                    status: "completed".into(),
                })
                .collect();
            run.attempts.push(attempt);
        }
        let run = state.runtime.runs.last().unwrap().id.clone();
        for run in state.runtime.runs.iter_mut().skip(1) {
            run.kind = crate::runtime::RunKind::BotWake;
        }
        crate::history::archive_run(&mut state.runtime.runs[turns - 2]).unwrap();
        let iterations = std::env::var("CONTROL_PROFILE_ITERATIONS")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(200);
        let mut samples = Vec::new();
        for _ in 0..9 {
            let started = Instant::now();
            for _ in 0..iterations {
                black_box(crate::chat::prompt(black_box(&state), &run).unwrap());
            }
            samples.push(started.elapsed().as_nanos() / u128::from(iterations));
        }
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"case":"turn_context","turns":turns,"median_ns":samples[4],"min_ns":samples[0],"prompt_bytes":crate::chat::prompt(&state,&run).unwrap().len(),"chat":id})
        );
    }
}

#[test]
fn recent_keeps_the_newest_entries_within_its_budget() {
    let (state, id) = chat(40, 900);
    let conversation = Conversation::of(&state, session(&state, &id));
    let window = Recent::default().window(&conversation, 39);
    let kept = entries(&window);
    assert_eq!(kept.last(), Some(&38), "ends just before the current entry");
    assert_eq!(window.omitted + kept.len(), 39);
    assert!(window.render(&conversation).concat().chars().count() <= 20_000);
    let one_more = conversation
        .entry(kept[0] - 1)
        .unwrap()
        .render()
        .chars()
        .count();
    assert!(
        window.render(&conversation).concat().chars().count() + one_more > 20_000,
        "as many as fit"
    );
    assert!(
        window.render(&conversation)[0].starts_with("User: turn")
            && window.render(&conversation)[0].contains("Assistant: reply")
    );
}

#[test]
fn carry_preserves_omitted_assignment_and_archived_observations_for_both_providers() {
    let (mut state, id) = chat(40, 900);
    for run in state.runtime.runs.iter_mut().skip(1) {
        run.kind = crate::runtime::RunKind::BotWake;
    }
    state.runtime.runs[5].kind = crate::runtime::RunKind::Steering;
    state.chats[0].messages[5].text = "Observe only; do not change the environment.".into();
    let previous = &mut state.runtime.runs[38];
    let mut attempt = crate::usage::Attempt::new();
    attempt.activity = vec![
        crate::usage::Activity {
            id: "result".into(),
            kind: "mcp_tool_call".into(),
            text: "step-status".into(),
            output: "Verified output λ".into(),
            status: "completed".into(),
        },
        crate::usage::Activity {
            id: "private".into(),
            kind: "reasoning".into(),
            text: "".into(),
            output: "Hidden reasoning".into(),
            status: "completed".into(),
        },
    ];
    previous.attempts.push(attempt);
    crate::history::archive_run(previous).unwrap();
    let run = state.runtime.runs[39].id.clone();
    state.runtime.runs[39].kind = crate::runtime::RunKind::BotWake;
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        state.runtime.runs[39].backend = backend;
        let prompt = crate::chat::prompt(&state, &run).unwrap();
        assert!(prompt.contains("Earlier task and user inputs") && prompt.contains("turn 0 "));
        assert!(
            prompt.contains("Observe only; do not change the environment."),
            "later steering survives alongside the original assignment"
        );
        assert!(prompt.contains("Verified output λ") && !prompt.contains("Hidden reasoning"));
        assert!(
            prompt.contains("continuing controller")
                && !prompt.contains("finish your turn after reporting the handles")
        );
    }
    let conversation = Conversation::of(&state, session(&state, &id));
    assert!(
        conversation
            .carry(39, &Passthrough.window(&conversation, 39))
            .instructions
            .is_empty(),
        "do not duplicate an assignment already present verbatim"
    );
    assert!(
        conversation
            .carry(0, &Recent::default().window(&conversation, 0))
            .previous
            .is_none(),
        "never carry future tool output into an earlier turn"
    );
}

#[test]
fn execution_handoff_is_bounded_and_excludes_reasoning() {
    let events = (0..100)
        .map(|i| crate::usage::Activity {
            id: i.to_string(),
            kind: "command_execution".into(),
            text: "cmd".repeat(100),
            output: "λ".repeat(5000),
            status: "completed".into(),
        })
        .collect::<Vec<_>>();
    let rendered = handoff(&[&"x".repeat(10000)], Some("r"), &events);
    assert_eq!(rendered.matches("[completed]:").count(), 6);
    assert!(rendered.chars().count() < 5500);
}

#[test]
#[ignore = "explicit worst-case retained-observation benchmark, no model calls"]
fn large_observation_profile() {
    use std::{hint::black_box, time::Instant};
    let (mut state, _) = chat(15, 900);
    let run = &mut state.runtime.runs[13];
    let mut attempt = crate::usage::Attempt::new();
    attempt.activity = (0..64)
        .map(|i| crate::usage::Activity {
            id: i.to_string(),
            kind: "command_execution".into(),
            text: "inspect".into(),
            output: "λ".repeat(4000),
            status: "completed".into(),
        })
        .collect();
    run.attempts.push(attempt);
    crate::history::archive_run(run).unwrap();
    let id = state.runtime.runs[14].id.clone();
    let mut samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..100 {
            black_box(crate::chat::prompt(&state, &id).unwrap());
        }
        samples.push(start.elapsed().as_nanos() / 100);
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"case":"64_large_archived_observations","median_ns":samples[4],"prompt_bytes":crate::chat::prompt(&state,&id).unwrap().len()})
    );
}

#[test]
fn passthrough_and_lens_compose_over_the_same_entries() {
    struct Fixed;
    impl Summaries for Fixed {
        fn summary(&self, block: Block) -> Option<String> {
            Some(format!("{} earlier turns", block.len()))
        }
    }
    let (state, id) = chat(100, 10);
    let conversation = Conversation::of(&state, session(&state, &id));
    let all = Passthrough.window(&conversation, 99);
    assert_eq!((entries(&all).len(), all.omitted), (99, 0));
    let lens = Lens {
        verbatim: 8,
        summaries: &Fixed,
    }
    .window(&conversation, 99);
    let kept = entries(&lens);
    assert!(
        kept.ends_with(&(91..99).collect::<Vec<_>>()),
        "at least the newest 8 verbatim: {kept:?}"
    );
    let rendered = lens.render(&conversation);
    assert!(
        rendered[0].starts_with("Summary of entries 1–"),
        "oldest history first, summarised"
    );
    assert!(
        rendered.len() < 30,
        "O(log n) items for 99 entries: {}",
        rendered.len()
    );
    let bare = Lens {
        verbatim: 8,
        summaries: &NoSummaries,
    }
    .window(&conversation, 99);
    assert_eq!(
        entries(&bare).len() + bare.omitted,
        99,
        "without summaries older blocks are omitted"
    );
    assert!(bare.omitted >= 80);
    assert_eq!(
        Store::new(&conversation, 3).unwrap().extract().message.text,
        "turn 3 xxxxxxxxxx",
        "a conversation is a comonad space"
    );
}
