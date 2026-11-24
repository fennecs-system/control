use super::*;
use crate::runtime::RunStatus;
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
    run.status = RunStatus::named(status);
    run.target.model = Some(model.into());
    run
}
fn filter(query: &str) -> Filter {
    Filter::from(&crate::fields(query))
}
fn get(state: &mut State, target: &str) -> String {
    let mut response = Vec::new();
    assert!(crate::session_http::handle(
        &mut response,
        state,
        "GET",
        target,
        "none",
        "",
        &[]
    ));
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
    turn(&mut state, Some(&new), RunKind::BotWake, "running", "claude-y")
        .attempts
        .push(Attempt {
            tokens: Some(Tokens {
                input_tokens: 1200,
                cached_input_tokens: 1000,
                output_tokens: 300,
            }),
            ..Default::default()
        });
    let html = page(&state, &Filter::default());
    assert!(
        !html.contains(">Conversations<") && !html.contains(">Runs<") && !html.contains("aria-label='History'"),
        "One view, no tabs"
    );
    assert_eq!(rows(&html), 2);
    assert!(
        html.find(">Newer<").unwrap() < html.find(">Old &lt;dm&gt;<").unwrap(),
        "Newest activity first"
    );
    assert!(!html.contains("Old <dm>"));
    assert!(html.contains("<span class='dm-count'>3 turns · <b>1 failed</b></span>"));
    assert!(html.contains("<span class='dm-count'>1 turn · 1 active</span><span class='dm-tokens'>1.5k</span>"));
    assert!(html.contains("<span class='dm-who'>claude-y</span>"));
    assert!(html.contains(&format!("data-turns='/bots/history/turns?dm={old}'")));
    assert!(
        html.contains(&format!("href='/bots/history?dm={old}'>Show turns</a>")),
        "Without the client a row links to its turns"
    );
    assert_eq!(turn_rows(&html), 0, "Turns load when a row opens");
    assert!(html.contains("<p class='history-found muted tiny'>2 DMs</p>"));
    for word in ["DM", "Turns"] {
        assert!(html.contains(&format!("<span>{word}</span>")), "{word}");
    }

    let response = get(&mut state, "/bots/history");
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains(&html));
    assert!(
        response.contains("chat-bootstrap.js"),
        "The client loads turns on demand"
    );
    assert!(
        response.contains("href='/?view=agents' aria-current='page'"),
        "History is a Bots sub-page"
    );
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
    wake.attempts.push(Attempt {
        termination: Some(crate::attempt_control::Termination::TimedOut),
        elapsed_ms: 92_000,
        ..Default::default()
    });
    turn(&mut state, Some(&id), RunKind::PortDecision, "running", "m");
    let spawn = turn(&mut state, Some(&id), RunKind::Spawned, "cancelled", "m");
    spawn.attempts.push(Attempt {
        context_trace: Some(Box::new(crate::turn_context::TraceRef {
            generation: "g".into(),
            at: 0,
            sha256: String::new(),
            bytes: 0,
            version: 1,
        })),
        ..Default::default()
    });
    let spawn = spawn.id().as_str().to_owned();
    let html = turns(&state, &filter(&format!("dm={id}"))).unwrap();
    assert_eq!(turn_rows(&html), 4);
    let at = |label: &str| {
        html.find(&format!("<span class='turn-kind'>{label}</span>"))
            .unwrap_or_else(|| panic!("{label}: {html}"))
    };
    assert!(
        at("spawn") < at("port decision") && at("port decision") < at("bot wake") && at("bot wake") < at("message"),
        "Newest first"
    );
    assert!(html.contains("<span class='turn-status'>timed out</span>"));
    assert!(html.contains("<span class='turn-time'>1m 32s</span>"));
    assert!(
        html.contains(&format!("href='/dm/{id}/context?run={spawn}&amp;attempt=1'")),
        "Link to the turn's input"
    );
    assert_eq!(
        html.matches(&format!("href='/dm/{id}' title='Open DM'>DM ↗</a>"))
            .count(),
        4
    );
    assert_eq!(html.matches("action='/cancel-run'").count(), 1);
    assert_eq!(
        html.matches("action='/retry-run'").count(),
        2,
        "Failed and cancelled turns can be retried"
    );
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
    assert!(
        single.contains("<details class='history-dm' role='listitem' data-dm=") && single.contains("' open><summary>")
    );
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
    assert_eq!(
        (rows(&expanded), turn_rows(&expanded)),
        (4, 5),
        "Show turns lists every DM's turns"
    );
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
    assert!(
        second.contains(&format!("data-dm='{}'", ids[PAGE]))
            && !second.contains(&format!("data-dm='{}'", ids[PAGE - 1]))
    );
    let last = page(&state, &filter(&format!("older={}", ids[2 * PAGE - 1])));
    assert_eq!(rows(&last), 20);
    assert!(!last.contains("Older →") && last.contains("← Newer"));
    dm(&mut state, "Brand new", "x");
    assert_eq!(
        page(&state, &filter(&format!("older={}", ids[PAGE - 1])))
            .split("<div class='history-dms'")
            .nth(1),
        second.split("<div class='history-dms'").nth(1),
        "New DMs do not shift an older page"
    );

    // A DM's turns arrive a bounded page at a time.
    let fragment = turns(&state, &filter(&format!("dm={busy}"))).unwrap();
    assert_eq!(turn_rows(&fragment), TURNS);
    let cursor = state.runtime.runs[130 - TURNS].id().as_str().to_owned();
    assert!(
        fragment.contains(&format!(
            "href='/bots/history?dm={busy}&amp;before={cursor}'>Older turns →</a>"
        )),
        "{fragment}"
    );
    let older = turns(&state, &filter(&format!("dm={busy}&before={cursor}"))).unwrap();
    assert_eq!(turn_rows(&older), TURNS);
    assert!(older.contains("← Newest turns"));
    let cursor = state.runtime.runs[130 - 2 * TURNS].id().as_str().to_owned();
    let oldest = turns(&state, &filter(&format!("dm={busy}&before={cursor}"))).unwrap();
    assert_eq!(turn_rows(&oldest), 30);
    assert!(!oldest.contains("Older turns"));

    // Filtered rows list a few matching turns and link to the rest.
    let failed = page(&state, &filter("status=failed"));
    assert_eq!((rows(&failed), turn_rows(&failed)), (1, INLINE));
    assert!(failed.contains(&format!(
        "href='/bots/history?status=failed&amp;dm={busy}'>All 130 matching turns →</a>"
    )));
    let expanded = page(&state, &filter("expand=1"));
    assert_eq!(rows(&expanded), PAGE);
    assert!(turn_rows(&expanded) <= PAGE * INLINE);
    assert!(expanded.contains("expand=1&amp;older="), "Cursors keep the filters");
}

/// With `CONTROL_PREVIEW_DIR`, writes self-contained pages for screenshots:
/// the default view with one DM opened and the failed-turn filter.
#[test]
fn preview_pages() {
    let Ok(directory) = std::env::var("CONTROL_PREVIEW_DIR") else {
        return;
    };
    let mut state = State::default();
    let mut opened = String::new();
    let models = ["gpt-5.1-codex", "claude-opus", "qwen3-coder-30b"];
    let kinds = [
        RunKind::Chat,
        RunKind::BotWake,
        RunKind::PortDecision,
        RunKind::Spawned,
        RunKind::AgentMessage,
    ];
    let dms = [
        ("ore-survey", "Survey tunnel 9 and report ore counts"),
        ("mc263 farm bot", "Keep the wheat farm running"),
        ("Release notes", "Draft the 0.9 release notes"),
        ("nebula", "Benchmark the scheduler"),
        ("Docs review", "Review docs/HISTORY.md"),
        ("relay", "Watch the build port"),
        ("atlas", "Map the caves"),
    ];
    let objectives = [
        "Check the furnace output and restock coal",
        "Wake: port 25565 reported a disconnect",
        "Decide whether to open port 8080 for the preview",
        "Spawned worker: sweep tunnel 4",
        "Reply to the room about the schedule",
    ];
    for (index, (name, title)) in dms.into_iter().enumerate() {
        let id = dm(&mut state, name, title);
        if index == 1 {
            state.chats.last_mut().unwrap().persona = Some("miner".into());
        }
        for turn_index in 0..(3 + index * 2) {
            let status = match (index + turn_index) % 9 {
                0 => "failed",
                4 => "cancelled",
                _ => "completed",
            };
            let run = turn(
                &mut state,
                Some(&id),
                kinds[(index + turn_index) % kinds.len()],
                status,
                models[index % models.len()],
            );
            *run.objective = objectives[turn_index % objectives.len()].into();
            let termination =
                (status == "failed" && turn_index % 2 == 1).then_some(crate::attempt_control::Termination::TimedOut);
            run.attempts.push(Attempt {
                elapsed_ms: 4_000 + (turn_index as u64 * 37_000) % 400_000,
                tokens: Some(Tokens {
                    input_tokens: 8_000 + turn_index as u64 * 1_300,
                    cached_input_tokens: 6_000,
                    output_tokens: 600,
                }),
                termination,
                context_trace: Some(Box::new(crate::turn_context::TraceRef {
                    generation: "g".into(),
                    at: 0,
                    sha256: String::new(),
                    bytes: 0,
                    version: 1,
                })),
                ..Default::default()
            });
        }
        if index == 4 {
            opened = id;
        }
    }
    turn(&mut state, Some(&opened), RunKind::BotWake, "running", "claude-opus");
    turn(&mut state, None, RunKind::Task, "completed", "gpt-5.1-codex");
    state.runtime.notice.clear();
    let inline = |html: String| {
        let mut page = crate::render(&state, "history", "none", &html);
        for (link, css) in [
            (
                "<link rel=\"stylesheet\" href=\"/style.css\">",
                include_str!("../../assets/style.css"),
            ),
            (
                "<link rel='stylesheet' href='/collective.css'>",
                include_str!("../../assets/collective.css"),
            ),
            (
                "<link rel='stylesheet' href='/runtime.css'>",
                include_str!("../../assets/runtime.css"),
            ),
            (
                "<link rel='stylesheet' href='/workspace.css'>",
                include_str!("../../assets/workspace.css"),
            ),
            (
                "<link rel='stylesheet' href='/navigation.css'>",
                include_str!("../../assets/navigation.css"),
            ),
        ] {
            page = page.replace(link, &format!("<style>{css}</style>"));
        }
        page
    };
    // The default view as it looks once the client has opened one DM.
    let fragment = turns(&state, &filter(&format!("dm={opened}"))).unwrap();
    let default = page(&state, &Filter::default());
    let marker = format!("data-dm='{opened}' data-turns='/bots/history/turns?dm={opened}'>");
    let (head, tail) = default.split_once(&marker).unwrap();
    let (summary, rest) = tail.split_once("<div class='turn-slot'").unwrap();
    let rest = rest.split_once("</div>").unwrap().1;
    let default = format!("{head}data-dm='{opened}' open>{summary}{fragment}{rest}");
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("history.html"), inline(default)).unwrap();
    std::fs::write(
        directory.join("history-failed.html"),
        inline(page(&state, &filter("status=failed"))),
    )
    .unwrap();
}

#[test]
fn every_run_status_keeps_its_row_label_and_colour_class() {
    let mut state = State::default();
    let table = [
        ("queued", "queued", "active"),
        ("running", "running", "active"),
        ("cancelling", "cancelling", "active"),
        ("waiting", "waiting", "active"),
        ("completed", "completed", "completed"),
        ("blocked", "blocked", "failed"),
        ("failed", "failed", "failed"),
        ("interrupted", "interrupted", "failed"),
        ("cancelled", "cancelled", "cancelled"),
        ("continued", "continued", "completed"),
    ];
    for (status, label, group) in table {
        let run = turn(&mut state, None, RunKind::Chat, status, "m");
        assert_eq!((status_label(run), status_group(run)), (label, group), "{status}");
    }
    let run = turn(&mut state, None, RunKind::Chat, "failed", "m");
    run.status = RunStatus::Unrecognised("legacy".into());
    assert_eq!((status_label(run), status_group(run)), ("legacy", "completed"));
}

#[test]
fn in_dm_agrees_with_the_key_of_every_run() {
    let mut state = State::default();
    let chats = [
        None,
        Some(""),
        Some("c-1"),
        Some("c-10"),
        Some(OUTSIDE),
        Some("bad id"),
        Some("C-1"),
    ];
    for chat in chats {
        turn(&mut state, chat, RunKind::Chat, "completed", "m");
    }
    for dm in ["c-1", "c-10", OUTSIDE, "bad id", "", "c-2"] {
        let in_dm = in_dm(dm);
        for run in state.runtime.runs.iter() {
            assert_eq!(in_dm(run), key(run) == dm, "dm {dm:?}, chat {:?}", run.chat_id);
        }
    }
}
