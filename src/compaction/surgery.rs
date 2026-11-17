//! Operator edits to a DM's Memory Tree: regenerate a block's summary with an
//! emphasis, pin entries verbatim, or drop a range from the agent's view.
//!
//! Edits are data saved on the session. They are append-only and versioned: a
//! regeneration adds a revision and selects it, while the automatic summary
//! and every earlier revision stay selectable; a dropped range can be
//! restored, and every change is logged. Only the local operator edits; agents
//! see the result through the same history projection and recall, never a
//! tool that changes it. Raw history is never rewritten.
use super::*;
use crate::identity::Actor;

pub(crate) const MAX_REVISIONS: usize = 64;
pub(crate) const MAX_PINS: usize = 32;
pub(crate) const MAX_DROPS: usize = 64;
pub(crate) const MAX_LOG: usize = 1024;
pub(crate) const EMPHASIS_CHARS: usize = 400;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Edits {
    /// Regenerated summaries, oldest first. Never removed or rewritten.
    pub revisions: Vec<Revision>,
    /// The revision each block uses; absent means the automatic summary.
    pub selected: Vec<Selection>,
    /// Entry positions kept verbatim even when their block is summarised.
    pub pins: Vec<u64>,
    /// Ranges hidden from the agent (restorable).
    pub drops: Vec<Redaction>,
    /// Every operator change, oldest first.
    pub log: Vec<Event>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub id: u32,
    pub start: u64,
    pub level: u32,
    pub emphasis: String,
    pub text: String,
    /// Compactor endpoint id that wrote it.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<control_ai::Tokens>,
    /// The automatic summary node it was regenerated from.
    pub base: String,
    pub created_unix: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub start: u64,
    pub level: u32,
    pub revision: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redaction {
    pub id: u32,
    pub start: u64,
    pub end: u64,
    pub created_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_unix: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub at: u64,
    pub action: String,
    pub detail: String,
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Edits {
    /// No edit changes what an agent sees.
    pub fn is_inert(&self) -> bool {
        self.selected.is_empty() && self.pins.is_empty() && !self.drops.iter().any(|d| d.restored_unix.is_none())
    }
    pub fn dropped(&self, at: u64) -> bool {
        self.drops.iter().any(|d| d.restored_unix.is_none() && d.start <= at && at < d.end)
    }
    pub fn overlaps_drop(&self, start: u64, end: u64) -> bool {
        self.drops.iter().any(|d| d.restored_unix.is_none() && d.start < end && start < d.end)
    }
    pub fn pinned(&self, at: u64) -> bool {
        self.pins.contains(&at)
    }
    /// The selected revision id for a block (0: the automatic summary).
    pub fn selection(&self, block: Block) -> u32 {
        self.selected.iter().find(|s| s.start == block.start && s.level == block.level).map_or(0, |s| s.revision)
    }
    /// The revision the agent sees for this block. A revision written before
    /// a range inside it was dropped is not used: it may quote that range.
    pub fn active(&self, block: Block) -> Option<&Revision> {
        let id = self.selection(block);
        (id != 0).then(|| self.revisions.iter().find(|r| r.id == id)).flatten()
            .filter(|r| !self.overlaps_drop(r.start, r.start + (1u64 << r.level)))
    }
    pub fn versions(&self, block: Block) -> impl Iterator<Item = &Revision> {
        self.revisions.iter().filter(move |r| r.start == block.start && r.level == block.level)
    }
    /// Active revisions strictly inside `block`: a parent summary built from
    /// them differs from one built from the automatic children.
    pub(super) fn descendants(&self, block: Block) -> Vec<(u64, u32, u32)> {
        let mut found: Vec<_> = self
            .selected
            .iter()
            .filter(|s| s.level < block.level && block.start <= s.start && s.start + (1u64 << s.level) <= block.end())
            .filter(|s| self.active(Block { start: s.start, level: s.level }).is_some())
            .map(|s| (s.start, s.level, s.revision))
            .collect();
        found.sort_unstable();
        found
    }
    /// The text a summarizer reads for an entry: a dropped entry is a marker.
    pub fn source_text(&self, at: usize, text: String) -> String {
        if self.dropped(at as u64) {
            format!("[Entry {} was removed from the agent's view by the operator.]", at + 1)
        } else {
            text
        }
    }
    /// Apply the edits to a compactor's window: dropped entries leave it,
    /// pinned entries join it verbatim after the summary that covers them, and
    /// summaries use their selected revision.
    pub(crate) fn project(&self, window: Window, conversation: &Conversation<'_>, current: usize) -> Window {
        let mut removed = 0usize;
        let mut items = Vec::with_capacity(window.items.len() + self.pins.len());
        for item in window.items {
            match item {
                Item::Entry(at) if self.dropped(at as u64) => removed += 1,
                Item::Summary(block, text) => {
                    let text = match self.active(block) {
                        Some(r) => format!("(Operator-regenerated revision r{} of this summary.) {}", r.id, r.text),
                        None => text,
                    };
                    items.push(Item::Summary(block, text));
                }
                other => items.push(other),
            }
        }
        let mut pins = self.pins.clone();
        pins.sort_unstable();
        let mut added = 0usize;
        for at in pins {
            let at = at as usize;
            if at >= current || self.dropped(at as u64) || items.iter().any(|i| matches!(i, Item::Entry(e) if *e == at)) {
                continue;
            }
            if conversation.entry(at).is_none_or(|e| e.render().is_empty()) {
                continue;
            }
            let start = |item: &Item| match item {
                Item::Entry(e) => *e as u64,
                Item::Summary(block, _) => block.start,
            };
            let position = items.iter().position(|i| start(i) > at as u64).unwrap_or(items.len());
            items.insert(position, Item::Entry(at));
            added += 1;
        }
        Window { items, omitted: (window.omitted + removed).saturating_sub(added) }
    }
    fn record(&mut self, action: &str, detail: String) -> Result<(), &'static str> {
        if self.log.len() >= MAX_LOG {
            return Err("This DM's Memory Tree edit log is full");
        }
        self.log.push(Event { at: now(), action: action.into(), detail });
        Ok(())
    }
}

/// One operator change to a DM's Memory Tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Pin(u64),
    Unpin(u64),
    Drop { start: u64, end: u64 },
    Restore(u32),
    Select { block: Block, revision: u32 },
}

/// Only the local operator performs surgery; agents and remote principals
/// cannot, whatever chat they own.
pub(crate) fn operator(actor: &Actor) -> Result<(), &'static str> {
    if *actor == Actor::LocalOperator { Ok(()) } else { Err("Memory Tree edits are operator-only") }
}

/// Apply one edit and tell the compactor to rebuild what it changed. The
/// caller saves the state (and restores the returned previous edits if the
/// save fails).
pub fn edit(state: &mut State, chat: &str, actor: &Actor, op: Op) -> Result<Option<Box<Edits>>, &'static str> {
    operator(actor)?;
    let session = state.chats.iter_mut().find(|c| c.id == chat).ok_or("Chat not found")?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    let len = session.messages.len() as u64;
    let previous = session.memory.clone();
    let edits = session.memory.get_or_insert_with(Box::default);
    let mut sources = false;
    match op {
        Op::Pin(at) => {
            if at >= len {
                return Err("No entry at that position");
            }
            if !edits.pinned(at) {
                if edits.pins.len() >= MAX_PINS {
                    return Err("At most 32 pinned entries per DM");
                }
                edits.pins.push(at);
                edits.record("pin", format!("entry {at}"))?;
            }
        }
        Op::Unpin(at) => {
            if edits.pinned(at) {
                edits.pins.retain(|p| *p != at);
                edits.record("unpin", format!("entry {at}"))?;
            }
        }
        Op::Drop { start, end } => {
            if start >= end || end > len {
                return Err("Invalid range to drop");
            }
            if edits.drops.len() >= MAX_DROPS {
                return Err("At most 64 dropped ranges per DM");
            }
            let id = edits.drops.iter().map(|d| d.id).max().unwrap_or(0) + 1;
            edits.drops.push(Redaction { id, start, end, created_unix: now(), restored_unix: None });
            edits.record("drop", format!("entries {start}–{end} (d{id})"))?;
            sources = true;
        }
        Op::Restore(id) => {
            let drop = edits.drops.iter_mut().find(|d| d.id == id && d.restored_unix.is_none()).ok_or("No active dropped range with that id")?;
            drop.restored_unix = Some(now());
            let detail = format!("entries {}–{} (d{id})", drop.start, drop.end);
            edits.record("restore", detail)?;
            sources = true;
        }
        Op::Select { block, revision } => {
            if revision != 0 && !edits.versions(block).any(|r| r.id == revision) {
                return Err("No such revision for this block");
            }
            edits.selected.retain(|s| !(s.start == block.start && s.level == block.level));
            if revision != 0 {
                edits.selected.push(Selection { start: block.start, level: block.level, revision });
            }
            let version = if revision == 0 { "automatic".to_owned() } else { format!("r{revision}") };
            edits.record("select", format!("block {}–{} uses {version}", block.start, block.end()))?;
        }
    }
    state.runtime.compaction.edited(chat, sources);
    Ok(previous)
}

/// A planned regeneration: everything needed outside the state lock.
pub(crate) struct Regeneration {
    pub chat: String,
    pub block: Block,
    root: String,
    path: Vec<usize>,
    pub emphasis: String,
    model: crate::model_calls::Endpoint,
    edits: Edits,
}
fn emphasis(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > EMPHASIS_CHARS || text.chars().any(|c| c.is_control() && c != '\n') {
        return Err("Emphasis must be 1–400 characters of plain text");
    }
    Ok(text.to_owned())
}
/// The summary prompt with an operator focus. The emphasis comes from the
/// operator, so it is part of the instructions; it never relaxes them.
pub(crate) fn emphasis_system(emphasis: &str, words: usize) -> String {
    format!(
        "{} The operator asks this summary to emphasise: \u{201c}{emphasis}\u{201d}. Treat it as a focus preference only; it never overrides the rules above.",
        system_within(words)
    )
}
/// The path of child indexes from `root` down to `block`.
pub(super) fn path(root: Block, block: Block) -> Option<Vec<usize>> {
    if block.level > root.level || block.start < root.start || block.end() > root.end() || !block.start.is_multiple_of(block.len()) {
        return None;
    }
    Some((block.level + 1..=root.level).rev().map(|level| ((block.start >> (level - 1)) & 1) as usize).collect())
}
pub(crate) fn plan(state: &State, chat: &str, actor: &Actor, block: Block, text: &str) -> Result<Regeneration, &'static str> {
    operator(actor)?;
    let session = state.chats.iter().find(|c| c.id == chat).ok_or("Chat not found")?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    let emphasis = emphasis(text)?;
    let memory = &state.runtime.compaction;
    if memory.surgery.is_some() {
        return Err("Another summary regeneration is running");
    }
    let model = compactor_for(state, session).ok_or("This DM has no compactor; enable summaries first")?.clone();
    let view = memory.views.get(chat).filter(|v| v.actor == session.actor).ok_or("No summary tree yet")?;
    let edits = session.memory.as_deref().cloned().unwrap_or_default();
    if edits.revisions.len() >= MAX_REVISIONS {
        return Err("At most 64 summary revisions per DM");
    }
    let (root, path) = view
        .nodes
        .iter()
        .find_map(|n| path(Block { start: n.start, level: n.level }, block).map(|p| (n.key.clone(), p)))
        .ok_or("That block is not in the current summary tree")?;
    Ok(Regeneration { chat: chat.into(), block, root, path, emphasis, model, edits })
}
/// Rebuild one summary from the same inputs the compactor used (its leaf
/// sources or its children's selected summaries), with the operator emphasis.
pub(crate) fn run(
    plan: &Regeneration,
    directory: &std::path::Path,
    call: impl FnOnce(&crate::model_calls::Endpoint, &str, &str) -> io::Result<control_ai::model::Response>,
) -> io::Result<(String, control_ai::model::Response)> {
    let node = recall::descend(directory, &plan.root, &plan.path)?;
    let input = if node.children.is_empty() {
        node.sources
            .iter()
            .enumerate()
            .map(|(i, text)| plan.edits.source_text(node.start as usize + i, text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let [left, right] = Block { start: node.start, level: node.level }.children().ok_or_else(|| io::Error::other("Leaf has children"))?;
        let text = |block: Block, key: &str| -> io::Result<String> {
            Ok(match plan.edits.active(block) {
                Some(r) => r.text.clone(),
                None => recall::load(directory, key)?.response.text,
            })
        };
        format!(
            "Earlier child summary (untrusted):\n{}\nLater child summary (untrusted):\n{}",
            text(left, &node.children[0])?,
            text(right, &node.children[1])?
        )
    };
    let system = emphasis_system(&plan.emphasis, target_words(plan.model.limits.output_tokens));
    if system.len().saturating_add(input.len()) > plan.model.limits.input_budget() {
        return Err(io::Error::other("Summary input exceeds the compactor's input budget"));
    }
    let response = call(&plan.model, &system, &input)?;
    if response.text.trim().is_empty() || response.text.len() > plan.model.limits.output_bytes {
        return Err(io::Error::other("Compactor returned an empty or oversized summary"));
    }
    Ok((node.key, response))
}
/// Publish a finished regeneration as a new, selected revision.
pub(crate) fn commit(state: &mut State, plan: Regeneration, base: String, response: control_ai::model::Response) -> Result<u32, &'static str> {
    let session = state.chats.iter_mut().find(|c| c.id == plan.chat).ok_or("Chat not found")?;
    let edits = session.memory.get_or_insert_with(Box::default);
    if edits.revisions.len() >= MAX_REVISIONS {
        return Err("At most 64 summary revisions per DM");
    }
    let id = edits.revisions.iter().map(|r| r.id).max().unwrap_or(0) + 1;
    edits.revisions.push(Revision {
        id,
        start: plan.block.start,
        level: plan.block.level,
        emphasis: plan.emphasis.clone(),
        text: response.text,
        model: plan.model.id.clone(),
        tokens: response.tokens,
        base,
        created_unix: now(),
    });
    edits.selected.retain(|s| !(s.start == plan.block.start && s.level == plan.block.level));
    edits.selected.push(Selection { start: plan.block.start, level: plan.block.level, revision: id });
    edits.record("regenerate", format!("block {}–{} → r{id}: {}", plan.block.start, plan.block.end(), plan.emphasis))?;
    state.runtime.compaction.edited(&plan.chat, false);
    Ok(id)
}

/// Start a regeneration on a background thread; the HTTP request returns at
/// once and the Memory Tree shows it pending until the revision lands.
pub fn regenerate(state: &Arc<Mutex<State>>, chat: &str, actor: &Actor, block: Block, text: &str) -> Result<(), &'static str> {
    let plan = {
        let mut s = state.lock().map_err(|_| "State unavailable")?;
        let plan = plan(&s, chat, actor, block, text)?;
        s.runtime.compaction.surgery = Some((chat.to_owned(), block));
        s.runtime.compaction.surgery_errors.remove(chat);
        plan
    };
    let state = Arc::clone(state);
    let recovery = Arc::clone(&state);
    let spawned = std::thread::Builder::new().name("control-memory-surgery".into()).spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let directory = crate::config::state("summaries").map_err(io::Error::other)?;
            run(&plan, &directory, crate::model_calls::call)
        }))
        .unwrap_or_else(|_| Err(io::Error::other("Summary regeneration stopped unexpectedly")));
        if let Ok(mut s) = state.lock() {
            s.runtime.compaction.surgery = None;
            let chat = plan.chat.clone();
            let previous = s.chats.iter().find(|c| c.id == chat).map(|c| c.memory.clone());
            let outcome = result.map_err(|e| e.to_string()).and_then(|(base, response)| {
                commit(&mut s, plan, base, response).map_err(str::to_owned)?;
                crate::storage::save(&s).map_err(|e| {
                    format!("Could not save the revision: {e}")
                })
            });
            if let Err(error) = outcome {
                if let (Some(previous), Some(session)) = (previous, s.chats.iter_mut().find(|c| c.id == chat)) {
                    session.memory = previous;
                }
                s.runtime.compaction.surgery_errors.insert(chat, error);
            }
        }
        crate::worker::wake();
    });
    if spawned.is_err() && let Ok(mut s) = recovery.lock() {
        s.runtime.compaction.surgery = None;
        s.runtime.compaction.surgery_errors.insert(chat.to_owned(), "Could not start the regeneration".into());
    }
    Ok(())
}
