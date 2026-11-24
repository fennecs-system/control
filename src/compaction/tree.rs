//! Memory Tree: a DM's compaction tree drawn as blocks.
//!
//! The overview is rendered from memory only (the published view's O(log n)
//! roots plus at most `MAX_ENTRY_CELLS` recent entries), so its cost does not
//! grow with history. A block's detail (children, summary text, versions,
//! source preview) loads lazily into a frame when the operator selects it,
//! reading at most one root-to-block path of node files outside the state
//! lock. No scripts: links, a named frame and forms.
use super::*;
use crate::identity::Actor;
use crate::store::Store;
use std::fmt::Write as _;

/// Recent verbatim cells drawn individually; older verbatim entries share one.
const MAX_ENTRY_CELLS: usize = 24;
/// Bytes of summary or entry text a detail page shows before linking to recall.
const TEXT_BYTES: usize = 16384;
const LEFT: f64 = 20.0;
const SPAN: f64 = 960.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// A summarised block of the published view.
    Summary,
    /// A block the next compaction will summarise (no view, or not yet built).
    Pending,
    /// One entry the agent sees verbatim.
    Entry,
    /// Several older verbatim entries drawn as one cell.
    Entries,
}
pub(super) struct Cell {
    pub start: u64,
    pub end: u64,
    pub level: u32,
    pub kind: Kind,
    node: Option<Facts>,
}
/// What the overview shows of a summarised block: its size and age, not its text.
#[derive(Clone, Copy)]
struct Facts {
    output_tokens: Option<u64>,
    bytes: usize,
    input_bytes: usize,
    created_unix: u64,
}
impl From<&ViewNode> for Facts {
    fn from(node: &ViewNode) -> Self {
        Self {
            output_tokens: node.response.tokens.map(|t| t.output_tokens),
            bytes: node.response.text.len(),
            input_bytes: node.input_bytes,
            created_unix: node.created_unix,
        }
    }
}
impl Cell {
    fn block(&self) -> Block {
        Block {
            start: self.start,
            level: self.level,
        }
    }
}

/// The cut the next turn starts from, oldest first, with bounded verbatim cells.
pub(super) fn cells(view: Option<&View>, len: usize, recent: u64) -> Vec<Cell> {
    let view = view.filter(|v| v.count <= len);
    let covered = view.map_or(len, |v| v.count) as u64;
    let mut cells: Vec<Cell> = cut(covered, recent)
        .map(|block| {
            let node = view.and_then(|v| {
                v.nodes
                    .iter()
                    .find(|n| n.start == block.start && n.level == block.level)
                    .map(Facts::from)
            });
            let kind = if block.is_item() {
                Kind::Entry
            } else if node.is_some() {
                Kind::Summary
            } else {
                Kind::Pending
            };
            Cell {
                start: block.start,
                end: block.end(),
                level: block.level,
                kind,
                node,
            }
        })
        .collect();
    cells.reverse();
    cells.extend((covered..len as u64).map(|at| Cell {
        start: at,
        end: at + 1,
        level: 0,
        kind: Kind::Entry,
        node: None,
    }));
    let entries = cells.iter().filter(|c| c.kind == Kind::Entry).count();
    if entries > MAX_ENTRY_CELLS {
        // Verbatim entries are the newest positions, so they are contiguous
        // at the end; merge the oldest of them into one cell.
        let first = cells.iter().position(|c| c.kind == Kind::Entry).unwrap();
        let merged = entries - (MAX_ENTRY_CELLS - 1);
        let start = cells[first].start;
        let end = cells[first + merged - 1].end;
        cells.splice(
            first..first + merged,
            [Cell {
                start,
                end,
                level: 0,
                kind: Kind::Entries,
                node: None,
            }],
        );
    }
    cells
}

fn tokens(node: &Facts) -> String {
    match node.output_tokens {
        Some(t) => format!("{} tok", compact(t)),
        None => format!("{} B", compact(node.bytes as u64)),
    }
}
fn compact(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}
/// `YYYY-MM-DD HH:MM UTC` from Unix seconds (civil-from-days).
pub(super) fn when(unix: u64) -> String {
    let days = (unix / 86400) as i64;
    let (h, m) = ((unix % 86400) / 3600, (unix % 3600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02} UTC")
}
fn esc(s: &str) -> String {
    crate::escape(s)
}
fn node_href(chat: &str, cell: &Cell) -> String {
    match cell.kind {
        Kind::Entry | Kind::Entries => format!("{}/node?entry={}", memory_url(chat), cell.start),
        _ => format!("{}/node?block={}.{}", memory_url(chat), cell.start, cell.level),
    }
}

/// The overview diagram: the cut as blocks above the journal they cover.
pub(super) fn svg(
    chat: &str,
    cells: &[Cell],
    len: usize,
    edits: &Edits,
    model: &str,
    focus: Option<(u64, u32)>,
) -> String {
    let len = len.max(1) as f64;
    let weight = |c: &Cell| match c.kind {
        Kind::Summary | Kind::Pending => 2.0 + 0.6 * c.level as f64,
        Kind::Entries => 2.0,
        Kind::Entry => 1.8,
    };
    let gap = if cells.len() > 40 { 2.0 } else { 5.0 };
    let total: f64 = cells.iter().map(weight).sum();
    let free = SPAN - gap * cells.len().saturating_sub(1) as f64;
    let ruler = |at: u64| LEFT + at as f64 / len * SPAN;
    let mut out = String::with_capacity(512 + cells.len() * 700);
    let _ = write!(
        out,
        "<svg class='mt-svg' xmlns='http://www.w3.org/2000/svg' viewBox='0 0 1000 250' role='img' aria-labelledby='mt-title'><title id='mt-title'>Memory Tree: {} blocks covering {} entries</title>",
        cells.len(),
        len as u64
    );
    out.push_str("<text x='20' y='22' class='mt-label'>SELECTED HISTORY · WHAT THE NEXT TURN STARTS FROM</text>");
    let mut x = LEFT;
    for cell in cells {
        let w = (weight(cell) / total * free).max(1.0);
        let (r0, r1) = (ruler(cell.start), ruler(cell.end).max(ruler(cell.start) + 1.0));
        let revised = edits.active(cell.block()).filter(|_| cell.kind == Kind::Summary);
        let dropped = edits.overlaps_drop(cell.start, cell.end);
        let pinned = edits.pins.iter().filter(|p| (cell.start..cell.end).contains(p)).count();
        let class = match cell.kind {
            Kind::Summary if dropped => "mt-sum mt-has-drop",
            Kind::Summary if revised.is_some() => "mt-sum mt-rev",
            Kind::Summary => "mt-sum",
            Kind::Pending => "mt-pending",
            Kind::Entry if dropped => "mt-entry mt-dropped",
            Kind::Entry if pinned > 0 => "mt-entry mt-pin",
            Kind::Entry | Kind::Entries => "mt-entry",
        };
        let fan = if matches!(cell.kind, Kind::Summary | Kind::Pending) {
            "mt-fan"
        } else {
            "mt-fan mt-fan-entry"
        };
        let _ = write!(
            out,
            "<path class='{fan}' d='M{x:.1} 104 L{:.1} 104 L{r1:.1} 166 L{r0:.1} 166 Z'/>",
            x + w
        );
        let range = if cell.end - cell.start == 1 {
            cell.start.to_string()
        } else {
            format!("{}–{}", cell.start, cell.end)
        };
        let mut title = match cell.kind {
            Kind::Summary => format!(
                "Summary of entries {range} · level {} · {} entries",
                cell.level,
                cell.end - cell.start
            ),
            Kind::Pending => format!("Entries {range} · level {} · not summarised yet", cell.level),
            Kind::Entry => format!("Entry {range} · verbatim"),
            Kind::Entries => format!("Entries {range} · {} verbatim entries", cell.end - cell.start),
        };
        if let Some(node) = &cell.node {
            let _ = write!(
                title,
                " · {} · {} summary bytes from {} input bytes · model {model} · {}",
                tokens(node),
                node.bytes,
                node.input_bytes,
                when(node.created_unix)
            );
        }
        if let Some(r) = revised {
            let _ = write!(title, " · operator revision r{} selected: {}", r.id, r.emphasis);
        }
        if pinned > 0 {
            let _ = write!(title, " · {pinned} pinned verbatim");
        }
        if dropped {
            title.push_str(" · contains entries dropped from the agent's view");
        }
        let _ = write!(
            out,
            "<a href='{}' target='memory-detail'><title>{}</title><rect class='{class}' x='{x:.1}' y='36' width='{w:.1}' height='68' rx='5'/>",
            esc(&node_href(chat, cell)),
            esc(&title)
        );
        if focus == Some((cell.start, cell.level)) && cell.kind != Kind::Entries {
            let _ = write!(
                out,
                "<rect class='mt-focus' x='{:.1}' y='32' width='{:.1}' height='76' rx='7'/>",
                x - 4.0,
                w + 8.0
            );
        }
        let fits = |text: &str| (text.chars().count() as f64) * 6.7 + 8.0 <= w;
        let center = x + w / 2.0;
        let detail = match cell.kind {
            Kind::Summary => cell
                .node
                .map(|n| {
                    format!(
                        "L{}·{}{}",
                        cell.level,
                        tokens(&n),
                        revised.map(|r| format!("·r{}", r.id)).unwrap_or_default()
                    )
                })
                .unwrap_or_default(),
            Kind::Pending => format!("L{}·pending", cell.level),
            Kind::Entry if dropped => "dropped".into(),
            Kind::Entry if pinned > 0 => "pinned".into(),
            Kind::Entry => String::new(),
            Kind::Entries => format!("{} verbatim", cell.end - cell.start),
        };
        if fits(&range) {
            let _ = write!(
                out,
                "<text x='{center:.1}' y='{}' class='mt-text' text-anchor='middle'>{}</text>",
                if detail.is_empty() || !fits(&detail) { 75 } else { 66 },
                esc(&range)
            );
            if !detail.is_empty() && fits(&detail) {
                let _ = write!(
                    out,
                    "<text x='{center:.1}' y='86' class='mt-small' text-anchor='middle'>{}</text>",
                    esc(&detail)
                );
            }
        }
        out.push_str("</a>");
        let segment = match cell.kind {
            Kind::Summary => "mt-seg-sum",
            Kind::Pending => "mt-seg-pending",
            _ => "mt-seg-entry",
        };
        let _ = write!(
            out,
            "<rect class='{segment}' x='{r0:.1}' y='166' width='{:.1}' height='14'/>",
            (r1 - r0).max(1.0)
        );
        x += w + gap;
    }
    for drop in edits.drops.iter().filter(|d| d.restored_unix.is_none()) {
        let (a, b) = (ruler(drop.start), ruler(drop.end));
        let _ = write!(
            out,
            "<rect class='mt-drop-band' x='{a:.1}' y='160' width='{:.1}' height='26'><title>Dropped from the agent's view: entries {}–{} (d{})</title></rect>",
            (b - a).max(2.0),
            drop.start,
            drop.end,
            drop.id
        );
    }
    for pin in &edits.pins {
        let p = ruler(*pin) + 0.5 / len * SPAN;
        let _ = write!(
            out,
            "<path class='mt-pin-tick' d='M{p:.1} 152 l4 6 l-4 6 l-4 -6 Z'><title>Pinned verbatim: entry #{pin}</title></path>"
        );
    }
    for i in 0..=4u64 {
        let at = (len as u64 * i) / 4;
        let anchor = match i {
            0 => "start",
            4 => "end",
            _ => "middle",
        };
        let _ = write!(
            out,
            "<text x='{:.1}' y='200' class='mt-small' text-anchor='{anchor}'>{at}</text>",
            ruler(at)
        );
    }
    let _ = write!(
        out,
        "<text x='20' y='232' class='mt-label'>RETAINED JOURNAL · {} ENTRIES · LINEAR POSITIONS →</text></svg>",
        len as u64
    );
    out
}

fn parse_block(text: &str) -> Option<Block> {
    let (start, level) = text.split_once('.')?;
    let block = Block {
        start: start.parse().ok()?,
        level: level.parse().ok()?,
    };
    (block.level < 63 && block.start.is_multiple_of(block.len()) && block.start.checked_add(block.len()).is_some())
        .then_some(block)
}

/// The overview page (inside the app frame), copied out of state so it
/// renders after the state lock. In-memory only.
pub(crate) struct Overview {
    chat: String,
    len: usize,
    edits: Edits,
    cells: Vec<Cell>,
    /// The compactor serving the chat.
    model: Option<String>,
    /// The model of the published view, when there is one.
    view_model: Option<String>,
    focus: Option<(u64, u32)>,
    detail: String,
    compacting: bool,
    error: Option<String>,
    /// The chat's block being regenerated.
    regenerating: Option<Block>,
    regeneration_error: Option<String>,
    /// `turn::listing`, for the step-tree links.
    turns: serde_json::Value,
    /// `compaction::status`.
    metadata: serde_json::Value,
}
impl Overview {
    pub(super) fn of(
        state: &State,
        session: &crate::chat::Session,
        query: &HashMap<String, String>,
        metadata: serde_json::Value,
    ) -> Self {
        let memory = &state.runtime.compaction;
        let chat = session.id.as_str();
        let len = session.messages.len();
        let view = memory.views.get(chat).filter(|v| v.actor == session.actor);
        let block = query.get("block").and_then(|b| parse_block(b));
        let entry = query.get("entry").and_then(|e| e.parse::<u64>().ok());
        Self {
            chat: chat.to_owned(),
            len,
            edits: session.memory.as_deref().cloned().unwrap_or_default(),
            cells: cells(view, len, memory.settings.recent_entries),
            model: compactor_for(state, session).map(|m| m.id.clone()),
            view_model: view.map(|v| v.model.clone()),
            focus: block.map(|b| (b.start, b.level)).or_else(|| entry.map(|e| (e, 0))),
            detail: match (block, entry) {
                (Some(b), _) => format!("{}/node?block={}.{}", memory_url(chat), b.start, b.level),
                (None, Some(e)) => format!("{}/node?entry={e}", memory_url(chat)),
                _ => format!("{}/node", memory_url(chat)),
            },
            compacting: memory.pending.as_ref().is_some_and(|(id, ..)| id == chat),
            error: memory.errors.get(chat).cloned(),
            regenerating: memory.surgery.as_ref().filter(|(id, _)| id == chat).map(|(_, b)| *b),
            regeneration_error: memory.surgery_errors.get(chat).cloned(),
            turns: super::turn::listing(memory, chat, &session.actor),
            metadata,
        }
    }
    pub(crate) fn render(&self) -> Result<String, &'static str> {
        let metadata = serde_json::to_string_pretty(&self.metadata).map_err(|_| "Cannot render compaction metadata")?;
        let (chat, len, cells, edits) = (self.chat.as_str(), self.len, &self.cells, &self.edits);
        let summaries = cells.iter().filter(|c| c.kind == Kind::Summary).count();
        let pending = cells.iter().filter(|c| c.kind == Kind::Pending).count();
        let verbatim: u64 = cells
            .iter()
            .filter(|c| matches!(c.kind, Kind::Entry | Kind::Entries))
            .map(|c| c.end - c.start)
            .sum();
        let status = match (self.compacting, &self.error) {
            (true, _) => "<span class='mt-badge'>compacting…</span>".to_owned(),
            (false, Some(error)) => format!(
                "<span class='mt-badge mt-warn' title='{}'>last compaction failed</span>",
                esc(error)
            ),
            (false, None) => String::new(),
        };
        let surgery = match (self.regenerating, &self.regeneration_error) {
            (Some(b), _) => format!(
                "<p class='mt-note'>Regenerating the summary of entries {}–{}… Reload to see the new revision.</p>",
                b.start,
                b.end()
            ),
            (None, Some(error)) => format!(
                "<p class='mt-note mt-warn'>Last regeneration failed: {}</p>",
                esc(error)
            ),
            (None, None) => String::new(),
        };
        let edited = if edits.log.is_empty() {
            String::new()
        } else {
            format!(
                " · <a href='{}/node' target='memory-detail'>{} operator edit{}</a>",
                memory_url(chat),
                edits.log.len(),
                if edits.log.len() == 1 { "" } else { "s" }
            )
        };
        Ok(format!(
            "<section class='memory-tree'><span class='eyebrow'>DM / MEMORY TREE</span><h2>Memory Tree</h2>\
<p class='mt-links'><a href='{}'>Return to DM</a> · <a href='/?view=providers'>Memory Tree model and window settings</a> · <a href='{}'>Read retained evidence</a></p>\
<p class='mt-stats'>{len} entries · {summaries} summary block{} · {pending} pending · {verbatim} verbatim · compactor {} {status}{edited}</p>{surgery}\
<div class='mt-legend' aria-label='Legend'><span><i class='mt-sw mt-sw-sum'></i>Summary</span><span><i class='mt-sw mt-sw-rev'></i>Operator revision</span><span><i class='mt-sw mt-sw-pending'></i>Not summarised yet</span><span><i class='mt-sw mt-sw-entry'></i>Verbatim entry</span><span><i class='mt-sw mt-sw-pin'></i>Pinned</span><span><i class='mt-sw mt-sw-drop'></i>Dropped from agent view</span></div>\
<div class='mt-visual' tabindex='0' role='region' aria-label='Memory Tree diagram; scroll horizontally on small screens'>{}</div>\
<p class='mt-hint'>Older history folds into aligned summary blocks (level L covers 2<sup>L</sup> entries); recent entries stay verbatim. Select a block to expand its children, summary, versions and source; regenerate with an emphasis, pin or drop entries there. Ranges are zero-based and end-exclusive. Summaries are untrusted evidence; the original journal is never rewritten.</p>\
<iframe class='mt-detail' name='memory-detail' title='Memory Tree block detail' src='{}'></iframe>{}\
<details class='mt-meta'><summary>Raw Memory Tree metadata</summary><pre>{}</pre></details></section>",
            esc(&crate::url_names::dm(chat)),
            esc(&evidence_url(chat)),
            if summaries == 1 { "" } else { "s" },
            esc(self.model.as_deref().unwrap_or("off")),
            svg(
                chat,
                cells,
                len,
                edits,
                self.view_model.as_deref().unwrap_or("?"),
                self.focus
            ),
            esc(&self.detail),
            super::turn::section(chat, &self.turns),
            esc(&metadata),
        ))
    }
}

/// What a detail page needs, captured under the lock.
struct Detail {
    chat: String,
    len: usize,
    target: Target,
    /// (root block, root key, path) when the block is in the published tree.
    tree: Option<(Block, String, Vec<usize>)>,
    edits: Edits,
    model: Option<String>,
    regenerating: Option<Block>,
    recent: u64,
    covered: usize,
}
enum Target {
    Overview,
    Block(Block),
    Entry(u64, String),
}
fn plan(state: &State, chat: &str, query: &HashMap<String, String>) -> Result<Detail, &'static str> {
    let session = state.chats.iter().find(|c| c.id == chat).ok_or("Chat not found")?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    let memory = &state.runtime.compaction;
    let len = session.messages.len();
    let target = if let Some(block) = query.get("block") {
        Target::Block(
            parse_block(block)
                .filter(|b| b.end() <= len as u64)
                .ok_or("Invalid block")?,
        )
    } else if let Some(entry) = query.get("entry") {
        let at = entry
            .parse::<usize>()
            .ok()
            .filter(|at| *at < len)
            .ok_or("Invalid entry")?;
        let conversation = Conversation::of(state, session);
        let mut text = conversation.entry(at).map(|e| e.render()).unwrap_or_default();
        if text.len() > TEXT_BYTES {
            let mut end = TEXT_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n…");
        }
        Target::Entry(at as u64, text)
    } else {
        Target::Overview
    };
    let view = memory.views.get(chat).filter(|v| v.actor == session.actor);
    let tree = match &target {
        Target::Block(block) => view.and_then(|v| {
            v.nodes.iter().find_map(|n| {
                let root = Block {
                    start: n.start,
                    level: n.level,
                };
                surgery::path(root, *block).map(|p| (root, n.key.clone(), p))
            })
        }),
        _ => None,
    };
    Ok(Detail {
        chat: chat.into(),
        len,
        target,
        tree,
        edits: session.memory.as_deref().cloned().unwrap_or_default(),
        model: compactor_for(state, session).map(|m| m.id.clone()),
        regenerating: memory.surgery.as_ref().filter(|(id, _)| id == chat).map(|(_, b)| *b),
        recent: memory.settings.recent_entries,
        covered: view.map_or(0, |v| v.count),
    })
}

/// Character entropy of `text` in bits per character: low for padded or
/// repetitive prose, high for terse, information-dense prose.
fn entropy(text: &str) -> f64 {
    let mut counts = std::collections::HashMap::<char, u32>::new();
    let mut total = 0u32;
    for c in text.chars() {
        *counts.entry(c).or_default() += 1;
        total += 1;
    }
    counts
        .values()
        .map(|&n| {
            let p = f64::from(n) / f64::from(total.max(1));
            -p * p.log2()
        })
        .sum()
}
/// Hard tokens: words carrying a digit or a path, ID or coordinate separator,
/// the facts a summary most often loses.
fn facts(text: &str) -> std::collections::BTreeSet<&str> {
    text.split(|c: char| c.is_whitespace() || ",;()[]{}\"'`".contains(c))
        .map(|word| word.trim_matches(|c: char| c == '.' || c == ':'))
        .filter(|word| word.len() >= 2 && word.chars().any(|c| c.is_ascii_digit() || c == '/' || c == '_'))
        .collect()
}
/// Hard tokens of `sources` that survive into `summary`: (kept, total).
fn kept(summary: &str, sources: &[&str]) -> (usize, usize) {
    let wanted = sources
        .iter()
        .flat_map(|text| facts(text))
        .collect::<std::collections::BTreeSet<_>>();
    let found = facts(summary);
    (wanted.iter().filter(|word| found.contains(*word)).count(), wanted.len())
}
fn clip(text: &str) -> (&str, bool) {
    if text.len() <= TEXT_BYTES {
        return (text, false);
    }
    let mut end = TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}
fn button(chat: &str, action: &str, fields: &[(&str, String)], label: &str, class: &str) -> String {
    let mut hidden = String::new();
    for (name, value) in fields {
        let _ = write!(hidden, "<input type='hidden' name='{name}' value='{}'>", esc(value));
    }
    format!(
        "<form method='post' action='{}' target='_top' class='mt-inline'><input type='hidden' name='action' value='{action}'>{hidden}<button class='{class}'>{}</button></form>",
        memory_url(chat),
        esc(label)
    )
}

/// Mini diagram: the selected block above its two children (or its entries).
fn subtree(chat: &str, block: Block, children: &[(Block, Option<&Node>)], edits: &Edits) -> String {
    let base = memory_url(chat);
    let mut out = String::from(
        "<svg class='mt-svg mt-sub' xmlns='http://www.w3.org/2000/svg' viewBox='0 0 1000 170' role='img' aria-label='Block and its children'>",
    );
    let class = |b: Block| {
        if edits.active(b).is_some() {
            "mt-sum mt-rev"
        } else if b.is_item() {
            "mt-entry"
        } else {
            "mt-sum"
        }
    };
    let _ = write!(
        out,
        "<rect class='{} ' x='300' y='10' width='400' height='50' rx='6'/><text x='500' y='41' class='mt-text' text-anchor='middle'>{}–{} · L{}</text>",
        class(block),
        block.start,
        block.end(),
        block.level
    );
    let n = children.len().max(1) as f64;
    let gap = 10.0;
    let w = (960.0 - gap * (n - 1.0)) / n;
    for (i, (child, node)) in children.iter().enumerate() {
        let x = 20.0 + i as f64 * (w + gap);
        let cx = x + w / 2.0;
        let _ = write!(out, "<path class='mt-wire' d='M500 60 V80 H{cx:.1} V104'/>");
        let href = if child.is_item() && node.is_none() {
            format!("{base}/node?entry={}", child.start)
        } else {
            format!("{base}/node?block={}.{}", child.start, child.level)
        };
        let label = if child.len() == 1 {
            format!("#{}", child.start)
        } else {
            format!("{}–{}", child.start, child.end())
        };
        let detail = node
            .map(|n| {
                format!(
                    "L{} · {} B{}",
                    child.level,
                    n.response.text.len(),
                    edits
                        .active(*child)
                        .map(|r| format!(" · r{}", r.id))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_else(|| {
                if edits.dropped(child.start) && child.is_item() {
                    "dropped".into()
                } else if edits.pinned(child.start) && child.is_item() {
                    "pinned".into()
                } else {
                    String::new()
                }
            });
        let cls = if child.is_item() && edits.dropped(child.start) {
            "mt-entry mt-dropped"
        } else if child.is_item() && edits.pinned(child.start) {
            "mt-entry mt-pin"
        } else {
            class(*child)
        };
        let _ = write!(
            out,
            "<a href='{}'><title>{}</title><rect class='{cls}' x='{x:.1}' y='104' width='{w:.1}' height='56' rx='5'/>",
            esc(&href),
            esc(&format!("{label} {detail}"))
        );
        if (label.chars().count() as f64) * 6.7 + 8.0 <= w {
            let _ = write!(
                out,
                "<text x='{cx:.1}' y='128' class='mt-text' text-anchor='middle'>{}</text>",
                esc(&label)
            );
            if !detail.is_empty() && (detail.chars().count() as f64) * 6.7 + 8.0 <= w {
                let _ = write!(
                    out,
                    "<text x='{cx:.1}' y='147' class='mt-small' text-anchor='middle'>{}</text>",
                    esc(&detail)
                );
            }
        }
        out.push_str("</a>");
    }
    out.push_str("</svg>");
    out
}

/// Nothing selected: a hint plus the active pins/drops and the edit log,
/// compactly. Pin, drop and regenerate forms live in a selection's detail.
fn edits_panel(d: &Detail) -> String {
    let chat = d.chat.as_str();
    let mut out = String::from("<p class='mt-empty'>Select a block to regenerate, pin or drop.</p>");
    let drops = d
        .edits
        .drops
        .iter()
        .filter(|drop| drop.restored_unix.is_none())
        .collect::<Vec<_>>();
    if !d.edits.pins.is_empty() || !drops.is_empty() {
        out.push_str("<ul class='mt-active'>");
        for pin in &d.edits.pins {
            let _ = write!(
                out,
                "<li>Pinned <a href='{}/node?entry={pin}'>#{pin}</a> {}</li>",
                memory_url(chat),
                button(chat, "unpin", &[("at", pin.to_string())], "Unpin", "mt-link")
            );
        }
        for drop in drops.iter().rev() {
            let _ = write!(
                out,
                "<li>Dropped {}–{} <span class='muted'>d{} · {}</span> {}</li>",
                drop.start,
                drop.end,
                drop.id,
                when(drop.created_unix),
                button(chat, "restore", &[("id", drop.id.to_string())], "Restore", "mt-link")
            );
        }
        out.push_str("</ul>");
    }
    if !d.edits.log.is_empty() {
        let _ = write!(
            out,
            "<details class='mt-log-panel' open><summary>Edit log · {}</summary><ol class='mt-log' reversed>",
            d.edits.log.len()
        );
        for event in d.edits.log.iter().rev().take(50) {
            let _ = write!(
                out,
                "<li><span class='muted'>{}</span> {} · {}</li>",
                when(event.at),
                esc(&event.action),
                esc(&event.detail)
            );
        }
        out.push_str("</ol></details>");
    }
    out
}

/// The selected node (absent outside the published tree) and its children.
type Loaded = (Option<Node>, Vec<(Block, Option<Node>)>);
fn detail_page(d: &Detail, nodes: io::Result<Loaded>) -> String {
    let chat = d.chat.as_str();
    let base = memory_url(chat);
    let mut body = String::new();
    if !matches!(d.target, Target::Overview) {
        let _ = write!(
            body,
            "<p class='mt-links'><a href='{base}/node'>All edits</a> · <a href='{}' target='_top'>Overview</a></p>",
            base
        );
    }
    match &d.target {
        Target::Overview => body.push_str(&edits_panel(d)),
        Target::Entry(at, text) => {
            let at = *at;
            let pinned = d.edits.pinned(at);
            let dropped = d.edits.dropped(at);
            let _ = write!(
                body,
                "<h2>Entry #{at}</h2><p class='mt-stats'>{}{}</p>",
                if dropped {
                    "Dropped from the agent's view"
                } else if (at as usize) >= d.covered || d.len - (at as usize) <= d.recent as usize {
                    "Verbatim in the next turn"
                } else {
                    "Folded into a summary"
                },
                if pinned { " · pinned verbatim" } else { "" }
            );
            body.push_str("<div class='mt-actions'>");
            body.push_str(&if pinned {
                button(chat, "unpin", &[("at", at.to_string())], "Unpin", "")
            } else {
                button(chat, "pin", &[("at", at.to_string())], "Pin verbatim", "primary")
            });
            if !dropped {
                body.push_str(&button(
                    chat,
                    "drop",
                    &[("start", at.to_string()), ("end", (at + 1).to_string())],
                    "Drop from agent view",
                    "",
                ));
            }
            body.push_str("</div>");
            let _ = write!(body, "<pre class='mt-text-block'>{}</pre>", esc(text));
        }
        Target::Block(block) => {
            let block = *block;
            let _ = write!(body, "<h2>Entries {}–{}</h2>", block.start, block.end());
            if let Some((root, ..)) = &d.tree {
                body.push_str("<p class='mt-crumbs'>");
                for level in (block.level + 1..=root.level).rev() {
                    let len = 1u64 << level;
                    let start = block.start / len * len;
                    let _ = write!(
                        body,
                        "<a href='{base}/node?block={start}.{level}'>{start}–{}</a> › ",
                        start + len
                    );
                }
                let _ = write!(body, "<strong>{}–{}</strong></p>", block.start, block.end());
            }
            match nodes {
                Err(error) => {
                    let _ = write!(body, "<p class='mt-note mt-warn'>{}</p>", esc(&error.to_string()));
                }
                Ok((None, _)) => {
                    let _ = write!(
                        body,
                        "<p class='mt-note'>Not summarised yet: this range is outside the published summary tree. Level {} · {} entries. It is summarised by the next compaction (the DM's compactor: {}).</p>",
                        block.level,
                        block.len(),
                        esc(d.model.as_deref().unwrap_or("off"))
                    );
                }
                Ok((Some(node), children)) => {
                    let refs: Vec<_> = children.iter().map(|(b, n)| (*b, n.as_ref())).collect();
                    body.push_str(&subtree(chat, block, &refs, &d.edits));
                    let active = d.edits.active(block);
                    let _ = write!(
                        body,
                        "<dl class='mt-dl'><dt>Range</dt><dd>{}–{} · level {} · {} entries</dd><dt>Model</dt><dd>{} ({}) · revision {}</dd><dt>Created</dt><dd>{}</dd><dt>Input</dt><dd>{} bytes from {}</dd><dt>Summary</dt><dd>{} bytes{}</dd><dt>Compression</dt><dd>{}</dd><dt>Node</dt><dd>{}</dd></dl>",
                        block.start,
                        block.end(),
                        block.level,
                        block.len(),
                        esc(&node.model.id),
                        esc(&node.model.model),
                        esc(&node.model.revision),
                        when(node.created_unix),
                        node.input_bytes,
                        if node.children.is_empty() {
                            "original entries"
                        } else {
                            "two child summaries"
                        },
                        node.response.text.len(),
                        node.response
                            .tokens
                            .map(|t| format!(" · {} input / {} output tokens", t.input_tokens, t.output_tokens))
                            .unwrap_or_default(),
                        compression(&node, &children),
                        &node.key[..16.min(node.key.len())]
                    );
                    body.push_str("<h3>Versions</h3><table class='mt-versions'><thead><tr><th>Version</th><th>Emphasis</th><th>Model</th><th>Written</th><th></th></tr></thead><tbody>");
                    let selected = d.edits.selection(block);
                    let row = |id: u32, emphasis: &str, model: &str, at: u64| {
                        let current = selected == id;
                        let label = if id == 0 {
                            "automatic".to_owned()
                        } else {
                            format!("r{id}")
                        };
                        let action = if current {
                            "<span class='mt-badge'>in use</span>".to_owned()
                        } else {
                            button(
                                chat,
                                "select",
                                &[
                                    ("block", format!("{}.{}", block.start, block.level)),
                                    ("revision", id.to_string()),
                                ],
                                "Use this version",
                                "",
                            )
                        };
                        format!(
                            "<tr{}><td>{label}</td><td>{}</td><td>{}</td><td>{}</td><td>{action}</td></tr>",
                            if current { " class='mt-current'" } else { "" },
                            esc(emphasis),
                            esc(model),
                            when(at)
                        )
                    };
                    body.push_str(&row(0, "—", &node.model.id, node.created_unix));
                    for r in d.edits.versions(block) {
                        body.push_str(&row(r.id, &r.emphasis, &r.model, r.created_unix));
                    }
                    body.push_str("</tbody></table>");
                    if selected != 0 && active.is_none() {
                        body.push_str("<p class='mt-note mt-warn'>The selected revision overlaps a dropped range, so the agent sees the automatic summary until it is regenerated.</p>");
                    }
                    let (text, more) = clip(active.map_or(node.response.text.as_str(), |r| r.text.as_str()));
                    let _ = write!(
                        body,
                        "<h3>Summary the agent sees{}</h3><pre class='mt-text-block'>{}{}</pre>",
                        active.map(|r| format!(" (r{})", r.id)).unwrap_or_default(),
                        esc(text),
                        if more { "\n…" } else { "" }
                    );
                    for r in d.edits.versions(block).filter(|r| Some(r.id) != active.map(|a| a.id)) {
                        let (text, _) = clip(&r.text);
                        let _ = write!(
                            body,
                            "<details class='mt-version'><summary>r{} · {}</summary><pre class='mt-text-block'>{}</pre></details>",
                            r.id,
                            esc(&r.emphasis),
                            esc(text)
                        );
                    }
                    if active.is_some() {
                        let (text, _) = clip(&node.response.text);
                        let _ = write!(
                            body,
                            "<details class='mt-version'><summary>automatic summary</summary><pre class='mt-text-block'>{}</pre></details>",
                            esc(text)
                        );
                    }
                    if node.children.is_empty() {
                        let _ = write!(
                            body,
                            "<p class='mt-links'><a href='{}?root={}&amp;path={}&amp;source=true' target='_top'>Read original source entries</a></p>",
                            esc(&evidence_url(chat)),
                            esc(&d.tree.as_ref().map(|t| t.1.clone()).unwrap_or_default()),
                            d.tree
                                .as_ref()
                                .map(|t| t.2.iter().map(usize::to_string).collect::<Vec<_>>().join("."))
                                .unwrap_or_default()
                        );
                    }
                    if let Some(busy) = d.regenerating {
                        let _ = write!(
                            body,
                            "<p class='mt-note'>Regenerating entries {}–{}… reload the overview to see the revision.</p>",
                            busy.start,
                            busy.end()
                        );
                    } else if d.model.is_some() {
                        let _ = write!(
                            body,
                            "<form method='post' action='{base}' target='_top' class='mt-form mt-regenerate'><input type='hidden' name='action' value='regenerate'><input type='hidden' name='block' value='{}.{}'><label>Regenerate this summary with an emphasis<textarea name='emphasis' maxlength='{}' required placeholder='Focus on the castle layout decisions; keep the port/relay details'></textarea></label><button class='primary'>Regenerate summary</button></form>",
                            block.start,
                            block.level,
                            surgery::EMPHASIS_CHARS
                        );
                    }
                }
            }
            body.push_str("<div class='mt-actions'>");
            if !d.edits.overlaps_drop(block.start, block.end()) || block.len() > 1 {
                body.push_str(&button(
                    chat,
                    "drop",
                    &[("start", block.start.to_string()), ("end", block.end().to_string())],
                    &format!("Drop entries {}–{} from agent view", block.start, block.end()),
                    "",
                ));
            }
            body.push_str("</div>");
            // Any other range, prefilled with this block's (end-exclusive).
            let _ = write!(
                body,
                "<details class='mt-more'><summary>Drop a different range…</summary><form method='post' action='{base}' target='_top' class='mt-form mt-range'><input type='hidden' name='action' value='drop'><label>Start<input name='start' type='number' min='0' max='{}' value='{}' required></label><label>End (exclusive)<input name='end' type='number' min='1' max='{}' value='{}' required></label><button>Drop range</button></form></details>",
                d.len.saturating_sub(1),
                block.start,
                d.len,
                block.end().min(d.len as u64)
            );
        }
    }
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><title>Control · Memory Tree block</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/runtime.css'></head><body class='mt-frame'><main>{body}</main></body></html>"
    )
}

/// Ratio, character entropy and, for a summary of two summaries, how many of
/// their hard tokens it kept.
fn compression(node: &Node, children: &[(Block, Option<Node>)]) -> String {
    let text = node.response.text.as_str();
    let ratio = node.input_bytes as f64 / text.len().max(1) as f64;
    let mut line = format!("{ratio:.1}× smaller · {:.2} bits/char", entropy(text));
    let sources = children
        .iter()
        .filter_map(|(_, child)| child.as_ref().map(|child| child.response.text.as_str()))
        .collect::<Vec<_>>();
    if !sources.is_empty() {
        let (kept, total) = kept(text, &sources);
        if let Some(share) = (kept * 100).checked_div(total) {
            let _ = write!(line, " · facts kept {kept}/{total} ({share}%)");
        }
    }
    line
}

/// Load the selected node and its children (one root-to-block path).
fn load_nodes(d: &Detail, directory: &std::path::Path) -> io::Result<Loaded> {
    let Target::Block(block) = d.target else {
        return Ok((None, Vec::new()));
    };
    let Some((_, root, path)) = &d.tree else {
        return Ok((None, Vec::new()));
    };
    let node = recall::descend(directory, root, path)?;
    let children = if let Some(halves) = block.children().filter(|_| !node.children.is_empty()) {
        halves
            .iter()
            .zip(&node.children)
            .map(|(b, key)| Ok((*b, Some(recall::load(directory, key)?))))
            .collect::<io::Result<Vec<_>>>()?
    } else {
        // A leaf: its entries (at most 16 drawn; the source link reads all).
        (block.start..block.end().min(block.start + 16))
            .map(|at| (Block { start: at, level: 0 }, None))
            .collect()
    };
    Ok((Some(node), children))
}

/// Memory Tree routes that need the shared state handle: lazy block detail
/// (disk reads outside the lock), operator edits, and retained evidence.
pub(crate) fn http(
    stream: &mut impl std::io::Write,
    state: &Store,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> bool {
    if recall::http(stream, state, method, path, query) {
        return true;
    }
    let Some(rest) = path.strip_prefix("/dm/") else {
        return false;
    };
    let Some((chat, operation)) = rest.split_once('/') else {
        return false;
    };
    if !crate::application::valid_id(chat) {
        return false;
    }
    match (method, operation) {
        ("GET", "memory/node") => {
            let fields = crate::fields(query);
            let detail = match state.read(|s| plan(s, chat, &fields)) {
                Ok(detail) => detail,
                Err(_) => Err("State unavailable"),
            };
            match detail {
                Ok(detail) => {
                    let nodes = crate::config::state("summaries")
                        .map_err(io::Error::other)
                        .and_then(|dir| load_nodes(&detail, &dir));
                    let _ = control_http::respond(
                        stream,
                        "200 OK",
                        "text/html; charset=utf-8",
                        detail_page(&detail, nodes).as_bytes(),
                        "",
                        control_http::Policy::Embedded,
                    );
                }
                Err(error) => {
                    let _ = control_http::respond(
                        stream,
                        "404 Not Found",
                        "text/html; charset=utf-8",
                        crate::error_page(error, &memory_url(chat)).as_bytes(),
                        "",
                        control_http::Policy::Embedded,
                    );
                }
            }
            true
        }
        ("POST", "memory") => {
            let fields = crate::fields(&String::from_utf8_lossy(body));
            let result = post(state, chat, &Actor::LocalOperator, &fields);
            match result {
                Ok(location) => {
                    crate::worker::wake();
                    let _ = control_http::respond(
                        stream,
                        "303 See Other",
                        "text/html; charset=utf-8",
                        b"",
                        &format!("Location: {location}\r\n"),
                        control_http::Policy::Embedded,
                    );
                }
                Err(error) => {
                    let _ = control_http::respond(
                        stream,
                        "400 Bad Request",
                        "text/html; charset=utf-8",
                        crate::error_page(error, &memory_url(chat)).as_bytes(),
                        "",
                        control_http::Policy::Embedded,
                    );
                }
            }
            true
        }
        _ => false,
    }
}

/// One operator form submission. The HTTP console is the local operator;
/// agents have no route or tool that reaches this.
pub(crate) fn post(
    state: &Store,
    chat: &str,
    actor: &Actor,
    fields: &HashMap<String, String>,
) -> Result<String, &'static str> {
    let number = |name: &str| {
        fields
            .get(name)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .ok_or("Invalid position")
    };
    let block = || fields.get("block").and_then(|b| parse_block(b)).ok_or("Invalid block");
    let back = |b: Block| format!("{}?block={}.{}", memory_url(chat), b.start, b.level);
    let action = fields.get("action").map(String::as_str).unwrap_or("");
    if action == "regenerate" {
        let block = block()?;
        regenerate(
            state,
            chat,
            actor,
            block,
            fields.get("emphasis").map(String::as_str).unwrap_or(""),
        )?;
        return Ok(back(block));
    }
    let (op, location) = match action {
        "pin" => (
            Op::Pin(number("at")?),
            format!("{}?entry={}", memory_url(chat), number("at")?),
        ),
        "unpin" => (
            Op::Unpin(number("at")?),
            format!("{}?entry={}", memory_url(chat), number("at")?),
        ),
        "drop" => (
            Op::Drop {
                start: number("start")?,
                end: number("end")?,
            },
            memory_url(chat),
        ),
        "restore" => (Op::Restore(number("id")? as u32), memory_url(chat)),
        "select" => {
            let block = block()?;
            (
                Op::Select {
                    block,
                    revision: number("revision")? as u32,
                },
                back(block),
            )
        }
        _ => return Err("Unknown Memory Tree action"),
    };
    state
        .write(|s| {
            let previous = edit(s, chat, actor, op)?;
            if crate::storage::save(s).is_err() {
                if let Some(session) = s.chats.iter_mut().find(|c| c.id == chat) {
                    session.memory = previous;
                }
                return Err("Could not save the Memory Tree edit");
            }
            Ok(location)
        })
        .map_err(|_| "State unavailable")?
}

#[cfg(test)]
mod tests;
