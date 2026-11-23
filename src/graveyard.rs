//! The Bots page graveyard: bots that were stopped and spawned agent chats
//! whose work has ended, newest first. One pass over runs and chats per
//! request projects every dead chat; only one page of rows is formatted.
//! `GET /bots/graveyard` returns the next page as an HTML fragment for the
//! client's scroll loader; without the client the same cursor is a link.
use crate::runtime::RunStatus;
use crate::{State, escape};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Rows rendered per page, server-side and per scroll fetch.
pub const PAGE: usize = 50;
/// Longest accepted search text, in characters.
pub const QUERY_CHARS: usize = 100;

/// The visitor's Bots page query: the page search, which filters live task
/// trees and the graveyard alike, and the dead chat after which the graveyard
/// page starts. Unknown cursors start at the newest row.
#[derive(Default)]
pub struct Query {
    pub search: String,
    pub older: Option<String>,
}
impl Query {
    pub fn from(fields: &HashMap<String, String>) -> Self {
        Self {
            search: fields
                .get("q")
                .map(|q| q.trim().chars().take(QUERY_CHARS).collect())
                .unwrap_or_default(),
            older: fields.get("older").filter(|id| crate::chat::valid_id(id)).cloned(),
        }
    }
}

/// Why a dead chat stopped, from its latest run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Stopped,
    Finished,
    Cancelled,
    Failed,
    Timeout,
    StepLimit,
    Interrupted,
}
impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Finished => "finished",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::StepLimit => "step limit",
            Self::Interrupted => "interrupted",
        }
    }
    fn of(run: Option<&crate::runtime::Run>, bot: bool) -> Self {
        use crate::attempt_control::Termination;
        let Some(run) = run else { return Self::Stopped };
        match &run.status {
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::Interrupted => Self::Interrupted,
            RunStatus::Failed | RunStatus::Blocked => match run.attempts.last().and_then(|attempt| attempt.termination)
            {
                Some(Termination::TimedOut) => Self::Timeout,
                Some(Termination::StepLimit) => Self::StepLimit,
                Some(Termination::Cancelled) => Self::Cancelled,
                _ => Self::Failed,
            },
            _ if bot => Self::Stopped,
            _ => Self::Finished,
        }
    }
}

/// One dead bot or agent chat.
pub struct Grave<'a> {
    pub chat: &'a crate::chat::Session,
    pub name: Cow<'a, str>,
    pub bot: bool,
    pub reason: Reason,
    /// Unix seconds its latest turn ended, when it ran one.
    pub stopped: Option<u64>,
    /// Position of its latest run: orders chats that stopped in the same second.
    order: usize,
}

/// Every dead chat matching `search`, newest first. A chat is dead when it ran
/// a bot turn or was spawned by another chat, is not a defined bot and has no
/// active run. Costs one pass over runs, one over chats, and a sort.
pub fn dead<'a>(state: &'a State, search: &str) -> Vec<Grave<'a>> {
    let mut latest: HashMap<&str, (usize, &crate::runtime::Run)> = HashMap::new();
    let mut bots: HashSet<&str> = HashSet::new();
    for (index, run) in state.runtime.runs.iter().enumerate() {
        if let Some(chat) = run.chat_id.as_deref() {
            latest.insert(chat, (index, run));
            if run.bot_turn.is_some() {
                bots.insert(chat);
            }
        }
    }
    let needle = search.to_lowercase();
    let callsigns = crate::callsigns::all(&state.chats);
    let mut graves: Vec<_> = state
        .chats
        .iter()
        .zip(callsigns)
        .filter_map(|(chat, callsign)| {
            let bot = bots.contains(chat.id.as_str());
            let run = latest.get(chat.id.as_str()).copied();
            // An agent chat that never ran has nothing to remember.
            if !(bot || (chat.parent_chat.is_some() && run.is_some()))
                || crate::bots::defined(state, &chat.id)
                || run.is_some_and(|(_, run)| run.status.is_active())
            {
                return None;
            }
            let name = crate::callsigns::label(chat, callsign);
            if !needle.is_empty()
                && !name.to_lowercase().contains(&needle)
                && !chat.title.to_lowercase().contains(&needle)
            {
                return None;
            }
            let stopped = run
                .and_then(|(_, run)| run.attempts.last())
                .map(|attempt| attempt.started_unix.saturating_add(attempt.elapsed_ms / 1000));
            Some(Grave {
                chat,
                name,
                bot,
                reason: Reason::of(run.map(|(_, run)| run), bot),
                stopped,
                order: run.map_or(0, |(index, _)| index + 1),
            })
        })
        .collect();
    graves.sort_unstable_by_key(|grave| std::cmp::Reverse((grave.stopped, grave.order)));
    graves
}

/// The rows after `query.older`, at most [`PAGE`], and the next page's cursor
/// with the number of rows after this page.
pub fn page<'g, 'a>(graves: &'g [Grave<'a>], query: &Query) -> (&'g [Grave<'a>], Option<(&'g str, usize)>) {
    let start = query
        .older
        .as_deref()
        .and_then(|id| graves.iter().position(|grave| grave.chat.id == id))
        .map_or(0, |index| index + 1);
    let end = start.saturating_add(PAGE).min(graves.len());
    let rows = &graves[start..end];
    (
        rows,
        (end < graves.len())
            .then(|| rows.last().map(|grave| (grave.chat.id.as_str(), graves.len() - end)))
            .flatten(),
    )
}

fn link(base: &str, query: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://localhost").unwrap().join(base).unwrap();
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query.iter().filter(|(_, value)| !value.is_empty()) {
            pairs.append_pair(key, value);
        }
    }
    match url.query() {
        Some(query) if !query.is_empty() => format!("{}?{query}", url.path()),
        _ => url.path().to_owned(),
    }
}

fn when(seconds: u64) -> String {
    let day = seconds % 86_400;
    format!(
        "{} {:02}:{:02} UTC",
        crate::usage::day_label(seconds),
        day / 3600,
        day % 3600 / 60
    )
}

fn clip(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= limit {
        line.to_owned()
    } else {
        format!("{}…", line.chars().take(limit - 1).collect::<String>())
    }
}

const REVIVE: &str = "Open its DM. Typing there revives it: your message starts a new turn with its full history (/bot on keeps it running).";

/// One headstone: the shared fennec skull, the name linking to its DM, why it
/// stopped, the day it stopped and a revive link. The objective, kind and
/// exact stop time are tooltips, so every stone keeps the same height.
fn row(stone: &Stone) -> String {
    let id = escape(&stone.id);
    let name = escape(&stone.name);
    let objective = &stone.objective;
    let kind = if stone.bot { "bot" } else { "agent" };
    let about = if objective.is_empty() {
        format!("{name} ({kind}) · no objective recorded")
    } else {
        format!("{name} ({kind}) · {}", escape(objective))
    };
    let date = stone.stopped.map_or_else(
        || "<span class='grave-when'>never ran</span>".to_owned(),
        |seconds| {
            format!(
                "<time class='grave-when' datetime='{seconds}' title='{}'>{}</time>",
                when(seconds),
                crate::usage::day_label(seconds)
            )
        },
    );
    let reason = stone.reason.label();
    format!(
        "<li class='grave' data-reason='{reason}'><svg class='grave-skull' viewBox='0 0 64 64' aria-hidden='true'><use href='#{SKULL}'/></svg><a class='grave-name' href='/dm/{id}' title='{about}'>{name}</a><span class='grave-reason'>{reason}</span>{date}<a class='grave-revive' href='/dm/{id}' title='{REVIVE}'>revive ↗</a></li>"
    )
}

/// The page's stones followed by the next-page slot, if any: the fragment the
/// scroll loader swaps in for the previous slot. Stones reference the skull
/// symbol [`render`] defined, and the CSS stagger depends only on a stone's
/// position in the list, so appended pages continue the layout.
#[cfg(test)]
pub fn rows(graves: &[Grave<'_>], query: &Query) -> String {
    Stones::of(graves, query).render()
}
/// One page of headstones and the next page's cursor, copied out of state so
/// the rows can be rendered after the state lock is released.
pub struct Stones {
    rows: Vec<Stone>,
    /// The last row's chat and how many rows follow this page.
    next: Option<(String, usize)>,
    search: String,
}
/// What one headstone shows.
struct Stone {
    id: String,
    name: String,
    /// The objective as the tooltip shows it.
    objective: String,
    bot: bool,
    reason: Reason,
    stopped: Option<u64>,
}
impl Stones {
    pub fn of(graves: &[Grave<'_>], query: &Query) -> Self {
        let (rows, next) = page(graves, query);
        Self {
            rows: rows
                .iter()
                .map(|grave| Stone {
                    id: grave.chat.id.clone(),
                    name: grave.name.clone().into_owned(),
                    objective: clip(&grave.chat.title, 120),
                    bot: grave.bot,
                    reason: grave.reason,
                    stopped: grave.stopped,
                })
                .collect(),
            next: next.map(|(id, remaining)| (id.to_owned(), remaining)),
            search: query.search.clone(),
        }
    }
    pub fn render(&self) -> String {
        let mut html = String::with_capacity(self.rows.len() * 420 + 200);
        for stone in &self.rows {
            html.push_str(&row(stone));
        }
        if let Some((next, remaining)) = &self.next {
            let fetch = link("/bots/graveyard", &[("older", next), ("q", &self.search)]);
            let page = link("/", &[("view", "agents"), ("older", next), ("q", &self.search)]);
            html.push_str(&format!(
                "<li class='graveyard-more' data-more='{}'><a href='{}#graveyard'>Show {} more ↓</a></li>",
                escape(&fetch),
                escape(&page),
                PAGE.min(*remaining)
            ));
        }
        html
    }
}

/// The fennec fox skull's symbol id. [`render`] defines it once per page;
/// every stone, including rows a scroll fetch adds later, reuses it.
const SKULL: &str = "fennec-skull";

/// A stylised fennec fox skull, front view, in a 64-unit square, drawn only
/// in straight segments: a cranium with a flat top and chamfered
/// corners, straight sides no wider than its shoulders, large evil eye sockets
/// set low (top edges scowling down towards the nose, flat bottoms, sharp
/// inner points), a large nasal cavity, and a muzzle tapering to a jaw whose
/// edge runs straight into a row of six pointed teeth (canines longest):
/// outline and teeth are one closed stroke. Eyes, nose and teeth keep clear
/// gaps that survive the 26px size.
const SKULL_SYMBOL: &str = "<svg class='svg-defs' aria-hidden='true' focusable='false'><symbol id='fennec-skull' viewBox='0 0 64 64'><g fill='none' stroke='currentColor' stroke-width='1.8' stroke-linejoin='miter' stroke-linecap='square'><path d='M24 10H40L51 18L51 28L47 36L43 40L41 45.5L39.5 50L38 46L36.5 48.5L35 46L33.5 48.5L32 46L30.5 48.5L29 46L27.5 48.5L26 46L24.5 50L23 45.5L21 40L17 36L13 28L13 18Z'/><path d='M17.1 22L26.4 23.2L30.1 25.75L27.3 29.5H20.8ZM46.9 22L37.6 23.2L33.9 25.75L36.7 29.5H43.2ZM32 31.2L35.85 35.6L33.65 40H30.35L28.15 35.6Z'/></g></symbol></svg>";

/// The graveyard: the ground beneath the live task trees on the Bots page,
/// with the dead bots' headstones (the page selected by `query`, whose search
/// the page's one search box sets).
#[cfg(test)]
pub fn render(state: &State, query: &Query) -> String {
    Yard::of(state, query).render()
}
/// What the graveyard shows: how many dead chats match, and one page of them.
pub struct Yard {
    total: usize,
    stones: Stones,
}
impl Yard {
    pub fn of(state: &State, query: &Query) -> Self {
        let graves = dead(state, &query.search);
        Self {
            total: graves.len(),
            stones: Stones::of(&graves, query),
        }
    }
    pub fn render(&self) -> String {
        let (total, search) = (self.total, &self.stones.search);
        let found = if search.is_empty() {
            format!(
                "{total} dead {} · newest first",
                if total == 1 { "bot" } else { "bots" }
            )
        } else {
            format!(
                "{total} dead {} for “{}”",
                if total == 1 { "match" } else { "matches" },
                escape(search)
            )
        };
        let list = if total == 0 {
            format!(
                "<p class='graveyard-empty'>{}</p>",
                if search.is_empty() {
                    "No dead bots. Stopped bots and finished agents rest here."
                } else {
                    "No dead bots match that search."
                }
            )
        } else {
            format!("<ol class='graveyard-grid'>{}</ol>", self.stones.render())
        };
        format!(
            "<section class='graveyard' id='graveyard' aria-labelledby='graveyard-title'>{SKULL_SYMBOL}<div class='graveyard-ground'><h2 id='graveyard-title'>Graveyard</h2><span>{found}. <span title='{REVIVE}'>Typing in a dead bot's DM revives it.</span></span><a href='/bots/history'>Full history ↗</a></div>{list}</section>"
        )
    }
}

/// `GET /bots/graveyard`: the scroll loader's next page, rows only.
pub(crate) const NEXT_PAGE: crate::Read<Query, Stones> = crate::Read {
    view: |state, query| Stones::of(&dead(state, &query.search), &query),
    render: |stones| crate::Reply {
        status: "200 OK",
        mime: "text/html; charset=utf-8",
        body: stones.render(),
        extra: String::new(),
        policy: control_http::Policy::Embedded,
    },
};

#[cfg(test)]
mod tests;
