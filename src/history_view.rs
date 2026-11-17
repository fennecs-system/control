//! Bots / History: one view of every DM and its turns. Internal names stay
//! run and chat; this page says DM and turn.
//!
//! Rows are DMs, newest activity first, [`PAGE`] per page. Each row is a
//! `<details>`; the client loads its turns from `GET /bots/history/turns?dm=ID`
//! the first time it opens, and without the client its link opens the DM's
//! turns as a page (`?dm=ID`). Turn filters (status, kind, model, or "show
//! turns") list the matching turns grouped under their DMs, at most [`INLINE`]
//! per DM with a link to the rest. Turns outside any DM are grouped under one
//! "Outside DMs" row. A request costs one pass over runs, one over chats and a
//! sort; only one page is formatted and every fragment is bounded.
use crate::application::runs::status;
use crate::runtime::{Run, RunKind};
use crate::{escape, State};
use std::collections::{BTreeSet, HashMap, VecDeque};

/// The page.
pub const PATH: &str = "/bots/history";
/// Where run actions (cancel, retry, website tasks) return: every DM with its
/// latest turns, so the changed turn is in view.
pub const AFTER_ACTION: &str = "/bots/history?expand=1";
/// DMs per page.
pub const PAGE: usize = 50;
/// Turns per DM fragment or single-DM page.
pub const TURNS: usize = 50;
/// Matching turns shown under each DM when filtering.
pub const INLINE: usize = 5;
/// Longest accepted search text, in characters.
pub const QUERY_CHARS: usize = 100;
/// Distinct models offered by the model filter.
const MODELS: usize = 64;
/// The `dm` value naming turns that ran outside any DM.
pub const OUTSIDE: &str = "-";

/// A turn's outcome as the status filter groups it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    TimedOut,
    Cancelled,
    Active,
}
const OUTCOMES: [(Outcome, &str, &str); 5] = [
    (Outcome::Completed, "completed", "Completed"),
    (Outcome::Failed, "failed", "Failed"),
    (Outcome::TimedOut, "timed-out", "Timed out"),
    (Outcome::Cancelled, "cancelled", "Cancelled"),
    (Outcome::Active, "active", "Running or queued"),
];
impl Outcome {
    fn parse(value: &str) -> Option<Self> {
        OUTCOMES.iter().find(|(_, name, _)| *name == value).map(|(outcome, _, _)| *outcome)
    }
    fn value(self) -> &'static str {
        OUTCOMES.iter().find(|(outcome, _, _)| *outcome == self).map_or("", |(_, name, _)| name)
    }
    fn matches(self, run: &Run) -> bool {
        match self {
            Self::Completed => matches!(run.status.as_str(), "completed" | "continued"),
            Self::Failed => failed(run),
            Self::TimedOut => cut_off(run) == Some(crate::attempt_control::Termination::TimedOut),
            Self::Cancelled => run.status == "cancelled",
            Self::Active => status::active(&run.status),
        }
    }
}
/// Finished without success and not by the operator's cancel.
fn failed(run: &Run) -> bool {
    matches!(run.status.as_str(), "failed" | "blocked" | "interrupted")
}
/// How a failed turn was cut off, when its last attempt says.
fn cut_off(run: &Run) -> Option<crate::attempt_control::Termination> {
    use crate::attempt_control::Termination;
    matches!(run.status.as_str(), "failed" | "blocked")
        .then(|| run.attempts.last()?.termination)
        .flatten()
        .filter(|termination| matches!(termination, Termination::TimedOut | Termination::StepLimit))
}
/// The status a turn row shows.
fn status_label(run: &Run) -> &str {
    match cut_off(run) {
        Some(crate::attempt_control::Termination::TimedOut) => "timed out",
        Some(_) => "step limit",
        None => &run.status,
    }
}
/// The colour group of a turn row.
fn status_group(run: &Run) -> &'static str {
    if status::active(&run.status) {
        "active"
    } else if failed(run) {
        "failed"
    } else if run.status == "cancelled" {
        "cancelled"
    } else {
        "completed"
    }
}

/// Every turn kind, in the order the kind filter lists them.
const KINDS: [RunKind; 15] = [
    RunKind::Chat,
    RunKind::BotWake,
    RunKind::BotDecision,
    RunKind::PortDecision,
    RunKind::Spawned,
    RunKind::Delegated,
    RunKind::DelegationResult,
    RunKind::AgentCompletion,
    RunKind::AgentMessage,
    RunKind::RoomNotification,
    RunKind::Steering,
    RunKind::DesignAnswer,
    RunKind::Task,
    RunKind::Job,
    RunKind::SelfImprovement,
];
/// What the page calls a turn of `kind`.
pub fn kind_label(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Chat => "message",
        RunKind::Task => "task",
        RunKind::Job => "job",
        RunKind::SelfImprovement => "self-improvement",
        RunKind::Steering => "steering",
        RunKind::DesignAnswer => "design answer",
        RunKind::Delegated => "delegation",
        RunKind::Spawned => "spawn",
        RunKind::DelegationResult => "delegation result",
        RunKind::AgentCompletion => "agent completion",
        RunKind::RoomNotification => "room notification",
        RunKind::PortDecision => "port decision",
        RunKind::AgentMessage => "agent message",
        RunKind::BotDecision => "bot decision",
        RunKind::BotWake => "bot wake",
    }
}

/// The visitor's filters and position. Unknown values are ignored and
/// unknown cursors start at the newest row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Text matched against DM names and objectives.
    pub search: String,
    pub status: Option<Outcome>,
    pub kind: Option<RunKind>,
    pub model: String,
    /// List every DM's latest turns, as the old run history did.
    pub expand: bool,
    /// One DM (or [`OUTSIDE`]) with its turns.
    pub dm: Option<String>,
    /// DM page cursors.
    pub older: Option<String>,
    pub newer: Option<String>,
    /// Turn cursor within one DM: list turns older than this run.
    pub before: Option<String>,
}
fn id_field(fields: &HashMap<String, String>, key: &str) -> Option<String> {
    fields.get(key).filter(|id| crate::chat::valid_id(id)).cloned()
}
impl Filter {
    pub fn from(fields: &HashMap<String, String>) -> Self {
        Self {
            search: fields.get("q").map(|q| q.trim().chars().take(QUERY_CHARS).collect()).unwrap_or_default(),
            status: fields.get("status").and_then(|value| Outcome::parse(value)),
            kind: fields.get("kind").and_then(|value| RunKind::parse(value)),
            model: fields.get("model").map(|model| model.trim().chars().take(200).collect()).unwrap_or_default(),
            expand: fields.get("expand").is_some_and(|value| value == "1"),
            dm: fields.get("dm").filter(|id| *id == OUTSIDE || crate::chat::valid_id(id)).cloned(),
            older: id_field(fields, "older"),
            newer: id_field(fields, "newer"),
            before: id_field(fields, "before"),
        }
    }
    /// Whether the filter narrows turns, not only DMs.
    fn narrows(&self) -> bool {
        self.status.is_some() || self.kind.is_some() || !self.model.is_empty()
    }
    /// Whether rows list their turns rather than loading them on demand.
    fn grouped(&self) -> bool {
        self.narrows() || self.expand
    }
    fn matches(&self, run: &Run) -> bool {
        self.status.is_none_or(|outcome| outcome.matches(run))
            && self.kind.is_none_or(|kind| run.kind == kind)
            && (self.model.is_empty() || run.model.as_deref() == Some(self.model.as_str()))
    }
    /// The page URL with these filters (never cursors) and `extra` pairs.
    fn href(&self, extra: &[(&str, &str)]) -> String {
        let mut url = reqwest::Url::parse("http://localhost").unwrap().join(PATH).unwrap();
        {
            let mut pairs = url.query_pairs_mut();
            let status = self.status.map(Outcome::value).unwrap_or_default();
            let kind = self.kind.map(RunKind::as_str).unwrap_or_default();
            let expand = if self.expand { "1" } else { "" };
            for (key, value) in [("q", self.search.as_str()), ("status", status), ("kind", kind), ("model", &self.model), ("expand", expand)].iter().chain(extra) {
                if !value.is_empty() {
                    pairs.append_pair(key, value);
                }
            }
        }
        match url.query() {
            Some(query) if !query.is_empty() => format!("{PATH}?{query}"),
            _ => PATH.to_owned(),
        }
    }
}

/// What one DM's turns add up to.
#[derive(Default)]
struct Summary<'a> {
    turns: usize,
    failed: usize,
    active: usize,
    tokens: u64,
    measured: bool,
    latest: Option<&'a Run>,
    /// Unix seconds its latest turn ended or started.
    last: Option<u64>,
    matched: usize,
    /// The newest matching turns, oldest first, when rows list them.
    recent: VecDeque<&'a Run>,
}
/// Tokens a turn's attempts reported, input plus output, when any did.
fn tokens(run: &Run) -> Option<u64> {
    run.attempts.iter().filter_map(|attempt| attempt.tokens).map(|t| t.input_tokens.saturating_add(t.output_tokens)).reduce(u64::saturating_add)
}
/// Unix seconds the turn was queued.
fn started(run: &Run) -> Option<u64> {
    run.created_ms().map(|ms| ms / 1000).or_else(|| run.attempts.first().map(|attempt| attempt.started_unix).filter(|s| *s > 0))
}
/// Unix seconds the turn's latest attempt ended (or was last measured).
fn ended(run: &Run) -> Option<u64> {
    run.attempts.last().filter(|attempt| attempt.started_unix > 0).map(|attempt| attempt.started_unix.saturating_add(attempt.elapsed_ms / 1000)).or_else(|| started(run))
}
/// The `dm` key of a run: its chat, or [`OUTSIDE`].
fn key(run: &Run) -> &str {
    run.chat_id.as_deref().filter(|id| crate::chat::valid_id(id)).unwrap_or(OUTSIDE)
}

/// A DM row: the chat (none for [`OUTSIDE`]) and its callsign.
struct Row<'a> {
    id: &'a str,
    chat: Option<(&'a crate::chat::Session, crate::callsigns::Callsign)>,
}

/// Everything one request shows, from one pass over runs and one over chats.
struct Projection<'a> {
    rows: Vec<Row<'a>>,
    summaries: HashMap<&'a str, Summary<'a>>,
    models: BTreeSet<&'a str>,
}
fn project<'a>(state: &'a State, filter: &Filter) -> Projection<'a> {
    let grouped = filter.grouped();
    let mut summaries: HashMap<&str, Summary> = HashMap::new();
    let mut models = BTreeSet::new();
    for run in state.runtime.runs.iter() {
        let summary = summaries.entry(key(run)).or_default();
        summary.turns += 1;
        summary.failed += usize::from(failed(run));
        summary.active += usize::from(status::active(&run.status));
        if let Some(tokens) = tokens(run) {
            summary.tokens = summary.tokens.saturating_add(tokens);
            summary.measured = true;
        }
        summary.latest = Some(run);
        summary.last = summary.last.max(ended(run));
        if filter.matches(run) {
            summary.matched += 1;
            if grouped {
                if summary.recent.len() == INLINE {
                    summary.recent.pop_front();
                }
                summary.recent.push_back(run);
            }
        }
        if let Some(model) = run.model.as_deref().filter(|_| models.len() < MODELS) {
            models.insert(model);
        }
    }
    let needle = filter.search.to_lowercase();
    let mut authorize = crate::identity::authorizer();
    let mut rows = Vec::new();
    for (index, (chat, callsign)) in state.chats.iter().zip(crate::callsigns::all(&state.chats)).enumerate() {
        if !crate::chat::valid_id(&chat.id) || filter.dm.as_deref().is_some_and(|dm| dm != chat.id) {
            continue;
        }
        if grouped && summaries.get(chat.id.as_str()).is_none_or(|summary| summary.matched == 0) {
            continue;
        }
        if !needle.is_empty() && !crate::callsigns::label(chat, callsign).to_lowercase().contains(&needle) && !chat.title.to_lowercase().contains(&needle) {
            continue;
        }
        if authorize(&chat.actor, crate::identity::Action::Task).is_err() {
            continue;
        }
        rows.push((std::cmp::Reverse((chat.activity_order, index)), Row { id: &chat.id, chat: Some((chat, callsign)) }));
    }
    rows.sort_unstable_by_key(|(order, _)| *order);
    let mut rows: Vec<_> = rows.into_iter().map(|(_, row)| row).collect();
    // Turns outside DMs lead the first page; a name search never matches them.
    let outside = filter.search.is_empty()
        && filter.dm.as_deref().is_none_or(|dm| dm == OUTSIDE)
        && summaries.get(OUTSIDE).is_some_and(|summary| summary.matched > 0);
    if outside {
        rows.insert(0, Row { id: OUTSIDE, chat: None });
    }
    Projection { rows, summaries, models }
}

/// The page: heading, filters, one page of DM rows and its cursors.
pub fn page(state: &State, filter: &Filter) -> String {
    let projection = project(state, filter);
    let rows = &projection.rows;
    let find = |id: Option<&str>| id.and_then(|id| rows.iter().position(|row| row.id == id));
    let range = crate::pagination::sized(rows.len(), find(filter.older.as_deref()), find(filter.newer.as_deref()), PAGE);
    let mut html = format!("<div class='section-heading'><h2>Bots<span class='slash' aria-hidden='true'>/</span>History</h2><a href='/dms?new=1'>+ New DM</a></div>{}", crate::navigation::bots_tabs(true));
    if !state.runtime.notice.is_empty() {
        html.push_str(&format!("<p role='status'>{}</p>", escape(&state.runtime.notice)));
    }
    html.push_str(&form(filter, &projection.models));
    let turns: usize = rows.iter().filter_map(|row| projection.summaries.get(row.id)).map(|summary| summary.matched).sum();
    let dms = rows.iter().filter(|row| row.chat.is_some()).count();
    let found = if filter.narrows() { format!("{} with {}", plural(dms, "DM"), plural(turns, "matching turn")) } else { format!("{} · {}", plural(dms, "DM"), plural(turns, "turn")) };
    html.push_str(&format!("<p class='history-found muted tiny'>{found}</p>"));
    if let Some(dm) = &filter.dm {
        html.push_str(&format!("<p class='history-back tiny'><a href='{}'>← All DMs</a></p>", escape(&Filter { dm: None, before: None, ..filter.clone() }.href(&[]))));
        if rows.is_empty() {
            html.push_str(&format!("<div class='empty'>No turns match these filters in {}.</div>", if dm == OUTSIDE { "turns outside DMs".to_owned() } else { format!("DM {}", escape(dm)) }));
            return html;
        }
    }
    if rows.is_empty() {
        html.push_str(&format!("<div class='empty'>{}</div>", if *filter == Filter::default() { "No DMs yet." } else { "No DMs match these filters." }));
        return html;
    }
    html.push_str("<div class='history-dms' role='list'><div class='history-head' aria-hidden='true'><span>DM</span><span>Persona · model</span><span>Last activity</span><span>Turns</span><span>Tokens</span><span></span></div>");
    for row in &rows[range.clone()] {
        let summary = projection.summaries.get(row.id);
        html.push_str(&dm_row(state, row, summary, filter));
    }
    html.push_str("</div>");
    if filter.dm.is_none() {
        let base = filter.href(&[]);
        html.push_str(&crate::pagination::controls(&base, (range.start > 0).then(|| rows[range.start].id), (range.end < rows.len()).then(|| rows[range.end - 1].id)));
    }
    html
}

fn form(filter: &Filter, models: &BTreeSet<&str>) -> String {
    let option = |value: &str, label: &str, selected: bool| format!("<option value='{}'{}>{}</option>", escape(value), if selected { " selected" } else { "" }, escape(label));
    let statuses: String = OUTCOMES.iter().map(|(outcome, value, label)| option(value, label, filter.status == Some(*outcome))).collect();
    let kinds: String = KINDS.iter().map(|kind| option(kind.as_str(), kind_label(*kind), filter.kind == Some(*kind))).collect();
    let mut model_options: String = models.iter().map(|model| option(model, model, filter.model == *model)).collect();
    if !filter.model.is_empty() && !models.contains(filter.model.as_str()) {
        model_options.push_str(&option(&filter.model, &filter.model, true));
    }
    let dm = filter.dm.as_deref().map(|dm| format!("<input type='hidden' name='dm' value='{}'>", escape(dm))).unwrap_or_default();
    let clear = if *filter == Filter::default() { String::new() } else { format!("<a href='{PATH}'>Clear</a>") };
    format!(
        "<form class='history-filters' method='get' action='{PATH}' role='search'>{dm}<input type='search' name='q' value='{}' maxlength='{QUERY_CHARS}' placeholder='Search DMs by name or objective' aria-label='Search DMs by name or objective'><select name='status' aria-label='Turn status'>{}{statuses}</select><select name='kind' aria-label='Turn kind'>{}{kinds}</select><select name='model' aria-label='Model'>{}{model_options}</select><label class='history-expand'><input type='checkbox' name='expand' value='1'{}> Show turns</label><button>Filter</button>{clear}</form>",
        escape(&filter.search),
        option("", "Any status", filter.status.is_none()),
        option("", "Any kind", filter.kind.is_none()),
        option("", "Any model", filter.model.is_empty()),
        if filter.expand { " checked" } else { "" },
    )
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn dm_row(state: &State, row: &Row, summary: Option<&Summary>, filter: &Filter) -> String {
    let empty = Summary::default();
    let summary = summary.unwrap_or(&empty);
    let (name, open_dm, who) = match row.chat {
        Some((chat, callsign)) => {
            let model = summary.latest.and_then(|run| run.model.as_deref()).or_else(|| chat.model.as_ref().map(|model| model.model.as_str()));
            let who = [chat.persona.as_deref().map(|persona| format!("persona {persona}")), model.map(str::to_owned)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            (crate::callsigns::label(chat, callsign).into_owned(), format!("<a class='dm-open' href='/dm/{}' title='Open DM'>Open ↗</a>", escape(&chat.id)), who)
        }
        None => ("Outside DMs".to_owned(), String::new(), "tasks and jobs".to_owned()),
    };
    let mut counts = plural(summary.turns, "turn");
    if summary.failed > 0 {
        counts.push_str(&format!(" · <b>{} failed</b>", summary.failed));
    }
    if summary.active > 0 {
        counts.push_str(&format!(" · {} active", summary.active));
    }
    let tokens = if summary.measured { compact(summary.tokens) } else { "—".to_owned() };
    let when = summary.last.map(when).unwrap_or_else(|| "—".to_owned());
    let title = row.chat.map(|(chat, _)| chat.title.as_str()).filter(|title| !title.is_empty()).map(|title| format!(" title='{}'", escape(&clip(title, 300)))).unwrap_or_default();
    let summary_html = format!(
        "<summary><span class='dm-name'{title}>{}</span><span class='dm-who'>{}</span><span class='dm-when'>{when}</span><span class='dm-count'>{counts}</span><span class='dm-tokens'>{tokens}</span>{open_dm}</summary>",
        escape(&name),
        escape(&who),
    );
    let single = filter.dm.is_some();
    let (attributes, body) = if single {
        (" open".to_owned(), dm_turns(state, row.id, filter))
    } else if filter.grouped() {
        let mut body = turn_list(summary.recent.iter().rev().copied());
        if summary.matched > summary.recent.len() {
            let link = filter.href(&[("dm", row.id)]);
            let what = if filter.narrows() { "matching turns" } else { "turns" };
            body.push_str(&format!("<a class='turn-more' href='{}'>All {} {what} →</a>", escape(&link), summary.matched));
        }
        (" open".to_owned(), body)
    } else {
        let fragment = format!("{PATH}/turns?dm={}", row.id);
        let fallback = filter.href(&[("dm", row.id)]);
        (format!(" data-turns='{}'", escape(&fragment)), format!("<div class='turn-slot' data-turns-slot><a href='{}'>Show turns</a></div>", escape(&fallback)))
    };
    format!("<details class='history-dm' role='listitem' data-dm='{}'{attributes}>{summary_html}{body}</details>", escape(row.id))
}

/// One page of `dm`'s turns matching `filter`, newest first, with a link to
/// older ones.
fn dm_turns(state: &State, dm: &str, filter: &Filter) -> String {
    let mut runs = state.runtime.runs.iter().rev().filter(|run| key(run) == dm && filter.matches(run));
    if let Some(before) = filter.before.as_deref() {
        // An unknown cursor starts at the newest turn.
        let mut skipped = runs.clone();
        if skipped.any(|run| run.id == before) {
            runs = skipped;
        }
    }
    let page: Vec<&Run> = runs.by_ref().take(TURNS).collect();
    let mut html = turn_list(page.iter().copied());
    let mut links = String::new();
    if filter.before.is_some() {
        links.push_str(&format!("<a href='{}'>← Newest turns</a>", escape(&filter.href(&[("dm", dm)]))));
    }
    if let (Some(last), Some(_)) = (page.last(), runs.next()) {
        links.push_str(&format!("<a href='{}'>Older turns →</a>", escape(&filter.href(&[("dm", dm), ("before", &last.id)]))));
    }
    if !links.is_empty() {
        html.push_str(&format!("<div class='history-controls' aria-label='Turn pages'>{links}</div>"));
    }
    html
}

/// `GET /bots/history/turns?dm=ID`: the turns a DM row shows when it opens.
/// Errors for a DM that does not exist or is not visible to the operator.
pub fn turns(state: &State, filter: &Filter) -> Result<String, &'static str> {
    let dm = filter.dm.as_deref().ok_or("Choose a DM.")?;
    if dm != OUTSIDE {
        let chat = state.chats.iter().find(|chat| chat.id == dm).ok_or("DM not found.")?;
        crate::identity::authorize(&chat.actor, crate::identity::Action::Task).map_err(|_| "DM not found.")?;
    }
    Ok(dm_turns(state, dm, filter))
}

fn turn_list<'a>(runs: impl Iterator<Item = &'a Run>) -> String {
    let rows: String = runs.map(turn_row).collect();
    if rows.is_empty() {
        "<p class='muted tiny turn-none'>No turns.</p>".to_owned()
    } else {
        format!("<ol class='history-turns'>{rows}</ol>")
    }
}

fn turn_row(run: &Run) -> String {
    let chat = run.chat_id.as_deref().filter(|id| crate::chat::valid_id(id));
    let when = started(run).map(when).unwrap_or_else(|| "—".to_owned());
    let model = run.model.as_deref().unwrap_or("—");
    let usage = crate::runtime::usage(run);
    let tokens = tokens(run).map(compact).unwrap_or_else(|| "—".to_owned());
    let elapsed: u64 = run.attempts.iter().map(|attempt| attempt.elapsed_ms).sum();
    let duration = if run.attempts.is_empty() { "—".to_owned() } else { duration(elapsed) };
    let mut links = Vec::new();
    if let Some(chat) = chat {
        if let Some((index, _)) = run.attempts.iter().enumerate().rev().find(|(_, attempt)| attempt.context_trace.is_some()) {
            links.push(format!("<a href='/dm/{}/context?run={}&amp;attempt={}' title='What the model was given'>input</a>", escape(chat), escape(&run.id), index + 1));
        }
        links.push(format!("<a href='/dm/{}' title='Open DM'>DM ↗</a>", escape(chat)));
    }
    if status::active(&run.status) && run.status != "cancelling" {
        links.push(format!("<form method='post' action='/cancel-run'><input type='hidden' name='id' value='{}'><button>Cancel</button></form>", escape(&run.id)));
    }
    if status::resumable(&run.status) {
        links.push(format!("<form method='post' action='/retry-run'><input type='hidden' name='id' value='{}'><button>Retry</button></form>", escape(&run.id)));
    }
    format!(
        "<li class='history-turn' data-status='{}' data-run='{}'><span class='turn-when'>{when}</span><span class='turn-kind'>{}</span><span class='turn-status'>{}</span><span class='turn-what' title='{}'>{}</span><span class='turn-model'>{}</span><span class='turn-tokens' title='{}'>{tokens}</span><span class='turn-time'>{duration}</span><span class='turn-links'>{}</span></li>",
        status_group(run),
        escape(&run.id),
        kind_label(run.kind),
        escape(status_label(run)),
        escape(&clip(&run.objective, 300)),
        escape(&clip(&run.objective, 90)),
        escape(model),
        escape(usage.trim_start_matches(" · ")),
        links.join(""),
    )
}

fn when(seconds: u64) -> String {
    let day = seconds % 86_400;
    format!("{} {:02}:{:02}", crate::usage::day_label(seconds), day / 3600, day % 3600 / 60)
}
/// `1234567` as `1.2M`.
fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0 => format!("{ms}ms"),
        1..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}
fn clip(text: &str, limit: usize) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    if line.chars().count() <= limit {
        line.to_owned()
    } else {
        format!("{}…", line.chars().take(limit - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests;
