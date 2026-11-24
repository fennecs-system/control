use super::*;
pub(super) fn fixture() -> (State, Job) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    for index in 0..32 {
        state.chats[0].messages.push(crate::chat::Message {
            speaker: Default::default(),
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
pub(super) fn hydrate(state: &State, mut job: Job) -> Job {
    let mut entries = Vec::new();
    while entries.len() < job.entries.len() {
        entries.extend(source_page(state, &job, entries.len(), (job.entries.len() - entries.len()).min(64)).unwrap());
    }
    job.entries = source::Records::Memory(entries);
    job
}
pub(super) fn scratch() -> std::path::PathBuf {
    crate::test_support::scratch("control-summary")
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
    state.runtime.compaction.views.insert(job.chat.clone(), view);
    let c = Conversation::of(&state, &state.chats[0]);
    let window = selector(&state).window(&c, 32);
    assert!(window.items.iter().any(|item| matches!(item, Item::Summary(..))));
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
    assert_ne!(node_key(&changed, Block { start: 0, level: 2 }).unwrap(), original);
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
    assert!(execute(&job, &dir, |_, _, _| panic!("cancel must prevent further calls")).is_err());
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
    assert!(calls > 10, "large leaf requests must split before inference");
    assert!(allowed(&state, &job));
    state.runtime.compaction.settings.chats.clear();
    assert!(prepare(&state).is_none());
    assert!(!allowed(&state, &job));
    state.runtime.compaction.settings.chats.push(job.chat.clone());
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
    state.runtime.compaction.views.insert(job.chat.clone(), view);
    crate::chat::send(&mut state, &job.chat, "Continue the task", "test").unwrap();
    for backend in [crate::backend::Backend::Codex, crate::backend::Backend::Claude] {
        state.runtime.backend = backend;
        let id = state.runtime.runs.last().unwrap().id().as_str().to_owned();
        state.runtime.runs.last_mut().unwrap().target.backend = backend;
        let prompt = crate::chat::prompt(&state, &id).unwrap();
        assert!(prompt.contains("LOCAL SUMMARY SENTINEL"));
        assert!(prompt.contains("Continue the task"));
        let plan = crate::turn_context::PlannedContext::prepare(&state, state.runtime.runs.len() - 1).unwrap();
        let trace = plan.persist().unwrap();
        let projected = crate::turn_context::read(&trace).unwrap();
        assert!(projected.text.contains("LOCAL SUMMARY SENTINEL"));
        assert!(projected.text.contains("Continue the task"));
    }
    state.chats[0].messages.push(crate::chat::Message {
        speaker: Default::default(),
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
    assert!(window.render(&conversation).iter().map(String::len).sum::<usize>() <= 1024);
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
            speaker: Default::default(),
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
    execute(&job, &dir, |_, _, _| panic!("completed subtrees must be reused")).unwrap();
    state.runtime.compaction.views.insert(job.chat.clone(), view);
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
fn a_local_model_chat_compacts_on_its_own_endpoint_by_default_and_its_turn_fits() {
    let (mut state, _) = fixture();
    // No operator opt-in: automatic summaries are off and list no chats.
    state.runtime.compaction.settings = Settings {
        recent_entries: 2,
        history_bytes: 6000,
        ..Default::default()
    };
    for message in &mut state.chats[0].messages {
        message.reply = "Long tool evidence ".repeat(40);
    }
    assert!(
        prepare(&state).is_none(),
        "a hosted-default chat is not compacted without opt-in"
    );
    let conversation = Conversation::of(&state, &state.chats[0]);
    let raw: usize = (0..32)
        .filter_map(|i| conversation.entry(i))
        .map(|e| e.render().len())
        .sum();
    assert!(raw > 6000, "the history outgrows the local budget");
    state.runtime.compaction.models[0].tools = true;
    state.chats[0].model = Some(crate::providers::Model::captured(
        crate::backend::Backend::Local,
        "local-qwen".into(),
    ));
    let job = hydrate(&state, prepare(&state).expect("local chats compact by default"));
    assert_eq!(job.model.id, "local-qwen");
    let dir = scratch();
    let mut models = std::collections::BTreeSet::new();
    let view = execute(&job, &dir, |model, system, input| {
        models.insert(model.id.clone());
        control_ai::model::Request {
            model: &model.model,
            system,
            input,
            limits: &model.limits,
        }
        .chat_completions()?;
        Ok(control_ai::model::Response {
            text: "Earlier entries: preserve the task and svc-a".into(),
            tokens: None,
        })
    })
    .unwrap();
    assert_eq!(
        models.into_iter().collect::<Vec<_>>(),
        vec!["local-qwen".to_owned()],
        "only the local endpoint summarizes"
    );
    state.runtime.compaction.views.insert(job.chat.clone(), view);
    let conversation = Conversation::of(&state, &state.chats[0]);
    let window = selector(&state).window(&conversation, 32);
    assert!(window.items.iter().any(|item| matches!(item, Item::Summary(..))));
    assert!(window.render(&conversation).iter().map(String::len).sum::<usize>() <= 6000);
    assert_eq!(
        status(&state.runtime, &state.chats, &job.chat)["policy"],
        "local chat model default"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// An infinite-style local chat over many turns: between turns the chat's
/// summary tree compacts at the edges of its history; inside each turn the
/// local loop's step lens keeps the long turn within the endpoint. The two
/// compose: a turn starts from the bounded history projection, and only its
/// final report (never its in-turn summaries) enters the chat history.
#[test]
fn a_long_local_chat_compacts_across_turns_and_within_each_turn() {
    use crate::local_agent::{Compactor as Steps, Host, Policy, Poll};
    use control_local::Meter;
    use serde_json::{Value, json};
    #[derive(Default)]
    struct Turn {
        steps: usize,
        requests: Vec<usize>,
        folds: usize,
    }
    impl Host for Turn {
        fn tools(&mut self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"name":"control_workspace_run","description":"Run","inputSchema":{"type":"object"}}),
            ])
        }
        fn call(&mut self, _: &str, _: &Value, _: &str) -> Value {
            json!({"content":[{"type":"text","text":"build log line\n".repeat(400)}]})
        }
        fn poll(&mut self) -> Poll {
            Poll::Continue
        }
        fn model(&mut self, body: &Value) -> io::Result<Value> {
            self.requests.push(body.to_string().len());
            self.steps += 1;
            let message = if self.steps <= 14 {
                json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("c{}",self.steps),"type":"function","function":{"name":"control_workspace_run","arguments":"{\"command\":\"make\"}"}}]})
            } else {
                json!({"role":"assistant","content":"Built and verified svc-a."})
            };
            Ok(
                json!({"choices":[{"finish_reason":if self.steps <= 14 {"tool_calls"} else {"stop"},"message":message}]}),
            )
        }
        fn summarize(&mut self, _: &str, _: usize) -> io::Result<String> {
            self.folds += 1;
            Ok("ran make; build logs were long".into())
        }
        fn record(&mut self, _: &str, _: &str, _: &str, _: &str, _: &str) {}
        fn metered(&mut self, _: &Meter) {}
    }
    let (mut state, _) = fixture();
    state.runtime.compaction.settings = Settings {
        recent_entries: 2,
        history_bytes: 6000,
        ..Default::default()
    };
    state.runtime.compaction.models[0].tools = true;
    state.chats[0].model = Some(crate::providers::Model::captured(
        crate::backend::Backend::Local,
        "local-qwen".into(),
    ));
    let limits = control_ai::model::Limits {
        context_tokens: 8192,
        output_tokens: 512,
        input_bytes: 28000,
        output_bytes: 2048,
    };
    let dir = scratch();
    let mut summarized_turns = 0;
    for turn in 0..6 {
        // Between turns: the chat's tree folds older entries at the edge.
        if let Some(job) = prepare(&state) {
            let job = hydrate(&state, job);
            let view = execute(&job, &dir, |_, _, _| {
                Ok(control_ai::model::Response {
                    text: "Earlier turns: built svc-a repeatedly".into(),
                    tokens: None,
                })
            })
            .unwrap();
            state.runtime.compaction.views.insert(job.chat.clone(), view);
        }
        let conversation = Conversation::of(&state, &state.chats[0]);
        let count = state.chats[0].messages.len();
        let window = selector(&state).window(&conversation, count);
        summarized_turns += usize::from(window.items.iter().any(|item| matches!(item, Item::Summary(..))));
        let history = window.render(&conversation).join("\n");
        assert!(
            history.len() <= 6000,
            "turn {turn}: the history projection stays bounded"
        );
        // Within the turn: fourteen bulky steps through the local step lens.
        let mut host = Turn::default();
        let prompt = format!("{history}\nCurrent request: build svc-a ({turn})");
        let outcome = crate::local_agent::run(
            &mut host,
            "m",
            &limits,
            Steps {
                input_bytes: 6000,
                output_tokens: 512,
            },
            &prompt,
            &Policy::default(),
        )
        .unwrap();
        assert_eq!(outcome.text, "Built and verified svc-a.");
        assert!(host.folds >= 1, "turn {turn}: the long turn folded older steps");
        let budget = ((limits.context_tokens - limits.output_tokens) * 3) as usize;
        assert!(
            host.requests.iter().all(|&bytes| bytes <= budget),
            "turn {turn}: every request fits"
        );
        state.chats[0].messages.push(crate::chat::Message {
            speaker: Default::default(),
            text: format!("Build svc-a ({turn})"),
            run_id: None,
            reply: outcome.text,
        });
        state.chats[0].revision += 1;
    }
    assert!(
        summarized_turns >= 5,
        "the chat tree served compacted history on later turns"
    );
    assert!(
        state.chats[0]
            .messages
            .iter()
            .all(|m| !m.reply.contains("Summary of steps")),
        "in-turn summaries stay in the turn"
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

#[test]
fn memory_command_reports_why_and_opts_a_dm_in_or_out() {
    use command::{command_with, summary};
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    let saved = std::cell::Cell::new(0);
    let save = |_: &crate::runtime::Runtime| {
        saved.set(saved.get() + 1);
        Ok(())
    };
    let run = |state: &mut State, option: &str| command_with(state, &chat, option, |s| save(s));
    // Explicit opt-in from the fixture.
    let line = run(&mut state, "").unwrap();
    assert!(
        line.starts_with(
            "Memory Tree on for this DM: compactor local-qwen (operator opt-in). 32 entries verbatim, 0 folded"
        ),
        "{line}"
    );
    assert!(line.ends_with(&memory_url(&chat)));
    // Off: listed nowhere and not local.
    state.runtime.compaction.settings = Settings {
        recent_entries: 2,
        ..Default::default()
    };
    assert!(
        run(&mut state, "")
            .unwrap()
            .starts_with("Memory Tree off for this DM (off): 32 entries, none folded")
    );
    // Local-model default, overridden by an explicit opt-out.
    state.runtime.compaction.models[0].tools = true;
    state.chats[0].model = Some(crate::providers::Model::captured(
        crate::backend::Backend::Local,
        "local-qwen".into(),
    ));
    assert!(
        run(&mut state, "")
            .unwrap()
            .contains("compactor local-qwen (local chat model default)")
    );
    let line = run(&mut state, "off").unwrap();
    assert!(line.starts_with("Memory Tree off for this DM (opted out)"), "{line}");
    assert_eq!(state.runtime.compaction.settings.opted_out, std::slice::from_ref(&chat));
    assert!(compactor_for(&state, &state.chats[0]).is_none() && prepare(&state).is_none());
    assert_eq!(status(&state.runtime, &state.chats, &chat)["policy"], "opted out");
    // Off again changes nothing and saves nothing.
    run(&mut state, "off").unwrap();
    assert_eq!(saved.get(), 1);
    // On without MODEL uses the DM's local model and clears the opt-out.
    let line = run(&mut state, "on").unwrap();
    assert!(line.contains("compactor local-qwen (operator opt-in)"), "{line}");
    let settings = &state.runtime.compaction.settings;
    assert!(settings.enabled && settings.chats == [chat.clone()] && settings.opted_out.is_empty());
    assert_eq!(settings.model.as_deref(), Some("local-qwen"));
    // Folded entries come from the published view.
    let job = hydrate(&state, prepare(&state).unwrap());
    let dir = scratch();
    let view = execute(&job, &dir, |_, _, _| {
        Ok(control_ai::model::Response {
            text: "summary".into(),
            tokens: None,
        })
    })
    .unwrap();
    state.runtime.compaction.views.insert(chat.clone(), view);
    assert!(summary(&state.runtime, &state.chats[0]).contains("2 entries verbatim, 30 folded into summaries"));
    std::fs::remove_dir_all(dir).unwrap();
    // Unknown models and stray arguments are refused without saving.
    let before = state.runtime.compaction.settings.clone();
    assert_eq!(
        run(&mut state, "on gpt-nonexistent"),
        Err("Unknown text model. Use /memory on MODEL with a text model configured in Manage → Models.")
    );
    assert_eq!(run(&mut state, "maybe"), Err(command::USAGE));
    assert_eq!(run(&mut state, "off now"), Err(command::USAGE));
    assert_eq!(state.runtime.compaction.settings, before);
    // A hosted DM with no configured compactor must name one.
    state.chats[0].model = None;
    state.runtime.compaction.settings = Settings::default();
    assert!(
        run(&mut state, "on")
            .unwrap_err()
            .starts_with("No Memory Tree model is configured")
    );
    assert!(
        run(&mut state, "on local-qwen")
            .unwrap()
            .contains("compactor local-qwen (operator opt-in)")
    );
    // A failed save keeps the previous settings.
    assert!(command_with(&mut state, &chat, "off", |_| Err(io::Error::other("disk"))).is_err());
    assert_eq!(state.runtime.compaction.settings.chats, std::slice::from_ref(&chat));
}

#[test]
fn memory_and_compact_are_answered_in_the_dm_and_kept_out_of_history() {
    let (mut state, _) = fixture();
    let chat = state.chats[0].id.clone();
    for text in ["/compact", "/memory"] {
        crate::chat::send(&mut state, &chat, text, "web").unwrap();
        let message = state.chats[0].messages.last().unwrap();
        assert!(
            message.reply.starts_with("Memory Tree on for this DM"),
            "{}",
            message.reply
        );
        assert!(crate::conversation::Entry::source(message, None).render().is_empty());
    }
    assert_eq!(
        crate::chat::send(&mut state, &chat, "/compact now", "web"),
        Err(command::USAGE.into())
    );
    assert!(
        status(&state.runtime, &state.chats, &chat)["summary"]
            .as_str()
            .unwrap()
            .starts_with("Memory Tree on")
    );
}
