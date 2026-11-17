use super::*;
use crate::attempt_control::Termination;
use crate::runtime::RunKind;
use crate::usage::Attempt;

/// A chat with one finished run; `bot` marks the run as a bot turn.
pub(crate) fn grave(state: &mut State, title: &str, status: &str, bot: bool, stopped: u64, termination: Option<Termination>) -> String {
    let chat = crate::chat::create(state);
    state.chats.last_mut().unwrap().title = title.into();
    if !bot {
        state.chats.last_mut().unwrap().parent_chat = Some("c-root".into());
    }
    state.runtime.enqueue(format!("turn for {title}"), RunKind::Chat, "test");
    let run = state.runtime.runs.last_mut().unwrap();
    run.chat_id = Some(chat.clone());
    run.status = status.into();
    run.attempts.push(Attempt { started_unix: stopped, elapsed_ms: 0, termination, ..Default::default() });
    if bot {
        run.bot_turn = Some(Box::new(crate::bots::Turn { bot: chat.clone(), turn: 1, reasons: Vec::new(), recovery: None }));
    }
    chat
}

fn ids(graves: &[Grave<'_>]) -> Vec<String> {
    graves.iter().map(|grave| grave.chat.id.clone()).collect()
}

#[test]
fn dead_bots_are_listed_newest_first_with_why_they_stopped() {
    let mut state = State::default();
    let plain = crate::chat::create(&mut state);
    let stopped = grave(&mut state, "Watch the build", "completed", true, 1_000, None);
    let cancelled = grave(&mut state, "Map the caves", "cancelled", true, 4_000, None);
    let timeout = grave(&mut state, "Benchmark parser", "failed", true, 3_000, Some(Termination::TimedOut));
    let failed = grave(&mut state, "Deploy preview", "failed", true, 2_000, Some(Termination::ProviderError));
    let agent = grave(&mut state, "Spawned helper", "completed", false, 5_000, None);
    let running = grave(&mut state, "Still going", "running", true, 6_000, None);
    let live = grave(&mut state, "Persistent watcher", "completed", true, 7_000, None);
    crate::bots::command(&mut state, &live, "on every 10").unwrap();
    let graves = dead(&state, "");
    assert_eq!(ids(&graves), [agent.clone(), cancelled.clone(), timeout.clone(), failed.clone(), stopped.clone()]);
    let reasons = graves.iter().map(|grave| grave.reason).collect::<Vec<_>>();
    assert_eq!(reasons, [Reason::Finished, Reason::Cancelled, Reason::Timeout, Reason::Failed, Reason::Stopped]);
    assert!(!graves[0].bot && graves[1].bot);
    for excluded in [&plain, &running, &live] {
        assert!(!ids(&graves).contains(excluded), "plain chats, active turns and defined bots are not dead");
    }
    let html = render(&state, &Query::default());
    assert!(html.contains("class='tombstone'") && html.contains("[X]"), "the header draws the tombstone inline");
    assert!(!html.contains("<img") && !html.contains("cyan"), "no images or cyan accents");
    let rows = html.split("<li class='grave'").skip(1).collect::<Vec<_>>();
    assert_eq!(rows.len(), 5);
    assert!(rows[1].starts_with(" data-reason='cancelled'><svg class='grave-skull' viewBox='0 0 64 64' aria-hidden='true'><use href='#fennec-skull'/></svg>"));
    assert!(rows[1].contains("Map the caves") && rows[1].contains("<span class='grave-reason'>cancelled</span>"));
    assert!(rows[2].contains("<span class='grave-reason'>timeout</span>") && rows[3].contains("<span class='grave-reason'>failed</span>") && rows[4].contains("<span class='grave-reason'>stopped</span>"));
    assert!(rows[1].contains("1970-01-01 01:06 UTC"), "stop time is its last attempt's end");
    assert!(rows[1].contains(&format!("class='grave-revive' href='/dm/{cancelled}' title='Open its DM. Typing there revives it")));
    assert!(html.contains("href='/bots/history'>Full history"));
    assert!(html.contains("5 dead bots · newest first"));
    // Stopping a live bot moves it to the graveyard at once.
    crate::bots::stop_all(&mut state, std::slice::from_ref(&live));
    assert_eq!(dead(&state, "")[0].chat.id, live);
}

#[test]
fn pages_are_bounded_and_cursors_and_search_select_rows() {
    let mut state = State::default();
    let mut chats = Vec::new();
    for index in 0..(PAGE * 2 + 7) {
        let title = if index % 10 == 0 { format!("Tunnel survey {index}") } else { format!("Ordinary job {index}") };
        chats.push(grave(&mut state, &title, "completed", true, 10_000 + index as u64, None));
    }
    chats.reverse();
    let graves = dead(&state, "");
    let first = rows(&graves, &Query::default());
    assert_eq!(first.matches("<li class='grave'").count(), PAGE);
    assert!(first.contains(&format!("data-more='/bots/graveyard?older={}'", chats[PAGE - 1])));
    assert!(first.contains(&format!("href='/?view=agents&amp;older={}#graveyard'>Show {PAGE} more", chats[PAGE - 1])));
    let second = rows(&graves, &Query { older: Some(chats[PAGE - 1].clone()), ..Default::default() });
    assert!(second.contains(&format!("/dm/{}'", chats[PAGE])) && !second.contains(&format!("/dm/{}'", chats[PAGE - 1])));
    assert!(second.contains("Show 7 more"));
    let last = rows(&graves, &Query { older: Some(chats[2 * PAGE - 1].clone()), ..Default::default() });
    assert_eq!(last.matches("<li class='grave'").count(), 7);
    assert!(!last.contains("data-more"), "the last page has no next slot");
    let unknown = rows(&graves, &Query { older: Some("c-missing".into()), ..Default::default() });
    assert_eq!(unknown, first, "an unknown cursor starts at the newest row");

    let query = Query::from(&crate::fields("view=agents&q=TUNNEL+survey"));
    assert_eq!(query.search, "TUNNEL survey");
    let found = dead(&state, &query.search);
    assert_eq!(found.len(), 11, "search matches objectives case-insensitively");
    let name = crate::callsigns::name(&state.chats, &chats[3]).unwrap().into_owned();
    let by_name = dead(&state, &name.to_uppercase());
    assert!(ids(&by_name).contains(&chats[3]), "and names");
    assert!(by_name.iter().all(|grave| grave.name.to_lowercase().contains(&name.to_lowercase())));
    let html = render(&state, &query);
    assert!(html.contains("11 matches for “TUNNEL survey”"));
    assert!(html.contains("value='TUNNEL survey'"));
    assert!(html.contains("href='/?view=agents#graveyard'>Clear"));
    let long = Query::from(&crate::fields(&format!("q={}&older=bad/id", "x".repeat(500))));
    assert_eq!(long.search.chars().count(), QUERY_CHARS);
    assert_eq!(long.older, None, "cursors must be chat ids");
    let hostile = render(&state, &Query { search: "<script>'".into(), ..Default::default() });
    assert!(!hostile.contains("<script>") && hostile.contains("No dead bots match that search."));
    // A search keeps its query on the next-page links.
    let mut many = State::default();
    for index in 0..(PAGE + 1) {
        grave(&mut many, &format!("Tunnel {index}"), "completed", true, index as u64, None);
    }
    let graves = dead(&many, "tunnel");
    assert!(rows(&graves, &Query { search: "tunnel".into(), ..Default::default() }).contains("&amp;q=tunnel"));
}

#[test]
fn bots_page_routes_serve_the_graveyard_and_its_next_pages() {
    let mut state = State::default();
    let mut newest = String::new();
    for index in 0..(PAGE + 3) {
        newest = grave(&mut state, &format!("Job {index}"), "cancelled", true, index as u64 * 60, None);
    }
    let page = crate::public_render(&state, &crate::fields("view=agents"), "none");
    assert!(page.contains("<section class='graveyard' id='graveyard'"));
    assert_eq!(page.matches("<li class='grave'").count(), PAGE, "only the first page renders server-side");
    assert!(page.contains("<nav class='tabs' aria-label='Bots'><a href='/?view=agents' aria-current='page'>Live</a><a href='/bots/history'>History</a>"));
    assert!(!page.contains("<span>History</span>"), "History is not a main navigation item");
    assert!(page.find(&format!("/dm/{newest}'")) < page.find("Job 3'"), "newest first");
    assert!(!page.contains("Job 2'"), "older rows wait for the next page");
    let searched = crate::public_render(&state, &crate::fields("view=agents&q=job+5"), "none");
    assert!(searched.contains("value='job 5'"));
    assert_eq!(searched.matches("<li class='grave'").count(), 4, "Job 5 and Job 50–52");
    let cursor = dead(&state, "")[PAGE - 1].chat.id.clone();
    let mut response = Vec::new();
    assert!(crate::session_http::handle(&mut response, &mut state, "GET", &format!("/bots/graveyard?older={cursor}"), "none", "", &[]));
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(response.matches("<li class='grave'").count(), 3);
    assert!(!response.contains("<html") && !response.contains("data-more"), "a fragment of rows, ending the list");
}

#[test]
fn tombstones_form_a_grid_sharing_one_fennec_skull() {
    let mut state = State::default();
    let mut newest = String::new();
    for index in 0..(PAGE + 5) {
        newest = grave(&mut state, &format!("Dig tunnel {index}"), "failed", true, 86_400 * 365 + index as u64 * 60, Some(Termination::TimedOut));
    }
    let page = crate::public_render(&state, &crate::fields("view=agents"), "none");
    let html = render(&state, &Query::default());
    assert!(html.contains("<ol class='graveyard-grid'><li class='grave' data-reason='timeout'>"), "one grid of stones, not table rows");
    assert!(!html.contains("graveyard-rows") && !html.contains("grave-x"));
    // The search form stays above the stones.
    assert!(html.find("class='graveyard-search'") < html.find("class='graveyard-grid'"));
    // The skull is defined once and every stone (and the header) reuses it.
    assert_eq!(page.matches("<symbol").count(), 1);
    assert_eq!(page.matches("id='fennec-skull'").count(), 1);
    assert_eq!(page.matches("href='#fennec-skull'").count(), PAGE + 1, "50 stones and the header tombstone");
    assert!(!html.split("</symbol>").nth(1).unwrap().contains("<path d="), "stones and the header carry no skull paths");
    assert!(html.contains("class='ts-skull' href='#fennec-skull'"), "the header tombstone shows the fennec skull too");
    // One stone: skull, name to its DM, why and when it stopped, revive.
    let stone = html.split("<li class='grave'").nth(1).unwrap().split("</li>").next().unwrap();
    let name = crate::callsigns::name(&state.chats, &newest).unwrap().into_owned();
    assert!(stone.contains(&format!("<a class='grave-name' href='/dm/{newest}' title='{name} (bot) · Dig tunnel {}'>{name}</a>", PAGE + 4)));
    assert!(stone.contains("<span class='grave-reason'>timeout</span>"));
    assert!(stone.contains("<time class='grave-when' datetime='31539240' title='1971-01-01 00:54 UTC'>1971-01-01</time>"), "a short date; the time is a tooltip");
    assert!(stone.ends_with(&format!("<a class='grave-revive' href='/dm/{newest}' title='{REVIVE}'>revive ↗</a>")));
    // Scroll fetches append stones that reuse the page's symbol.
    let cursor = dead(&state, "")[PAGE - 1].chat.id.clone();
    let more = rows(&dead(&state, ""), &Query { older: Some(cursor), ..Default::default() });
    assert_eq!(more.matches("<li class='grave'").count(), 5);
    assert_eq!(more.matches("href='#fennec-skull'").count(), 5);
    assert!(!more.contains("<symbol") && !more.contains("<path"), "fragments never redefine the skull");
    // Search results use the same stones.
    let found = render(&state, &Query { search: "tunnel 7".into(), ..Default::default() });
    assert!(found.contains("<ol class='graveyard-grid'><li class='grave'") && found.matches("id='fennec-skull'").count() == 1);
    let empty = render(&State::default(), &Query::default());
    assert!(empty.contains("id='fennec-skull'"), "the header skull needs the symbol even with no stones");
}

#[test]
fn stagger_continues_across_appended_pages() {
    // Odd rows hold one stone fewer and start half a stone in, so the first
    // stone of each odd row repeats every 2c-1 stones for c columns. The rule
    // depends only on a stone's position in the list, so stones the scroll
    // loader appends continue the pattern.
    let css = include_str!("../../assets/runtime.css");
    let graveyard = css.lines().find(|line| line.starts_with(".graveyard{")).unwrap();
    for (columns, query) in [(2, "width<420px"), (3, "420px<=width<560px"), (4, "560px<=width<700px"), (5, "700px<=width<840px"), (6, "840px<=width<980px"), (7, "width>=980px")] {
        let block = graveyard.split(&format!("@container graveyard ({query}){{")).nth(1).unwrap_or_else(|| panic!("{columns} columns at {query}"));
        let block = block.split("}}").next().unwrap();
        assert!(block.contains(&format!(".grave:nth-child({}n+{}){{grid-column:2/span 2", 2 * columns - 1, columns + 1)), "{columns} columns");
        if columns > 2 {
            assert!(block.starts_with(&format!(".graveyard-grid{{grid-template-columns:repeat({},minmax(0,1fr))}}", 2 * columns)));
        }
    }
    assert!(graveyard.contains(".graveyard-grid{list-style:none;padding:0 4px;margin:0;display:grid;grid-template-columns:repeat(4,minmax(0,1fr));"));
    assert!(graveyard.contains(".grave{grid-column:span 2;") && graveyard.contains(".graveyard-more{grid-column:1/-1;"));
    assert!(!graveyard.contains("cyan") && !graveyard.contains("8cdacb"), "greys and purples only");
}
