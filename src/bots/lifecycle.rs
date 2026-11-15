//! Host facts about one task tree, offered on its `task:<root>` topic to the
//! advisor subscribed to it. Facts are edge-triggered from records reconcile
//! already reads; there is no clock, classifier or supervisor state for them.
//! They are gaps a lead could miss (the team went quiet, a member keeps
//! failing, a restart loop was retired, a claim lost its owner, members became
//! blocked on a claim), never single failures. Judge turns, retained peer
//! evidence (runs without an attempt) and turns woken only by advisor notes,
//! including the lead's, are not team activity, so an advisor cannot wake
//! itself through the team it advises. The model interprets; the host only
//! observes.
use super::{Message, Reason};
use crate::State;
use crate::rooms::claims::{self, Claim, Status};
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

/// One coalescing, non-urgent, non-direct reason for an advisor turn. Facts are
/// gaps a lead could miss, not churn: a single failed, interrupted or cancelled
/// turn (provider errors included) or a single failed exit is never one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// Members whose latest turns failed or were blocked the configured number
    /// of times in a row (`advisor_failure_streak`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeated: u32,
    /// Watched restart loops the host retired after repeated exits.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retired: u32,
    /// Live claims whose owner the host observed as lost.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub abandoned: u32,
    /// Member claims newly marked blocked, one entry per claim they wait on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<Blocked>,
    /// The last active member yielded with nothing queued or running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quiescent: bool,
    /// Chats the facts concern, newest first and bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<String>,
}
/// Member claims that became blocked on one claim, so several members waiting
/// on one producer read as one bottleneck. The producer's owner and status are
/// as the host saw them when the newest member became blocked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    pub room: String,
    /// The claim key they are blocked on (their `blocked_on`).
    pub on: String,
    /// Owner of that claim while it is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// working, blocked, done, abandoned, released or unclaimed.
    pub status: String,
    /// The newly blocked claims, bounded.
    pub waiting: Vec<Waiter>,
}
/// One member's blocked claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    pub key: String,
    pub chat: String,
}
impl Blocked {
    fn of(state: &State, claim: &Claim) -> Self {
        let on = claim.blocked_on.clone().unwrap_or_default();
        let (producer, status) = match claims::find(state, &claim.room, &on) {
            Some(held) if held.abandoned && held.status != Status::Released => (Some(held.owner.clone()), "abandoned"),
            Some(held) => match held.status {
                Status::Working => (Some(held.owner.clone()), "working"),
                Status::Blocked => (Some(held.owner.clone()), "blocked"),
                Status::Done => (Some(held.owner.clone()), "done"),
                Status::Released => (None, "released"),
            },
            None => (None, "unclaimed"),
        };
        Self { room: claim.room.clone(), on, producer, status: status.into(), waiting: Vec::new() }
    }
    fn wait(&mut self, waiter: &Waiter) {
        if self.waiting.len() < CHATS && !self.waiting.iter().any(|known| known.key == waiter.key) {
            self.waiting.push(waiter.clone());
        }
    }
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}
fn is_zero_revision(value: &u64) -> bool {
    *value == 0
}
impl Facts {
    pub(super) fn merge(&mut self, newer: &Self) {
        self.repeated = self.repeated.saturating_add(newer.repeated);
        self.retired = self.retired.saturating_add(newer.retired);
        self.abandoned = self.abandoned.saturating_add(newer.abandoned);
        for blocked in &newer.blocked {
            self.block(blocked);
        }
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
    /// Folds members blocked on the same claim into one entry, refreshing the
    /// producer's state to the newer observation.
    fn block(&mut self, newer: &Blocked) {
        if let Some(held) = self.blocked.iter_mut().find(|held| held.room == newer.room && held.on == newer.on) {
            (held.producer, held.status) = (newer.producer.clone(), newer.status.clone());
            for waiter in &newer.waiting {
                held.wait(waiter);
            }
        } else if self.blocked.len() < CHATS {
            self.blocked.push(newer.clone());
        }
    }
    fn is_empty(&self) -> bool {
        self.repeated == 0 && self.retired == 0 && self.abandoned == 0 && self.blocked.is_empty() && !self.quiescent
    }
    pub(super) fn describe(&self, root: &str) -> String {
        let mut parts = Vec::new();
        if self.repeated > 0 { parts.push(format!("{} member(s) failed several turns in a row", self.repeated)); }
        if self.retired > 0 { parts.push(format!("{} watched restart loop(s) retired after repeated exits", self.retired)); }
        if self.abandoned > 0 { parts.push(format!("{} claim(s) abandoned by a lost owner", self.abandoned)); }
        for blocked in &self.blocked {
            let producer = blocked.producer.as_ref().map_or(blocked.status.clone(), |owner| format!("owner {owner}, {}", blocked.status));
            let keys = blocked.waiting.iter().map(|waiter| waiter.key.as_str()).collect::<Vec<_>>().join(", ");
            parts.push(format!("{} claim(s) newly blocked on {}/{} ({producer}): {keys}", blocked.waiting.len(), blocked.room, blocked.on));
        }
        if self.quiescent { parts.push("the team went quiet: no member turn is running or queued".into()); }
        let chats = if self.chats.is_empty() { String::new() } else { format!(" (chats: {})", self.chats.join(", ")) };
        format!("host facts for task {root}: {}{chats}", parts.join("; "))
    }
}

/// What the host last observed of an advised team; the cursor orders its facts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Team {
    pub(super) cursor: u64,
    /// Members currently on a reported failure streak, in chat order and
    /// bounded; a member is reported again only after its streak breaks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) failing: Vec<String>,
    /// Settled member turns when the team last went quiet.
    pub(super) settled: usize,
    /// Latest `blocked_at` revision of a member claim already observed; a
    /// blocked state is reported once, and again only after a new transition.
    #[serde(default, skip_serializing_if = "is_zero_revision")]
    pub(super) blocked: u64,
}
/// Members on a failure streak a snapshot remembers.
const FAILING: usize = 64;

pub(super) fn topic(root: &str) -> Topic {
    Topic::new(KIND, root)
}
pub(super) fn source(root: &str) -> Source {
    Source::Topic(topic(root))
}
/// Roots with an advisor subscribed whose root chat still selects the advisor
/// lens, without allocating when there are none. With the lens off, a tree's
/// topic is not computed at all.
fn advised(state: &State) -> Vec<&str> {
    let mut roots = state.bots.supervisor.definitions().flat_map(|definition| &definition.sources).filter_map(|source| match source {
        Source::Topic(topic) if topic.kind == KIND => Some(topic.name.as_str()),
        _ => None,
    }).collect::<Vec<_>>();
    if roots.is_empty() {
        return roots;
    }
    roots.sort_unstable();
    roots.dedup();
    roots.retain(|root| super::advisor_offered(state, root));
    roots
}
pub(super) fn advises(definition: &control_bot::Definition) -> bool {
    team(definition).is_some()
}
/// The task tree an advisor's definition subscribes to.
pub(super) fn team(definition: &control_bot::Definition) -> Option<&str> {
    definition.sources.iter().find_map(|source| match source {
        Source::Topic(topic) if topic.kind == KIND => Some(topic.name.as_str()),
        _ => None,
    })
}

/// A turn woken only by advisor notes is the advisor's echo, not new activity.
fn echo(run: &crate::runtime::Run) -> bool {
    run.bot_turn.as_ref().is_some_and(|turn| !turn.reasons.is_empty()
        && turn.reasons.iter().all(|reason| matches!(reason, Reason::Info { info: Message::Note, .. })))
}

/// Edge-triggered quiescence, repeated-failure and blocked-claim facts per
/// advised team, with the snapshot to record. Read-only; one pass over chats,
/// runs and claims, and only when an advisor exists.
pub(super) fn observe(state: &State) -> Vec<(String, Team, Facts)> {
    let roots = advised(state);
    if roots.is_empty() {
        return Vec::new();
    }
    let index = state.chats.iter().enumerate().map(|(i, chat)| (chat.id.as_str(), i)).collect::<Fast<_, _>>();
    // Member chat → its position in `state.chats`; `team_at` gives its team.
    let mut member: Fast<&str, usize> = Fast::default();
    let mut team_at = vec![usize::MAX; state.chats.len()];
    for (position, chat) in state.chats.iter().enumerate() {
        let mut session = chat;
        for _ in 0..64 {
            match session.parent_chat.as_deref().and_then(|parent| index.get(parent)) {
                Some(&parent) => session = &state.chats[parent],
                None => break,
            }
        }
        if session.parent_chat.is_none() && let Ok(team) = roots.binary_search(&session.id.as_str()) {
            member.insert(chat.id.as_str(), position);
            team_at[position] = team;
        }
    }
    let threshold = state.runtime.execution.advisor_failure_streak.max(2);
    // Per member, newest first: consecutive failed or blocked turns, until a
    // turn that settled otherwise. Interrupted and cancelled turns neither
    // extend nor break a streak.
    let mut streak = vec![(0usize, true); state.chats.len()];
    struct Scan { active: bool, settled: usize }
    let mut scans = roots.iter().map(|_| Scan { active: false, settled: 0 }).collect::<Vec<_>>();
    for run in state.runtime.runs.iter().rev() {
        let Some(&position) = run.chat_id.as_deref().and_then(|chat| member.get(chat)) else { continue };
        if crate::delegation::is_judge(run) {
            continue;
        }
        let scan = &mut scans[team_at[position]];
        match run.status.as_str() {
            "queued" | "running" | "waiting" | "cancelling" => scan.active = true,
            status if !run.attempts.is_empty() && super::terminal(status) => {
                if !echo(run) { scan.settled += 1; }
                let (count, open) = &mut streak[position];
                match status {
                    "failed" | "blocked" if *open => *count += 1,
                    "interrupted" | "cancelled" | "failed" | "blocked" => {}
                    _ => *open = false,
                }
            }
            _ => {}
        }
    }
    for request in &state.bots.requests {
        if let Some(&position) = member.get(request.chat.0.as_str()) { scans[team_at[position]].active = true; }
    }
    // Live member claims blocked on another claim, by team.
    let mut blocked = roots.iter().map(|_| Vec::new()).collect::<Vec<Vec<&Claim>>>();
    for claim in &state.collective.claims {
        if claim.status != Status::Blocked || claim.abandoned || claim.blocked_at == 0 {
            continue;
        }
        if let Some(&position) = member.get(claim.owner.as_str()) {
            blocked[team_at[position]].push(claim);
        }
    }
    let mut failing = roots.iter().map(|_| Vec::new()).collect::<Vec<Vec<&str>>>();
    for (position, (count, _)) in streak.iter().enumerate() {
        if *count >= threshold && failing[team_at[position]].len() < FAILING {
            failing[team_at[position]].push(state.chats[position].id.as_str());
        }
    }
    roots.into_iter().zip(scans).zip(failing).zip(blocked).filter_map(|(((root, scan), failing), blocked)| {
        let newest = blocked.iter().map(|claim| claim.blocked_at).max().unwrap_or(0);
        let Some(known) = state.bots.teams.get(root) else {
            // Subscription starts here: earlier history is not reported.
            let failing = failing.into_iter().map(str::to_owned).collect();
            return Some((root.to_owned(), Team { cursor: 0, failing, settled: scan.settled, blocked: newest }, Facts::default()));
        };
        let mut team = known.clone();
        let mut facts = Facts::default();
        for chat in failing.iter().filter(|chat| !known.failing.iter().any(|known| known == *chat)) {
            facts.repeated += 1;
            facts.concern(chat);
        }
        if team.failing.len() != failing.len() || team.failing.iter().zip(&failing).any(|(known, now)| known != now) {
            team.failing = failing.into_iter().map(str::to_owned).collect();
        }
        for claim in blocked.iter().filter(|claim| claim.blocked_at > known.blocked) {
            let mut entry = Blocked::of(state, claim);
            entry.wait(&Waiter { key: claim.key.clone(), chat: claim.owner.clone() });
            facts.block(&entry);
            facts.concern(&claim.owner);
        }
        team.blocked = team.blocked.max(newest);
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
    if !state.bots.teams.contains_key(root) || !super::advisor_offered(state, root) {
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
