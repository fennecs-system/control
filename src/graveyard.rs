//! The Bots page graveyard: bots that were stopped and spawned agent chats
//! whose work has ended, newest first. One pass over runs and chats per
//! request projects every dead chat; only one page of rows is formatted.
//! `GET /bots/graveyard` returns the next page as an HTML fragment for the
//! client's scroll loader; without the client the same cursor is a link.
use crate::{escape, State};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Rows rendered per page, server-side and per scroll fetch.
pub const PAGE: usize = 50;
/// Longest accepted search text, in characters.
pub const QUERY_CHARS: usize = 100;

/// The visitor's position in the graveyard: an optional search and the chat
/// after which the page starts. Unknown cursors start at the newest row.
#[derive(Default)]
pub struct Query {
    pub search: String,
    pub older: Option<String>,
}
impl Query {
    pub fn from(fields: &HashMap<String, String>) -> Self {
        Self {
            search: fields.get("q").map(|q| q.trim().chars().take(QUERY_CHARS).collect()).unwrap_or_default(),
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
        match run.status.as_str() {
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            "failed" | "blocked" => match run.attempts.last().and_then(|attempt| attempt.termination) {
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
    let mut graves = Vec::new();
    for (chat, callsign) in state.chats.iter().zip(callsigns) {
        let bot = bots.contains(chat.id.as_str());
        let run = latest.get(chat.id.as_str()).copied();
        // An agent chat that never ran has nothing to remember.
        if !(bot || (chat.parent_chat.is_some() && run.is_some())) || crate::bots::defined(state, &chat.id)
            || run.is_some_and(|(_, run)| crate::application::runs::status::active(&run.status)) {
            continue;
        }
        let name = crate::callsigns::label(chat, callsign);
        if !needle.is_empty() && !name.to_lowercase().contains(&needle) && !chat.title.to_lowercase().contains(&needle) {
            continue;
        }
        let stopped = run.and_then(|(_, run)| run.attempts.last()).map(|attempt| attempt.started_unix.saturating_add(attempt.elapsed_ms / 1000));
        graves.push(Grave { chat, name, bot, reason: Reason::of(run.map(|(_, run)| run), bot), stopped, order: run.map_or(0, |(index, _)| index + 1) });
    }
    graves.sort_unstable_by_key(|grave| std::cmp::Reverse((grave.stopped, grave.order)));
    graves
}

/// The rows after `query.older`, at most [`PAGE`], and the next page's cursor
/// with the number of rows after this page.
pub fn page<'g, 'a>(graves: &'g [Grave<'a>], query: &Query) -> (&'g [Grave<'a>], Option<(&'g str, usize)>) {
    let start = query.older.as_deref()
        .and_then(|id| graves.iter().position(|grave| grave.chat.id == id))
        .map_or(0, |index| index + 1);
    let end = start.saturating_add(PAGE).min(graves.len());
    let rows = &graves[start..end];
    (rows, (end < graves.len()).then(|| rows.last().map(|grave| (grave.chat.id.as_str(), graves.len() - end))).flatten())
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
    format!("{} {:02}:{:02} UTC", crate::usage::day_label(seconds), day / 3600, day % 3600 / 60)
}

fn clip(text: &str, limit: usize) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    if line.chars().count() <= limit {
        line.to_owned()
    } else {
        format!("{}…", line.chars().take(limit - 1).collect::<String>())
    }
}

const REVIVE: &str = "Open its DM. Typing there revives it: your message starts a new turn with its full history (/bot on keeps it running).";

/// One tombstone: the shared fennec skull, the name linking to its DM, why it
/// stopped, the day it stopped and a revive link. The objective, kind and
/// exact stop time are tooltips, so every stone keeps the same height.
fn row(grave: &Grave<'_>) -> String {
    let id = escape(&grave.chat.id);
    let name = escape(&grave.name);
    let objective = clip(&grave.chat.title, 120);
    let kind = if grave.bot { "bot" } else { "agent" };
    let about = if objective.is_empty() { format!("{name} ({kind}) · no objective recorded") } else { format!("{name} ({kind}) · {}", escape(&objective)) };
    let date = grave.stopped.map_or_else(|| "<span class='grave-when'>never ran</span>".to_owned(),
        |seconds| format!("<time class='grave-when' datetime='{seconds}' title='{}'>{}</time>", when(seconds), crate::usage::day_label(seconds)));
    let reason = grave.reason.label();
    format!("<li class='grave' data-reason='{reason}'><svg class='grave-skull' viewBox='0 0 64 64' aria-hidden='true'><use href='#{SKULL}'/></svg><a class='grave-name' href='/dm/{id}' title='{about}'>{name}</a><span class='grave-reason'>{reason}</span>{date}<a class='grave-revive' href='/dm/{id}' title='{REVIVE}'>revive ↗</a></li>")
}

/// The page's stones followed by the next-page slot, if any: the fragment the
/// scroll loader swaps in for the previous slot. Stones reference the skull
/// symbol [`render`] defined, and the CSS stagger depends only on a stone's
/// position in the list, so appended pages continue the layout.
pub fn rows(graves: &[Grave<'_>], query: &Query) -> String {
    let (rows, next) = page(graves, query);
    let mut html = String::with_capacity(rows.len() * 420 + 200);
    for grave in rows {
        html.push_str(&row(grave));
    }
    if let Some((next, remaining)) = next {
        let fetch = link("/bots/graveyard", &[("older", next), ("q", &query.search)]);
        let page = link("/", &[("view", "agents"), ("older", next), ("q", &query.search)]);
        html.push_str(&format!("<li class='graveyard-more' data-more='{}'><a href='{}#graveyard'>Show {} more ↓</a></li>", escape(&fetch), escape(&page), PAGE.min(remaining)));
    }
    html
}

/// The fennec fox skull's symbol id. [`render`] defines it once per page;
/// every stone, including rows a scroll fetch adds later, reuses it.
const SKULL: &str = "fennec-skull";

/// A stylised fennec fox skull in outline: huge ears, wide cheekbones, a
/// slender snout and big eye sockets, in a 64-unit square.
const SKULL_SYMBOL: &str = "<svg class='svg-defs' aria-hidden='true' focusable='false'><symbol id='fennec-skull' viewBox='0 0 64 64'><g fill='none' stroke='currentColor' stroke-width='1.6' stroke-linejoin='round' stroke-linecap='round'><path d='M20 27C14 19 8 10 4 2c9 3 18 7 24.5 13.4M44 27c6-8 12-17 16-25-9 3-18 7-24.5 13.4'/><path d='M18.5 21L9 6.5l14 8M45.5 21L55 6.5l-14 8' opacity='.55'/><path d='M20 27c-1-8 5-12 12-12s13 4 12 12c4 3 5 7 1 10-4 3-7 7-8 13-1 5-3 8-5 8s-4-3-5-8c-1-6-4-10-8-13-4-3-3-7 1-10z'/><path d='M20.5 31c0-4.5 4.5-5.5 9-3 2 2 1 6.5-2.5 7.5-4.5 1-6.5-1.5-6.5-4.5zM43.5 31c0-4.5-4.5-5.5-9-3-2 2-1 6.5 2.5 7.5 4.5 1 6.5-1.5 6.5-4.5z'/><path d='M32 37v7M30.5 50l1.5-3 1.5 3-1.5 3zM29.5 55l1 2.5M34.5 55l-1 2.5' opacity='.8'/></g></symbol></svg>";

/// An outline tombstone with the fennec skull and an [X] cross, drawn in the
/// page's purple line style. Inline so the page loads no images.
const TOMBSTONE: &str = "<svg class='tombstone' viewBox='0 0 132 156' role='img' aria-label='Tombstone with a fennec fox skull and a crossed-out mark'><path class='ts-stone' d='M18 144V58C18 31 39 10 66 10s48 21 48 48v86'/><path class='ts-inner' d='M27 144V60c0-22 17-40 39-40s39 18 39 40v84'/><use class='ts-skull' href='#fennec-skull' x='42' y='30' width='48' height='48'/><text class='ts-text' x='66' y='106'>[X]</text><text class='ts-small' x='66' y='126'>EOL</text><path class='ts-ground' d='M4 144h124M14 150h104'/></svg>";

/// The graveyard section, closing the Bots page.
pub fn render(state: &State, query: &Query) -> String {
    let graves = dead(state, &query.search);
    let total = graves.len();
    let found = if query.search.is_empty() {
        format!("{total} dead {} · newest first", if total == 1 { "bot" } else { "bots" })
    } else {
        format!("{total} {} for “{}”", if total == 1 { "match" } else { "matches" }, escape(&query.search))
    };
    let list = if graves.is_empty() {
        format!("<p class='empty'>{}</p>", if query.search.is_empty() { "No dead bots. Stopped bots and finished agents rest here." } else { "No dead bots match that search." })
    } else {
        format!("<ol class='graveyard-grid'>{}</ol>", rows(&graves, query))
    };
    let clear = if query.search.is_empty() { String::new() } else { "<a href='/?view=agents#graveyard'>Clear</a>".into() };
    format!("<section class='graveyard' id='graveyard' aria-labelledby='graveyard-title'>{SKULL_SYMBOL}<div class='graveyard-header'>{TOMBSTONE}<div class='graveyard-title'><span class='eyebrow'>// TERMINATED PROCESSES</span><h2 id='graveyard-title'>Graveyard</h2><p class='muted tiny'>{found}. <span title='{REVIVE}'>Typing in a dead bot's DM revives it.</span></p><a href='/bots/history'>Full history ↗</a></div></div><form class='graveyard-search' method='get' action='/#graveyard' role='search'><input type='hidden' name='view' value='agents'><input type='search' name='q' value='{}' maxlength='{QUERY_CHARS}' placeholder='Search by name or objective' aria-label='Search dead bots by name or objective'><button>Search</button>{clear}</form>{list}</section>",
        escape(&query.search))
}

#[cfg(test)]
mod tests;
