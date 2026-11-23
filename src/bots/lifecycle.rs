//! Host facts about one task tree, offered on its `task:<root>` topic to the
//! advisor subscribed to it. Facts are edge-triggered from records reconcile
//! already reads; there is no clock, classifier or supervisor state for them.
//! They are gaps a lead could miss (the team went quiet, a member keeps
//! failing, a restart loop was retired, a claim lost its owner, members became
//! blocked on a claim, members report a similar blocker, turns keep settling
//! while open claims do not advance, a claim holder sits idle while peers
//! work, one claim holds up several members, a member's turns keep ending
//! without an accepted handoff, a turn reached its time limit), never other
//! single failures. Judge turns, retained peer evidence (runs without an
//! attempt) and turns woken only by observer notes, including the lead's, are
//! not team activity, so an advisor cannot wake itself through the team it
//! advises. The model interprets; the host only observes.
use super::pathology::{self, Burn, Churn, Loop, Polling};
use super::{Message, Reason};
use crate::State;
use crate::rooms::claims::graph::{Fast, Fx, Waits};
use crate::rooms::claims::{self, Claim, Status};
use crate::runtime::RunStatus;
use control_bot::{Source, Topic};
use control_effect::machine::Machine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::Hasher;

pub(super) const KIND: &str = "task";
/// The topic an acceptance check hears: only its team's `finished` fact.
pub(super) const DONE_KIND: &str = "done";
/// The topic an ask-only advisor holds: it names the advisor's team and
/// carries no events, so only its members' questions wake it.
pub(super) const ASK_KIND: &str = "ask";
/// Chats named in one coalesced fact; the monitor reads the rest itself.
const CHATS: usize = 8;

/// One coalescing, non-urgent, non-direct reason for an advisor turn. Facts are
/// gaps a lead could miss, not churn: a single failed, interrupted or cancelled
/// turn (provider errors included) or a single failed exit is never one; a
/// turn stopped at its time limit is (`timed_out`).
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
    /// The team went quiet with no open claim and at least one done: it
    /// considers its goal finished. Also offered alone on the `done` topic.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finished: bool,
    /// Members holding a working claim with no turn running or queued and no
    /// running watch while other team turns kept settling: idle with nothing
    /// that will wake them. One entry per member, naming one such claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub idle: Vec<Waiter>,
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
    /// Members, the lead included, whose latest turn reached its attempt time
    /// limit and was stopped. Unlike other single failures this is a fact: the
    /// turn's scope did not fit, and its unpublished work is only a recovery
    /// candidate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timed_out: Vec<String>,
    /// Pathology: file- or tool-building commands kept running while the
    /// team's claims and score probes did not advance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub churn: Option<Churn>,
    /// Pathology: a member's same command failed again and again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<Loop>,
    /// Pathology: a member keeps issuing status-only commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub polling: Vec<Polling>,
    /// Pathology: many tokens spent since the team last advanced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burn: Option<Burn>,
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
        Self {
            room: claim.room.clone(),
            on,
            producer,
            status: status.into(),
            waiting: Vec::new(),
        }
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
/// claims; dependents are their live blocked claims. The walk is the claim
/// graph's (`rooms::claims::graph`), shared with `control_room_plan`.
pub(super) fn bottlenecks(state: &State, claims: &[&Claim], threshold: usize) -> Vec<(u64, Bottleneck)> {
    let waiting = Waits::of(claims.iter().copied());
    if waiting.edges() < threshold {
        return Vec::new();
    }
    let live = state.collective.claims.iter().filter(|claim| {
        matches!(claim.status, Status::Working | Status::Blocked) && waiting.waited(&claim.room, &claim.key)
    });
    let mut found: Vec<_> = live
        .filter_map(|held| {
            let reach = waiting.reach(&held.room, &held.key, Some(&held.owner));
            let (seen, members, cycle) = (&reach.dependents, &reach.members, reach.cycle);
            if members.len() < threshold {
                return None;
            }
            let mut keys = seen.iter().map(|claim| claim.key.as_str()).collect::<Vec<_>>();
            keys.sort_unstable();
            let mut identity = Fx::default();
            for part in [held.room.as_str(), held.key.as_str()].into_iter().chain(keys) {
                identity.write(part.as_bytes());
            }
            identity.add(held.incarnation);
            let seq = state.collective.claim_seq;
            Some((
                identity.finish(),
                Bottleneck {
                    room: held.room.clone(),
                    key: held.key.clone(),
                    owner: held.owner.clone(),
                    status: if held.abandoned {
                        "abandoned"
                    } else if held.status == Status::Blocked {
                        "blocked"
                    } else {
                        "working"
                    }
                    .into(),
                    blocked_on: held.blocked_on.clone(),
                    open: seq.saturating_sub(held.incarnation),
                    idle: seq.saturating_sub(held.incarnation.max(held.advanced_at)),
                    members: members
                        .iter()
                        .take(FAILING)
                        .map(|member| (*member).to_owned())
                        .collect(),
                    waiting: seen
                        .iter()
                        .take(CHATS)
                        .map(|claim| Waiter {
                            key: claim.key.clone(),
                            chat: claim.owner.clone(),
                        })
                        .collect(),
                    dependents: u32::try_from(seen.len()).unwrap_or(u32::MAX),
                    cycle,
                },
            ))
        })
        .collect();
    // Widest first; the bound keeps the most consequential.
    found.sort_by(|(_, a), (_, b)| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(b.dependents.cmp(&a.dependents))
            .then_with(|| (&a.room, &a.key).cmp(&(&b.room, &b.key)))
    });
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
    let mut words = text
        .split(|c: char| !c.is_alphanumeric())
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
            Some((_, _, members)) => {
                if !members.contains(&member) {
                    members.push(member)
                }
            }
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
        self.finished |= newer.finished;
        for idle in &newer.idle {
            if self.idle.len() < CHATS && !self.idle.iter().any(|known| known.chat == idle.chat) {
                self.idle.push(idle.clone());
            }
        }
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
        for chat in &newer.timed_out {
            self.timed_out(chat);
        }
        self.churn = newer.churn.clone().or(self.churn.take());
        self.burn = newer.burn.or(self.burn);
        for found in &newer.loops {
            if let Some(held) = self
                .loops
                .iter_mut()
                .find(|held| held.chat == found.chat && held.command == found.command)
            {
                held.clone_from(found);
            } else if self.loops.len() < CHATS {
                self.loops.push(found.clone());
            }
        }
        for found in &newer.polling {
            if let Some(held) = self.polling.iter_mut().find(|held| held.chat == found.chat) {
                held.clone_from(found);
            } else if self.polling.len() < CHATS {
                self.polling.push(found.clone());
            }
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
        if let Some(held) = self.shared.iter_mut().find(|held| {
            held.reason == newer.reason || held.members.iter().any(|member| newer.members.contains(member))
        }) {
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
        if let Some(held) = self
            .bottlenecks
            .iter_mut()
            .find(|held| held.room == newer.room && held.key == newer.key)
        {
            held.clone_from(newer);
        } else if self.bottlenecks.len() < CHATS {
            self.bottlenecks.push(newer.clone());
        }
    }
    fn timed_out(&mut self, chat: &str) {
        if self.timed_out.len() < CHATS && !self.timed_out.iter().any(|known| known == chat) {
            self.timed_out.push(chat.to_owned());
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
        if let Some(held) = self
            .blocked
            .iter_mut()
            .find(|held| held.room == newer.room && held.on == newer.on)
        {
            (held.producer, held.status) = (newer.producer.clone(), newer.status.clone());
            for waiter in &newer.waiting {
                held.wait(waiter);
            }
        } else if self.blocked.len() < CHATS {
            self.blocked.push(newer.clone());
        }
    }
    fn is_empty(&self) -> bool {
        self.repeated == 0
            && self.retired == 0
            && self.abandoned == 0
            && self.blocked.is_empty()
            && !self.quiescent
            && !self.finished
            && self.idle.is_empty()
            && self.stalled.is_none()
            && self.shared.is_empty()
            && self.bottlenecks.is_empty()
            && self.uncheckpointed.is_empty()
            && self.timed_out.is_empty()
            && self.churn.is_none()
            && self.loops.is_empty()
            && self.polling.is_empty()
            && self.burn.is_none()
    }
    /// The facts an antibody hears: gaps that suggest a team is off course.
    /// Quiet teams, single blocked claims and abandoned claims are ordinary
    /// coordination its lead or advisor handles.
    pub(super) fn pathological(&self) -> Self {
        let facts = Self {
            quiescent: false,
            finished: false,
            blocked: Vec::new(),
            abandoned: 0,
            ..self.clone()
        };
        if facts.is_empty() { Self::default() } else { facts }
    }
    pub(super) fn describe(&self, root: &str) -> String {
        let mut parts = Vec::new();
        if self.repeated > 0 {
            parts.push(format!("{} member(s) failed several turns in a row", self.repeated));
        }
        if self.retired > 0 {
            parts.push(format!(
                "{} watched restart loop(s) retired after repeated exits",
                self.retired
            ));
        }
        if self.abandoned > 0 {
            parts.push(format!("{} claim(s) abandoned by a lost owner", self.abandoned));
        }
        for neck in &self.bottlenecks {
            let on = neck.blocked_on.as_ref().map_or(String::new(), |on| format!(" on {on}"));
            let cycle = if neck.cycle { ", in a circular wait" } else { "" };
            let mut members = neck
                .members
                .iter()
                .take(CHATS)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if neck.members.len() > CHATS {
                members.push_str(", …");
            }
            let keys = neck
                .waiting
                .iter()
                .map(|waiter| waiter.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("bottleneck {}/{} (owner {}, {}{on}{cycle}; acquired {} claim updates ago, {} since it last advanced) holds up {} claim(s) of {} member(s) directly or transitively ({members}): {keys}",
                neck.room, neck.key, neck.owner, neck.status, neck.open, neck.idle, neck.dependents, neck.members.len()));
        }
        for blocked in &self.blocked {
            let producer = blocked.producer.as_ref().map_or(blocked.status.clone(), |owner| {
                format!("owner {owner}, {}", blocked.status)
            });
            let keys = blocked
                .waiting
                .iter()
                .map(|waiter| waiter.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "{} claim(s) newly blocked on {}/{} ({producer}): {keys}",
                blocked.waiting.len(),
                blocked.room,
                blocked.on
            ));
        }
        for shared in &self.shared {
            parts.push(format!(
                "{} members report a similar blocker ({}): \"{}\"",
                shared.members.len(),
                shared.members.join(", "),
                shared.reason
            ));
        }
        for idle in &self.idle {
            if idle.key.is_empty() {
                parts.push(format!("idle member: member {} holds no claim and has no turn running or queued and no running routine while the team's claims are still open", idle.chat));
            } else {
                parts.push(format!("idle claim holder: member {} holds working claim {} but has no turn running or queued and no running routine while other team turns kept settling", idle.chat, idle.key));
            }
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
        for chat in &self.timed_out {
            let role = if chat == root { "the lead" } else { "member" };
            parts.push(format!(
                "{role} {chat}'s turn reached its time limit and was stopped before it checkpointed"
            ));
        }
        if let Some(churn) = &self.churn {
            parts.push(format!("harness churn: {} file- or tool-building command(s) ran while no claim advanced and no score rose (members: {})", churn.commands, churn.members.join(", ")));
        }
        for found in &self.loops {
            parts.push(format!(
                "repetition loop: member {} ran the same command {} times and it failed each time: \"{}\"",
                found.chat, found.failures, found.command
            ));
        }
        for found in &self.polling {
            parts.push(format!("polling: {} of member {}'s recent commands only read status; a routine (control_watch_start) could report instead", found.commands, found.chat));
        }
        if let Some(burn) = self.burn {
            parts.push(format!(
                "cost burn: {} tokens spent since a claim last advanced or a score last rose",
                burn.tokens
            ));
        }
        if self.finished {
            parts.push("the team went quiet with every claim done: it considers its goal finished".into());
        } else if self.quiescent {
            parts.push("the team went quiet: no member turn is running or queued".into());
        }
        let chats = if self.chats.is_empty() {
            String::new()
        } else {
            format!(" (chats: {})", self.chats.join(", "))
        };
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
    /// This completion was reported; a claim advancing or opening re-arms it,
    /// so idle check-ins by a finished team do not announce it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) finished: bool,
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
    /// Members whose latest settled turn timed out, in chat order and
    /// bounded; a member is reported again only after a later turn settles
    /// otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) timed: Vec<String>,
    /// Members currently idle with a working claim, sorted and bounded; a
    /// member is reported again only after it takes a turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) idle: Vec<String>,
    /// Pathology observations (`pathology`).
    #[serde(default, skip_serializing_if = "is_default")]
    pub(super) pathology: pathology::Snapshot,
}
impl Team {
    /// What currently holds for this team, one phrase each, for briefs.
    pub(super) fn conditions(&self) -> Vec<String> {
        let mut found = Vec::new();
        if !self.failing.is_empty() {
            found.push(format!("{} member(s) on a failure streak", self.failing.len()));
        }
        if !self.sharing.is_empty() {
            found.push(format!("{} member(s) report a similar blocker", self.sharing.len()));
        }
        if !self.necks.is_empty() {
            found.push(format!("{} bottleneck claim(s)", self.necks.len()));
        }
        if self.stalled {
            found.push("busy but stalled".into());
        }
        if !self.idle.is_empty() {
            found.push(format!("idle claim holder(s): {}", self.idle.join(", ")));
        }
        if !self.timed.is_empty() {
            found.push(format!("{} member(s) whose latest turn timed out", self.timed.len()));
        }
        let sick = &self.pathology;
        if !sick.loops.is_empty() {
            found.push(format!("{} repetition loop(s)", sick.loops.len()));
        }
        if !sick.polling.is_empty() {
            found.push(format!("polling: {}", sick.polling.join(", ")));
        }
        if sick.churned {
            found.push(format!(
                "harness churn ({} building commands without progress)",
                sick.churn
            ));
        }
        if sick.burned {
            found.push("cost burn without progress".into());
        }
        found
    }
}
fn is_default(value: &pathology::Snapshot) -> bool {
    *value == pathology::Snapshot::default()
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
pub(super) fn done_topic(root: &str) -> Topic {
    Topic::new(DONE_KIND, root)
}
pub(super) fn done_source(root: &str) -> Source {
    Source::Topic(done_topic(root))
}
pub(super) fn ask_source(root: &str) -> Source {
    Source::Topic(Topic::new(ASK_KIND, root))
}
/// Roots with a monitor or acceptance check subscribed whose root chat still
/// selects the opt-in lens, without allocating when there are none. With the
/// lens off, or only an ask-only advisor, a tree's facts are not computed.
fn advised(state: &State) -> Vec<&str> {
    let mut roots = state
        .bots
        .supervisor
        .definitions()
        .filter_map(|definition| heard(definition).filter(|root| super::offered(state, root, gene(definition))))
        .collect::<Vec<_>>();
    roots.sort_unstable();
    roots.dedup();
    roots
}
pub(super) fn advises(definition: &control_bot::Definition) -> bool {
    team(definition).is_some()
}
/// The task tree a team observer's definition belongs to: a monitor's facts,
/// an acceptance check's completion or an ask-only advisor's team.
pub(super) fn team(definition: &control_bot::Definition) -> Option<&str> {
    definition.sources.iter().find_map(|source| match source {
        Source::Topic(topic) if topic.kind == KIND || topic.kind == DONE_KIND || topic.kind == ASK_KIND => {
            Some(topic.name.as_str())
        }
        _ => None,
    })
}
/// The task tree whose host facts a definition hears: a monitor's or an
/// acceptance check's, never an ask-only advisor's.
fn heard(definition: &control_bot::Definition) -> Option<&str> {
    team(definition).filter(|_| !asks(definition))
}
/// The gene whose selection on the root keeps a fact-hearing definition
/// subscribed: the acceptance check's for completion, else the monitor's.
fn gene(definition: &control_bot::Definition) -> &'static str {
    if accepts(definition) {
        super::ACCEPTANCE_TOOL
    } else {
        super::MONITOR_TOOL
    }
}
/// Whether a definition is an ask-only advisor's: the one members may ask.
pub(super) fn asks(definition: &control_bot::Definition) -> bool {
    definition
        .sources
        .iter()
        .any(|source| matches!(source, Source::Topic(topic) if topic.kind == ASK_KIND))
}
/// Whether a definition is a monitor's: it hears its team's host facts.
pub(super) fn monitors(definition: &control_bot::Definition) -> bool {
    definition
        .sources
        .iter()
        .any(|source| matches!(source, Source::Topic(topic) if topic.kind == KIND))
}
/// Whether a definition is an acceptance check's: it hears only completion.
pub(super) fn accepts(definition: &control_bot::Definition) -> bool {
    definition
        .sources
        .iter()
        .any(|source| matches!(source, Source::Topic(topic) if topic.kind == DONE_KIND))
}

/// A turn woken only by observer notes is the observer's echo, not new activity.
fn echo(run: &crate::runtime::Run) -> bool {
    run.bot_turn.as_ref().is_some_and(|turn| {
        !turn.reasons.is_empty()
            && turn.reasons.iter().all(|reason| {
                matches!(
                    reason,
                    Reason::Info {
                        info: Message::Note,
                        ..
                    }
                )
            })
    })
}

/// Edge-triggered quiescence, repeated-failure and blocked-claim facts per
/// advised team, with the snapshot to record. Read-only; one pass over chats,
/// runs and claims, and only when an advisor exists.
pub(super) fn observe(state: &State) -> Vec<(String, Team, Facts)> {
    let mut roots = advised(state);
    let immune = super::antibodies_active(state);
    if roots.is_empty() && !immune {
        return Vec::new();
    }
    let index = state
        .chats
        .iter()
        .enumerate()
        .map(|(i, chat)| (chat.id.as_str(), i))
        .collect::<Fast<_, _>>();
    // Each chat's top-level chat, as a position in `state.chats`.
    let tops = state
        .chats
        .iter()
        .map(|chat| {
            let mut session = chat;
            for _ in 0..64 {
                match session.parent_chat.as_deref().and_then(|parent| index.get(parent)) {
                    Some(&parent) => session = &state.chats[parent],
                    None => break,
                }
            }
            session.parent_chat.is_none().then(|| index[session.id.as_str()])
        })
        .collect::<Vec<_>>();
    if immune {
        // An antibody observes every team (a root with spawned members),
        // except trees that run antibodies themselves.
        let mut teams = tops
            .iter()
            .enumerate()
            .filter_map(|(position, top)| top.filter(|top| *top != position))
            .collect::<Vec<_>>();
        teams.sort_unstable();
        teams.dedup();
        roots.extend(
            teams
                .into_iter()
                .map(|top| state.chats[top].id.as_str())
                .filter(|root| !super::antibody_offered(state, root)),
        );
        roots.sort_unstable();
        roots.dedup();
    }
    if roots.is_empty() {
        return Vec::new();
    }
    // Member chat → its position in `state.chats`; `team_at` gives its team.
    let mut member: Fast<&str, usize> = Fast::default();
    let mut team_at = vec![usize::MAX; state.chats.len()];
    for (position, chat) in state.chats.iter().enumerate() {
        if let Some(top) = tops[position]
            && let Ok(team) = roots.binary_search(&state.chats[top].id.as_str())
        {
            member.insert(chat.id.as_str(), position);
            team_at[position] = team;
        }
    }
    // Retained finished commands, by the run that requested them; the run
    // pass below resolves runs to members.
    let finished = crate::services::finished_commands(state).collect::<Vec<_>>();
    let mut by_run: Fast<&str, usize> = finished.iter().map(|command| (command.run, usize::MAX)).collect();
    let mut unresolved = by_run.len();
    let threshold = state.runtime.execution.advisor_failure_streak.max(2);
    // Per member, newest first: consecutive failed or blocked turns, until a
    // turn that settled otherwise. Interrupted and cancelled turns neither
    // extend nor break a streak.
    let mut streak = vec![(0usize, true); state.chats.len()];
    // Per member, its newest failed or blocked turn: the reason a streak reports.
    let mut latest = vec![None; state.chats.len()];
    // Per member, whether its newest settled turn reached its time limit.
    let mut timed = vec![None; state.chats.len()];
    struct Scan {
        active: bool,
        settled: usize,
    }
    let mut scans = roots
        .iter()
        .map(|_| Scan {
            active: false,
            settled: 0,
        })
        .collect::<Vec<_>>();
    // Per member: team turns settled after its newest turn, and whether a
    // turn of its own is running or queued.
    let mut since = vec![None; state.chats.len()];
    let mut busy = vec![false; state.chats.len()];
    for (index, run) in state.runtime.runs.iter().enumerate().rev() {
        let Some(&position) = run.chat_id.as_deref().and_then(|chat| member.get(chat)) else {
            continue;
        };
        if crate::delegation::is_judge(run) {
            continue;
        }
        // Retained peer evidence (a completed run without an attempt) is not
        // a turn of its own: a member messaged every minute is still idle.
        if !(run.attempts.is_empty() && run.status == RunStatus::Completed) {
            since[position].get_or_insert(scans[team_at[position]].settled);
        }
        busy[position] |= run.status.is_active();
        // Commands come from recent runs; the newest-first walk stops looking
        // once every one is resolved.
        if unresolved > 0
            && let Some(slot) = by_run.get_mut(run.id.as_str()).filter(|slot| **slot == usize::MAX)
        {
            *slot = position;
            unresolved -= 1;
        }
        let scan = &mut scans[team_at[position]];
        match &run.status {
            status if status.is_active() => scan.active = true,
            // A continued turn's outcome belongs to its continuation.
            status if !run.attempts.is_empty() && status.is_finished() && *status != RunStatus::Continued => {
                if !echo(run) {
                    scan.settled += 1;
                }
                timed[position].get_or_insert(
                    *status == RunStatus::Failed
                        && run.attempts.last().is_some_and(|attempt| {
                            attempt.termination == Some(crate::attempt_control::Termination::TimedOut)
                        }),
                );
                let (count, open) = &mut streak[position];
                match status {
                    RunStatus::Failed | RunStatus::Blocked if *open => {
                        *count += 1;
                        latest[position].get_or_insert((index, run.result.as_str()));
                    }
                    RunStatus::Interrupted | RunStatus::Cancelled | RunStatus::Failed | RunStatus::Blocked => {}
                    RunStatus::Completed
                    | RunStatus::Queued
                    | RunStatus::Running
                    | RunStatus::Cancelling
                    | RunStatus::Waiting
                    | RunStatus::Continued
                    | RunStatus::Unrecognised(_) => *open = false,
                }
            }
            _ => {}
        }
    }
    for request in &state.bots.requests {
        if let Some(&position) = member.get(request.chat.0.as_str()) {
            (scans[team_at[position]].active, busy[position]) = (true, true);
        }
    }
    // Per team: members holding a working claim with no turn running or
    // queued and no running watch while `idle_turns` other team turns settled.
    let idle_turns = (state.runtime.execution.advisor_stall_turns / 3).max(4);
    let mut idle = roots.iter().map(|_| Vec::new()).collect::<Vec<Vec<Waiter>>>();
    for claim in state
        .collective
        .claims
        .iter()
        .filter(|claim| claim.status == Status::Working && !claim.abandoned)
    {
        let Some(&position) = member.get(claim.owner.as_str()) else {
            continue;
        };
        let team = &mut idle[team_at[position]];
        if !busy[position]
            && since[position].unwrap_or(scans[team_at[position]].settled) >= idle_turns
            && team.len() < FAILING
            && !team.iter().any(|known| known.chat == claim.owner)
            && !state
                .watches
                .iter()
                .any(|watch| watch.chat == claim.owner && watch.active())
        {
            team.push(Waiter {
                key: claim.key.clone(),
                chat: claim.owner.clone(),
            });
        }
    }
    // And persistent members holding no live claim at all (a finished slice,
    // nothing picked up) while their team still has working claims: an empty
    // key. One-off helpers, the lead and observers are not members to nudge.
    let open_teams = state
        .collective
        .claims
        .iter()
        .filter(|claim| claim.status == Status::Working && !claim.abandoned)
        .filter_map(|claim| member.get(claim.owner.as_str()).map(|&position| team_at[position]))
        .collect::<std::collections::HashSet<_>>();
    for chat in state.chats.iter().filter(|chat| chat.parent_chat.is_some()) {
        let Some(&position) = member.get(chat.id.as_str()) else {
            continue;
        };
        let team = &mut idle[team_at[position]];
        if open_teams.contains(&team_at[position])
            && !busy[position]
            && since[position].unwrap_or(scans[team_at[position]].settled) >= idle_turns
            && team.len() < FAILING
            && state.bots.persists(&chat.id)
            && !state.bots.advises(&chat.id)
            && !state.collective.claims.iter().any(|claim| {
                claim.owner == chat.id && !claim.abandoned && matches!(claim.status, Status::Working | Status::Blocked)
            })
            && !state
                .watches
                .iter()
                .any(|watch| watch.chat == chat.id && watch.active())
        {
            team.push(Waiter {
                key: String::new(),
                chat: chat.id.clone(),
            });
        }
    }
    for team in &mut idle {
        team.sort_by(|a, b| a.chat.cmp(&b.chat));
    }
    // Per team: live member claims blocked on another claim, open (working or
    // blocked) claims, the latest claim advance, reported blocker reasons and
    // a fingerprint of every member claim revision.
    #[derive(Default)]
    struct Claims<'a> {
        blocked: Vec<&'a Claim>,
        open: u32,
        done: bool,
        progress: u64,
        reasons: Vec<(&'a str, &'a str, u64)>,
        revisions: Fx,
    }
    let mut held = roots.iter().map(|_| Claims::default()).collect::<Vec<_>>();
    for claim in &state.collective.claims {
        let Some(&position) = member.get(claim.owner.as_str()) else {
            continue;
        };
        let team = &mut held[team_at[position]];
        team.progress = team.progress.max(claim.advanced_at);
        team.revisions.add(claim.revision);
        team.done |= claim.status == Status::Done;
        if claim.abandoned || !matches!(claim.status, Status::Working | Status::Blocked) {
            continue;
        }
        team.open += 1;
        if claim.status == Status::Blocked && claim.blocked_at != 0 {
            team.blocked.push(claim);
            if !claim.note.is_empty() {
                team.reasons
                    .push((claim.owner.as_str(), claim.note.as_str(), claim.revision));
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
                held[team_at[position]]
                    .reasons
                    .push((chat, reason, index as u64 | 1 << 63));
            }
        }
    }
    let mut timing = roots.iter().map(|_| Vec::new()).collect::<Vec<Vec<&str>>>();
    for (position, timed) in timed.iter().enumerate() {
        if *timed == Some(true) && timing[team_at[position]].len() < FAILING {
            timing[team_at[position]].push(state.chats[position].id.as_str());
        }
    }
    // Per team: pathology inputs (member commands, tokens, score advances).
    // Token totals change only as turns settle, so a team's runs are summed
    // again only when its settled count changed; otherwise the last total holds.
    let mut sick = roots
        .iter()
        .zip(&scans)
        .map(|(root, scan)| {
            let known = state
                .bots
                .teams
                .get(*root)
                .map(|team| &team.pathology)
                .filter(|known| known.turns == scan.settled);
            pathology::Scan {
                tokens: known.map_or(u64::MAX, |known| known.spent),
                turns: scan.settled,
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();
    if sick.iter().any(|scan| scan.tokens == u64::MAX) {
        for scan in sick.iter_mut().filter(|scan| scan.tokens == u64::MAX) {
            scan.tokens = 0;
            scan.recount = true;
        }
        for run in &state.runtime.runs {
            let Some(&position) = run.chat_id.as_deref().and_then(|chat| member.get(chat)) else {
                continue;
            };
            let scan = &mut sick[team_at[position]];
            if scan.recount && !crate::delegation::is_judge(run) {
                scan.tokens += run
                    .attempts
                    .iter()
                    .filter_map(|attempt| attempt.tokens)
                    .map(|t| {
                        t.input_tokens
                            .saturating_sub(t.cached_input_tokens)
                            .saturating_add(t.output_tokens)
                    })
                    .sum::<u64>();
            }
        }
    }
    for command in &finished {
        if let Some(&position) = by_run.get(command.run).filter(|position| **position != usize::MAX) {
            sick[team_at[position]].commands.push((
                state.chats[position].id.as_str(),
                command.id,
                command.command,
                command.failed,
            ));
        }
    }
    for probe in &state.scores {
        if let Some(&position) = member.get(probe.chat.as_str()) {
            let scan = &mut sick[team_at[position]];
            scan.score = scan.score.max(pathology::raised(&probe.samples));
        }
    }
    let limits = pathology::Limits::of(&state.runtime.execution);
    let window = state.runtime.execution.advisor_stall_turns.max(4);
    let crowd = state.runtime.execution.advisor_bottleneck_members.max(2);
    roots
        .into_iter()
        .zip(scans)
        .zip(failing)
        .zip(held)
        .zip(timing)
        .zip(sick)
        .zip(idle)
        .enumerate()
        .filter_map(|(slot, ((((((root, scan), failing), held), timing), sick), idle))| {
            let Claims {
                blocked,
                open,
                done,
                progress,
                reasons,
                revisions,
            } = held;
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
            let shared = if fingerprint != 0 && known.is_none_or(|known| known.reasons != fingerprint) {
                share(&reasons)
            } else {
                Vec::new()
            };
            let mut sharing = shared
                .iter()
                .flat_map(|(.., members)| members.iter().copied())
                .collect::<Vec<_>>();
            sharing.sort_unstable();
            sharing.dedup();
            sharing.truncate(FAILING);
            // The dependency graph is walked only when a member claim changed (any
            // update, abandonment or release gives it a new revision).
            let revisions = revisions.finish();
            let necks = known.is_none_or(|known| known.claims != revisions).then(|| {
                let claims = state
                    .collective
                    .claims
                    .iter()
                    .filter(|claim| {
                        member
                            .get(claim.owner.as_str())
                            .is_some_and(|&position| team_at[position] == slot)
                    })
                    .collect::<Vec<_>>();
                bottlenecks(state, &claims, crowd)
            });
            let Some(known) = known else {
                // Subscription starts here: earlier history is not reported.
                let failing = failing.into_iter().map(str::to_owned).collect();
                let sharing = sharing.into_iter().map(str::to_owned).collect();
                let necks = identities(necks.as_deref().unwrap_or_default());
                let timed = timing.into_iter().map(str::to_owned).collect();
                let idle = idle.into_iter().map(|idle| idle.chat).collect();
                let (pathology, _) = pathology::fold(None, &sick, false, limits);
                return Some((
                    root.to_owned(),
                    Team {
                        cursor: 0,
                        failing,
                        settled: scan.settled,
                        blocked: newest,
                        progress,
                        since: scan.settled,
                        open: open > 0,
                        stalled: false,
                        finished: false,
                        sharing,
                        reasons: fingerprint,
                        claims: revisions,
                        necks,
                        timed,
                        idle,
                        pathology,
                    },
                    Facts::default(),
                ));
            };
            let mut team = known.clone();
            let mut facts = Facts::default();
            let (pathology, found) = pathology::fold(Some(&known.pathology), &sick, progress > known.progress, limits);
            team.pathology = pathology;
            for found in &found.loops {
                facts.concern(&found.chat);
            }
            for found in &found.polling {
                facts.concern(&found.chat);
            }
            (facts.churn, facts.loops, facts.polling, facts.burn) =
                (found.churn, found.loops, found.polling, found.burn);
            for chat in failing
                .iter()
                .filter(|chat| !known.failing.iter().any(|known| known == *chat))
            {
                facts.repeated += 1;
                facts.concern(chat);
            }
            if team.failing.len() != failing.len() || team.failing.iter().zip(&failing).any(|(known, now)| known != now)
            {
                team.failing = failing.into_iter().map(str::to_owned).collect();
            }
            // A timed-out turn is reported once, until the member settles otherwise.
            for chat in timing
                .iter()
                .filter(|chat| !known.timed.iter().any(|known| known == *chat))
            {
                facts.timed_out(chat);
                facts.concern(chat);
            }
            if team.timed.len() != timing.len() || team.timed.iter().zip(&timing).any(|(known, now)| known != now) {
                team.timed = timing.into_iter().map(str::to_owned).collect();
            }
            for claim in blocked.iter().filter(|claim| claim.blocked_at > known.blocked) {
                let mut entry = Blocked::of(state, claim);
                entry.wait(&Waiter {
                    key: claim.key.clone(),
                    chat: claim.owner.clone(),
                });
                facts.block(&entry);
                facts.concern(&claim.owner);
            }
            team.blocked = team.blocked.max(newest);
            // A shared blocker is reported when a member joins it.
            for (_, reason, members) in shared.iter().filter(|(.., members)| {
                members.iter().any(|member| {
                    known
                        .sharing
                        .binary_search_by(|known| known.as_str().cmp(member))
                        .is_err()
                })
            }) {
                facts.share(&Shared {
                    reason: excerpt(reason),
                    members: members.iter().take(CHATS).map(|member| (*member).to_owned()).collect(),
                });
                for member in members {
                    facts.concern(member);
                }
            }
            if fingerprint != known.reasons {
                team.reasons = fingerprint;
                if team.sharing.len() != sharing.len()
                    || team.sharing.iter().zip(&sharing).any(|(known, now)| known != now)
                {
                    team.sharing = sharing.into_iter().map(str::to_owned).collect();
                }
            }
            // A bottleneck is reported when it appears or its dependents change.
            if let Some(necks) = necks {
                for (_, neck) in necks
                    .iter()
                    .filter(|(identity, _)| known.necks.binary_search(identity).is_err())
                {
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
                (team.progress, team.open, team.since, team.stalled, team.finished) =
                    (progress.max(known.progress), open_now, scan.settled, false, false);
            } else if open_now && !known.stalled && scan.settled - known.since >= window {
                facts.stalled = Some(Stall {
                    turns: u32::try_from(scan.settled - known.since).unwrap_or(u32::MAX),
                    open,
                });
                team.stalled = true;
            }
            // An idle claim holder is reported once, until it takes a turn.
            for idle in idle.iter().filter(|idle| known.idle.binary_search(&idle.chat).is_err()) {
                facts.concern(&idle.chat);
                facts.idle.push(idle.clone());
            }
            team.idle = idle.into_iter().map(|idle| idle.chat).collect();
            if !scan.active && scan.settled > team.settled {
                facts.quiescent = true;
                facts.finished = done && open == 0 && !team.finished;
                team.finished |= facts.finished;
                team.settled = scan.settled;
            }
            (team != *known).then(|| (root.to_owned(), team, facts))
        })
        .collect()
}

/// Accumulates facts reconcile observes later in its pass for one chat's team.
/// Teams without a recorded snapshot have not started their subscription.
pub(super) fn concern(state: &State, pending: &mut BTreeMap<String, Facts>, chat: &str, add: impl FnOnce(&mut Facts)) {
    let Some(root) = super::root(state, chat) else { return };
    if !state.bots.teams.contains_key(root)
        || !(super::offered(state, root, super::MONITOR_TOOL)
            || super::offered(state, root, super::ACCEPTANCE_TOOL)
            || super::antibodies_active(state))
    {
        return;
    }
    let facts = pending.entry(root.to_owned()).or_default();
    add(facts);
    facts.concern(chat);
}

/// Offers each team's coalesced facts once, at its next cursor, to its
/// advisor; and the pathological ones of every team, as one node event, to
/// antibodies.
pub(super) fn offer(
    state: &mut State,
    pending: BTreeMap<String, Facts>,
    now: control_effect::machine::Now,
) -> Vec<control_effect::machine::Planned<control_bot::BotId, super::Effect>> {
    let mut effects = Vec::new();
    let immune = super::antibodies_active(state);
    let mut node = BTreeMap::new();
    for (root, facts) in pending {
        // Each topic is published only while the root still selects its gene.
        let (monitored, accepted) = (
            super::offered(state, &root, super::MONITOR_TOOL),
            super::offered(state, &root, super::ACCEPTANCE_TOOL),
        );
        let Some(team) = state.bots.teams.get_mut(&root).filter(|_| !facts.is_empty()) else {
            continue;
        };
        team.cursor += 1;
        let cursor = team.cursor;
        if immune {
            let sick = facts.pathological();
            if !sick.is_empty() && node.len() < NODE_TEAMS {
                node.insert(root.clone(), sick);
            }
        }
        // An acceptance check hears completion alone, at the team's cursor.
        if facts.finished && accepted {
            let done = Facts {
                finished: true,
                ..Facts::default()
            };
            effects.extend(state.bots.supervisor.step(
                control_bot::Event::Info {
                    topic: done_topic(&root),
                    cursor,
                    from: None,
                    info: Message::Lifecycle(Box::new(done)),
                },
                now,
            ));
        }
        if monitored {
            effects.extend(state.bots.supervisor.step(
                control_bot::Event::Info {
                    topic: topic(&root),
                    cursor,
                    from: None,
                    info: Message::Lifecycle(Box::new(facts)),
                },
                now,
            ));
        }
    }
    if !node.is_empty() {
        state.bots.node_cursor += 1;
        let cursor = state.bots.node_cursor;
        effects.extend(state.bots.supervisor.step(
            control_bot::Event::Info {
                topic: node_topic(),
                cursor,
                from: None,
                info: Message::Node(node),
            },
            now,
        ));
    }
    effects
}
/// Teams named in one node event; an antibody reads the rest with its brief.
pub(super) const NODE_TEAMS: usize = 16;
/// The node topic antibodies subscribe to.
pub(super) const NODE_KIND: &str = "node";
pub(super) fn node_topic() -> Topic {
    Topic::new(NODE_KIND, "teams")
}
pub(super) fn node_source() -> Source {
    Source::Topic(node_topic())
}
/// Whether a definition is an antibody's: it hears the node topic.
pub(super) fn immune(definition: &control_bot::Definition) -> bool {
    definition.sources.contains(&node_source())
}

/// A stopped advisor's snapshot is dropped, so a later advisor for the same
/// team starts from its own subscription rather than an old observation. It
/// stays while another observer of that team still runs.
pub(super) fn forget(state: &mut State, chat: &str) {
    // The last antibody's snapshots of teams no advisor watches go with it.
    let last = state
        .bots
        .supervisor
        .definitions()
        .filter(|definition| immune(definition))
        .all(|definition| definition.chat.0 == chat);
    if last
        && state
            .bots
            .supervisor
            .definitions()
            .any(|definition| definition.chat.0 == chat && immune(definition))
    {
        let advised = advised(state).into_iter().map(str::to_owned).collect::<Vec<_>>();
        state.bots.teams.retain(|root, _| advised.contains(root));
    }
    let roots = state
        .bots
        .supervisor
        .definitions()
        .filter(|definition| definition.chat.0 == chat)
        .filter_map(heard)
        .filter(|root| {
            !state.bots.supervisor.definitions().any(|other| {
                other.chat.0 != chat
                    && heard(other) == Some(*root)
                    && state.bots.supervisor.status(&other.bot) != Some(control_bot::Status::Stopping)
            })
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for root in roots {
        state.bots.teams.remove(&root);
    }
}
