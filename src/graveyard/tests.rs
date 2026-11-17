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
    assert!(html.contains("11 dead matches for “TUNNEL survey”"));
    let page = crate::bots::page(&state, &query);
    assert!(page.contains("value='TUNNEL survey'") && page.contains("href='/?view=agents'>Clear"), "the page search box keeps the query");
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
fn headstones_form_a_grid_sharing_one_fennec_skull() {
    let mut state = State::default();
    let mut newest = String::new();
    for index in 0..(PAGE + 5) {
        newest = grave(&mut state, &format!("Dig tunnel {index}"), "failed", true, 86_400 * 365 + index as u64 * 60, Some(Termination::TimedOut));
    }
    let page = crate::public_render(&state, &crate::fields("view=agents"), "none");
    let html = render(&state, &Query::default());
    assert!(html.contains("<ol class='graveyard-grid'><li class='grave' data-reason='timeout'>"), "one grid of stones, not table rows");
    assert!(!html.contains("graveyard-rows") && !html.contains("grave-x") && !html.contains("tombstone"));
    // The skull is defined once and every stone reuses it.
    assert_eq!(page.matches("<symbol").count(), 1);
    assert_eq!(page.matches("id='fennec-skull'").count(), 1);
    assert_eq!(page.matches("href='#fennec-skull'").count(), PAGE, "one use per stone");
    assert!(!html.split("</symbol>").nth(1).unwrap().contains("<path d="), "stones carry no skull paths");
    // One stone: skull, name to its DM, why and when it stopped, revive.
    let stone = html.split("<li class='grave'").nth(1).unwrap().split("</li>").next().unwrap();
    let name = crate::callsigns::name(&state.chats, &newest).unwrap().into_owned();
    assert!(stone.contains(&format!("<a class='grave-name' href='/dm/{newest}' title='{name} (bot) · Dig tunnel {}'>{name}</a>", PAGE + 4)));
    assert!(stone.contains("<span class='grave-reason'>timeout</span>"));
    assert!(stone.contains("<time class='grave-when' datetime='31539240' title='1971-01-01 00:54 UTC'>1971-01-01</time>"), "a short date; the time is a tooltip");
    assert!(stone.ends_with(&format!("<a class='grave-revive' href='/dm/{newest}' title='{REVIVE}'>revive ↗</a>")));
    assert!(stone.len() < 600, "a stone stays light: {} bytes", stone.len());
    // Scroll fetches append stones that reuse the page's symbol.
    let cursor = dead(&state, "")[PAGE - 1].chat.id.clone();
    let more = rows(&dead(&state, ""), &Query { older: Some(cursor), ..Default::default() });
    assert_eq!(more.matches("<li class='grave'").count(), 5);
    assert_eq!(more.matches("href='#fennec-skull'").count(), 5);
    assert!(!more.contains("<symbol") && !more.contains("<path"), "fragments never redefine the skull");
    // Search results use the same stones.
    let found = render(&state, &Query { search: "tunnel 7".into(), ..Default::default() });
    assert!(found.contains("<ol class='graveyard-grid'><li class='grave'") && found.matches("id='fennec-skull'").count() == 1);
}

type Point = (f64, f64);
type Segment = (Point, Point);

/// The closed polygons of an absolute `M`/`L`/`H`/`V`/`Z` path, as points.
fn polygons(path: &str) -> Vec<Vec<Point>> {
    let mut shapes: Vec<Vec<Point>> = Vec::new();
    let mut rest = path;
    while let Some(command) = rest.chars().next() {
        let end = rest[1..].find(|c: char| c.is_ascii_alphabetic()).map_or(rest.len(), |index| index + 1);
        let numbers = rest[1..end].split_whitespace().map(|n| n.parse::<f64>().unwrap()).collect::<Vec<_>>();
        let last = shapes.last().and_then(|shape| shape.last()).copied().unwrap_or_default();
        match command {
            'M' => shapes.push(vec![(numbers[0], numbers[1])]),
            'L' => shapes.last_mut().unwrap().push((numbers[0], numbers[1])),
            'H' => shapes.last_mut().unwrap().push((numbers[0], last.1)),
            'V' => shapes.last_mut().unwrap().push((last.0, numbers[0])),
            'Z' => {}
            other => panic!("straight segments only, not {other}"),
        }
        rest = &rest[end..];
    }
    shapes
}
fn segments(shape: &[Point]) -> Vec<Segment> {
    (0..shape.len()).map(|index| (shape[index], shape[(index + 1) % shape.len()])).collect()
}
fn to_segment(p: Point, (a, b): Segment) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}
/// Smallest distance between the centre lines of two sets of segments that
/// do not cross: the nearest endpoint-to-segment distance.
fn clearance(one: &[Segment], other: &[Segment]) -> f64 {
    let mut best = f64::INFINITY;
    for &first in one {
        for &second in other {
            for (point, segment) in [(first.0, second), (first.1, second), (second.0, first), (second.1, first)] {
                best = best.min(to_segment(point, segment));
            }
        }
    }
    best
}

#[test]
fn the_fennec_skull_is_squarish_earless_toothed_and_clear_at_26px() {
    let symbol = SKULL_SYMBOL.split("<symbol").nth(1).unwrap();
    let paths = symbol.split(" d='").skip(1).map(|path| path.split('\'').next().unwrap()).collect::<Vec<_>>();
    assert_eq!(paths.len(), 2, "one closed outline with the teeth, and the sockets: no facet lines or lower jaw");
    assert!(SKULL_SYMBOL.contains("stroke-linejoin='miter'"), "angular joins");
    assert_eq!(SKULL_SYMBOL.matches("stroke-width").count(), 1, "one stroke width");
    assert!(SKULL_SYMBOL.contains("stroke-width='1.8'") && SKULL_SYMBOL.contains("fill='none'") && !SKULL_SYMBOL.contains("<polygon") && !SKULL_SYMBOL.contains("<rect"));
    let stroke = 1.8;
    let outline = paths[0];
    assert!(outline.ends_with('Z') && outline.matches('M').count() == 1, "one closed subpath");
    let head = polygons(outline).remove(0);
    let sockets = polygons(paths[1]);
    assert_eq!(sockets.len(), 3, "two eyes and a nose");
    let (left_eye, right_eye, nose) = (&sockets[0], &sockets[1], &sockets[2]);

    // A squarish head, not a bulb: a flat top at y 14, nothing above it, and
    // no point more than a tenth wider than the top.
    assert!(outline.starts_with("M16 14H48L"), "a flat top: {outline}");
    assert!(head.iter().all(|(_, y)| *y >= 14.0), "no ears or dome above the flat top");
    let widest = head.iter().map(|p| p.0).fold(f64::MIN, f64::max) - head.iter().map(|p| p.0).fold(f64::MAX, f64::min);
    assert!(widest <= 1.1 * (48.0 - 16.0), "widest {widest} against a top of 32");

    // Mean eyes: each slit has horizontal top and bottom edges and slants in
    // towards the nose; the nose is flat-bottomed.
    for eye in [left_eye, right_eye] {
        assert_eq!(eye.len(), 5);
        assert!(eye[0].1 == eye[1].1 && eye[3].1 == eye[4].1, "flat top and bottom: {eye:?}");
    }
    assert!(left_eye[2].0 > left_eye[1].0 && right_eye[2].0 < right_eye[1].0, "inner points aim at the nose");
    assert!(paths[1].contains("M32 32L35.5 36L33.5 40H30.5L28.5 36Z"), "the nose keeps its shape");

    // Teeth: one zigzag in the outline's own stroke, hanging below the snout.
    assert!(outline.contains("L40 46L38 51.5L36 46.5L34 49.5L32 46.5L30 49.5L28 46.5L26 51.5L24 46L"), "{outline}");
    let edges = segments(&head);
    let teeth = edges.iter().copied().filter(|(a, b)| a.1 >= 46.0 && b.1 >= 46.0).collect::<Vec<_>>();
    assert_eq!(teeth.len(), 8, "four teeth (canines and two incisors), each two strokes, wide enough to read at 26px");

    // Clear gaps that survive 26px (0.4 px per unit): between eyes and the
    // outline, eyes and the nose, the two eyes, and the nose and the teeth.
    // Distances are between stroke centre lines, so a gap of g needs g + one
    // stroke width; miter joins reach further at the teeth, hence the margin.
    let eyes = [segments(left_eye), segments(right_eye)];
    for eye in &eyes {
        let gap = clearance(eye, &edges) - stroke;
        assert!(gap >= stroke + 0.5, "eye to outline gap {gap}");
        let gap = clearance(eye, &segments(nose)) - stroke;
        assert!(gap >= stroke + 0.5, "eye to nose gap {gap}");
    }
    assert!(clearance(&eyes[0], &eyes[1]) - stroke >= stroke, "the two eyes stay apart");
    // A 45° valley's miter reaches half a stroke / sin(22.5°) past its corner.
    let miter = stroke / 2.0 / 22.5_f64.to_radians().sin();
    let gap = clearance(&segments(nose), &teeth) - stroke / 2.0 - miter;
    assert!(gap >= 1.5 * stroke, "nose to teeth gap {gap}, beyond the valleys' miter");
    let gap = clearance(&segments(nose), &edges) - stroke;
    assert!(gap >= 1.5 * stroke, "nose to snout gap {gap}");
}

#[test]
fn one_search_filters_live_trees_and_graves() {
    let mut state = State::default();
    let tunnel_dead = grave(&mut state, "Dig the north tunnel", "completed", true, 1_000, None);
    let other_dead = grave(&mut state, "Count sheep", "cancelled", true, 2_000, None);
    let tunnel_live = grave(&mut state, "Guard the tunnel mouth", "completed", true, 3_000, None);
    crate::bots::command(&mut state, &tunnel_live, "on every 10").unwrap();
    let other_live = grave(&mut state, "Water the garden", "completed", true, 4_000, None);
    crate::bots::command(&mut state, &other_live, "on every 10").unwrap();

    let page = crate::public_render(&state, &crate::fields("view=agents"), "none");
    assert_eq!(page.matches("role='search'").count(), 1, "one search box for the whole page");
    assert!(!page.contains("graveyard-search"), "the graveyard has no search of its own");
    let scene = page.split("<div class='bots-scene'>").nth(1).expect("one scene");
    assert!(scene.find("class='process-tree'") < scene.find("class='graveyard'"), "the trees stand above the graveyard");
    for chat in [&tunnel_dead, &other_dead, &tunnel_live, &other_live] {
        assert!(page.contains(&format!("/dm/{chat}'")), "unfiltered, every bot shows");
    }

    let searched = crate::public_render(&state, &crate::fields("view=agents&q=TUNNEL"), "none");
    assert!(searched.contains("value='TUNNEL'"));
    assert!(searched.contains(&format!("href='/dm/{tunnel_live}'")) && searched.contains(&format!("href='/dm/{tunnel_dead}'")), "hits live and dead");
    assert!(!searched.contains(&format!("/dm/{other_live}'")) && !searched.contains(&format!("/dm/{other_dead}'")));
    assert!(searched.contains("<svg class='searching' preserveAspectRatio") && searched.contains(" pt-match'"), "the live hit is marked");
    assert!(searched.contains("1 dead match for “TUNNEL”"));
    // A name search reaches live bots too.
    let name = crate::callsigns::name(&state.chats, &other_live).unwrap().into_owned();
    let by_name = crate::bots::page(&state, &Query { search: name.to_uppercase(), ..Default::default() });
    assert!(by_name.contains(&format!("href='/dm/{other_live}'")));

    // Empty states, per half of the scene.
    let miss = crate::public_render(&state, &crate::fields("view=agents&q=zebra"), "none");
    assert!(miss.contains("No live bots match that search.") && miss.contains("No dead bots match that search."));
    assert!(miss.contains("href='/?view=agents'>Clear"));
    let only_live = crate::bots::page(&state, &Query { search: "garden".into(), ..Default::default() });
    assert!(only_live.contains("No dead bots match that search.") && !only_live.contains("No live bots match"));
    let empty = crate::bots::page(&State::default(), &Query::default());
    assert!(empty.contains("No bots, services or active agents") && empty.contains("No dead bots. Stopped bots and finished agents rest here."));
    assert_eq!(empty.matches("id='fennec-skull'").count(), 1);
    assert!(!empty.contains("Clear"));
    for html in [&page, &searched, &miss, &empty] {
        assert!(!html.contains("cyan") && !html.contains("8cdacb"), "greys and purples only");
    }
}

#[test]
fn headstones_are_small_low_poly_stones_in_staggered_rows() {
    // Odd rows hold one stone fewer and start half a stone in, so the first
    // stone of each odd row repeats every 2c-1 stones for c columns. The rule
    // depends only on a stone's position in the list, so stones the scroll
    // loader appends continue the pattern.
    let css = include_str!("../../assets/runtime.css");
    let graveyard = css.lines().find(|line| line.starts_with(".bots-search{")).unwrap();
    let bands = graveyard.split("@container graveyard (").skip(1).collect::<Vec<_>>();
    assert_eq!(bands.len(), 9);
    for (band, columns) in bands.iter().zip(3..) {
        assert!(band.contains(&format!("{{.graveyard-grid{{grid-template-columns:repeat({},minmax(0,1fr))}}.grave:nth-child({}n+{}){{grid-column:2/span 2}}}}", 2 * columns, 2 * columns - 1, columns + 1)), "{columns} columns: {band}");
    }
    // Many stones per row: eleven on a wide screen, four on a phone.
    assert!(bands[8].starts_with("width>=1020px)") && bands[1].starts_with("320px<=width<420px)"));
    assert!(graveyard.contains(".graveyard-grid{list-style:none;padding:0 4px;margin:0;display:grid;grid-template-columns:repeat(4,minmax(0,1fr));"));
    assert!(graveyard.contains(".grave{grid-column:span 2;") && graveyard.contains(".graveyard-more{grid-column:1/-1;"));
    // A low-poly silhouette in one unbroken outline: the body's straight
    // sides rise into chamfered shoulders and a flat top, with no overhang,
    // step or seam. One six-point polygon; the inner layer repeats it.
    let shape = "clip-path:polygon(26% 0,74% 0,92% 16%,92% 100%,8% 100%,8% 16%)";
    assert!(graveyard.contains(&format!(".grave:before,.grave:after{{content:'';position:absolute;z-index:-1;{shape};")));
    assert_eq!(graveyard.matches("clip-path").count(), 1, "one shape for both layers");
    let points = shape.trim_start_matches("clip-path:polygon(").trim_end_matches(')').split(',').map(|point| {
        let (x, y) = point.split_once(' ').unwrap();
        (x.trim_end_matches('%').parse::<f64>().unwrap(), y.trim_end_matches('%').parse::<f64>().unwrap())
    }).collect::<Vec<_>>();
    assert_eq!(points.len(), 6);
    assert!(points[0].1 == 0.0 && points[1].1 == 0.0 && points[0].0 > points[5].0 && points[1].0 < points[2].0, "a flat top narrower than the body");
    assert!(points[2].0 == points[3].0 && points[4].0 == points[5].0, "straight vertical sides from shoulder to base");
    assert!(points.iter().map(|p| p.0).fold(0.0, f64::max) == points[2].0, "nothing overhangs the body");
    // Quiet at rest: a dim outline and fill, muted skull, name and date; the
    // status stays readable. Hover or keyboard focus inside a stone brings it
    // back up within about 120ms, without motion when reduced motion is asked.
    assert!(graveyard.contains(".grave:before{inset:0;background:#8f81ac4d;transition:background-color .12s}"));
    assert!(graveyard.contains(".grave-name,.grave-when{opacity:.6;") && graveyard.contains(".grave-reason{opacity:.9;"));
    for rule in [
        ".grave:hover:before,.grave:focus-within:before{background:#8f81ac8c}",
        ".grave:hover .grave-skull,.grave:focus-within .grave-skull{opacity:1;color:var(--purple)}",
        ".grave:hover :is(.grave-name,.grave-when,.grave-reason),.grave:focus-within :is(.grave-name,.grave-when,.grave-reason){opacity:1}",
        ".grave:hover .grave-revive,.grave:focus-within .grave-revive{opacity:1}",
        "@media(prefers-reduced-motion:reduce){.grave:before,.grave:after,.grave-skull,.grave-name,.grave-when,.grave-reason,.grave-revive{transition:none}}",
    ] {
        assert!(graveyard.contains(rule), "{rule}");
    }
    let stone = graveyard.split(".grave{").nth(1).unwrap().split('}').next().unwrap();
    assert!(!stone.contains("border-radius") && !graveyard.contains("50% 50%"));
    assert!(graveyard.contains(".grave-skull{width:26px;height:26px;"), "smaller than the 48px skulls before");
    assert!(!graveyard.contains("cyan") && !graveyard.contains("8cdacb"), "greys and purples only");
    // One scene: no dot grid behind the trees, no card borders between halves,
    // and a plain flat line (no gradient or glow) with room above it.
    assert!(graveyard.contains(".graveyard{margin-top:36px;padding:20px 0 18px;border-top:1px solid var(--line);"));
    assert!(!graveyard.contains("gradient"), "no gradients in the scene");
    let tree = css.lines().find(|line| line.starts_with(".process-tree{")).unwrap();
    assert!(!tree.contains("radial-gradient") && !tree.contains("cyan") && !tree.contains("8cdacb"));
    assert!(!graveyard.contains(".graveyard{margin-top:44px;padding-top:26px;border-top"));
}
