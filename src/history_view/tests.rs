use super::*;
use crate::usage::{Attempt, Tokens};

fn dm(state: &mut State, name: &str, title: &str) -> String {
    let id = crate::chat::create(state);
    let chat = state.chats.iter_mut().find(|chat| chat.id == id).unwrap();
    chat.name = Some(name.into());
    chat.title = title.into();
    id
}
fn turn<'a>(state: &'a mut State, chat: Option<&str>, kind: RunKind, status: &str, model: &str) -> &'a mut Run {
    state.runtime.enqueue(format!("{} turn", kind.as_str()), kind, "test");
    let run = state.runtime.runs.last_mut().unwrap();
    run.chat_id = chat.map(Into::into);
    run.status = status.into();
    run.model = Some(model.into());
    run
}
fn filter(query: &str) -> Filter {
    Filter::from(&crate::fields(query))
}
fn get(state: &mut State, target: &str) -> String {
    let mut response = Vec::new();
    assert!(crate::session_http::handle(&mut response, state, "GET", target, "none", "", &[]));
    String::from_utf8(response).unwrap()
}
fn rows(html: &str) -> usize {
    html.matches("<details class='history-dm'").count()
}
fn turn_rows(html: &str) -> usize {
    html.matches("<li class='history-turn'").count()
}

#[test]
fn one_view_lists_dms_newest_first_with_summaries_and_lazy_turns() {
    let mut state = State::default();
    let old = dm(&mut state, "Old <dm>", "First objective");
    let new = dm(&mut state, "Newer", "Survey tunnels");
    for status in ["completed", "failed", "completed"] {
        turn(&mut state, Some(&old), RunKind::Chat, status, "gpt-x");
    }
    turn(&mut state, Some(&new), RunKind::BotWake, "running", "claude-y").attempts.push(Attempt {
        tokens: Some(Tokens { input_tokens: 1200, cached_input_tokens: 1000, output_tokens: 300 }),
        ..Default::default()
    });
    let html = page(&state, &Filter::default());
    assert!(!html.contains(">Conversations<") && !html.contains(">Runs<") && !html.contains("aria-label='History'"), "One view, no tabs");
    assert_eq!(rows(&html), 2);
    assert!(html.find(">Newer<").unwrap() < html.find(">Old &lt;dm&gt;<").unwrap(), "Newest activity first");
    assert!(!html.contains("Old <dm>"));
    assert!(html.contains("<span class='dm-count'>3 turns · <b>1 failed</b></span>"));
    assert!(html.contains("<span class='dm-count'>1 turn · 1 active</span><span class='dm-tokens'>1.5k</span>"));
    assert!(html.contains("<span class='dm-who'>claude-y</span>"));
    assert!(html.contains(&format!("data-turns='/bots/history/turns?dm={old}'")));
    assert!(html.contains(&format!("href='/bots/history?dm={old}'>Show turns</a>")), "Without the client a row links to its turns");
    assert_eq!(turn_rows(&html), 0, "Turns load when a row opens");
    assert!(html.contains("2 DMs · 4 turns"));
    for word in ["DM", "Turns"] {
        assert!(html.contains(&format!("<span>{word}</span>")), "{word}");
    }

    let response = get(&mut state, "/bots/history");
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains(&html));
    assert!(response.contains("chat-bootstrap.js"), "The client loads turns on demand");
    assert!(response.contains("href='/?view=agents' aria-current='page'"), "History is a Bots sub-page");
    assert!(response.contains("href='/bots/history' aria-current='page'>History</a>"));
    assert!(!response.contains("body-grid chat-workspace"));
    let empty = page(&State::default(), &Filter::default());
    assert!(empty.contains("No DMs yet."));
}

#[test]
fn an_opened_dm_lists_its_turns_newest_first_with_links() {
    let mut state = State::default();
    let id = dm(&mut state, "Miner", "Mine");
    turn(&mut state, Some(&id), RunKind::Chat, "completed", "m");
    let wake = turn(&mut state, Some(&id), RunKind::BotWake, "failed", "m");
    wake.attempts.push(Attempt { termination: Some(crate::attempt_control::Termination::TimedOut), elapsed_ms: 92_000, ..Default::default() });
    turn(&mut state, Some(&id), RunKind::PortDecision, "running", "m");
    let spawn = turn(&mut state, Some(&id), RunKind::Spawned, "cancelled", "m");
    spawn.attempts.push(Attempt {
        context_trace: Some(Box::new(crate::turn_context::TraceRef { generation: "g".into(), at: 0, sha256: String::new(), bytes: 0, version: 1 })),
        ..Default::default()
    });
    let spawn = spawn.id.clone();
    let html = turns(&state, &filter(&format!("dm={id}"))).unwrap();
    assert_eq!(turn_rows(&html), 4);
    let at = |label: &str| html.find(&format!("<span class='turn-kind'>{label}</span>")).unwrap_or_else(|| panic!("{label}: {html}"));
    assert!(at("spawn") < at("port decision") && at("port decision") < at("bot wake") && at("bot wake") < at("message"), "Newest first");
    assert!(html.contains("<span class='turn-status'>timed out</span>"));
    assert!(html.contains("<span class='turn-time'>1m 32s</span>"));
    assert!(html.contains(&format!("href='/dm/{id}/context?run={spawn}&amp;attempt=1'")), "Link to the turn's input");
    assert_eq!(html.matches(&format!("href='/dm/{id}' title='Open DM'>DM ↗</a>")).count(), 4);
    assert_eq!(html.matches("action='/cancel-run'").count(), 1);
    assert_eq!(html.matches("action='/retry-run'").count(), 2, "Failed and cancelled turns can be retried");
    assert!(turns(&state, &filter("dm=c-missing")).is_err());
    assert!(turns(&state, &Filter::default()).is_err());

    let response = get(&mut state, &format!("/bots/history/turns?dm={id}"));
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.ends_with(&html), "A fragment, not a page");
    assert!(!response.contains("<html"));
    assert!(get(&mut state, "/bots/history/turns?dm=c-missing").starts_with("HTTP/1.1 404"));

    // The same turns as a page, without the client.
    let single = page(&state, &filter(&format!("dm={id}")));
    assert_eq!(rows(&single), 1);
    assert!(single.contains("<details class='history-dm' role='listitem' data-dm=") && single.contains("' open><summary>"));
    assert!(single.contains(&html));
    assert!(single.contains("href='/bots/history'>← All DMs</a>"));
}

#[test]
fn filters_show_matching_turns_grouped_under_their_dms() {
    let mut state = State::default();
    let a = dm(&mut state, "Alpha", "Map the caves");
    let b = dm(&mut state, "Bravo", "Survey TUNNELS");
    let c = dm(&mut state, "Charlie", "Port watch");
    turn(&mut state, Some(&a), RunKind::Chat, "completed", "m1");
    turn(&mut state, Some(&a), RunKind::Chat, "failed", "m1");
    turn(&mut state, Some(&b), RunKind::Chat, "completed", "m1");
    turn(&mut state, Some(&c), RunKind::BotWake, "failed", "m2");
    turn(&mut state, None, RunKind::Task, "failed", "m2");

    let failed = page(&state, &filter("status=failed"));
    assert_eq!(rows(&failed), 3, "Two DMs and turns outside DMs");
    assert!(!failed.contains(">Bravo<"));
    assert!(failed.contains(">Outside DMs<"));
    assert_eq!(turn_rows(&failed), 3, "Only failed turns, already listed");
    assert!(!failed.contains("data-turns="));
    assert!(!failed.contains("data-status='completed'"));
    assert!(failed.contains("2 DMs with 3 matching turns"));
    assert!(failed.contains("<option value='failed' selected>Failed</option>"));

    let wakes = page(&state, &filter("kind=bot-wake"));
    assert_eq!((rows(&wakes), turn_rows(&wakes)), (1, 1));
    assert!(wakes.contains(">Charlie<") && wakes.contains("<option value='bot-wake' selected>bot wake</option>"));

    let model = page(&state, &filter("model=m1&status=completed"));
    assert_eq!((rows(&model), turn_rows(&model)), (2, 2));
    assert!(model.contains(">Alpha<") && model.contains(">Bravo<"));

    let search = page(&state, &filter("q=tunnels"));
    assert_eq!(rows(&search), 1, "Search matches objectives, ignoring case");
    assert!(search.contains(">Bravo<") && !search.contains("Outside DMs"));
    assert_eq!(rows(&page(&state, &filter("q=charl"))), 1, "and names");
    assert!(page(&state, &filter("q=nothing")).contains("No DMs match these filters."));

    let unknown = filter("status=bogus&kind=nope&dm=..%2F&older=%3Cx%3E");
    assert_eq!(unknown, Filter::default(), "Unknown values are ignored");
    let expanded = page(&state, &filter("expand=1"));
    assert_eq!((rows(&expanded), turn_rows(&expanded)), (4, 5), "Show turns lists every DM's turns");
}

#[test]
fn pages_and_fragments_are_bounded_and_cursors_stable() {
    let mut state = State::default();
    let busy = dm(&mut state, "Busy", "Many turns");
    for _ in 0..130 {
        turn(&mut state, Some(&busy), RunKind::BotWake, "failed", "m");
    }
    let mut ids = vec![busy.clone()];
    for index in 0..119 {
        let id = dm(&mut state, &format!("dm {index}"), "Quiet");
        turn(&mut state, Some(&id), RunKind::Chat, "completed", "m");
        ids.push(id);
    }
    ids.reverse(); // Newest activity first.
    let first = page(&state, &Filter::default());
    assert_eq!(rows(&first), PAGE);
    assert!(first.contains(&format!("href='/bots/history?older={}'>Older →</a>", ids[PAGE - 1])));
    assert!(!first.contains("← Newer"));
    let second = page(&state, &filter(&format!("older={}", ids[PAGE - 1])));
    assert_eq!(rows(&second), PAGE);
    assert!(second.contains(&format!("data-dm='{}'", ids[PAGE])) && !second.contains(&format!("data-dm='{}'", ids[PAGE - 1])));
    let last = page(&state, &filter(&format!("older={}", ids[2 * PAGE - 1])));
    assert_eq!(rows(&last), 20);
    assert!(!last.contains("Older →") && last.contains("← Newer"));
    dm(&mut state, "Brand new", "x");
    assert_eq!(page(&state, &filter(&format!("older={}", ids[PAGE - 1]))).split("<div class='history-dms'").nth(1), second.split("<div class='history-dms'").nth(1), "New DMs do not shift an older page");

    // A DM's turns arrive a bounded page at a time.
    let fragment = turns(&state, &filter(&format!("dm={busy}"))).unwrap();
    assert_eq!(turn_rows(&fragment), TURNS);
    let cursor = state.runtime.runs[130 - TURNS].id.clone();
    assert!(fragment.contains(&format!("href='/bots/history?dm={busy}&amp;before={cursor}'>Older turns →</a>")), "{fragment}");
    let older = turns(&state, &filter(&format!("dm={busy}&before={cursor}"))).unwrap();
    assert_eq!(turn_rows(&older), TURNS);
    assert!(older.contains("← Newest turns"));
    let cursor = state.runtime.runs[130 - 2 * TURNS].id.clone();
    let oldest = turns(&state, &filter(&format!("dm={busy}&before={cursor}"))).unwrap();
    assert_eq!(turn_rows(&oldest), 30);
    assert!(!oldest.contains("Older turns"));

    // Filtered rows list a few matching turns and link to the rest.
    let failed = page(&state, &filter("status=failed"));
    assert_eq!((rows(&failed), turn_rows(&failed)), (1, INLINE));
    assert!(failed.contains(&format!("href='/bots/history?status=failed&amp;dm={busy}'>All 130 matching turns →</a>")));
    let expanded = page(&state, &filter("expand=1"));
    assert_eq!(rows(&expanded), PAGE);
    assert!(turn_rows(&expanded) <= PAGE * INLINE);
    assert!(expanded.contains("expand=1&amp;older="), "Cursors keep the filters");
}
