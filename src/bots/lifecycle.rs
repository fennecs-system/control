//! Host facts about one task tree, offered on its `task:<root>` topic to the
//! advisor subscribed to it. Facts are edge-triggered from records reconcile
//! already reads; there is no clock, classifier or supervisor state for them.
//! Judge turns, retained peer evidence (runs without an attempt) and turns woken
//! only by advisor notes are not team activity, so an advisor cannot wake itself
//! through the team it advises. The model interprets; the host only observes.
use super::{Message, Reason};
use crate::State;
use control_effect::machine::Machine;
use control_bot::{Source, Topic};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};

/// Word-at-a-time multiplicative hash (rustc's Fx) for the per-pass maps over
/// local chat IDs. SipHash lookups dominated the measured scan; these maps live
/// for one pass and never key on remote input.
#[derive(Default)]
struct Fx(u64);
impl Fx {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}
impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        let mut tail = [0; 8];
        tail[..rest.len()].copy_from_slice(rest);
        self.add(u64::from_le_bytes(tail));
    }
    fn write_u8(&mut self, byte: u8) {
        self.add(u64::from(byte));
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
type Fast<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

pub(super) const KIND: &str = "task";
/// Chats named in one coalesced fact; the advisor reads the rest itself.
const CHATS: usize = 8;

/// One coalescing, non-urgent, non-direct reason for an advisor turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// Member turns that ended failed, interrupted or blocked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failed: u32,
    /// Watch exits with a failure status, a signal, or a retired restart loop.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub exits: u32,
    /// Live claims whose owner the host observed as lost.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub abandoned: u32,
    /// The last active member yielded with nothing queued or running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quiescent: bool,
    /// Chats the facts concern, newest first and bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<String>,
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}
impl Facts {
    pub(super) fn merge(&mut self, newer: &Self) {
        self.failed = self.failed.saturating_add(newer.failed);
        self.exits = self.exits.saturating_add(newer.exits);
        self.abandoned = self.abandoned.saturating_add(newer.abandoned);
        self.quiescent |= newer.quiescent;
        for chat in &newer.chats {
            self.concern(chat);
        }
    }
    fn concern(&mut self, chat: &str) {
        if self.chats.len() < CHATS && !self.chats.iter().any(|known| known == chat) {
            self.chats.push(chat.to_owned());
        }
    }
    fn is_empty(&self) -> bool {
        self.failed == 0 && self.exits == 0 && self.abandoned == 0 && !self.quiescent
    }
    pub(super) fn describe(&self, root: &str) -> String {
        let mut parts = Vec::new();
        if self.failed > 0 { parts.push(format!("{} member turn(s) failed, were interrupted or blocked", self.failed)); }
        if self.exits > 0 { parts.push(format!("{} watched process(es) failed or were retired", self.exits)); }
        if self.abandoned > 0 { parts.push(format!("{} claim(s) abandoned by a lost owner", self.abandoned)); }
        if self.quiescent { parts.push("the team went quiet: no member turn is running or queued".into()); }
        let chats = if self.chats.is_empty() { String::new() } else { format!(" (chats: {})", self.chats.join(", ")) };
        format!("host facts for task {root}: {}{chats}", parts.join("; "))
    }
}

/// What the host last observed of an advised team; the cursor orders its facts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Team {
    pub(super) cursor: u64,
    pub(super) failed: usize,
    /// Settled member turns when the team last went quiet.
    pub(super) settled: usize,
}

pub(super) fn topic(root: &str) -> Topic {
    Topic::new(KIND, root)
}
pub(super) fn source(root: &str) -> Source {
    Source::Topic(topic(root))
}
/// Roots with an advisor subscribed, without allocating when there are none.
fn advised(state: &State) -> Vec<&str> {
    let mut roots = state.bots.supervisor.definitions().flat_map(|definition| &definition.sources).filter_map(|source| match source {
        Source::Topic(topic) if topic.kind == KIND => Some(topic.name.as_str()),
        _ => None,
    }).collect::<Vec<_>>();
    roots.sort_unstable();
    roots.dedup();
    roots
}
pub(super) fn advises(definition: &control_bot::Definition) -> bool {
    definition.sources.iter().any(|source| matches!(source, Source::Topic(topic) if topic.kind == KIND))
}

/// A turn woken only by advisor notes is the advisor's echo, not new activity.
fn echo(run: &crate::runtime::Run) -> bool {
    run.bot_turn.as_ref().is_some_and(|turn| !turn.reasons.is_empty()
        && turn.reasons.iter().all(|reason| matches!(reason, Reason::Info { info: Message::Note, .. })))
}

/// Edge-triggered quiescence and failure facts per advised team, with the
/// snapshot to record. Read-only; one pass over chats and runs, and only when
/// an advisor exists.
pub(super) fn observe(state: &State) -> Vec<(String, Team, Facts)> {
    let roots = advised(state);
    if roots.is_empty() {
        return Vec::new();
    }
    let index = state.chats.iter().enumerate().map(|(i, chat)| (chat.id.as_str(), i)).collect::<Fast<_, _>>();
    let mut team_of: Fast<&str, usize> = Fast::default();
    for chat in &state.chats {
        let mut session = chat;
        for _ in 0..64 {
            match session.parent_chat.as_deref().and_then(|parent| index.get(parent)) {
                Some(&parent) => session = &state.chats[parent],
                None => break,
            }
        }
        if session.parent_chat.is_none() && let Ok(team) = roots.binary_search(&session.id.as_str()) {
            team_of.insert(chat.id.as_str(), team);
        }
    }
    struct Scan<'a> { active: bool, settled: usize, failed: usize, failing: Vec<&'a str> }
    let mut scans = roots.iter().map(|_| Scan { active: false, settled: 0, failed: 0, failing: Vec::new() }).collect::<Vec<_>>();
    for run in state.runtime.runs.iter().rev() {
        let Some(&team) = run.chat_id.as_deref().and_then(|chat| team_of.get(chat)) else { continue };
        if crate::delegation::is_judge(run) {
            continue;
        }
        let scan = &mut scans[team];
        match run.status.as_str() {
            "queued" | "running" | "waiting" | "cancelling" => scan.active = true,
            status if !run.attempts.is_empty() && super::terminal(status) => {
                if !echo(run) { scan.settled += 1; }
                if matches!(status, "failed" | "interrupted" | "blocked") {
                    scan.failed += 1;
                    if scan.failing.len() < CHATS { scan.failing.push(run.chat_id.as_deref().unwrap()); }
                }
            }
            _ => {}
        }
    }
    for request in &state.bots.requests {
        if let Some(&team) = team_of.get(request.chat.0.as_str()) { scans[team].active = true; }
    }
    roots.into_iter().zip(scans).filter_map(|(root, scan)| {
        let Some(known) = state.bots.teams.get(root) else {
            // Subscription starts here: earlier history is not reported.
            return Some((root.to_owned(), Team { cursor: 0, failed: scan.failed, settled: scan.settled }, Facts::default()));
        };
        let mut team = known.clone();
        let mut facts = Facts::default();
        if scan.failed > team.failed {
            facts.failed = u32::try_from(scan.failed - team.failed).unwrap_or(u32::MAX);
            for chat in scan.failing.iter().take(scan.failed - team.failed) { facts.concern(chat); }
        }
        team.failed = scan.failed;
        if !scan.active && scan.settled > team.settled {
            facts.quiescent = true;
            team.settled = scan.settled;
        }
        (team != *known).then(|| (root.to_owned(), team, facts))
    }).collect()
}

/// Accumulates facts reconcile observes later in its pass for one chat's team.
/// Teams without a recorded snapshot have not started their subscription.
pub(super) fn concern(state: &State, pending: &mut BTreeMap<String, Facts>, chat: &str, add: impl FnOnce(&mut Facts)) {
    let Some(root) = super::root(state, chat) else { return };
    if !state.bots.teams.contains_key(root) {
        return;
    }
    let facts = pending.entry(root.to_owned()).or_default();
    add(facts);
    facts.concern(chat);
}

/// Offers each team's coalesced facts once, at its next cursor.
pub(super) fn offer(state: &mut State, pending: BTreeMap<String, Facts>, now: control_effect::machine::Now) -> Vec<control_effect::machine::Planned<control_bot::BotId, super::Effect>> {
    let mut effects = Vec::new();
    for (root, facts) in pending {
        let Some(team) = state.bots.teams.get_mut(&root).filter(|_| !facts.is_empty()) else { continue };
        team.cursor += 1;
        let cursor = team.cursor;
        effects.extend(state.bots.supervisor.step(control_bot::Event::Info { topic: topic(&root), cursor, from: None, info: Message::Lifecycle(facts) }, now));
    }
    effects
}

/// A stopped advisor's snapshot is dropped, so a later advisor for the same
/// team starts from its own subscription rather than an old observation.
pub(super) fn forget(state: &mut State, chat: &str) {
    let roots = state.bots.supervisor.definitions().filter(|definition| definition.chat.0 == chat)
        .flat_map(|definition| &definition.sources).filter_map(|source| match source {
            Source::Topic(topic) if topic.kind == KIND => Some(topic.name.clone()),
            _ => None,
        }).collect::<Vec<_>>();
    for root in roots {
        state.bots.teams.remove(&root);
    }
}
