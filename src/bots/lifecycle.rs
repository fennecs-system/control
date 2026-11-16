//! Host facts about one task tree, offered on its `task:<root>` topic to the
//! advisor subscribed to it. Facts are edge-triggered from records reconcile
//! already reads; there is no clock, classifier or supervisor state for them.
//! They are gaps a lead could miss (the team went quiet, a member keeps
//! failing, a restart loop was retired, a claim lost its owner, members became
//! blocked on a claim, members report a similar blocker, turns keep settling
//! while open claims do not advance, one claim holds up several members, a
//! member's turns keep ending without an accepted handoff), never
//! single failures. Judge turns, retained peer
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
    /// Team turns kept settling while claims stayed open and none advanced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled: Option<Stall>,
    /// Members blocked or failing with similar reasons, one entry per reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared: Vec<Shared>,
    /// Live claims that several members wait on, directly or transitively.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bottlenecks: Vec<Bottleneck>,
    /// Bots whose turns keep ending without an accepted handoff past their
    /// recovery allowance; they now wait with a backoff recheck.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncheckpointed: Vec<Uncheckpointed>,
    /// Chats the facts concern, newest first and bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<String>,
}
/// A member that cannot checkpoint: `turns` consecutive turns ended without an
/// accepted handoff. `rejection` is the host's last refusal, shortened; none
/// means its turns ended without calling the handoff at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uncheckpointed {
    pub chat: String,
    pub turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<String>,
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
/// Activity without progress: `turns` member turns settled since any claim
/// last advanced (became done, or resumed working after being blocked) while
/// `open` claims stayed working or blocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stall {
    pub turns: u32,
    pub open: u32,
}
/// Two or more members reporting a similar blocker: a blocked claim's note or
/// the latest result of a member on a failure streak. `reason` is one member's
/// text as reported, untrusted and shortened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    pub reason: String,
    pub members: Vec<String>,
}
/// A live (working or blocked) claim whose transitive dependents through
/// `blocked_on` belong to at least `advisor_bottleneck_members` members other
/// than its owner: the team's critical path. Ages count node-wide claim updates
/// (the claim revision sequence), not time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bottleneck {
    pub room: String,
    pub key: String,
    pub owner: String,
    /// working, blocked or abandoned.
    pub status: String,
    /// The claim this one itself waits on, when blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    /// Claim updates since this incarnation was acquired.
    pub open: u64,
    /// Claim updates since it last advanced, or since it was acquired.
    pub idle: u64,
    /// Distinct dependent members other than the owner, sorted and bounded.
    pub members: Vec<String>,
    /// Dependent claims, nearest first and bounded.
    pub waiting: Vec<Waiter>,
    /// All dependent claims, direct and transitive.
    pub dependents: u32,
    /// The claim waits on itself through its dependents: a circular wait.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cycle: bool,
}
/// Bottlenecks a snapshot remembers.
const BOTTLENECKS: usize = 64;

/// Live claims that `threshold` or more members wait on, each with an identity
/// of (claim incarnation, dependent claim set). `claims` are the team members'
/// claims; dependents are their live blocked claims. Visited sets make cycles
/// in `blocked_on` terminate.
fn bottlenecks(state: &State, claims: &[&Claim], threshold: usize) -> Vec<(u64, Bottleneck)> {
    // Live members' claims waiting on each (room, key).
    let mut waiting: Fast<(&str, &str), Vec<&Claim>> = Fast::default();
    for claim in claims.iter().filter(|claim| claim.status == Status::Blocked && !claim.abandoned) {
        if let Some(on) = claim.blocked_on.as_deref() {
            waiting.entry((claim.room.as_str(), on)).or_default().push(claim);
        }
    }
    if waiting.values().map(Vec::len).sum::<usize>() < threshold {
        return Vec::new();
    }
    let live = state.collective.claims.iter()
        .filter(|claim| matches!(claim.status, Status::Working | Status::Blocked) && waiting.contains_key(&(claim.room.as_str(), claim.key.as_str())));
    let mut found = Vec::new();
    for held in live {
        let mut visited: Fast<&str, ()> = Fast::default();
        let (mut cycle, mut next) = (false, 0);
        let mut frontier = vec![held];
        // Breadth first, so the nearest dependents are listed first.
        while next < frontier.len() {
            let at = frontier[next];
            next += 1;
            for &claim in waiting.get(&(at.room.as_str(), at.key.as_str())).into_iter().flatten() {
                if claim.key == held.key {
                    cycle = true;
                } else if visited.insert(claim.key.as_str(), ()).is_none() {
                    frontier.push(claim);
                }
            }
        }
        let seen = &frontier[1..];
        let mut members = seen.iter().map(|claim| claim.owner.as_str()).filter(|owner| *owner != held.owner).collect::<Vec<_>>();
        members.sort_unstable();
        members.dedup();
        if members.len() < threshold {
            continue;
        }
        let mut keys = seen.iter().map(|claim| claim.key.as_str()).collect::<Vec<_>>();
        keys.sort_unstable();
        let mut identity = Fx::default();
        for part in [held.room.as_str(), held.key.as_str()].into_iter().chain(keys) {
            identity.write(part.as_bytes());
        }
        identity.add(held.incarnation);
        let seq = state.collective.claim_seq;
        found.push((identity.finish(), Bottleneck {
            room: held.room.clone(),
            key: held.key.clone(),
            owner: held.owner.clone(),
            status: if held.abandoned { "abandoned" } else if held.status == Status::Blocked { "blocked" } else { "working" }.into(),
            blocked_on: held.blocked_on.clone(),
            open: seq.saturating_sub(held.incarnation),
            idle: seq.saturating_sub(held.incarnation.max(held.advanced_at)),
            members: members.iter().take(FAILING).map(|member| (*member).to_owned()).collect(),
            waiting: seen.iter().take(CHATS).map(|claim| Waiter { key: claim.key.clone(), chat: claim.owner.clone() }).collect(),
            dependents: u32::try_from(seen.len()).unwrap_or(u32::MAX),
            cycle,
        }));
    }
    // Widest first; the bound keeps the most consequential.
    found.sort_by(|(_, a), (_, b)| b.members.len().cmp(&a.members.len()).then(b.dependents.cmp(&a.dependents)).then_with(|| (&a.room, &a.key).cmp(&(&b.room, &b.key))));
    found.truncate(BOTTLENECKS);
    found
}
fn identities(necks: &[(u64, Bottleneck)]) -> Vec<u64> {
    let mut identities = necks.iter().map(|(identity, _)| *identity).collect::<Vec<_>>();
    identities.sort_unstable();
    identities
}
/// Characters of a reported reason kept in a fact.
const REASON: usize = 120;
/// Reasons compared per team per pass; the comparison is pairwise.
const REASONS: usize = 64;

/// A reason's distinct words for a cheap similarity check: lower-cased words of
/// three or more letters, hashed and sorted. Words containing digits (IDs,
/// ports, counts, times) are dropped because they vary between members
/// reporting one cause. No embeddings or vocabulary.
fn words(text: &str) -> Vec<u64> {
    let text = &text[..text.floor_char_boundary(512)];
    let mut words = text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().nth(2).is_some() && !word.chars().any(|c| c.is_numeric()))
        .map(|word| {
            let mut hash = Fx::default();
            for c in word.chars().flat_map(char::to_lowercase) {
                hash.add(u64::from(c));
            }
            hash.finish()
        })
        .take(64)
        .collect::<Vec<_>>();
    words.sort_unstable();
    words.dedup();
    words
}
/// Whether two word sets share at least two words and most of their union
/// (Jaccard ≥ 0.6).
fn similar(a: &[u64], b: &[u64]) -> bool {
    let (mut i, mut j, mut common) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => (i, j, common) = (i + 1, j + 1, common + 1),
        }
    }
    common >= 2 && common * 5 >= (a.len() + b.len() - common) * 3
}
/// Groups `(member, reason)` pairs with similar reasons; only groups of two or
/// more distinct members are shared blockers.
fn share<'a>(reasons: &[(&'a str, &'a str, u64)]) -> Vec<(Vec<u64>, &'a str, Vec<&'a str>)> {
    let mut groups: Vec<(Vec<u64>, &str, Vec<&str>)> = Vec::new();
    for &(member, reason, _) in reasons.iter().take(REASONS) {
        let words = words(reason);
        if words.len() < 2 {
            continue;
        }
        match groups.iter_mut().find(|(known, ..)| similar(known, &words)) {
            Some((_, _, members)) => if !members.contains(&member) { members.push(member) },
            None => groups.push((words, reason, vec![member])),
        }
    }
    groups.retain(|(.., members)| members.len() >= 2);
    groups
}
fn excerpt(reason: &str) -> String {
    let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    match reason.char_indices().nth(REASON) {
        Some((end, _)) => format!("{}…", &reason[..end]),
        None => reason,
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
        self.stalled = newer.stalled.or(self.stalled);
        for shared in &newer.shared {
            self.share(shared);
        }
        for bottleneck in &newer.bottlenecks {
            self.bottleneck(bottleneck);
        }
        for stuck in &newer.uncheckpointed {
            self.uncheckpointed(stuck.clone());
        }
        for chat in &newer.chats {
            self.concern(chat);
        }
    }
    /// A newer report for the same member replaces the held one.
    pub(super) fn uncheckpointed(&mut self, mut newer: Uncheckpointed) {
        newer.rejection = newer.rejection.as_deref().map(excerpt);
        if let Some(held) = self.uncheckpointed.iter_mut().find(|held| held.chat == newer.chat) {
            *held = newer;
        } else if self.uncheckpointed.len() < CHATS {
            self.uncheckpointed.push(newer);
        }
    }
    /// Folds a shared blocker into an entry with the same reason or a common
    /// member, keeping the newer reason text.
    fn share(&mut self, newer: &Shared) {
        if let Some(held) = self.shared.iter_mut().find(|held| held.reason == newer.reason || held.members.iter().any(|member| newer.members.contains(member))) {
            held.reason.clone_from(&newer.reason);
            for member in &newer.members {
                if held.members.len() < CHATS && !held.members.contains(member) {
                    held.members.push(member.clone());
                }
            }
        } else if self.shared.len() < CHATS {
            self.shared.push(newer.clone());
        }
    }
    /// A newer observation of the same claim replaces the held one.
    fn bottleneck(&mut self, newer: &Bottleneck) {
        if let Some(held) = self.bottlenecks.iter_mut().find(|held| held.room == newer.room && held.key == newer.key) {
            held.clone_from(newer);
        } else if self.bottlenecks.len() < CHATS {
            self.bottlenecks.push(newer.clone());
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
            && self.stalled.is_none() && self.shared.is_empty() && self.bottlenecks.is_empty() && self.uncheckpointed.is_empty()
    }
    pub(super) fn describe(&self, root: &str) -> String {
        let mut parts = Vec::new();
        if self.repeated > 0 { parts.push(format!("{} member(s) failed several turns in a row", self.repeated)); }
        if self.retired > 0 { parts.push(format!("{} watched restart loop(s) retired after repeated exits", self.retired)); }
        if self.abandoned > 0 { parts.push(format!("{} claim(s) abandoned by a lost owner", self.abandoned)); }
        for neck in &self.bottlenecks {
            let on = neck.blocked_on.as_ref().map_or(String::new(), |on| format!(" on {on}"));
            let cycle = if neck.cycle { ", in a circular wait" } else { "" };
            let mut members = neck.members.iter().take(CHATS).map(String::as_str).collect::<Vec<_>>().join(", ");
            if neck.members.len() > CHATS { members.push_str(", …"); }
            let keys = neck.waiting.iter().map(|waiter| waiter.key.as_str()).collect::<Vec<_>>().join(", ");
            parts.push(format!("bottleneck {}/{} (owner {}, {}{on}{cycle}; acquired {} claim updates ago, {} since it last advanced) holds up {} claim(s) of {} member(s) directly or transitively ({members}): {keys}",
                neck.room, neck.key, neck.owner, neck.status, neck.open, neck.idle, neck.dependents, neck.members.len()));
        }
        for blocked in &self.blocked {
            let producer = blocked.producer.as_ref().map_or(blocked.status.clone(), |owner| format!("owner {owner}, {}", blocked.status));
            let keys = blocked.waiting.iter().map(|waiter| waiter.key.as_str()).collect::<Vec<_>>().join(", ");
            parts.push(format!("{} claim(s) newly blocked on {}/{} ({producer}): {keys}", blocked.waiting.len(), blocked.room, blocked.on));
        }
        for shared in &self.shared {
            parts.push(format!("{} members report a similar blocker ({}): \"{}\"", shared.members.len(), shared.members.join(", "), shared.reason));
        }
        if let Some(stall) = self.stalled {
            parts.push(format!("busy but stalled: {} team turns settled while {} claim(s) stayed open and none became done or resumed from blocked", stall.turns, stall.open));
        }
        for stuck in &self.uncheckpointed {
            parts.push(match &stuck.rejection {
                Some(rejection) => format!("member {} cannot checkpoint: {} turn(s) ended without an accepted handoff; last rejection: \"{rejection}\"", stuck.chat, stuck.turns),
                None => format!("member {} cannot checkpoint: {} turn(s) ended without calling control_turn_handoff", stuck.chat, stuck.turns),
            });
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
    /// Latest member claim `advanced_at` observed.
    #[serde(default, skip_serializing_if = "is_zero_revision")]
    pub(super) progress: u64,
    /// Settled member turns when a claim last advanced or open claims last
    /// appeared or cleared: the start of the stall window.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub(super) since: usize,
    /// Whether members held working or blocked claims when last observed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) open: bool,
    /// The current stall was reported; progress or clearing claims re-arms it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) stalled: bool,
    /// Members currently in a reported shared blocker, sorted and bounded; a
    /// member is reported again only after it leaves every such group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) sharing: Vec<String>,
    /// Fingerprint of the blocker reasons `sharing` was computed from, so
    /// unchanged reasons are not compared again.
    #[serde(default, skip_serializing_if = "is_zero_revision")]
    pub(super) reasons: u64,
    /// Fingerprint of the members' claim revisions `necks` was computed
    /// from, so unchanged claims are not walked again.
    #[serde(default, skip_serializing_if = "is_zero_revision")]
    pub(super) claims: u64,
    /// Identities of the current bottlenecks (claim incarnation and dependent
    /// set), sorted; one is reported again only after its dependents change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) necks: Vec<u64>,
}
fn is_zero_count(value: &usize) -> bool {
    *value == 0
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
    // Per member, its newest failed or blocked turn: the reason a streak reports.
    let mut latest = vec![None; state.chats.len()];
    struct Scan { active: bool, settled: usize }
    let mut scans = roots.iter().map(|_| Scan { active: false, settled: 0 }).collect::<Vec<_>>();
    for (index, run) in state.runtime.runs.iter().enumerate().rev() {
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
                    "failed" | "blocked" if *open => {
                        *count += 1;
                        latest[position].get_or_insert((index, run.result.as_str()));
                    }
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
    // Per team: live member claims blocked on another claim, open (working or
    // blocked) claims, the latest claim advance, reported blocker reasons and
    // a fingerprint of every member claim revision.
    #[derive(Default)]
    struct Claims<'a> { blocked: Vec<&'a Claim>, open: u32, progress: u64, reasons: Vec<(&'a str, &'a str, u64)>, revisions: Fx }
    let mut held = roots.iter().map(|_| Claims::default()).collect::<Vec<_>>();
    for claim in &state.collective.claims {
        let Some(&position) = member.get(claim.owner.as_str()) else { continue };
        let team = &mut held[team_at[position]];
        team.progress = team.progress.max(claim.advanced_at);
        team.revisions.add(claim.revision);
        if claim.abandoned || !matches!(claim.status, Status::Working | Status::Blocked) {
            continue;
        }
        team.open += 1;
        if claim.status == Status::Blocked && claim.blocked_at != 0 {
            team.blocked.push(claim);
            if !claim.note.is_empty() {
                team.reasons.push((claim.owner.as_str(), claim.note.as_str(), claim.revision));
            }
        }
    }
    let mut failing = roots.iter().map(|_| Vec::new()).collect::<Vec<Vec<&str>>>();
    for (position, (count, _)) in streak.iter().enumerate() {
        if *count >= threshold && failing[team_at[position]].len() < FAILING {
            let chat = state.chats[position].id.as_str();
            failing[team_at[position]].push(chat);
            if let Some((index, reason)) = latest[position].filter(|(_, reason)| !reason.is_empty()) {
                // Run positions are tagged apart from claim revisions.
                held[team_at[position]].reasons.push((chat, reason, index as u64 | 1 << 63));
            }
        }
    }
    let window = state.runtime.execution.advisor_stall_turns.max(4);
    let crowd = state.runtime.execution.advisor_bottleneck_members.max(2);
    roots.into_iter().zip(scans).zip(failing).zip(held).enumerate().filter_map(|(slot, (((root, scan), failing), held))| {
        let Claims { blocked, open, progress, reasons, revisions } = held;
        let newest = blocked.iter().map(|claim| claim.blocked_at).max().unwrap_or(0);
        // Reasons are compared only when two members could share one and the
        // reasons changed since the last observation (claim revisions and run
        // positions identify them), so steady blocked teams do no text work.
        let mut fingerprint = Fx::default();
        for (_, _, tag) in &reasons {
            fingerprint.add(*tag);
        }
        let fingerprint = if reasons.len() >= 2 { fingerprint.finish() } else { 0 };
        let known = state.bots.teams.get(root);
        let shared = if fingerprint != 0 && known.is_none_or(|known| known.reasons != fingerprint) { share(&reasons) } else { Vec::new() };
        let mut sharing = shared.iter().flat_map(|(.., members)| members.iter().copied()).collect::<Vec<_>>();
        sharing.sort_unstable();
        sharing.dedup();
        sharing.truncate(FAILING);
        // The dependency graph is walked only when a member claim changed (any
        // update, abandonment or release gives it a new revision).
        let revisions = revisions.finish();
        let necks = known.is_none_or(|known| known.claims != revisions).then(|| {
            let claims = state.collective.claims.iter()
                .filter(|claim| member.get(claim.owner.as_str()).is_some_and(|&position| team_at[position] == slot))
                .collect::<Vec<_>>();
            bottlenecks(state, &claims, crowd)
        });
        let Some(known) = known else {
            // Subscription starts here: earlier history is not reported.
            let failing = failing.into_iter().map(str::to_owned).collect();
            let sharing = sharing.into_iter().map(str::to_owned).collect();
            let necks = identities(necks.as_deref().unwrap_or_default());
            return Some((root.to_owned(), Team { cursor: 0, failing, settled: scan.settled, blocked: newest, progress, since: scan.settled, open: open > 0, stalled: false, sharing, reasons: fingerprint, claims: revisions, necks }, Facts::default()));
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
        // A shared blocker is reported when a member joins it.
        for (_, reason, members) in shared.iter().filter(|(.., members)| members.iter().any(|member| known.sharing.binary_search_by(|known| known.as_str().cmp(member)).is_err())) {
            facts.share(&Shared { reason: excerpt(reason), members: members.iter().take(CHATS).map(|member| (*member).to_owned()).collect() });
            for member in members {
                facts.concern(member);
            }
        }
        if fingerprint != known.reasons {
            team.reasons = fingerprint;
            if team.sharing.len() != sharing.len() || team.sharing.iter().zip(&sharing).any(|(known, now)| known != now) {
                team.sharing = sharing.into_iter().map(str::to_owned).collect();
            }
        }
        // A bottleneck is reported when it appears or its dependents change.
        if let Some(necks) = necks {
            for (_, neck) in necks.iter().filter(|(identity, _)| known.necks.binary_search(identity).is_err()) {
                facts.bottleneck(neck);
                facts.concern(&neck.owner);
            }
            team.claims = revisions;
            team.necks = identities(&necks);
        }
        // Busy but stalled: turns keep settling while claims stay open and
        // none advances. Progress, or open claims appearing or clearing,
        // restarts the window; a stall is reported once until then.
        let open_now = open > 0;
        if progress > known.progress || open_now != known.open || scan.settled < known.since {
            (team.progress, team.open, team.since, team.stalled) = (progress.max(known.progress), open_now, scan.settled, false);
        } else if open_now && !known.stalled && scan.settled - known.since >= window {
            facts.stalled = Some(Stall { turns: u32::try_from(scan.settled - known.since).unwrap_or(u32::MAX), open });
            team.stalled = true;
        }
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
