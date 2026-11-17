//! Persistent bots: chats that sleep until events wake them. The pure
//! `control_bot::Supervisor` decides; this adapter feeds it room messages, turn
//! completions and time, and interprets its turn requests as ordinary queued
//! turns. Supervisor state, requested turns and queued runs share one checkpoint.
mod lifecycle;
mod pathology;
mod tools;
pub use lifecycle::Facts;
pub use tools::{BotCall, BotOps, Tools};
use crate::runtime::RunKind;
use crate::{State, identity, tool_registry::AttemptRef};
use control_bot::{BotId, Budget, ChatId, Definition, Event, Restart, Source, Status, Topic, TurnId};
use control_effect::machine::{Machine, Now, Planned};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, sync::{Arc, Mutex}};

const COOLDOWN_MS: u64 = 30_000;
const WINDOW_MS: u64 = 3_600_000;
const TURNS_PER_WINDOW: u32 = 60;
const MAX_INTERVAL_MINUTES: u64 = 1_440;
/// Room posts offered per pass, bounding work under the state lock.
const POSTS_PER_PASS: usize = 256;

/// What wakes a bot, lifted in from Control's event sources. Coalescing is
/// decided here; the supervisor never inspects messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Post,
    /// Targeted peer evidence. It releases an open wait; a wait that selected
    /// topics retains it unless it selected this conversation.
    Peer,
    /// Retained for earlier persisted reasons; claims now wake only on resolution.
    Claim,
    /// The awaited claim incarnation resolved; only waits naming it select it.
    Resolved(crate::rooms::claims::Outcome),
    /// New output lines from a watch.
    Lines(u64),
    /// Explicit application notifications from a watch's separate descriptor.
    Notifications(u64),
    /// The watched process exited: the latest exit, and how many since the last turn.
    Exit { code: Option<i32>, signal: Option<i32>, exits: u32, gave_up: bool },
    Gap { after: u64, through: u64 },
    /// Host facts about the advised task tree, for its advisor only.
    Lifecycle(Box<Facts>),
    /// Pathological host facts about teams on the node, by root chat, for
    /// antibodies only.
    Node(std::collections::BTreeMap<String, Facts>),
    /// An advisor's evidence note. Direct like `Peer`; a turn woken only by
    /// notes is not team activity, so it cannot re-trigger lifecycle facts.
    Note,
    /// Non-urgent peer messages. They wake nobody: the next wake carries them
    /// as an unread digest, or they start one after the node's digest bound.
    Retained,
}
impl control_bot::Info for Message {
    fn urgent(&self) -> bool { matches!(self, Self::Exit{..} | Self::Gap{..}) }
    fn direct(&self) -> bool { matches!(self, Self::Peer | Self::Note) }
    fn deferred(&self) -> bool { matches!(self, Self::Retained) }
    fn merge(&mut self, newer: &Self) -> bool {
        match (self, newer) {
            (Message::Post, Message::Post) | (Message::Peer, Message::Peer) | (Message::Note, Message::Note) | (Message::Retained, Message::Retained) | (Message::Claim, Message::Claim) => true,
            (Message::Lifecycle(held), Message::Lifecycle(newer)) => {
                held.merge(newer);
                true
            }
            (Message::Node(held), Message::Node(newer)) => {
                for (root, facts) in newer {
                    let room = held.len() < lifecycle::NODE_TEAMS;
                    match held.get_mut(root) {
                        Some(known) => known.merge(facts),
                        None if room => { held.insert(root.clone(), facts.clone()); }
                        None => {}
                    }
                }
                true
            }
            (Message::Resolved(held), Message::Resolved(outcome)) => {
                *held = *outcome;
                true
            }
            (Message::Lines(held), Message::Lines(more)) => {
                *held = held.saturating_add(*more);
                true
            }
            (Message::Notifications(held), Message::Notifications(more)) => {
                *held = held.saturating_add(*more);
                true
            }
            (Message::Exit { code, signal, exits, gave_up }, Message::Exit { code: c, signal: s, exits: more, gave_up: g }) => {
                (*code, *signal, *gave_up) = (*c, *s, *g);
                *exits = exits.saturating_add(*more);
                true
            }
            (Message::Gap { through, .. }, Message::Gap { through: newer, .. }) => {
                *through = (*through).max(*newer);
                true
            }
            _ => false,
        }
    }
}
type Supervisor = control_bot::Supervisor<Message>;
type Reason = control_bot::Reason<Message>;
type Effect = control_bot::Effect<Message>;
fn room(name: &str) -> Topic {
    Topic::new("room", name)
}
fn watch(id: &str) -> Topic {
    Topic::new("watch", id)
}
fn claim(name: &str) -> Topic {
    Topic::new("claim", name)
}
/// Offers a claim incarnation's resolution to bots waiting on it. Progress
/// updates are never offered. Revisions are the cursor; whoever caused the
/// resolution is not woken by it.
pub(crate) fn claim_resolved(state: &mut State, resolved: &crate::rooms::claims::Resolved) {
    claim_resolved_at(state, resolved, Now(now_ms()));
}
fn claim_resolved_at(state: &mut State, resolved: &crate::rooms::claims::Resolved, now: Now) {
    let topic = claim(&crate::rooms::claims::topic_name(&resolved.room, &resolved.key, resolved.incarnation));
    let from = resolved.by.clone().map(ChatId);
    let effects = state.bots.supervisor.step(Event::Info { topic, cursor: resolved.revision, from, info: Message::Resolved(resolved.outcome) }, now);
    record(state, effects);
}
/// Claim and conversation subscriptions are host-owned for exactly as long as
/// the wait that selected them. A claim incarnation that resolved before the
/// wait was admitted releases it at once.
fn select_wait(state: &mut State, chat: &str, selected: &[String], now: Now, effects: &mut Vec<Planned<BotId, Effect>>) -> Vec<Topic> {
    let Some(mut definition) = state.bots.supervisor.definitions().find(|d| d.chat.0 == chat).cloned() else { return Vec::new() };
    let topics: Vec<Topic> = selected.iter().filter_map(|source| source.split_once(':'))
        .filter(|(kind, name)| *kind != "claim" || crate::rooms::claims::parse_topic(name).is_some())
        .map(|(kind, name)| Topic::new(kind, name)).collect();
    let conversation = Topic::new("conversation", chat);
    let before = definition.sources.clone();
    definition.sources.retain(|source| !matches!(source, Source::Topic(topic) if topic.kind == "claim"));
    for topic in topics.iter().filter(|topic| topic.kind == "claim" || **topic == conversation) {
        if !definition.sources.contains(&Source::Topic(topic.clone())) {
            definition.sources.push(Source::Topic(topic.clone()));
        }
    }
    let bot = definition.bot.clone();
    if definition.sources != before {
        effects.extend(state.bots.supervisor.step(Event::Define(definition), now));
    }
    for topic in topics.iter().filter(|topic| topic.kind == "claim") {
        let Some((room, key, incarnation)) = crate::rooms::claims::parse_topic(&topic.name) else { continue };
        effects.extend(state.bots.supervisor.step(Event::StartAt { bot: bot.clone(), topic: topic.clone(), after: incarnation }, now));
        if let Some((outcome, cursor, owner)) = crate::rooms::claims::resolution(state, room, key, incarnation) {
            let from = (outcome != crate::rooms::claims::Outcome::Abandoned).then_some(ChatId(owner));
            effects.extend(state.bots.supervisor.step(Event::Info { topic: topic.clone(), cursor, from, info: Message::Resolved(outcome) }, now));
        }
    }
    topics
}

/// Marks a run as a bot turn so its completion releases the supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub bot: String,
    pub turn: u64,
    /// Typed trigger provenance; text descriptions are presentation only.
    #[serde(default, skip_serializing_if="Vec::is_empty")]
    pub reasons: Vec<Reason>,
    /// A successfully published report whose session omitted its handoff.
    /// This reference carries evidence, never additional authority.
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub recovery: Option<String>,
}

/// A requested turn not yet queued, retried until queued or no longer eligible.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Request {
    bot: BotId,
    chat: ChatId,
    turn: TurnId,
    reasons: Vec<Reason>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bots {
    supervisor: Supervisor,
    /// Room posts before this index were already offered to the supervisor.
    room_cursor: usize,
    requests: Vec<Request>,
    /// Bot requests from agents, decided by the operator.
    approvals: Vec<Approval>,
    /// Published working revisions already offered as runnable work. Recorded
    /// with the supervisor so a restart cannot request the successor twice.
    #[serde(skip_serializing_if="std::collections::BTreeMap::is_empty")]
    continuations:std::collections::BTreeMap<String,u64>,
    /// What the host last observed of each advised task tree.
    #[serde(skip_serializing_if="std::collections::BTreeMap::is_empty")]
    teams: std::collections::BTreeMap<String, lifecycle::Team>,
    /// Cursor of the node topic antibodies hear.
    #[serde(default, skip_serializing_if="is_zero")]
    node_cursor: u64,
    /// Operator notices (`control_operator_notify`), newest last and bounded.
    #[serde(default, skip_serializing_if="Vec::is_empty")]
    pub(crate) notices: Vec<crate::intel::Notice>,
}
fn is_zero(value: &u64) -> bool { *value == 0 }
impl Bots {
    pub(crate) fn retains_watch(&self, id: &str) -> bool {
        self.supervisor.retains(&watch(id))
    }
    /// Whether `chat` runs as an approved advisor: a persistent read-only judge
    /// woken by its team's lifecycle facts. Only advisors may send notes.
    pub(crate) fn advises(&self, chat: &str) -> bool {
        self.advised_team(chat).is_some() || self.antibody(chat)
    }
    /// Whether `chat` runs as an approved antibody: a persistent read-only
    /// judge woken by pathological facts about any team on the node. It may
    /// note any team's lead and notify the operator.
    pub(crate) fn antibody(&self, chat: &str) -> bool {
        self.supervisor.definitions().any(|definition| definition.chat.0 == chat && definition.restart == Restart::Permanent && lifecycle::immune(definition))
    }
    /// The root (steering) chat whose team `chat` advises, if it is an approved
    /// advisor. That chat is the only recipient of its notes.
    pub(crate) fn advised_team(&self, chat: &str) -> Option<&str> {
        self.supervisor.definitions()
            .filter(|definition| definition.chat.0 == chat && definition.restart == Restart::Permanent)
            .find_map(lifecycle::team)
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis().min(u64::MAX as u128) as u64)
}
/// Each chat runs at most one bot, named after the chat.
fn bot(chat: &str) -> BotId {
    BotId(chat.into())
}
/// Records every effect of a supervisor step: turn requests go to the outbox so
/// no step, including a command's, can drop one.
fn record(state: &mut State, effects: Vec<Planned<BotId, Effect>>) {
    for Planned { effect, .. } in effects {
        match effect {
            Effect::RequestTurn { bot, chat, turn, reasons } => state.bots.requests.push(Request { bot, chat, turn, reasons }),
            Effect::BudgetExhausted { bot, .. } => {
                state.runtime.notice = format!("Bot in chat {} reached its turn budget; events stay pending.", bot.0);
            }
        }
    }
}

pub(crate) fn defined(state: &State, chat: &str) -> bool {
    state.bots.supervisor.status(&bot(chat)).is_some()
}
pub(crate) fn peer_pending(state: &State, chat: &str, through: u64) -> bool {
    state.bots.peer_pending(chat, through)
}
impl Bots {
    pub(crate) fn peer_pending(&self, chat: &str, through: u64) -> bool {
        self.supervisor.definitions().any(|definition| definition.chat.0 == chat && definition.restart == Restart::Permanent)
            && self.supervisor.unseen(&bot(chat), &Topic::new("conversation", chat), through)
    }
    /// An approved persistent bot that has not been stopped. A failed or
    /// interrupted turn ends that turn, not the bot: its supervisor owns the
    /// next wake, so retained peer evidence waits for it without restarting work.
    pub(crate) fn persists(&self, chat: &str) -> bool {
        self.supervisor.status(&bot(chat)).is_some_and(|status| status != Status::Stopping)
            && self.supervisor.definitions().any(|definition| definition.chat.0 == chat && definition.restart == Restart::Permanent)
    }
}
/// The latest pending peer message and advisor note per chat, in cursor order,
/// so a turn woken by both is never mistaken for an advisor echo.
fn peer_events(state: &State) -> Vec<(&str, u64, Message)> {
    let mut latest = std::collections::BTreeMap::new();
    for (chat, cursor, note, urgent) in crate::agent_message::peer_cursors(state) {
        // Advisor notes keep waking: they are the host-facts escalation path.
        let kind = if note { 2 } else if urgent { 1 } else { 0 };
        latest.entry((chat, kind)).and_modify(|head: &mut u64| *head = (*head).max(cursor)).or_insert(cursor);
    }
    if latest.is_empty() { return Vec::new(); }
    let busy = state.runtime.runs.iter().filter(|run| matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling"))
        .filter_map(|run| run.chat_id.as_deref()).collect::<std::collections::HashSet<_>>();
    // Active/queued workers already consume conversation evidence. Waiting for
    // their publication prevents admission between a read and its checkpoint.
    let mut events = latest.into_iter().map(|((chat, kind), through)| (chat, through, match kind { 2 => Message::Note, 1 => Message::Peer, _ => Message::Retained }))
        .filter(|(chat, through, _)| !busy.contains(chat) && peer_pending(state, chat, *through)).collect::<Vec<_>>();
    events.sort_unstable_by_key(|(chat, through, _)| (*chat, *through));
    events
}
/// Agent messages a bot has not yet processed, as a count and bounded digest.
fn unread(state: &State, chat: &str) -> Option<(usize, String)> {
    let after = state.bots.supervisor.processed(&bot(chat), &Topic::new("conversation", chat));
    crate::agent_message::digest::digest(state, chat, after, |_| true)
}
/// Test drivers reconcile after the turn cooldown without sleeping.
#[cfg(test)]
pub(crate) fn reconcile_after_cooldown(state: &mut State) -> Result<(), &'static str> {
    reconcile_at(state, now_ms().saturating_add(COOLDOWN_MS + 1), |_| Ok(()))
}
/// Test drivers reconcile at an explicit time.
#[cfg(test)]
pub(crate) fn reconcile_at_ms(state: &mut State, now: u64) -> Result<(), &'static str> {
    reconcile_at(state, now, |_| Ok(()))
}
/// Lift evidence only after publication/processing has been reconciled.
#[cfg(test)]
fn peer_message_at(state: &mut State, chat: &str, through: u64, now: Now) {
    offer_at(state, chat, through, now, Message::Peer);
}
fn offer_at(state: &mut State, chat: &str, through: u64, now: Now, info: Message) {
    let Some(mut definition) = state.bots.supervisor.definitions()
        .find(|definition| definition.chat.0 == chat && definition.restart == Restart::Permanent).cloned() else { return; };
    let topic = Topic::new("conversation", chat);
    let source = Source::Topic(topic.clone());
    if !definition.sources.contains(&source) {
        definition.sources.push(source);
        let effects = state.bots.supervisor.step(Event::Define(definition), now);
        record(state, effects);
    }
    let effects = state.bots.supervisor.step(Event::Info { topic, cursor: through, from: None, info }, now);
    record(state, effects);
}
pub(crate) fn may_start(state: &State, run: &crate::runtime::Run) -> bool {
    run.bot_turn.as_ref().is_none_or(|turn| state.bots.supervisor.may_start(&BotId(turn.bot.clone()), &TurnId(turn.turn)))
}
pub(crate) fn continuation_enabled(state: &State, chat: &str) -> bool {
    state.bots.supervisor.definitions().any(|definition| definition.chat.0 == chat && definition.restart == Restart::Permanent)
}
/// Host-owned subscriptions a handoff may narrow while idle. Working notes
/// cannot create subscriptions, resource access or a new timer authority.
/// A persistent bot may also select its own conversation, so direct peer
/// messages release that wait; claim waits are validated separately.
pub(crate) fn wait_topics(state: &State, chat: &str) -> Vec<String> {
    let mut topics: Vec<String> = state.bots.supervisor.definitions().find(|definition| definition.chat.0 == chat)
        .into_iter().flat_map(|definition| &definition.sources)
        .filter_map(|source| match source {
            Source::Topic(topic) if topic.kind != "claim" => Some(format!("{}:{}",topic.kind,topic.name)),
            _ => None,
        }).collect();
    let conversation = format!("conversation:{chat}");
    if continuation_enabled(state, chat) && !topics.contains(&conversation) {
        topics.push(conversation);
    }
    topics
}
/// A stopped owner will not finish its claims; their dependents wake now.
fn abandon_claims(state: &mut State, chat: &str, now: Now) {
    for resolved in crate::rooms::claims::abandon_owned(state, chat) {
        claim_resolved_at(state, &resolved, now);
    }
}
/// Stops every bot running in `chats`; a running turn finishes first.
pub(crate) fn stop_all(state: &mut State, chats: &[String]) -> usize {
    let now = Now(now_ms());
    let mut stopped = 0;
    for chat in chats {
        if !defined(state, chat) {
            continue;
        }
        lifecycle::forget(state, chat);
        let effects = state.bots.supervisor.step(Event::Stop(bot(chat)), now);
        record(state, effects);
        cancel_queued_turns(state, chat);
        abandon_claims(state, chat, now);
        stopped += 1;
    }
    stopped
}
fn cancel_queued_turns(state: &mut State, chat: &str) {
    for run in state.runtime.runs.iter_mut().filter(|run| run.chat_id.as_deref() == Some(chat)
        && run.bot_turn.is_some() && run.status == "queued") {
        run.status = "cancelled".into();
        run.result = "Bot stopped before dispatch; retained conversation evidence is unchanged.".into();
    }
}

pub(crate) fn status(state: &State, chat: &str) -> Option<String> {
    let id = bot(chat);
    let status = state.bots.supervisor.status(&id)?;
    let definition = state.bots.supervisor.definitions().find(|definition| definition.bot == id)?;
    let stuck = if state.chats.iter().find(|c| c.id == chat).and_then(|c| c.continuity.as_ref()).is_some_and(|c| c.checkpointing.reported) { " · cannot checkpoint" } else { "" };
    let mut sources = definition.sources.iter().map(|source| match source {
        Source::Topic(Topic { kind, name }) if kind == "room" => format!("room #{name}"),
        Source::Topic(Topic { kind, name }) if kind == lifecycle::KIND => format!("team facts for {name}"),
        Source::Topic(Topic { kind, .. }) if kind == lifecycle::NODE_KIND => "node pathology facts".into(),
        Source::Topic(Topic { kind, name }) => format!("{kind} {name}"),
        Source::Timer { every_ms } => format!("every {} min", every_ms / 60_000),
    }).collect::<Vec<_>>().join(", ");
    if sources.is_empty() {
        sources = "no sources".into();
    }
    // An outstanding supervisor request may still be queued, or may continue
    // running after pause. Project intent and actual execution independently.
    let activity = if matches!(status, Status::WaitingTurn | Status::Paused) {
        state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(chat)
            && matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling"))
            .map(|run| run.status.as_str())
    } else { None };
    let state = match status {
        _ if status != Status::Stopping && state.bots.supervisor.paused(&id) => match activity {
            Some("running") => "paused · current turn running",
            Some("cancelling") => "paused · current turn stopping",
            _ => "paused",
        },
        Status::Ready => "sleeping",
        Status::WaitingTurn => match activity {
            Some("running") => "working",
            Some("cancelling") => "stopping current turn",
            Some("queued") => "queued",
            Some("waiting") => "waiting for decision",
            _ => "waiting for turn result",
        },
        Status::Paused => "paused",
        Status::Stopping => "stopping",
    };
    let label = if definition.restart == Restart::Temporary { "Wake once" } else if lifecycle::immune(definition) { "Antibody" } else if lifecycle::advises(definition) { "Advisor" } else { "Bot" };
    Some(format!("{label} · {state} · {sources}{stuck}"))
}

pub(crate) fn background_allowed(state: &State, chat: &str) -> bool {
    !state.bots.supervisor.paused(&bot(chat)) && state.bots.supervisor.status(&bot(chat)) != Some(Status::Stopping)
}

/// The live forest: task trees with their bots, agents, services and
/// processes that `search` matches (all of them without a search), or why
/// there are none.
pub(crate) fn overview(state: &State, search: &str) -> String {
    match crate::process_tree::render(state, search) {
        Some(tree) => tree,
        None if search.is_empty() => "<p class='forest-empty'>No bots, services or active agents. Use /bot on in a DM or approve a bot request.</p>".into(),
        None => "<p class='forest-empty'>No live bots match that search.</p>".into(),
    }
}
/// The whole Bots page: one search over live and dead bots, then one scene with
/// the live task trees above and the graveyard (the page `query` selects) on
/// the ground beneath them, then queued turns.
pub(crate) fn page(state: &State, query: &crate::graveyard::Query) -> String {
    let clear = if query.search.is_empty() { "" } else { "<a href='/?view=agents'>Clear</a>" };
    let mut html = format!(
        "<div class='section-heading'><h2>Bots</h2></div>{}<form class='bots-search' method='get' action='/' role='search'><input type='hidden' name='view' value='agents'><input type='search' name='q' value='{}' maxlength='{}' placeholder='Search live and dead bots by name or objective' aria-label='Search live and dead bots by name or objective'><button>Search</button>{clear}</form><div class='bots-scene'>{}{}</div><p class='muted tiny'>{}</p>",
        crate::navigation::bots_tabs(false),
        crate::escape(&query.search),
        crate::graveyard::QUERY_CHARS,
        overview(state, &query.search),
        crate::graveyard::render(state, query),
        crate::process_tree::LEGEND,
    );
    html.push_str(&state.runtime.render("agents", "").unwrap_or_default());
    html
}

/// `/bot on [every N]`, `/bot off`, `/bot pause` and `/bot resume`. Reached only
/// through the authenticated chat command route, never from room text or tools.
pub(crate) fn command(state: &mut State, chat: &str, option: &str) -> Result<&'static str, &'static str> {
    command_at(state, chat, option, Now(now_ms()))
}
fn command_at(state: &mut State, chat: &str, option: &str, now: Now) -> Result<&'static str, &'static str> {
    let session = state.chats.iter().find(|session| session.id == chat).ok_or("Chat not found.")?;
    identity::authorize(&session.actor, identity::Action::Task)?;
    let words = option.split_whitespace().collect::<Vec<_>>();
    let id = bot(chat);
    match words.as_slice() {
        ["off"] => {
            lifecycle::forget(state, chat);
            let effects = state.bots.supervisor.step(Event::Stop(id), now);
            record(state, effects);
            cancel_queued_turns(state, chat);
            abandon_claims(state, chat, now);
            Ok("Bot stopped. A running turn finishes first.")
        }
        ["pause"] if defined(state, chat) => {
            let effects = state.bots.supervisor.step(Event::Pause(id), now);
            record(state, effects);
            Ok("Bot paused. Events are kept for when it resumes.")
        }
        ["resume"] if defined(state, chat) => {
            let effects = state.bots.supervisor.step(Event::Resume(id), now);
            record(state, effects);
            Ok("Bot resumed.")
        }
        ["pause" | "resume"] => Err("This chat has no bot. Start one with /bot on."),
        ["allow", id] => decide(state, id, true, Some(chat), now),
        ["deny", id] => decide(state, id, false, Some(chat), now),
        [] | ["on"] | ["on", "every", _] | ["every", _] => {
            let minutes = match words.last() {
                Some(value) if words.len() > 1 => Some(value.parse::<u64>().ok().filter(|m| (1..=MAX_INTERVAL_MINUTES).contains(m)).ok_or("Use /bot on every N, with N from 1 to 1440 minutes.")?),
                _ => None,
            };
            start(state, chat, minutes, true, now)
        }
        _ => Err("Use /bot on [every N], /bot pause, /bot resume or /bot off."),
    }
}

fn start(state: &mut State, chat: &str, minutes: Option<u64>, room: bool, now: Now) -> Result<&'static str, &'static str> {
    let fresh=!defined(state,chat);
    if crate::rooms::listening(state, chat) {
        return Err("Room listening is on. Stop it with /listen off before starting a bot.");
    }
    let session = state.chats.iter().find(|session| session.id == chat).ok_or("Chat not found.")?;
    let origin = state.runtime.runs.iter().rev()
        .find(|run| run.chat_id.as_deref() == Some(chat) && run.actor == session.actor)
        .ok_or("Give this chat a task before starting a bot.")?;
    identity::authorize(&session.actor, origin.action)?;
    if crate::delegation::is_judge(origin) {
        return advise(state, chat, minutes, now);
    }
    let bootstrap=fresh && matches!(origin.status.as_str(),"running"|"completed")
        && origin.attempts.last().is_some_and(|attempt|!attempt.review_only
            && attempt.workspace_id.as_deref().is_some_and(crate::chat::valid_id));
    let mut sources = Vec::new();
    if let Some(room) = session.room.as_deref().filter(|linked| room && crate::rooms::valid(linked)) {
        sources.push(Source::Topic(self::room(room)));
    }
    if let Some(minutes) = minutes {
        sources.push(Source::Timer { every_ms: minutes * 60_000 });
    }
    sources.extend(watch_sources(state, chat));
    if sources.is_empty() {
        return Err("Link a room, start a watch or give an interval, for example /bot on every 10.");
    }
    crate::rooms::ensure_inbox(state, chat)?;
    if fresh {
        crate::rooms::start_at_edge(state, chat)?;
    }
    if state.bots.supervisor.definitions().next().is_none() {
        state.bots.room_cursor = state.collective.posts.len();
    }
    let definition = Definition {
        bot: bot(chat),
        chat: ChatId(chat.into()),
        sources,
        budget: Budget { cooldown_ms: COOLDOWN_MS, window_ms: WINDOW_MS, turns_per_window: TURNS_PER_WINDOW },
        restart: Restart::Permanent,
        defer_ms: Some(digest_ms(state)),
    };
    definition.validate()?;
    let effects = state.bots.supervisor.step(Event::Define(definition), now);
    record(state, effects);
    if state.bots.supervisor.status(&bot(chat)) == Some(Status::Stopping) {
        return Err("The previous bot is still finishing its turn. Try again shortly.");
    }
    if fresh && let Some(name) = state.chats.iter().find(|c|c.id==chat).and_then(|c|c.room.as_deref()) {
        let effects=state.bots.supervisor.step(Event::StartAt{bot:bot(chat),topic:self::room(name),after:state.collective.posts.len() as u64},now);
        record(state,effects);
    }
    if bootstrap {
        let effects=state.bots.supervisor.step(Event::Continue(bot(chat)),now);
        record(state,effects);
    }
    Ok("Bot started. It sleeps until room messages or its timer wake it.")
}

/// A read-only judge runs as its team's advisor: a persistent bot whose only
/// source is the lifecycle topic of its task tree. It gets no room inbox, watch,
/// timer or grant; approval (or the operator's `/bot on`) is the authority.
///
/// In a tree whose operator-owned root selected the antibody lens, the judge
/// runs as an antibody instead: it hears the node topic (pathological facts
/// about every team) and may take a patrol timer.
fn advise(state: &mut State, chat: &str, minutes: Option<u64>, now: Now) -> Result<&'static str, &'static str> {
    let team = root(state, chat).filter(|team| *team != chat).ok_or("An advisor must belong to a spawned task tree.")?.to_owned();
    // The mode is the captured persona's visibility (else the root's opt-in);
    // each mode keeps its own opt-in gene gate.
    let antibody = crate::personas::observes_node(state, chat);
    if minutes.is_some() && !antibody {
        return Err("An advisor wakes only on its team's host facts; it takes no timer.");
    }
    if crate::personas::observer_offered(state, chat) == Some(false) {
        return Err(if antibody { ANTIBODY_OFF } else { ADVISOR_OFF });
    }
    if !antibody && !advisor_offered(state, &team) {
        return Err(ADVISOR_OFF);
    }
    let mut sources = if antibody { vec![lifecycle::node_source()] } else { vec![lifecycle::source(&team)] };
    if let Some(minutes) = minutes {
        sources.push(Source::Timer { every_ms: minutes * 60_000 });
    }
    let definition = Definition {
        bot: bot(chat),
        chat: ChatId(chat.into()),
        sources,
        budget: Budget { cooldown_ms: COOLDOWN_MS, window_ms: WINDOW_MS, turns_per_window: TURNS_PER_WINDOW },
        restart: Restart::Permanent,
        defer_ms: Some(digest_ms(state)),
    };
    definition.validate()?;
    if state.bots.supervisor.definitions().next().is_none() {
        state.bots.room_cursor = state.collective.posts.len();
    }
    let effects = state.bots.supervisor.step(Event::Define(definition), now);
    record(state, effects);
    if state.bots.supervisor.status(&bot(chat)) == Some(Status::Stopping) {
        return Err("The previous bot is still finishing its turn. Try again shortly.");
    }
    Ok(if antibody { "Antibody started. It sleeps until pathological facts about any team, or its patrol timer, wake it." } else { "Advisor started. It sleeps until host facts about its team wake it." })
}

/// A bot whose unlinked chat links itself to a room by posting also hears
/// that room, with the inbox room tools read.
pub(crate) fn link_room(state: &mut State, chat: &str, name: &str) -> Result<(), &'static str> {
    let Some(definition) = state.bots.supervisor.definitions().find(|d| d.chat.0 == chat).cloned() else { return Ok(()) };
    crate::rooms::ensure_inbox(state, chat)?;
    let mut sources = definition.sources.into_iter().filter(|s| !matches!(s, Source::Topic(t) if t.kind == "room")).collect::<Vec<_>>();
    sources.push(Source::Topic(room(name)));
    let effects = state.bots.supervisor.step(Event::Define(Definition { sources, ..definition }), Now(now_ms()));
    record(state, effects);
    let effects = state.bots.supervisor.step(Event::StartAt { bot: bot(chat), topic: room(name), after: state.collective.posts.len() as u64 }, Now(now_ms()));
    record(state, effects);
    Ok(())
}

/// Arms a one-shot wake: a temporary bot on the chat's watches that ends after
/// one turn. Returns false when the chat already runs as a bot.
pub(crate) fn arm(state: &mut State, chat: &str) -> Result<bool, &'static str> {
    arm_at(state, chat, Now(now_ms()))
}
fn arm_at(state: &mut State, chat: &str, now: Now) -> Result<bool, &'static str> {
    let id = bot(chat);
    let restart = state.bots.supervisor.definitions().find(|d| d.bot == id).map(|d| d.restart);
    if let Some(restart) = restart {
        let rearm = restart == Restart::Temporary && state.bots.supervisor.status(&id) == Some(Status::WaitingTurn);
        if rearm {
            let effects = state.bots.supervisor.step(Event::Rearm(id), now);
            record(state, effects);
        }
        return Ok(rearm);
    }
    let sources = watch_sources(state, chat);
    if sources.is_empty() {
        return Err("Start a watch before arming a wake.");
    }
    for w in state.watches.iter_mut().filter(|w| w.chat == chat) {
        w.lines = 0;
        if let Some(notifications) = &mut w.notifications { notifications.pending = 0; }
        w.exits.clear();
        w.gap = None;
    }
    if state.bots.supervisor.definitions().next().is_none() {
        state.bots.room_cursor = state.collective.posts.len();
    }
    let definition = Definition {
        bot: bot(chat),
        chat: ChatId(chat.into()),
        sources,
        budget: Budget { cooldown_ms: 0, window_ms: WINDOW_MS, turns_per_window: 1 },
        restart: Restart::Temporary,
        defer_ms: None,
    };
    let effects = state.bots.supervisor.step(Event::Define(definition), now);
    record(state, effects);
    Ok(true)
}

/// Every active watch a chat owns is a topic of its bot; `pending` also keeps
/// finished watches whose last output and exit are not yet offered.
fn watch_sources(state: &State, chat: &str) -> Vec<Source> {
    watch_topics(state, chat, false)
}
fn watch_topics(state: &State, chat: &str, pending: bool) -> Vec<Source> {
    state.watches.iter().filter(|w| w.chat == chat && (w.active() || (pending && (w.lines > 0 || w.pending_notifications() > 0 || w.held_notifications() > 0 || !w.exits.is_empty() || w.gap.is_some())))).map(|w| Source::Topic(watch(&w.id))).collect()
}
/// Redefinitions that keep each bot subscribed to exactly its chat's watches.
fn drifted(state: &State, pending: bool) -> Vec<Definition> {
    state.bots.supervisor.definitions().filter_map(|definition| {
        let mut sources = definition.sources.iter().filter(|s| !matches!(s, Source::Topic(t) if t.kind == "watch")).cloned().collect::<Vec<_>>();
        sources.extend(watch_topics(state, &definition.chat.0, pending));
        // Permanent bots follow the node's current digest bound.
        let defer_ms = (definition.restart == Restart::Permanent).then(|| digest_ms(state));
        ((sources != definition.sources || defer_ms != definition.defer_ms) && !sources.is_empty())
            .then(|| Definition { sources, defer_ms, ..definition.clone() })
    }).collect()
}
/// How long a bot's non-urgent peer messages wait for another wake.
fn digest_ms(state: &State) -> u64 {
    state.runtime.execution.peer_digest_seconds.saturating_mul(1000)
}
/// Spooled watch lines and exits not yet offered, ordered by cursor per topic.
fn watch_events(state: &mut State) -> Vec<(Topic, u64, Message)> {
    let mut events = Vec::new();
    for w in &mut state.watches {
        if w.lines > 0 {
            events.push((watch(&w.id), w.wake_seq.unwrap_or(w.line_seq), Message::Lines(std::mem::take(&mut w.lines))));
        }
        if let Some(notifications) = &mut w.notifications && notifications.pending > 0 {
            events.push((watch(&w.id), notifications.through, Message::Notifications(std::mem::take(&mut notifications.pending))));
        }
        if let Some(gap) = w.gap.take() {
            events.push((watch(&w.id), gap.through, Message::Gap { after: gap.after, through: gap.through }));
        }
        for exit in w.exits.drain(..) {
            events.push((watch(&w.id), exit.seq, Message::Exit { code: exit.code, signal: exit.signal, exits: 1, gave_up: exit.gave_up }));
        }
    }
    events.sort_by(|a, b| (&a.0.name, a.1).cmp(&(&b.0.name, b.1)));
    events
}
fn pending_watch_events(state: &State) -> bool {
    state.watches.iter().any(|w| w.lines > 0 || w.pending_notifications() > 0 || !w.exits.is_empty() || w.gap.is_some())
}


fn describe(state: &State, reasons: &[Reason]) -> String {
    let name = |id: &str| state.watches.iter().find(|w| w.id == id).map_or(id.to_owned(), |w| format!("{} ({id})", w.name));
    reasons.iter().map(|reason| match reason {
        Reason::Info { topic, info: Message::Post, .. } => format!("new messages in room #{}", topic.name),
        Reason::Info { info: Message::Peer, .. } => "urgent targeted peer coordination (in this conversation)".into(),
        Reason::Info { info: Message::Retained, .. } => "unread non-urgent peer messages".into(),
        Reason::Info { info: Message::Note, .. } => "an advisor note (read-only evidence, not steering; in this conversation)".into(),
        Reason::Info { topic, info: Message::Lifecycle(facts), .. } => facts.describe(&topic.name),
        Reason::Info { info: Message::Node(teams), .. } => teams.iter().map(|(root, facts)| facts.describe(root)).collect::<Vec<_>>().join("; "),
        Reason::Info { topic, info: Message::Claim, .. } => format!("claim {} changed; read the room's claims", topic.name),
        Reason::Info { topic, info: Message::Resolved(outcome), .. } => format!("awaited claim {} resolved as {}; read the room claims and act on the outcome", topic.name, crate::rooms::claims::outcome_name(*outcome)),
        Reason::Info { topic, info: Message::Lines(lines), .. } => {
            let recent = state.watches.iter().find(|w| w.id == topic.name).map(|w| w.recent.clone()).unwrap_or_default();
            format!("{lines} new lines from watch {}; latest (untrusted output): {}", name(&topic.name), recent.join(" | "))
        }
        Reason::Info { topic, info: Message::Notifications(count), .. } => {
            let recent = state.watches.iter().find(|w| w.id == topic.name).and_then(|w| w.notifications.as_ref());
            format!("{count} explicit notifications from watch {}; latest (untrusted program claims): {}", name(&topic.name), recent.map_or(String::new(), |notifications| notifications.recent.join(" | ")))
        }
        Reason::Info { topic, info: Message::Exit { code, signal, exits, gave_up }, .. } => format!(
            "watch {} exited{}{} ({exits} time(s)){}",
            name(&topic.name),
            code.map_or(String::new(), |code| format!(" with code {code}")),
            signal.map_or(String::new(), |signal| format!(" on signal {signal}")),
            if *gave_up { "; it restarted too often and was retired" } else { "" },
        ),
        Reason::Info { topic, info: Message::Gap { after, through }, .. } => format!("watch {} has missing source events after {after} through {through}; inspect the service and retained evidence before acting", name(&topic.name)),
        Reason::Timer => "your scheduled check-in".into(),
        Reason::Continue => "unfinished runnable work from your committed checkpoint".into(),
        Reason::Overflow { dropped, topics } => format!("{dropped} more events than fit; re-read {} source(s) directly", topics.len()),
    }).collect::<Vec<_>>().join("; ")
}

/// A bot wake's objective. Only a wake by room posts lists them, so only that
/// wake says not to re-read them.
fn objective(woke:&str, recovery:Option<&str>, posts:bool)->String {
    let woke=woke.chars().take(400).collect::<String>();
    let recovery=recovery.map_or(String::new(),|note|format!(" {}",note.chars().take(RECOVERY_NOTE).collect::<String>()));
    let listed=if posts { " This wake lists the room posts that woke you; do not re-read them." } else { "" };
    format!("Bot wake: continue the original goal in this active session.{listed} Triggered by: {woke}.{recovery}")
}
/// Whether new room posts are among the wake reasons.
fn posts(reasons:&[Reason]) -> bool {
    reasons.iter().any(|reason| matches!(reason, Reason::Info { info: Message::Post, .. }))
}
/// Characters of recovery guidance in a wake objective; the host's rejection
/// is bounded separately when recorded.
const RECOVERY_NOTE: usize = 1_600;
/// Wake guidance after a turn of `chat` ended without an accepted handoff:
/// the host's last rejection for that attempt, so the agent repairs its
/// receipts instead of re-verifying a world its notes already describe.
/// Typed host records only; the model's prose is never parsed.
fn recovery_note(state:&State, chat:&str, failed:&crate::runtime::Run) -> String {
    let generation=failed.attempts.last().and_then(|attempt|attempt.workspace_id.as_deref());
    let rejected=state.chats.iter().find(|c|c.id==chat).and_then(|c|c.continuity.as_ref())
        .and_then(|c|c.checkpointing.rejected.as_ref()).filter(|r|Some(r.generation.as_str())==generation);
    match rejected {
        Some(rejection) => format!("Checkpoint recovery: your previous turn's control_turn_handoff was rejected, so nothing it did was committed. Host error: \"{}\". Your workspace, services and watches are as that turn left them; do not re-verify them or start new watches. Fix the handoff itself: call control_turn_handoff {{\"disposition\":{{\"kind\":\"continue\"}}}} (or wait or done) and omit processed, so the host applies every receipt delivered in this session that follows the committed position.{}", if unseen_range(&rejection.error) { rejection.error.split(". ").next().unwrap_or_default() } else { &rejection.error }, if unseen_range(&rejection.error) { UNSEEN_RANGE } else { "" }),
        None => "Checkpoint recovery: your previous turn ended without calling control_turn_handoff, so nothing was committed and it is treated as waiting. Do not repeat its checks; act only on new evidence, and call control_turn_handoff (continue, wait or done) before you yield.".into(),
    }
}
/// Wake guidance after the chat's latest turn was cut off before its handoff
/// (time limit, model-step cap or provider failure): its unpublished work is a
/// recovery candidate and its claims are still its own.
fn timeout_note(latest:&crate::runtime::Run) -> Option<String> {
    use crate::attempt_control::Termination;
    cut_off(latest).then(|| match latest.attempts.last().and_then(|attempt| attempt.termination) {
        Some(Termination::TimedOut) => TIMEOUT_NOTE,
        Some(Termination::StepLimit) => STEP_LIMIT_NOTE,
        _ => PROVIDER_NOTE,
    }.to_owned())
}
/// The run failed because the host stopped it at its attempt time limit.
fn timed_out(run:&crate::runtime::Run) -> bool {
    stopped_by(run, crate::attempt_control::Termination::TimedOut)
}
fn stopped_by(run:&crate::runtime::Run, cause:crate::attempt_control::Termination) -> bool {
    run.status=="failed" && run.attempts.last().is_some_and(|attempt|attempt.termination==Some(cause))
}
use crate::attempt_control::cut_off;
pub(crate) const TIMEOUT_NOTE:&str="Your previous turn hit its time limit before checkpointing: resume from the host checkpoint in your working notes rather than redoing it, restoring files with the control_recovery call it names; keep your claims, delegate or run long work in a routine, and hand off before the limit.";
pub(crate) const STEP_LIMIT_NOTE:&str="Your previous turn used all its model steps before checkpointing: resume from the host checkpoint in your working notes rather than redoing it, restoring files with the control_recovery call it names; do not retry a failing command in a loop, and hand off (waiting on what you depend on) before the step limit.";
pub(crate) const PROVIDER_NOTE:&str="Your previous turn ended on a model provider failure before checkpointing: resume from the host checkpoint in your working notes rather than redoing it, restoring files with the control_recovery call it names, and hand off before you yield.";
/// The handoff named a receipt that starts after its source's committed
/// position: an earlier receipt of that source was left out. These are the
/// host's own mailbox errors, not model prose.
fn unseen_range(error:&str) -> bool {
    error.starts_with("Cannot process an unseen event range") || error.starts_with("First event range requires")
}
pub(crate) const UNSEEN_RANGE:&str=" That error means a listed receipt starts after the last committed position of its source, because an earlier receipt from the same source was left out. Receipts from the previous turn are not valid now. Omit processed from your handoff: the host then applies only receipts that follow each source's committed position and reports the rest as left_pending.";
/// An advisor turn is a fresh read-only review of its team, rebuilt from the
/// journal; its captured lenses carry the procedure. Notes to the lead are its
/// only effect.
pub(crate) fn advice(woke: &str, lead: &str) -> String {
    let woke = woke.chars().take(1000).collect::<String>();
    format!("You are this task tree's read-only advisor, woken by host facts: {woke}. Inspect the team with control_room_read and control_room_plan and, following the Team advisor guidance, send the lead's steering chat {lead} at most one concise note with control_agent_message; send nothing when evidence is unchanged. End the turn when done.")
}
/// An antibody turn: a fresh read-only review of the teams named in its facts,
/// or a patrol of the node on its timer.
pub(crate) fn patrol(woke: &str) -> String {
    let woke = woke.chars().take(2000).collect::<String>();
    format!("You are this node's read-only antibody, woken by: {woke}. Following the Antibodies guidance, check the named teams (or the whole node on a patrol) with control_intel_brief. For a team working against its goal, send its lead's steering chat at most one concise note with control_agent_message; escalate with control_operator_notify only when the lead is not fixing it or the operator is needed. Send nothing when evidence is unchanged. End the turn when done.")
}
/// Queues one bot turn in its chat, reusing the chat's latest task context like
/// room listening does. Errors leave the request for a later attempt.
fn queue(state: &mut State, request: &Request) -> Result<(), &'static str> {
    let chat = request.chat.0.as_str();
    let session = state.chats.iter().find(|session| session.id == chat).ok_or("Bot chat is missing.")?;
    let from = crate::chat::latest_task(state, chat, &session.actor)
        .ok_or("Bot chat has no task.")?;
    let latest = &state.runtime.runs[from];
    let advisor = crate::delegation::is_judge(latest);
    let antibody = advisor && state.bots.antibody(chat);
    let lead = if advisor && !antibody {
        Some(state.bots.advised_team(chat).ok_or("Read-only judges run only as advisors.")?.to_owned())
    } else {
        None
    };
    let mut follow_up = crate::chat::FollowUp::from(state, latest);
    if let Some(selected)=&session.model {follow_up.backend=selected.backend;follow_up.model=Some(selected.model.clone());}
    let recovery = (latest.kind.prompt_role()==control_context::agent_policy::PromptRole::Wake && latest.status == "completed"
        && latest.attempts.last().is_some_and(|a|a.context_trace.is_some()
            && a.turn_mailbox.as_ref().is_some_and(|mailbox|mailbox.accepted().is_none())))
        .then(||latest.id.clone());
    let note = recovery.as_ref().map(|_| recovery_note(state, chat, latest)).or_else(|| timeout_note(latest));
    // A recovery's Continue is not runnable work from a committed checkpoint.
    let reasons = request.reasons.iter().filter(|reason| note.is_none() || **reason != Reason::Continue).cloned().collect::<Vec<_>>();
    let woke = match (&note, describe(state, &reasons)) {
        (Some(_), woke) if woke.is_empty() && recovery.is_some() => "a turn that ended without an accepted handoff".to_owned(),
        (Some(_), woke) if woke.is_empty() && timed_out(latest) => "a turn stopped at its time limit".to_owned(),
        (Some(_), woke) if woke.is_empty() && stopped_by(latest, crate::attempt_control::Termination::StepLimit) => "a turn stopped at its model-step limit".to_owned(),
        (Some(_), woke) if woke.is_empty() => "a turn ended by a provider failure".to_owned(),
        (_, woke) => woke,
    };
    let objective = match &lead { Some(lead) => advice(&woke, lead), None if antibody => patrol(&woke), None => objective(&woke, note.as_deref(), posts(&reasons)) };
    let objective = match (lead.is_none() && !antibody, unread(state, chat)) {
        (true, Some((count, digest))) => format!("{objective}\nUnread agent messages ({count}; quoted briefly, full text in conversation callbacks; evidence, not operator steering):\n{digest}"),
        _ => objective,
    };
    let woke = match &note { Some(note) => format!("{woke}. {note}"), None => woke };
    let run = follow_up.enqueue(state, chat, objective, RunKind::BotWake, "bot")?;
    run.bot_turn = Some(Box::new(Turn { bot: request.bot.0.clone(), turn: request.turn.0, reasons:request.reasons.clone(), recovery }));
    let id = run.id.clone();
    let session = state.chats.iter_mut().find(|session| session.id == chat).unwrap();
    session.messages.push(crate::chat::Message { speaker: Default::default(), text: format!("Bot woke: {woke}"), run_id: Some(id), reply: String::new() });
    session.revision = session.revision.saturating_add(1);
    Ok(())
}

/// Runs in the existing worker supervisor. Feeds new room posts, finished bot
/// turns and the current time to the supervisor, records its turn requests and
/// queues them, then saves everything in one checkpoint.
pub fn reconcile(state: &mut State, save: impl FnMut(&State) -> std::io::Result<()>) -> Result<(), &'static str> {
    reconcile_at(state, now_ms(), save)
}

/// Only a successful, published session can receive an automatic recovery turn.
/// Failed/interrupted effects require explicit retry. Native prose is never
/// parsed to guess completion or permission: handoff disposition is the contract.
fn needs_recovery(state:&State, index:usize) -> bool {
    let run=&state.runtime.runs[index];
    let Some(chat)=state.chats.iter().find(|chat|Some(&chat.id)==run.chat_id.as_ref() && chat.actor==run.actor) else{return false;};
    let Some(continuity)=chat.continuity.as_ref() else{return false;};
    let Some(attempt)=run.attempts.last() else{return false;};
    let head=if run.action==identity::Action::Improve {&chat.improve_workspace}else{&chat.workspace};
    // A turn stopped at its time limit was cut off mid-work, not waiting; it
    // published nothing, so its workspace head is the previous one.
    let stopped=cut_off(run);
    if !(run.status=="completed" || stopped) || attempt.review_only || attempt.context_trace.as_ref().is_none_or(|trace|Some(trace.generation.as_str())!=attempt.workspace_id.as_deref())
        || (!stopped && (attempt.artifact.is_none() || &attempt.artifact!=head))
        || attempt.turn_mailbox.as_ref().is_none_or(|mailbox|mailbox.accepted().is_some())
        || continuity.working.as_ref().is_some_and(|working|matches!(working.disposition,control_context::turn_mailbox::Disposition::Done)
            && state.runtime.runs.iter().position(|previous|previous.id==working.run).is_none_or(|previous|
                state.runtime.runs.get(previous.saturating_add(1)..=index).is_none_or(|newer|!newer.iter().any(|input|input.chat_id==run.chat_id && input.actor==run.actor
                    && input.kind.prompt_role()==control_context::agent_policy::PromptRole::Instruction))))
        || run.question.as_ref().is_some_and(|question|question.answer.is_none())
        || crate::delegation::pausing(run)
        || chat.project.as_ref().is_some_and(|project|project.pending || project.grant.is_none())
        || state.bots.approvals.iter().any(|approval|approval.requester==chat.id && approval.decision==Decision::Pending)
        || !crate::ports::pending_for_chat(state,&chat.id).is_empty()
        || !crate::design::pending(state,&chat.id).is_empty()
        || state.runtime.runs.iter().filter(|parent|parent.chat_id==run.chat_id && parent.actor==run.actor && parent.action==run.action)
            .flat_map(|parent|&parent.spawns).any(|batch|batch.awaits_callback(state))
    {return false;}
    // A queued decision/steering/child completion already supplies the next
    // writer. Never race it with a generic recovery or supersede newer input.
    !state.runtime.runs[index+1..].iter().any(|newer|newer.chat_id==run.chat_id
        && (matches!(newer.status.as_str(),"queued"|"running"|"waiting")
            || newer.kind.prompt_role()==control_context::agent_policy::PromptRole::Instruction))
}

/// Answers a completed turn of `state.chats[index]` that ended without an
/// accepted handoff. Each attempt has a new generation, so the stamp alone
/// would re-arm recovery after every failed turn; the streak bounds it.
/// A rejected handoff, or a turn stopped at its time limit, gets up to
/// `bot_recovery_continues` immediate turns. A
/// turn that never called the handoff, or a streak past that allowance, waits
/// on the bot's subscriptions with a doubling recheck. Past the allowance the
/// team's advisor and the node notice hear once per streak that the member
/// cannot checkpoint. Only typed host records decide; prose is never parsed.
fn recover(state:&mut State, index:usize, generation:&str, now:u64, facts:&mut std::collections::BTreeMap<String,Facts>, effects:&mut Vec<Planned<BotId, Effect>>) {
    let (allowance,base)=(state.runtime.execution.bot_recovery_continues,state.runtime.execution.bot_recovery_recheck_seconds);
    let chat=state.chats[index].id.clone();
    let mode=state.chats[index].continuity.as_mut().unwrap();
    mode.recovered=Some(generation.to_owned());
    let streak=&mut mode.checkpointing;
    streak.failures=streak.failures.saturating_add(1);
    let rejection=streak.rejected.as_ref().filter(|r|r.generation==generation).map(|r|r.error.clone());
    let failures=streak.failures;
    let report=failures as usize>allowance && !streak.reported;
    streak.reported|=report;
    if !continuation_enabled(state,&chat) { return; }
    // A rejected handoff or a turn cut off at its time limit has runnable work.
    let stopped=state.runtime.runs.iter().rev().find(|run|run.chat_id.as_deref()==Some(chat.as_str()))
        .is_some_and(|run|cut_off(run) && run.attempts.last().and_then(|attempt|attempt.workspace_id.as_deref())==Some(generation));
    if (rejection.is_some() || stopped) && failures as usize<=allowance {
        effects.extend(state.bots.supervisor.step(Event::Continue(bot(&chat)),Now(now)));
    } else {
        park(state,&chat,recheck(base,failures,allowance),now,effects);
    }
    if report {
        state.runtime.notice=format!("Bot in chat {chat} cannot checkpoint: {failures} turn(s) ended without an accepted handoff; it now waits for events with a backoff recheck.");
        lifecycle::concern(state,facts,&chat,|facts|facts.uncheckpointed(lifecycle::Uncheckpointed{chat:chat.clone(),turns:failures,rejection}));
    }
}
/// Seconds until a parked bot's recheck: `base` for the first parks, then
/// doubling with each failed turn past the allowance, up to 64 times `base`
/// or one day.
fn recheck(base:u64, failures:u32, allowance:usize) -> u64 {
    let doublings=failures.saturating_sub(1).saturating_sub(u32::try_from(allowance).unwrap_or(u32::MAX)).min(6);
    base.saturating_mul(1<<doublings).min(86_400)
}
/// The default wait: every current subscription (own conversation first,
/// then rooms and other topics, watches last, as a wait selects at most 16;
/// urgent watch exits and gaps release any wait) with a recheck deadline.
fn park(state:&mut State, chat:&str, seconds:u64, now:u64, effects:&mut Vec<Planned<BotId, Effect>>) {
    let mut topics=wait_topics(state,chat);
    topics.sort_by_key(|topic| match topic.split_once(':').map_or("",|(kind,_)|kind) {
        "conversation"=>0, "room"=>1, "watch"=>3, _=>2,
    });
    topics.truncate(16);
    let topics=select_wait(state,chat,&topics,Now(now),effects);
    effects.extend(state.bots.supervisor.step(Event::Wait{bot:bot(chat),topics,until:Some(now.saturating_add(seconds.saturating_mul(1000)))},Now(now)));
}

fn reconcile_at(state: &mut State, now: u64, save: impl FnMut(&State) -> std::io::Result<()>) -> Result<(), &'static str> {
    reconcile_with_wake_at(state, now, save, crate::worker::wake)
}

fn reconcile_with_wake_at(state: &mut State, now: u64, mut save: impl FnMut(&State) -> std::io::Result<()>, wake: impl FnOnce()) -> Result<(), &'static str> {
    // Salience debounces end on this pass (at least every two seconds) even
    // when their routine has gone quiet.
    for w in &mut state.watches { w.release(now); }
    crate::agent_message::digest::reconcile(state, now, &mut save)?;
    let peers = peer_events(state).into_iter().map(|(chat, cursor, info)| (chat.to_owned(), cursor, info)).collect::<Vec<_>>();
    let teams = lifecycle::observe(state);
    let recoveries=state.chats.iter().enumerate().filter_map(|(chat_index,chat)| {
        if !defined(state,&chat.id) { return None; }
        let continuity=chat.continuity.as_ref()?;
        let index=state.runtime.runs.iter().rposition(|run|run.chat_id.as_deref()==Some(&chat.id))?;
        let generation=state.runtime.runs[index].attempts.last()?.workspace_id.as_ref()?;
        (continuity.recovered.as_ref()!=Some(generation) && needs_recovery(state,index))
            .then(||(chat_index,generation.clone()))
    }).collect::<Vec<_>>();
    let publications=state.chats.iter().filter_map(|chat| {
        if !defined(state,&chat.id) { return None; }
        let working=chat.continuity.as_ref()?.working.as_ref()?;
        if state.bots.continuations.get(&chat.id)==Some(&working.revision) { return None; }
        let run=state.runtime.runs.iter().position(|run|run.id==working.run && run.status=="completed" && run.chat_id.as_deref()==Some(&chat.id))?;
        let head=if state.runtime.runs[run].action==identity::Action::Improve {&chat.improve_workspace}else{&chat.workspace};
        let processed=working.processed.iter().filter_map(|range|match range {
            control_context::turn_mailbox::Range::Conversation{chat:id,through,..} if id==&chat.id=>Some(*through as usize),_=>None,
        }).max().unwrap_or(0);
        let conversation=crate::conversation::Conversation::of(state,chat);
        let later_input=state.runtime.runs[run+1..].iter().any(|r|r.chat_id.as_deref()==Some(&chat.id)
            && r.kind.prompt_role()==control_context::agent_policy::PromptRole::Instruction
            && (r.status!="completed" || conversation.position(&r.id).is_none_or(|position|position>=processed)));
        (head.as_deref()==Some(&working.checkpoint)).then(||(chat.id.clone(),working.clone(),!later_input))
    }).collect::<Vec<_>>();
    let resumable = state.bots.approvals.iter().any(|approval| approval.decision != Decision::Pending && !approval.acknowledged);
    if state.bots.supervisor.definitions().next().is_none() && state.bots.requests.is_empty() && !resumable {
        state.bots.room_cursor = state.collective.posts.len();
        watch_events(state);
        return Ok(());
    }
    let outstanding = state.bots.supervisor.outstanding().next().is_some();
    if peers.is_empty() && teams.is_empty() && recoveries.is_empty() && publications.is_empty() && !outstanding && !resumable && state.bots.requests.is_empty() && state.bots.room_cursor == state.collective.posts.len()
        && state.bots.supervisor.next_deadline(Now(now)).is_none_or(|due| due.0 > now)
        && !pending_watch_events(state) && drifted(state, false).is_empty()
        && !crate::rooms::claims::abandonment_due(state)
    {
        return Ok(());
    }
    let previous = state.bots.clone();
    let claims = (state.collective.claims.clone(), state.collective.claim_seq);
    let watches = state.watches.clone();
    let (runs, notice) = (state.runtime.runs.len(), state.runtime.notice.clone());
    let chats = state.chats.iter().map(|session| (session.messages.len(), session.revision,session.continuity.as_ref().map(|mode|(mode.recovered.clone(),mode.checkpointing.clone())))).collect::<Vec<_>>();
    let (decided, resumed) = advance(state);
    let observed_teams = !teams.is_empty();
    let mut facts = std::collections::BTreeMap::new();
    for (root, team, observed) in teams {
        state.bots.teams.insert(root.clone(), team);
        facts.insert(root, observed);
    }
    // Turns requested earlier and now finished; decided before new events so a
    // turn requested in this pass is never mistaken for a finished one.
    let finished = if outstanding {
        let index = state.runtime.runs.iter().enumerate().map(|(i, run)| (run.id.as_str(), i)).collect::<std::collections::HashMap<_, _>>();
        let tagged = state.runtime.runs.iter().enumerate()
            .filter_map(|(i, run)| run.bot_turn.as_ref().map(|turn| ((turn.bot.as_str(), turn.turn), i)))
            .collect::<std::collections::HashMap<_, _>>();
        // A turn that delegated or awaited a design answer continues in another run;
        // it finishes when that chain's leaf does. A broken chain releases the bot.
        state.bots.supervisor.outstanding()
            .filter(|(bot, _, turn)| match tagged.get(&(bot.0.as_str(), turn.0)) {
                Some(&run) => crate::rooms::continuation_leaf(state, run, &index).is_none_or(|leaf| crate::application::runs::status::finished(&state.runtime.runs[leaf].status)),
                None => !state.bots.requests.iter().any(|request| &request.bot == *bot && &request.turn == *turn),
            })
            .map(|(bot, _, turn)| (bot.clone(), *turn))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let completed = !finished.is_empty();
    let mut effects = Vec::new();
    // Publication, not a read or draft, authorises retiring pending callbacks.
    // Offer runnable continuation before completing the old turn so it folds
    // with already pending events into one budgeted successor.
    for (chat,working,resume) in &publications {
        let grants_applied = state.runtime.runs.iter().find(|run| run.id == working.run).is_some_and(|published| {
            crate::chat::latest_task(state, chat, &published.actor).is_none_or(|latest|
                state.runtime.runs[latest].resource_grants.iter().all(|grant|
                    state.services.iter().filter(|service| grant.matches(service)).all(|service|
                        crate::services::accessible(service, &service.resource.owner.node, published))))
        });
        for range in &working.processed {
            let topic=match range {
                control_context::turn_mailbox::Range::Room{room:name,..}=>Some(room(name)),
                control_context::turn_mailbox::Range::Watch{watch:id,..}=>Some(watch(id)),
                // Reading a permission offer is not activating it in the
                // current immutable attempt. Preserve that next-turn wake.
                control_context::turn_mailbox::Range::Conversation{chat,..} if grants_applied=>Some(Topic::new("conversation",chat)),
                control_context::turn_mailbox::Range::Conversation{..}=>None,
            };
            if let Some(topic)=topic {effects.extend(state.bots.supervisor.step(Event::Processed{bot:bot(chat),topic,through:range.through()},Now(now)));}
        }
        if *resume { match &working.disposition {
            control_context::turn_mailbox::Disposition::Continue => {
                select_wait(state,chat,&[],Now(now),&mut effects);
                effects.extend(state.bots.supervisor.step(Event::Continue(bot(chat)),Now(now)));
            }
            control_context::turn_mailbox::Disposition::Wait{topics,recheck_seconds} => {
                let topics = select_wait(state,chat,topics,Now(now),&mut effects);
                effects.extend(state.bots.supervisor.step(Event::Wait{bot:bot(chat),topics,until:recheck_seconds.map(|seconds| now.saturating_add(u64::from(seconds)*1000))},Now(now)));
            }
            control_context::turn_mailbox::Disposition::Done => { select_wait(state,chat,&[],Now(now),&mut effects); }
        } }
        state.bots.continuations.insert(chat.clone(),working.revision);
        // An accepted, published handoff ends any failure streak. A rejection
        // stays: it names its own attempt, which may be newer than this one.
        if let Some(mode)=state.chats.iter_mut().find(|c|c.id==*chat).and_then(|c|c.continuity.as_mut()) {
            (mode.checkpointing.failures,mode.checkpointing.reported)=(0,false);
        }
    }
    for (chat,generation) in &recoveries {
        recover(state,*chat,generation,now,&mut facts,&mut effects);
    }
    for (bot, turn) in finished {
        effects.extend(state.bots.supervisor.step(Event::TurnCompleted { bot, turn }, Now(now)));
    }
    // Do not request a callback before a concurrently active worker's published
    // conversation receipts have had a chance to retire this evidence.
    for (chat, cursor, info) in &peers {
        // Approval reconciliation above can itself enqueue a decision callback.
        if !state.runtime.runs.iter().any(|run| run.chat_id.as_deref() == Some(chat)
            && matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling")) {
            offer_at(state, chat, *cursor, Now(now), info.clone());
        }
    }
    let start = state.bots.room_cursor.min(state.collective.posts.len());
    let end = start.saturating_add(POSTS_PER_PASS).min(state.collective.posts.len());
    for (index, post) in state.collective.posts.iter().enumerate().take(end).skip(start) {
        if post.delivery == crate::rooms::Delivery::Record { continue; }
        let event = Event::Info { topic: room(&post.room), cursor: index as u64 + 1, from: post.chat.clone().map(ChatId), info: Message::Post };
        effects.extend(state.bots.supervisor.step(event, Now(now)));
    }
    let offered = state.bots.room_cursor != end;
    state.bots.room_cursor = end;
    // Subscribe to new watches and keep finished ones until their last output
    // is offered, then drop them.
    let mut redefined = false;
    for definition in drifted(state, true) {
        redefined = true;
        effects.extend(state.bots.supervisor.step(Event::Define(definition), Now(now)));
    }
    let observed = watch_events(state);
    let watched = !observed.is_empty();
    for (topic, cursor, info) in observed {
        // `from` suppresses room echoes, not owned process results. Creator
        // provenance cannot justify dropping this bot's own exits or failures.
        // Only a retired restart loop tells an advisor something; a single
        // failed exit is the owning member's ordinary work.
        if let Message::Exit { gave_up: true, .. } = &info
            && let Some(chat) = state.watches.iter().find(|w| w.id == topic.name).map(|w| w.chat.clone()) {
            lifecycle::concern(state, &mut facts, &chat, |facts| facts.retired += 1);
        }
        effects.extend(state.bots.supervisor.step(Event::Info { topic, cursor, from: None, info }, Now(now)));
    }
    for definition in drifted(state, false) {
        redefined = true;
        effects.extend(state.bots.supervisor.step(Event::Define(definition), Now(now)));
    }
    // A publication may have processed a finished watch's last event. Delete
    // such records so the next publication retires their watermarks.
    let collected = publications.iter().flat_map(|(chat, ..)| crate::watches::delivery::drained(state, chat, |id| state.bots.retains_watch(id))).collect::<Vec<_>>();
    if !collected.is_empty() {
        state.watches.retain(|w| !collected.contains(&w.id));
    }
    // Owners that can no longer act resolve their claims, so dependents wake
    // instead of sleeping on a producer that will never report.
    let abandoned = crate::rooms::claims::observe_abandoned(state);
    for resolved in &abandoned {
        claim_resolved_at(state, resolved, Now(now));
    }
    for resolved in &abandoned {
        if let Some(owner) = state.collective.claims.iter()
            .find(|claim| claim.room == resolved.room && claim.key == resolved.key && claim.incarnation == resolved.incarnation)
            .map(|claim| claim.owner.clone()) {
            lifecycle::concern(state, &mut facts, &owner, |facts| facts.abandoned += 1);
        }
    }
    effects.extend(lifecycle::offer(state, facts, Now(now)));
    effects.extend(state.bots.supervisor.step(Event::Tick, Now(now)));
    let planned = !effects.is_empty();
    record(state, effects);
    let mut queued = false;
    let requests = std::mem::take(&mut state.bots.requests);
    for request in requests {
        if !state.bots.supervisor.eligible(&request.bot, &request.turn) {
            queued = true;
            continue;
        }
        match queue(state, &request) {
            Ok(()) => queued = true,
            Err(_) => state.bots.requests.push(request),
        }
    }
    if !(offered || observed_teams || completed || planned || queued || decided || redefined || watched || !abandoned.is_empty() || !peers.is_empty() || !publications.is_empty() || !recoveries.is_empty()) {
        return Ok(());
    }
    if save(state).is_err() {
        state.bots = previous;
        (state.collective.claims, state.collective.claim_seq) = claims;
        state.watches = watches;
        state.runtime.runs.truncate(runs);
        state.runtime.notice = notice;
        for (session, (messages, revision,continuity)) in state.chats.iter_mut().zip(chats) {
            session.messages.truncate(messages);
            session.revision = revision;
            if let (Some(mode),Some((recovered,checkpointing)))=(session.continuity.as_mut(),continuity){(mode.recovered,mode.checkpointing)=(recovered,checkpointing);}
        }
        return Err("Could not save bot progress; events remain pending.");
    }
    // Spools go only after the deletion is durable. Unit tests never touch the
    // node's spool directory.
    if !cfg!(test) { crate::watches::discard(&collected); }
    if resumed { wake(); }
    Ok(())
}

const MAX_APPROVALS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Decision {
    Pending,
    Approved,
    Denied,
}

/// An agent's request to run a chat as a bot. Granting it is the operator's
/// decision; the request itself carries no authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Approval {
    id: String,
    chat: String,
    requester: String,
    origin: String,
    every_minutes: Option<u64>,
    room: bool,
    decision: Decision,
    acknowledged: bool,
}
impl Approval {
    fn summary(&self) -> Value {
        json!({"request_id":self.id,"chat_id":self.chat,"every_minutes":self.every_minutes,"room":self.room,
            "decision":match self.decision { Decision::Pending => "pending", Decision::Approved => "approved", Decision::Denied => "denied" }})
    }
}

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
pub(crate) fn digest_id(parts: &[&str]) -> String {
    digest(parts)
}
/// The conditions currently observed for team `root` (one phrase each), or
/// `None` when no advisor or antibody observes it.
pub(crate) fn conditions(state: &State, root: &str) -> Option<Vec<String>> {
    state.bots.teams.get(root).map(lifecycle::Team::conditions)
}
fn digest(parts: &[&str]) -> String {
    let joined = parts.join("\u{0}");
    ring::digest::digest(&ring::digest::SHA256, joined.as_bytes()).as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}
/// The top-level chat `chat` was spawned from, directly or through children.
fn root<'a>(state: &'a State, chat: &str) -> Option<&'a str> {
    let mut session = state.chats.iter().find(|session| session.id == chat)?;
    for _ in 0..64 {
        let Some(parent) = session.parent_chat.as_deref() else { return Some(session.id.as_str()) };
        session = state.chats.iter().find(|candidate| candidate.id == parent)?;
    }
    None
}
/// The catalogue tool the `advisor` lens offers.
pub(crate) const ADVISOR_TOOL: &str = "team.advisor";
pub(crate) const ADVISOR_OFF: &str = "Advisors are opt-in: select the advisor lens on the root chat (/use advisor) first.";
/// Antibody personas keep the antibody opt-in gate.
pub(crate) const ANTIBODY_OFF: &str = "Antibodies are opt-in: select the antibody gene on an operator-owned root chat (/use antibody) first.";
/// Whether the task tree rooted at `top` opted in to an advisor: the root
/// chat's selected lenses (ticked in the UI or with `/use advisor`) offer
/// [`ADVISOR_TOOL`]. This is availability, not authority: without it there is
/// no advisor option or lifecycle topic; with it, operator bot approval still
/// decides whether one runs.
pub(crate) fn advisor_offered(state: &State, top: &str) -> bool {
    state.chats.iter().find(|session| session.id == top && session.parent_chat.is_none())
        .is_some_and(|session| crate::lens_library::offers(state, &crate::chat::selected_lenses(session), ADVISOR_TOOL))
}
/// The catalogue tool the `antibody` lens offers.
pub(crate) const ANTIBODY_TOOL: &str = "node.antibody";
/// Whether the tree rooted at `top` opted in to antibodies: the root chat is
/// the local operator's (an antibody reads every team on the node) and its
/// selected lenses offer [`ANTIBODY_TOOL`]. Availability, not authority: the
/// operator's bot approval still decides whether one runs.
pub(crate) fn antibody_offered(state: &State, top: &str) -> bool {
    state.chats.iter().find(|session| session.id == top && session.parent_chat.is_none())
        .is_some_and(|session| session.actor == identity::Actor::LocalOperator
            && crate::lens_library::offers(state, &crate::chat::selected_lenses(session), ANTIBODY_TOOL))
}
/// [`antibody_offered`] for the task tree `chat` belongs to.
pub(crate) fn antibody_offered_for(state: &State, chat: &str) -> bool {
    root(state, chat).is_some_and(|top| antibody_offered(state, top))
}
/// Whether any approved antibody runs whose tree still selects the lens.
/// Without one the node computes no node-wide facts.
pub(crate) fn antibodies_active(state: &State) -> bool {
    state.bots.supervisor.definitions().any(|definition| definition.restart == Restart::Permanent && lifecycle::immune(definition)
        && antibody_offered_for(state, &definition.chat.0))
}
/// [`advisor_offered`] for the task tree `chat` belongs to.
pub(crate) fn advisor_offered_for(state: &State, chat: &str) -> bool {
    root(state, chat).is_some_and(|top| advisor_offered(state, top))
}
/// Whether two chats of one actor belong to the same top-level conversation:
/// a lead, its helpers and every agent they spawned.
fn related(state: &State, chat: &str, requester: &str, actor: &identity::Actor) -> bool {
    state.chats.iter().any(|session| session.id == chat && &session.actor == actor) && root(state, chat).is_some_and(|top| root(state, requester) == Some(top))
}

/// Worker tool entry point: agents request bots and read their state.
pub(crate) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: BotCall) -> io::Result<Value> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    handle(&mut state, attempt, call, crate::storage::save)
}
fn handle(state: &mut State, attempt: &AttemptRef<'_>, call: BotCall, save: impl Fn(&State) -> io::Result<()>) -> io::Result<Value> {
    let run = state.runtime.runs.iter()
        .find(|run| run.id == attempt.run && run.status == "running"
            && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation))
        .ok_or_else(|| fail("Attempt is not active"))?;
    identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::is_judge(run) {
        return Err(fail("Judges cannot request bots"));
    }
    let requester = run.chat_id.clone().ok_or_else(|| fail("A chat is required"))?;
    let (origin, actor) = (run.id.clone(), run.actor.clone());
    match call {
        BotCall::Pause => {
            if !defined(state, &requester) { return Err(fail("This chat has no bot to pause")); }
            let previous = state.bots.clone();
            let notice = state.runtime.notice.clone();
            let effects = state.bots.supervisor.step(Event::Pause(bot(&requester)), Now(now_ms()));
            record(state, effects);
            if let Err(error) = save(state) {
                state.bots = previous;
                state.runtime.notice = notice;
                return Err(error);
            }
            Ok(json!({"paused":true,"chat_id":requester,"resume":"The operator can use /bot resume in this chat. Services and pending events are retained."}))
        }
        BotCall::Status { chat_id } => {
            let chat = chat_id.unwrap_or_else(|| requester.clone());
            if !related(state, &chat, &requester, &actor) {
                return Err(fail("Read bot state only for chats in this conversation"));
            }
            let requests = state.bots.approvals.iter().filter(|a| a.requester == requester).map(Approval::summary).collect::<Vec<_>>();
            Ok(json!({"chat_id":chat,"bot":status(state, &chat),"requests":requests}))
        }
        BotCall::Request { chat_id, every_minutes, room } => {
            // A persona's schedule behaviour is the default interval.
            let every_minutes = every_minutes.or_else(|| crate::personas::schedule(state, &chat_id));
            if !attempt.nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || attempt.nonce.is_empty() || attempt.nonce.len() > 128 {
                return Err(fail("Invalid request identity"));
            }
            let id = format!("bot-{}", digest(&[attempt.run, attempt.generation, attempt.nonce, &chat_id]));
            if let Some(existing) = state.bots.approvals.iter().find(|a| a.id == id) {
                return Ok(existing.summary());
            }
            let target = state.chats.iter().find(|session| session.id == chat_id).ok_or_else(|| fail("Chat not found"))?;
            if !related(state, &chat_id, &requester, &actor) {
                return Err(fail("Request bots only for chats in this conversation"));
            }
            if every_minutes.is_some_and(|m| !(1..=MAX_INTERVAL_MINUTES).contains(&m)) {
                return Err(fail("every_minutes must be 1–1440"));
            }
            let linked = target.room.as_deref().is_some_and(crate::rooms::valid);
            // A read-only judge becomes its team's advisor: host facts, no timer or room.
            let advisor = judge_chat(state, &chat_id);
            let antibody = advisor && crate::personas::observes_node(state, &chat_id);
            if advisor && crate::personas::observer_offered(state, &chat_id) == Some(false) {
                return Err(fail(if antibody { ANTIBODY_OFF } else { ADVISOR_OFF }));
            }
            if advisor && !antibody && !advisor_offered_for(state, &chat_id) {
                return Err(fail(ADVISOR_OFF));
            }
            if advisor && !antibody && every_minutes.is_some() {
                return Err(fail("An advisor wakes only on its team's host facts; omit every_minutes"));
            }
            if !advisor && !(room && linked) && every_minutes.is_none() && watch_sources(state, &chat_id).is_empty() {
                return Err(fail("The chat has no room or watch to wake it; give every_minutes or start a watch"));
            }
            if !state.runtime.runs.iter().any(|run| run.chat_id.as_deref() == Some(chat_id.as_str())) {
                return Err(fail("The chat has no task yet; give it one before it becomes a bot"));
            }
            if crate::rooms::listening(state, &chat_id) {
                return Err(fail("That chat listens to its room; it must turn off /listen first"));
            }
            if defined(state, &chat_id) || state.bots.approvals.iter().any(|a| a.chat == chat_id && a.decision == Decision::Pending) {
                return Err(fail("That chat already runs a bot or has a pending request"));
            }
            if state.bots.approvals.iter().filter(|a| a.decision == Decision::Pending).count() >= MAX_APPROVALS {
                return Err(fail("Too many pending bot requests; wait for the operator"));
            }
            let previous = state.bots.clone();
            state.bots.approvals.retain(|a| !(a.acknowledged && a.decision != Decision::Pending));
            let approval = Approval { id, chat: chat_id, requester: requester.clone(), origin, every_minutes, room: room && linked && !advisor, decision: Decision::Pending, acknowledged: false };
            let mut value = approval.summary();
            value["approval"] = json!(format!("Waiting for the operator: approve in this chat or with /bot allow {} (also over Discord). End your turn; you will be resumed with the decision.", approval.id));
            state.bots.approvals.push(approval);
            bump(state, &requester);
            if let Err(error) = save(state) {
                state.bots = previous;
                return Err(error);
            }
            Ok(value)
        }
    }
}
/// A spawned persona's wake behaviours (schedule, room) file the bot request
/// `control_bot_request` would, on behalf of the spawning chat, for the operator
/// to approve like any other. Nothing runs until approval. `None` when the
/// persona asks for no wake or the request cannot be filed now (the agent can
/// still request one itself).
pub(crate) fn persona_request(state: &mut State, requester: &str, origin: &str, chat: &str, behaviours: &[control_context::persona::Behaviour]) -> Option<String> {
    use control_context::persona::Behaviour;
    let every_minutes = behaviours.iter().find_map(|b| match b { Behaviour::Schedule { every_minutes } => Some(*every_minutes), _ => None });
    let linked = state.chats.iter().find(|session| session.id == chat).and_then(|session| session.room.as_deref()).is_some_and(crate::rooms::valid);
    let advisor = judge_chat(state, chat);
    let room = behaviours.contains(&Behaviour::Room) && linked && !advisor;
    if (every_minutes.is_none() && !room)
        || every_minutes.is_some_and(|m| !(1..=MAX_INTERVAL_MINUTES).contains(&m))
        || defined(state, chat)
        || state.bots.approvals.iter().any(|a| a.chat == chat && a.decision == Decision::Pending)
        || state.bots.approvals.iter().filter(|a| a.decision == Decision::Pending).count() >= MAX_APPROVALS
    {
        return None;
    }
    let id = format!("bot-{}", digest(&["persona", origin, chat]));
    state.bots.approvals.retain(|a| !(a.acknowledged && a.decision != Decision::Pending));
    state.bots.approvals.push(Approval { id: id.clone(), chat: chat.into(), requester: requester.into(), origin: origin.into(), every_minutes, room, decision: Decision::Pending, acknowledged: false });
    bump(state, requester);
    Some(id)
}
/// Whether a chat's latest task is a read-only judge, so it can only advise.
fn judge_chat(state: &State, chat: &str) -> bool {
    state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(chat)).is_some_and(crate::delegation::is_judge)
}
fn bump(state: &mut State, chat: &str) {
    if let Some(session) = state.chats.iter_mut().find(|session| session.id == chat) {
        session.revision = session.revision.saturating_add(1);
    }
}

/// The operator's decision, from the chat panel or `/bot allow|deny ID` in the
/// requesting or target chat. Approval starts the bot; the requester resumes later.
fn decide(state: &mut State, id: &str, allow: bool, chat: Option<&str>, now: Now) -> Result<&'static str, &'static str> {
    let index = state.bots.approvals.iter()
        .position(|a| a.id == id && a.decision == Decision::Pending && chat.is_none_or(|chat| visible(state, a, chat)))
        .ok_or("No pending bot request with that ID in this conversation.")?;
    let approval = state.bots.approvals[index].clone();
    let session = state.chats.iter().find(|session| session.id == approval.requester).ok_or("Requesting chat is missing.")?;
    identity::authorize(&session.actor, identity::Action::Task)?;
    if allow {
        start(state, &approval.chat, approval.every_minutes, approval.room, now)?;
    }
    state.bots.approvals[index].decision = if allow { Decision::Approved } else { Decision::Denied };
    bump(state, &approval.requester);
    Ok(if allow { "Bot request approved; the chat now runs as a bot." } else { "Bot request denied." })
}

/// Resumes each requesting agent once all its requests are decided, if the
/// requesting turn (or its continuation) is still the latest in that chat;
/// otherwise records the decisions in the chat without a new turn.
fn advance(state: &mut State) -> (bool, bool) {
    let mut changed = false;
    let mut resumed = false;
    let requesters = state.bots.approvals.iter()
        .filter(|a| a.decision != Decision::Pending && !a.acknowledged)
        .map(|a| a.requester.clone()).collect::<std::collections::BTreeSet<_>>();
    for requester in requesters {
        let mine = |a: &&Approval| a.requester == requester;
        if state.bots.approvals.iter().filter(mine).any(|a| a.decision == Decision::Pending) {
            continue;
        }
        let decided = state.bots.approvals.iter().filter(mine).filter(|a| !a.acknowledged).cloned().collect::<Vec<_>>();
        let lines = decided.iter().map(|a| {
            let title = crate::callsigns::name_or_id(&state.chats, &a.chat);
            format!("{title} ({}): {}", a.chat, if a.decision == Decision::Approved { "approved, now a persistent bot" } else { "denied" })
        }).collect::<Vec<_>>().join("\n");
        let origins = decided.iter().map(|a| a.origin.as_str()).collect::<Vec<_>>();
        let task = state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(requester.as_str())).map(|run| run.objective.clone()).unwrap_or_default();
        let outcome = crate::approvals::resume(state, crate::approvals::Decisions {
            chat: &requester, origins: &origins, heading: crate::application::approval_resume::BOT_DECISIONS, lines: &lines,
            instruction: "Continue the original task. Approved chats now sleep until room messages or their timers wake them.",
            task: &task, kind: RunKind::BotDecision, source: "bot", grants: Vec::new(),
        });
        if outcome == crate::approvals::Outcome::Wait {
            continue;
        }
        resumed |= matches!(outcome, crate::approvals::Outcome::Resumed(_));
        for approval in state.bots.approvals.iter_mut().filter(|a| a.requester == requester && a.decision != Decision::Pending) {
            approval.acknowledged = true;
        }
        changed = true;
    }
    (changed, resumed)
}

fn visible(state: &State, approval: &Approval, chat: &str) -> bool {
    let Some(viewer) = state.chats.iter().find(|session| session.id == chat) else { return false; };
    state.chats.iter().any(|requester| requester.id == approval.requester && requester.actor == viewer.actor)
        && (approval.chat == chat || crate::chat::within(state, &approval.requester, chat))
}
fn pending<'a>(state: &'a State, chat: &'a str) -> impl Iterator<Item = &'a Approval> {
    state.bots.approvals.iter().filter(move |a| a.decision == Decision::Pending && visible(state, a, chat))
}
pub(crate) fn pending_count(state: &State, chat: &str) -> usize { pending(state,chat).count() }
/// Requesting chats of every pending request, for the node-wide view.
pub(crate) fn pending_chats(state: &State) -> impl Iterator<Item = &str> {
    state.bots.approvals.iter().filter(|a| a.decision == Decision::Pending).map(|a| a.requester.as_str())
}
pub(crate) fn decision_revision(state: &State) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for approval in &state.bots.approvals { (&approval.id, &approval.requester, &approval.chat, approval.decision).hash(&mut hash); }
    hash.finish()
}
/// Pending requests from this chat and its same-actor descendants.
pub(crate) fn pending_panel(state: &State, chat: &str) -> String {
    pending(state, chat).map(|a| {
        let title = crate::callsigns::name_or_id(&state.chats, &a.chat);
        let mut sources = Vec::new();
        if a.room { sources.push("room messages".to_owned()); }
        if judge_chat(state, &a.chat) { sources.push("team facts (read-only advisor)".to_owned()); }
        if let Some(minutes) = a.every_minutes { sources.push(format!("every {minutes} min")); }
        format!("<article class='assistant-message port-request' data-transcript-key='bot-{}'><h3>Bot request · {}</h3><p>{} · up to {TURNS_PER_WINDOW} turns/hour</p><form class='decision-form' method='post' action='/tools/bots' target='_top'><input type='hidden' name='id' value='{}'><input type='hidden' name='return_chat' value='{}'><button class='primary' name='allow' value='yes'>Allow bot</button><button name='deny' value='yes' formnovalidate>Deny</button><p role='alert' hidden></p></form></article>",
            crate::escape(&a.id), crate::escape(&title), crate::escape(&sources.join(" and ")), crate::escape(&a.id), crate::escape(chat))
    }).collect()
}
pub(crate) fn pending_json(state: &State, chat: &str) -> Value {
    Value::from(pending(state, chat).map(|a| { let mut summary = a.summary(); summary["name"] = json!(crate::callsigns::name_or_id(&state.chats, &a.chat)); summary }).collect::<Vec<_>>())
}

/// `POST /tools/bots` from the chat panel: allow or deny, then return to the chat.
pub fn http(stream: &mut impl io::Write, shared: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8]) -> bool {
    http_with_save(stream, shared, method, path, body, crate::storage::save)
}
fn http_with_save(stream: &mut impl io::Write, shared: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8], save: impl Fn(&State) -> io::Result<()>) -> bool {
    if path != "/tools/bots" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let asynchronous = fields.get("response").is_some_and(|value| value == "json");
    let chat = fields.get("return_chat").filter(|id| crate::chat::valid_id(id)).cloned();
    let result = (|| -> Result<(), &'static str> {
        if method != "POST" {
            return Err("Use POST");
        }
        let id = fields.get("id").ok_or("Missing request ID")?;
        let chat = chat.as_deref().ok_or("Missing chat")?;
        let mut state = shared.lock().map_err(|_| "State unavailable")?;
        let checkpoint = {
            let targets = state.bots.approvals.iter().filter(|a| &a.id == id).flat_map(|a| [a.chat.as_str(), a.requester.as_str()]).collect();
            crate::chat::Checkpoint::capture(&state, &targets)
        };
        let result = decide(&mut state, id, fields.contains_key("allow"), Some(chat), Now(now_ms()));
        if result.is_err() || save(&state).is_err() {
            checkpoint.restore(&mut state);
            return result.and(Err("Could not save the decision. Try again."));
        }
        Ok(())
    })();
    let _ = match (result, chat) {
        (result, _) if asynchronous => {
            let (status, value) = match result { Ok(()) => ("200 OK", json!({"ok":true})), Err(error) => ("400 Bad Request", json!({"error":error})) };
            control_http::respond(stream, status, "application/json", value.to_string().as_bytes(), "", control_http::Policy::Page)
        }
        (Ok(()), Some(chat)) => control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: /dm/{chat}\r\n"), control_http::Policy::Page),
        (Ok(()), None) => control_http::respond(stream, "204 No Content", "text/plain", b"", "", control_http::Policy::Page),
        (Err(error), back) => control_http::respond(stream, "400 Bad Request", "text/html; charset=utf-8", crate::error_page(error, &back.map_or("/".to_owned(), |chat| format!("/dm/{chat}"))).as_bytes(), "", control_http::Policy::Page),
    };
    true
}

#[cfg(test)]
mod tests;
