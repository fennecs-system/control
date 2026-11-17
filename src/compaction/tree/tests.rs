use super::*;
use crate::compaction::tests::{fixture, hydrate, scratch};

fn summarise(text: &'static str) -> impl FnMut(&crate::model_calls::Endpoint, &str, &str) -> io::Result<control_ai::model::Response> {
    move |_, _, _| Ok(control_ai::model::Response { text: text.into(), tokens: None })
}
/// The fixture chat (32 entries, 2 verbatim) with a published summary tree
/// on disk: roots 0–16 (L4), 16–24 (L3), 24–28 (L2), 28–30 (L1).
fn published() -> (State, String, std::path::PathBuf) {
    let (mut state, job) = fixture();
    let dir = scratch();
    let view = execute(&job, &dir, summarise("AUTOMATIC SUMMARY")).unwrap();
    state.runtime.compaction.views.insert(job.chat.clone(), view);
    (state, job.chat, dir)
}
fn window_text(state: &State, chat: &str) -> String {
    let session = state.chats.iter().find(|c| c.id == chat).unwrap();
    let conversation = Conversation::of(state, session);
    selector(state).window(&conversation, session.messages.len()).render(&conversation).concat()
}
fn window(state: &State, chat: &str) -> Vec<Item> {
    let session = state.chats.iter().find(|c| c.id == chat).unwrap();
    let conversation = Conversation::of(state, session);
    selector(state).window(&conversation, session.messages.len()).items
}
const ROOT: Block = Block { start: 0, level: 4 };

/// A view of `n` entries with every cut block summarised, without disk.
fn synthetic(state: &mut State, n: usize, covered: usize) -> String {
    let chat = crate::chat::create(state);
    let message = crate::chat::Message { speaker: Default::default(), text: "Port 25565 relay decision".into(), run_id: None, reply: "Recorded".into() };
    state.chats.last_mut().unwrap().messages = vec![message; n];
    let nodes = cut(covered as u64, state.runtime.compaction.settings.recent_entries)
        .filter(|b| !b.is_item())
        .map(|b| ViewNode {
            key: format!("{:064x}", b.start * 64 + b.level as u64),
            start: b.start,
            level: b.level,
            input_bytes: 4000,
            response: control_ai::model::Response { text: "s".repeat(600), tokens: Some(control_ai::Tokens { input_tokens: 1000, cached_input_tokens: 0, output_tokens: 180 }) },
            created_unix: 1_763_380_000,
        })
        .collect();
    state.runtime.compaction.views.insert(chat.clone(), View {
        identity: "synthetic".into(),
        revision: 0,
        count: covered,
        source_hash: String::new(),
        actor: crate::identity::Actor::LocalOperator,
        model: "qwen-7b".into(),
        nodes,
    });
    chat
}

#[test]
fn overview_is_bounded_and_fast_for_long_histories() {
    for n in [100usize, 1000, 10_000, 100_000] {
        let mut state = State::default();
        let chat = synthetic(&mut state, n, n);
        let view = state.runtime.compaction.views.get(&chat);
        let cells = cells(view, n, 8);
        let summaries = cells.iter().filter(|c| c.kind == Kind::Summary).count();
        assert!(summaries <= 2 * (usize::BITS - n.leading_zeros()) as usize, "{n}: {summaries}");
        assert!(cells.len() <= summaries + MAX_ENTRY_CELLS);
        // Cells tile 0..n exactly, oldest first.
        assert_eq!(cells.first().unwrap().start, 0);
        assert_eq!(cells.last().unwrap().end, n as u64);
        assert!(cells.windows(2).all(|w| w[0].end == w[1].start));
        let html = render(&state, &chat, "").unwrap();
        assert!(html.contains("<h2>MemoryTree</h2>"));
        assert!(html.len() < 96 * 1024, "{n}: {} bytes", html.len());
        // Summary text is lazy: the overview carries none of it.
        assert!(!html.contains(&"s".repeat(600)));
    }
    // A compactor far behind (100 of 5,000 entries covered) still draws a
    // bounded tail: older verbatim entries share one cell.
    let mut state = State::default();
    let chat = synthetic(&mut state, 5000, 100);
    let cells = cells(state.runtime.compaction.views.get(&chat), 5000, 8);
    assert!(cells.len() <= 2 * 7 + MAX_ENTRY_CELLS);
    // cut(100, 8) keeps 92..100 verbatim; they and the uncovered entries
    // after them merge from 92.
    assert!(cells.iter().any(|c| c.kind == Kind::Entries && c.start == 92));
    assert_eq!(cells.last().unwrap().end, 5000);
}

/// `cargo test --release memory_tree_render_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "timing report"]
fn memory_tree_render_benchmark() {
    for n in [100usize, 1000, 10_000, 100_000] {
        let mut state = State::default();
        let chat = synthetic(&mut state, n, n);
        let rounds = 200;
        let started = std::time::Instant::now();
        let mut bytes = 0;
        for _ in 0..rounds {
            bytes = render(&state, &chat, "block=0.3").unwrap().len();
        }
        let cells = cells(state.runtime.compaction.views.get(&chat), n, 8).len();
        eprintln!("overview {n:>6} entries: {cells:>3} cells, {bytes:>6} bytes, {:>7.1} µs/render", started.elapsed().as_secs_f64() * 1e6 / rounds as f64);
    }
    let (state, chat, dir) = published();
    let fields = crate::fields("block=0.2");
    let rounds = 200;
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        let detail = plan(&state, &chat, &fields).unwrap();
        let nodes = load_nodes(&detail, &dir);
        assert!(detail_page(&detail, nodes).contains("AUTOMATIC SUMMARY"));
    }
    eprintln!("detail (path depth 2, 4 node files): {:.1} µs/page", started.elapsed().as_secs_f64() * 1e6 / rounds as f64);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn selecting_a_block_lazily_expands_its_children_summary_and_source() {
    let (state, chat, dir) = published();
    let overview = render(&state, &chat, "").unwrap();
    assert!(overview.contains("/memory/node?block=0.4"), "root block links to its lazy detail");
    assert!(overview.contains("target='memory-detail'"));
    assert!(!overview.contains("AUTOMATIC SUMMARY"), "the overview reads no summary text");
    let page = |query: &str| {
        let detail = plan(&state, &chat, &crate::fields(query)).unwrap();
        let nodes = load_nodes(&detail, &dir);
        detail_page(&detail, nodes)
    };
    let root = page("block=0.4");
    assert!(root.contains("AUTOMATIC SUMMARY"));
    assert!(root.contains("node?block=0.3") && root.contains("node?block=8.3"), "two children");
    assert!(root.contains("Regenerate summary"));
    assert!(root.contains("<td>automatic</td>"));
    // Descend to a leaf: its entries and original source link.
    let leaf = page("block=4.2");
    assert!(leaf.contains("0–16</a> › ") && leaf.contains("node?entry=4"));
    assert!(leaf.contains("compaction/read?root="));
    let entry = page("entry=5");
    assert!(entry.contains("Entry 5: preserve the original task"));
    assert!(entry.contains("Pin verbatim"));
    // Out-of-range or misaligned blocks are rejected.
    assert!(plan(&state, &chat, &crate::fields("block=3.2")).is_err());
    assert!(plan(&state, &chat, &crate::fields("block=0.9")).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn regenerating_with_emphasis_adds_a_selectable_revision_the_next_turn_uses() {
    let (mut state, chat, dir) = published();
    let operator = crate::identity::Actor::LocalOperator;
    assert!(window_text(&state, &chat).contains("AUTOMATIC SUMMARY"));
    let plan = surgery::plan(&state, &chat, &operator, ROOT, "  focus on the castle layout decisions ").unwrap();
    let (base, response) = surgery::run(&plan, &dir, |_, system, input| {
        assert!(system.contains("focus on the castle layout decisions"));
        assert!(system.starts_with(SYSTEM), "emphasis extends, never replaces, the summary rules");
        assert!(input.contains("Earlier child summary"));
        Ok(control_ai::model::Response { text: "CASTLE LAYOUT REVISION".into(), tokens: None })
    })
    .unwrap();
    let r1 = surgery::commit(&mut state, plan, base.clone(), response).unwrap();
    assert_eq!(r1, 1);
    assert!(window(&state, &chat).iter().any(|i| matches!(i, Item::Summary(b, text) if *b == ROOT && text.contains("CASTLE LAYOUT REVISION") && !text.contains("AUTOMATIC"))));
    // The next turn's projection (managed context and legacy prompt) uses it.
    crate::chat::send(&mut state, &chat, "Continue the build", "test").unwrap();
    let index = state.runtime.runs.len() - 1;
    let id = state.runtime.runs[index].id.clone();
    let trace = crate::turn_context::PlannedContext::prepare(&state, index).unwrap().persist().unwrap();
    assert!(crate::turn_context::read(&trace).unwrap().text.contains("CASTLE LAYOUT REVISION"));
    assert!(crate::chat::prompt(&state, &id).unwrap().contains("CASTLE LAYOUT REVISION"));
    // A second revision keeps the first; every version stays selectable.
    let plan = surgery::plan(&state, &chat, &operator, ROOT, "keep the port/relay details").unwrap();
    let (_, response) = surgery::run(&plan, &dir, summarise("PORT RELAY REVISION")).unwrap();
    assert_eq!(surgery::commit(&mut state, plan, base, response).unwrap(), 2);
    let edits = state.chats[0].memory.clone().unwrap();
    assert_eq!(edits.revisions.len(), 2);
    assert_eq!(edits.selection(ROOT), 2);
    assert!(window_text(&state, &chat).contains("PORT RELAY REVISION"));
    edit(&mut state, &chat, &operator, Op::Select { block: ROOT, revision: 1 }).unwrap();
    assert!(window_text(&state, &chat).contains("CASTLE LAYOUT REVISION"));
    edit(&mut state, &chat, &operator, Op::Select { block: ROOT, revision: 0 }).unwrap();
    let text = window_text(&state, &chat);
    assert!(text.contains("AUTOMATIC SUMMARY") && !text.contains("REVISION"));
    assert!(edit(&mut state, &chat, &operator, Op::Select { block: ROOT, revision: 9 }).is_err());
    assert!(edit(&mut state, &chat, &operator, Op::Select { block: Block { start: 16, level: 3 }, revision: 1 }).is_err(), "a revision belongs to its block");
    let edits = state.chats[0].memory.clone().unwrap();
    assert_eq!(edits.revisions.len(), 2, "selection never deletes a revision");
    assert_eq!(edits.log.iter().filter(|e| e.action == "regenerate").count(), 2);
    // The detail page marks the versions and which one is in use.
    let detail = super::plan(&state, &chat, &crate::fields("block=0.4")).unwrap();
    let html = detail_page(&detail, load_nodes(&detail, &dir));
    assert!(html.contains("<td>r1</td>") && html.contains("<td>r2</td>") && html.contains("keep the port/relay details"));
    assert!(html.contains("class='mt-current'><td>automatic</td>"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_revised_child_rebuilds_only_its_ancestors_from_the_revision() {
    let (mut state, chat, dir) = published();
    let operator = crate::identity::Actor::LocalOperator;
    let child = Block { start: 0, level: 3 };
    let plan = surgery::plan(&state, &chat, &operator, child, "keep the port/relay details").unwrap();
    let (base, response) = surgery::run(&plan, &dir, summarise("CHILD REVISION")).unwrap();
    surgery::commit(&mut state, plan, base, response).unwrap();
    assert!(!state.runtime.compaction.attempted.contains_key(&chat), "the compactor runs again");
    let job = hydrate(&state, prepare(&state).unwrap());
    let mut inputs = Vec::new();
    let view = execute(&job, &dir, |_, _, input| {
        inputs.push(input.to_owned());
        Ok(control_ai::model::Response { text: "REBUILT PARENT".into(), tokens: None })
    })
    .unwrap();
    assert_eq!(inputs.len(), 1, "only the root above the revision is rebuilt: {inputs:?}");
    assert!(inputs[0].contains("CHILD REVISION"));
    state.runtime.compaction.views.insert(chat.clone(), view);
    assert!(window_text(&state, &chat).contains("REBUILT PARENT"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pinned_entries_stay_verbatim_until_unpinned() {
    let (mut state, chat, dir) = published();
    let operator = crate::identity::Actor::LocalOperator;
    assert!(!window(&state, &chat).iter().any(|i| matches!(i, Item::Entry(3))));
    edit(&mut state, &chat, &operator, Op::Pin(3)).unwrap();
    let items = window(&state, &chat);
    let summary = items.iter().position(|i| matches!(i, Item::Summary(b, _) if *b == ROOT)).unwrap();
    assert!(matches!(items[summary + 1], Item::Entry(3)), "pinned entry follows the summary that folds it");
    assert!(window_text(&state, &chat).contains("Entry 3: preserve"));
    assert!(edit(&mut state, &chat, &operator, Op::Pin(99)).is_err());
    let html = render(&state, &chat, "").unwrap();
    assert!(html.contains("mt-pin-tick") && html.contains("1 pinned verbatim"));
    edit(&mut state, &chat, &operator, Op::Unpin(3)).unwrap();
    assert!(!window(&state, &chat).iter().any(|i| matches!(i, Item::Entry(3))));
    // Pins also hold under the default compactor (no summaries).
    let mut plain = State::default();
    let id = crate::chat::create(&mut plain);
    for i in 0..400 {
        plain.chats[0].messages.push(crate::chat::Message { speaker: Default::default(), text: format!("Note {i} {}", "x".repeat(200)), run_id: None, reply: String::new() });
    }
    assert!(!window(&plain, &id).iter().any(|i| matches!(i, Item::Entry(0))));
    edit(&mut plain, &id, &operator, Op::Pin(0)).unwrap();
    assert!(matches!(window(&plain, &id)[0], Item::Entry(0)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dropped_ranges_leave_the_agent_view_and_summaries_until_restored() {
    let (mut state, chat, dir) = published();
    let operator = crate::identity::Actor::LocalOperator;
    state.chats[0].messages[30].text = "SECRET relay password hunter2".into();
    state.chats[0].messages[0].text = "SECRET original assignment".into();
    edit(&mut state, &chat, &operator, Op::Drop { start: 30, end: 31 }).unwrap();
    edit(&mut state, &chat, &operator, Op::Drop { start: 0, end: 1 }).unwrap();
    assert!(!state.runtime.compaction.views.contains_key(&chat), "the stale view is withdrawn");
    let text = window_text(&state, &chat);
    assert!(!text.contains("SECRET"), "{text}");
    // Rebuilt summaries read a redaction marker instead of the entry.
    let job = hydrate(&state, prepare(&state).unwrap());
    let mut sources = String::new();
    let view = execute(&job, &dir, |_, _, input| {
        sources.push_str(input);
        Ok(control_ai::model::Response { text: "REDACTED REBUILD".into(), tokens: None })
    })
    .unwrap();
    assert!(!sources.contains("SECRET") && sources.contains("[Entry 1 was removed from the agent's view by the operator.]"));
    state.runtime.compaction.views.insert(chat.clone(), view);
    let text = window_text(&state, &chat);
    assert!(text.contains("REDACTED REBUILD") && !text.contains("SECRET"));
    // The turn projection drops it from history, carried instructions and
    // the original assignment.
    crate::chat::send(&mut state, &chat, "Continue", "test").unwrap();
    let index = state.runtime.runs.len() - 1;
    let id = state.runtime.runs[index].id.clone();
    let trace = crate::turn_context::PlannedContext::prepare(&state, index).unwrap().persist().unwrap();
    assert!(!crate::turn_context::read(&trace).unwrap().text.contains("SECRET"));
    assert!(!crate::chat::prompt(&state, &id).unwrap().contains("SECRET"));
    let html = render(&state, &chat, "").unwrap();
    assert!(html.contains("mt-drop-band") && html.contains("mt-dropped"));
    // Restoring returns the entries; both actions stay in the log.
    let drop = state.chats[0].memory.as_ref().unwrap().drops[0].id;
    edit(&mut state, &chat, &operator, Op::Restore(drop)).unwrap();
    assert!(edit(&mut state, &chat, &operator, Op::Restore(drop)).is_err());
    assert!(window_text(&state, &chat).contains("hunter2"));
    let edits = state.chats[0].memory.clone().unwrap();
    assert_eq!(edits.drops.len(), 2);
    assert_eq!(edits.log.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(), ["drop", "drop", "restore"]);
    assert!(edit(&mut state, &chat, &operator, Op::Drop { start: 5, end: 5 }).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn surgery_is_operator_only() {
    let (mut state, chat, dir) = published();
    let agent = crate::identity::Actor::Unknown;
    let discord = crate::identity::Actor::Discord { user_id: "123".parse().unwrap(), channel_id: "456".parse().unwrap() };
    for actor in [&agent, &discord] {
        assert_eq!(edit(&mut state, &chat, actor, Op::Pin(1)), Err("MemoryTree edits are operator-only"));
        assert_eq!(edit(&mut state, &chat, actor, Op::Drop { start: 0, end: 4 }), Err("MemoryTree edits are operator-only"));
        assert!(surgery::plan(&state, &chat, actor, ROOT, "focus").is_err());
    }
    let shared = Arc::new(Mutex::new(state));
    let fields = crate::fields("action=pin&at=1");
    assert!(post(&shared, &chat, &agent, &fields).is_err());
    assert!(regenerate(&shared, &chat, &agent, ROOT, "focus").is_err());
    let state = shared.lock().unwrap();
    assert!(state.chats[0].memory.is_none(), "rejected edits leave no trace");
    // Agents' only history tool stays read-only.
    let tools = crate::compaction::Tools;
    let definitions = crate::tool_registry::AttemptTools::definitions(&tools);
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["annotations"]["readOnlyHint"], true);
    // Invalid emphasis is refused before any model call.
    assert!(surgery::plan(&state, &chat, &crate::identity::Actor::LocalOperator, ROOT, "  ").is_err());
    assert!(surgery::plan(&state, &chat, &crate::identity::Actor::LocalOperator, ROOT, &"x".repeat(401)).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn operator_forms_edit_and_redirect_to_the_memory_tree() {
    let (state, chat, dir) = published();
    let shared = Arc::new(Mutex::new(state));
    let mut out = Vec::new();
    assert!(http(&mut out, &shared, "POST", &memory_url(&chat), "", b"action=pin&at=2"));
    let response = String::from_utf8_lossy(&out);
    assert!(response.starts_with("HTTP/1.1 303"), "{response}");
    assert!(response.to_lowercase().contains(&format!("location: {}?entry=2", memory_url(&chat))), "{response}");
    assert!(shared.lock().unwrap().chats[0].memory.as_ref().unwrap().pinned(2));
    let mut out = Vec::new();
    assert!(http(&mut out, &shared, "GET", &format!("{}/node", memory_url(&chat)), "", b""));
    let page = String::from_utf8_lossy(&out);
    assert!(page.contains("Pinned entries") && page.contains("Edit log"));
    assert!(page.contains("frame-ancestors 'self'"), "detail pages load only in Control's frame");
    let mut out = Vec::new();
    http(&mut out, &shared, "POST", &memory_url(&chat), "", b"action=unknown");
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"));
    assert!(!http(&mut Vec::new(), &shared, "GET", "/dm/../memory/node", "", b""));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn edits_survive_a_state_round_trip_and_old_state_has_none() {
    let (mut state, chat, dir) = published();
    edit(&mut state, &chat, &crate::identity::Actor::LocalOperator, Op::Pin(4)).unwrap();
    let json = serde_json::to_value(&state.chats[0]).unwrap();
    assert_eq!(json["memory"]["pins"], serde_json::json!([4]));
    let back: crate::chat::Session = serde_json::from_value(json).unwrap();
    assert!(back.memory.unwrap().pinned(4));
    let mut old = serde_json::to_value(&state.chats[0]).unwrap();
    old.as_object_mut().unwrap().remove("memory");
    let back: crate::chat::Session = serde_json::from_value(old.clone()).unwrap();
    assert!(back.memory.is_none());
    state.chats[0].memory = None;
    assert!(serde_json::to_value(&state.chats[0]).unwrap().get("memory").is_none(), "unedited chats save unchanged");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A self-contained scratch render (inline styles, detail frame as srcdoc)
/// for `control browser capture`:
/// `MEMORY_TREE_PREVIEW=/abs/preview.html cargo test memory_tree_preview -- --ignored`
#[test]
#[ignore = "writes a preview file"]
fn memory_tree_preview() {
    let target = std::env::var("MEMORY_TREE_PREVIEW").expect("MEMORY_TREE_PREVIEW=/absolute/preview.html");
    let (mut state, _) = fixture();
    let message = |i: usize| crate::chat::Message { speaker: Default::default(), text: format!("Entry {i}: castle wall at ({}, 64, {}) via relay port {}", i * 3, i * 7, 25565 + i % 4), run_id: None, reply: "Placed blocks and verified the route".into() };
    state.chats[0].messages = (0..213).map(message).collect();
    state.runtime.compaction.settings.recent_entries = 6;
    let chat = state.chats[0].id.clone();
    let dir = scratch();
    let job = hydrate(&state, prepare(&state).unwrap());
    let view = execute(&job, &dir, |_, _, input| {
        Ok(control_ai::model::Response {
            text: format!("Built the castle's outer wall and gatehouse; relay on port 25565 forwards to the lobby. ({} input bytes summarised.)", input.len()),
            tokens: Some(control_ai::Tokens { input_tokens: input.len() as u64, cached_input_tokens: 0, output_tokens: 40 + input.len() as u64 % 90 }),
        })
    })
    .unwrap();
    state.runtime.compaction.views.insert(chat.clone(), view);
    let operator = crate::identity::Actor::LocalOperator;
    let focus = Block { start: 64, level: 6 };
    let plan = surgery::plan(&state, &chat, &operator, focus, "focus on the castle layout decisions").unwrap();
    let (base, response) = surgery::run(&plan, &dir, summarise("Castle layout: keep at (384, 64, 896); four towers; gatehouse faces the relay spawn; moat deferred.")).unwrap();
    surgery::commit(&mut state, plan, base, response).unwrap();
    edit(&mut state, &chat, &operator, Op::Pin(37)).unwrap();
    edit(&mut state, &chat, &operator, Op::Pin(150)).unwrap();
    edit(&mut state, &chat, &operator, Op::Select { block: focus, revision: 1 }).unwrap();
    let drops = state.runtime.compaction.views.remove(&chat);
    edit(&mut state, &chat, &operator, Op::Drop { start: 196, end: 199 }).unwrap();
    state.runtime.compaction.views.insert(chat.clone(), drops.unwrap());
    let query = format!("block={}.{}", focus.start, focus.level);
    let detail = plan_detail(&state, &chat, &query);
    let detail = detail_page(&detail, load_nodes(&detail, &dir));
    let styles = format!("{}{}", include_str!("../../../assets/style.css"), include_str!("../../../assets/runtime.css"));
    let detail = detail.replace("<link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/runtime.css'>", &format!("<style>{styles}</style>"));
    let overview = render(&state, &chat, &query).unwrap();
    let src = format!("src='{}'", crate::escape(&format!("{}/node?{query}", memory_url(&chat))));
    assert!(overview.contains(&src));
    let overview = overview.replace(&src, &format!("srcdoc='{}'", crate::escape(&detail)));
    std::fs::write(&target, format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><title>MemoryTree preview</title><style>{styles}</style></head><body><main>{overview}</main></body></html>")).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
fn plan_detail(state: &State, chat: &str, query: &str) -> Detail {
    plan(state, chat, &crate::fields(query)).unwrap()
}

#[test]
fn civil_time_is_utc() {
    assert_eq!(when(0), "1970-01-01 00:00 UTC");
    assert_eq!(when(1_763_380_800), "2025-11-17 12:00 UTC");
    assert_eq!(when(951_782_400), "2000-02-29 00:00 UTC");
}
