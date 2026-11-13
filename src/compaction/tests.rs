use super::*;
pub(super) fn fixture() -> (State, Job) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    for index in 0..32 {
        state.chats[0].messages.push(crate::chat::Message {
            text: format!("Entry {index}: preserve the original task and resource id svc-a"),
            run_id: None,
            reply: "Verified a finding".into(),
        });
    }
    state.chats[0].revision = 1;
    state
        .runtime
        .compaction
        .models
        .push(crate::model_calls::tests::endpoint());
    state.runtime.compaction.settings = Settings {
        enabled: true,
        chats: vec![chat.clone()],
        model: Some("local-qwen".into()),
        recent_entries: 2,
        ..Default::default()
    };
    let job = hydrate(&state, prepare(&state).unwrap());
    assert_eq!(job.chat, chat);
    (state, job)
}
fn hydrate(state: &State, mut job: Job) -> Job {
    let mut entries = Vec::new();
    while entries.len() < job.entries.len() {
        entries.extend(
            source_page(
                state,
                &job,
                entries.len(),
                (job.entries.len() - entries.len()).min(64),
            )
            .unwrap(),
        );
    }
    job.entries = source::Records::Memory(entries);
    job
}
pub(super) fn scratch() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "control-summary-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
#[test]
fn bounded_tree_survives_restart_and_is_scoped_to_model_revision_and_sources() {
    let (mut state, job) = fixture();
    let dir = scratch();
    let mut calls = 0;
    let view = execute(&job, &dir, |model, system, input| {
        calls += 1;
        control_ai::model::Request {
            model: &model.model,
            system,
            input,
            limits: &model.limits,
        }
        .chat_completions()?;
        Ok(control_ai::model::Response {
            text: "Goals, constraints and svc-a retained as untrusted evidence".into(),
            tokens: None,
        })
    })
    .unwrap();
    assert!(calls > 1);
    assert!(view.nodes.len() < job.entries.len());
    let cached = execute(&job, &dir, |_, _, _| {
        panic!("warm persistent nodes must not call a model")
    })
    .unwrap();
    assert_eq!(cached.nodes.len(), view.nodes.len());
    state
        .runtime
        .compaction
        .views
        .insert(job.chat.clone(), view);
    let c = Conversation::of(&state, &state.chats[0]);
    let window = selector(&state).window(&c, 32);
    assert!(
        window
            .items
            .iter()
            .any(|item| matches!(item, Item::Summary(..)))
    );
    state.chats[0].messages[0].text = "Changed source must invalidate the view".into();
    let c = Conversation::of(&state, &state.chats[0]);
    assert!(
        selector(&state)
            .window(&c, 32)
            .items
            .iter()
            .all(|item| matches!(item, Item::Entry(_)))
    );
    let block = Block { start: 0, level: 2 };
    let original = node_key(&job, block).unwrap();
    let corrupt = &cached.nodes[0];
    std::fs::write(dir.join(format!("{}.json", corrupt.key)), b"{}").unwrap();
    assert!(
        execute(&job, &dir, |_, _, _| panic!(
            "corrupt cached evidence must be surfaced, not silently regenerated"
        ))
        .is_err()
    );
    let mut changed = job;
    changed.identity = "different-model-revision".into();
    assert_ne!(
        node_key(&changed, Block { start: 0, level: 2 }).unwrap(),
        original
    );
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn small_context_failure_and_cancel_do_not_publish_a_view_or_lose_raw_history() {
    let (state, mut job) = fixture();
    let dir = scratch();
    job.model.limits.context_tokens = 1024;
    job.model.limits.output_tokens = 256;
    job.entries = source::Records::Memory(vec!["x".repeat(1000); 32]);
    assert!(
        execute(&job, &dir, |model, system, input| {
            control_ai::model::Request {
                model: &model.model,
                system,
                input,
                limits: &model.limits,
            }
            .chat_completions()?;
            panic!("oversized input must not reach inference")
        })
        .is_err()
    );
    assert!(state.runtime.compaction.views.is_empty());
    assert_eq!(state.chats[0].messages.len(), 32);
    job.cancel.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        execute(&job, &dir, |_, _, _| panic!(
            "cancel must prevent further calls"
        ))
        .is_err()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn smaller_contexts_split_leaves_and_explicit_scope_controls_background_work() {
    let (mut state, mut job) = fixture();
    let dir = scratch();
    job.model.limits.context_tokens = 2048;
    job.model.limits.output_tokens = 256;
    job.entries = source::Records::Memory(vec!["evidence ".repeat(70); 32]);
    let mut calls = 0;
    execute(&job, &dir, |model, system, input| {
        control_ai::model::Request {
            model: &model.model,
            system,
            input,
            limits: &model.limits,
        }
        .chat_completions()?;
        calls += 1;
        Ok(control_ai::model::Response {
            text: "bounded findings".into(),
            tokens: None,
        })
    })
    .unwrap();
    assert!(
        calls > 10,
        "large leaf requests must split before inference"
    );
    assert!(allowed(&state, &job));
    state.runtime.compaction.settings.chats.clear();
    assert!(prepare(&state).is_none());
    assert!(!allowed(&state, &job));
    state
        .runtime
        .compaction
        .settings
        .chats
        .push(job.chat.clone());
    state.runtime.compaction.settings.enabled = false;
    assert!(!allowed(&state, &job));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_local_summaries_feed_both_cli_chat_projections() {
    let (mut state, job) = fixture();
    let dir = scratch();
    let view = execute(&job, &dir, |_, _, _| {
        Ok(control_ai::model::Response {
            text: "LOCAL SUMMARY SENTINEL: original constraints preserved".into(),
            tokens: None,
        })
    })
    .unwrap();
    state
        .runtime
        .compaction
        .views
        .insert(job.chat.clone(), view);
    crate::chat::send(&mut state, &job.chat, "Continue the task", "test").unwrap();
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        state.runtime.backend = backend;
        let id = state.runtime.runs.last().unwrap().id.clone();
        state.runtime.runs.last_mut().unwrap().backend = backend;
        let prompt = crate::chat::prompt(&state, &id).unwrap();
        assert!(prompt.contains("LOCAL SUMMARY SENTINEL"));
        assert!(prompt.contains("Continue the task"));
        let plan =
            crate::turn_context::PlannedContext::prepare(&state, state.runtime.runs.len() - 1)
                .unwrap();
        let trace = plan.persist().unwrap();
        let projected = crate::turn_context::read(&trace).unwrap();
        assert!(projected.text.contains("LOCAL SUMMARY SENTINEL"));
        assert!(projected.text.contains("Continue the task"));
    }
    state.chats[0].messages.push(crate::chat::Message {
        text: "New appended evidence".into(),
        run_id: None,
        reply: "A fresh result".into(),
    });
    let conversation = Conversation::of(&state, &state.chats[0]);
    let window = selector(&state).window(&conversation, state.chats[0].messages.len());
    let rendered = window.render(&conversation).concat();
    assert!(rendered.contains("LOCAL SUMMARY SENTINEL"));
    assert!(rendered.contains("New appended evidence"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fallback_obeys_the_configured_byte_budget_for_unicode_without_a_summary() {
    let (mut state, job) = fixture();
    state.runtime.compaction.settings.history_bytes = 1024;
    for message in &mut state.chats[0].messages {
        message.reply = "🦊".repeat(200);
    }
    let conversation = Conversation::of(&state, &state.chats[0]);
    let window = selector(&state).window(&conversation, 32);
    assert!(
        window
            .render(&conversation)
            .iter()
            .map(String::len)
            .sum::<usize>()
            <= 1024
    );
    assert!(window.omitted > 0);
    assert!(!state.runtime.compaction.views.contains_key(&job.chat));
}

#[test]
fn paged_history_exceeds_old_entry_and_text_limits_with_bounded_model_inputs() {
    let (mut state, _) = fixture();
    let dir = scratch();
    private_directory(&dir).unwrap();
    state.chats[0].messages.resize(
        5000,
        crate::chat::Message {
            text: "Retained constraint ".repeat(30),
            reply: "Verified finding".into(),
            run_id: None,
        },
    );
    state.runtime.compaction.settings.leaf_entries = 64;
    state.runtime.compaction.models[0].limits.context_tokens = 262144;
    state.runtime.compaction.models[0].limits.input_bytes = 200000;
    let mut job = prepare(&state).unwrap();
    let mut largest = 0;
    job.entries = source::Records::capture(job.entries.len(), &dir, |start, count| {
        let entries = source_page(&state, &job, start, count)?;
        largest = largest.max(serde_json::to_vec(&entries).unwrap().len());
        Ok(entries)
    })
    .unwrap();
    assert!(largest <= source::PAGE_BYTES);
    assert_eq!(job.entries.len(), 5000);
    let view = execute(&job, &dir, |model, system, input| {
        control_ai::model::Request {
            model: &model.model,
            system,
            input,
            limits: &model.limits,
        }
        .chat_completions()?;
        Ok(control_ai::model::Response {
            text: "Retained constraint and finding".into(),
            tokens: None,
        })
    })
    .unwrap();
    assert_eq!(view.count, 5000);
    assert!(view.nodes.len() < 20);
    execute(&job, &dir, |_, _, _| {
        panic!("completed subtrees must be reused")
    })
    .unwrap();
    state
        .runtime
        .compaction
        .views
        .insert(job.chat.clone(), view);
    let conversation = Conversation::of(&state, &state.chats[0]);
    assert!(
        selector(&state)
            .window(&conversation, 5000)
            .items
            .iter()
            .any(|item| matches!(item, Item::Summary(..)))
    );
    state.chats[0].messages[0].text = "An old source changed".into();
    let conversation = Conversation::of(&state, &state.chats[0]);
    assert!(
        selector(&state)
            .window(&conversation, 5000)
            .items
            .iter()
            .all(|item| matches!(item, Item::Entry(..)))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "explicit release benchmark; synthetic evidence and no model traffic"]
fn selection_profile() {
    use std::{hint::black_box, time::Instant};
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    for count in [32, 256, 1024] {
        let message = state.chats[0].messages[0].clone();
        state.chats[0].messages.resize(count, message);
        let job = hydrate(&state, prepare(&state).unwrap());
        let dir = scratch();
        let mut calls = 0;
        let view = execute(&job, &dir, |_, _, _| {
            calls += 1;
            Ok(control_ai::model::Response {
                text: "Concrete verified findings and pending requests".into(),
                tokens: None,
            })
        })
        .unwrap();
        state.runtime.compaction.views.insert(chat.clone(), view);
        let conversation = Conversation::of(&state, &state.chats[0]);
        let iterations = std::env::var("CONTROL_PROFILE_ITERATIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(200);
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(selector(black_box(&state)).window(&conversation, count));
            }
            samples.push(start.elapsed().as_nanos() / u128::from(iterations));
        }
        samples.sort_unstable();
        println!(
            "{}",
            serde_json::json!({"case":"compaction_selection","entries":count,"median_ns":samples[3],"mock_cold_calls":calls,"selected_nodes":state.runtime.compaction.views[&chat].nodes.len()})
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
