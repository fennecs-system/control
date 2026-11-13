use control_effect::machine::{Durability, Machine, Now, Planned, Stamp};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Debug;

/// Coalesced reasons a sleeping bot keeps before its next turn.
const PENDING: usize = 16;
/// Charged turns remembered for budget accounting, independent of the window,
/// so editing a budget never erases turns it already charged.
const HISTORY: usize = 256;

macro_rules! id {
    ($name:ident, $inner:ty) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(transparent))]
        pub struct $name(pub $inner);
    };
}
id!(BotId, String);
id!(ChatId, String);
id!(TurnId, u64);
impl Copy for TurnId {}

/// A subscribable source of messages, named by the application: a room, a
/// watch, a service. The supervisor only compares topics.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Topic {
    pub kind: String,
    pub name: String,
}
impl Topic {
    pub fn new(kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self { kind: kind.into(), name: name.into() }
    }
    /// Cursor key; the length prefix keeps `("a:b", "c")` and `("a", "b:c")` apart.
    fn key(&self) -> String {
        format!("{}:{}:{}", self.kind.len(), self.kind, self.name)
    }
}

/// What a bot listens to: application topics, and a timer while idle.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Source {
    Topic(Topic),
    Timer { every_ms: u64 },
}

/// An application message, like a GenServer's `handle_info` input. The
/// supervisor never inspects it; it only asks whether a newer message on the
/// same topic folds into one already pending.
pub trait Info: Clone + Debug + PartialEq {
    /// Folds `newer` into `self` and returns true, or returns false to keep
    /// both as distinct reasons.
    fn merge(&mut self, newer: &Self) -> bool;
    /// Failures may interrupt an idle wait; ordinary telemetry does not.
    fn urgent(&self) -> bool { false }
}

/// Bounds automatic turns: a minimum gap between turns and a cap per window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Budget {
    pub cooldown_ms: u64,
    pub window_ms: u64,
    pub turns_per_window: u32,
}

/// Like an OTP child spec: a permanent bot keeps waking; a temporary one ends
/// after its first turn completes, a one-shot wake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum Restart {
    #[default]
    Permanent,
    Temporary,
}

/// A bot's identity, the conversation its turns run in, its subscriptions and
/// budget. Configuration, not a grant: authority is checked by the application.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Definition {
    pub bot: BotId,
    pub chat: ChatId,
    pub sources: Vec<Source>,
    pub budget: Budget,
    #[cfg_attr(feature = "serde", serde(default))]
    pub restart: Restart,
}
impl Definition {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sources.is_empty() {
            return Err("A bot needs at least one event source");
        }
        if self.budget.window_ms == 0 || !(1..=HISTORY as u32).contains(&self.budget.turns_per_window) {
            return Err("A budget needs a positive window and 1–256 turns per window");
        }
        if self.sources.iter().any(|source| matches!(source, Source::Timer { every_ms: 0 })) {
            return Err("Timers need a positive interval");
        }
        Ok(())
    }
    fn timer(&self) -> Option<u64> {
        self.sources.iter().filter_map(|source| match source {
            Source::Timer { every_ms } => Some(*every_ms),
            Source::Topic(_) => None,
        }).min()
    }
    fn subscribes(&self, topic: &Topic) -> bool {
        self.sources.iter().any(|source| matches!(source, Source::Topic(t) if t == topic))
    }
}

/// Admitted inputs. Commands come only from authorized routes; a message saying
/// "stop" is data. Cursors are per topic and increase; replays at or below a
/// bot's cursor are ignored. Every `RequestTurn` is answered by `TurnCompleted`
/// with its turn ID, including when the turn could not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event<I> {
    Define(Definition),
    Pause(BotId),
    Resume(BotId),
    /// Explicitly request one more callback after a temporary bot's current turn.
    Rearm(BotId),
    /// Runnable work remains after a successfully published handoff. Coalesces
    /// with pending callbacks and obeys the existing budget; never rearms a
    /// temporary bot or resurrects a stopped one.
    Continue(BotId),
    /// Narrow an idle bot's wake conditions without changing its subscriptions.
    /// The host validates the deadline and topic authority before admission.
    Wait { bot: BotId, topics: Vec<Topic>, until: Option<u64> },
    /// Processing is distinct from ingestion. Clear only reasons whose exact
    /// source watermark was committed with the agent's workspace.
    Processed { bot: BotId, topic: Topic, through: u64 },
    /// Host-selected origin of a new subscription. Earlier retained events
    /// were never subscribed to; this is not a processing acknowledgement.
    StartAt { bot: BotId, topic: Topic, after: u64 },
    Stop(BotId),
    /// A message on `topic`. `from` names the chat that caused it, so a bot is
    /// not woken by its own posts.
    Info { topic: Topic, cursor: u64, from: Option<ChatId>, info: I },
    TurnCompleted { bot: BotId, turn: TurnId },
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Reason<I> {
    Info { topic: Topic, through: u64, info: I },
    Timer,
    Continue,
    /// More distinct messages arrived than a turn carries. Dropped messages are
    /// acknowledged, not replayed: the bot resynchronises from these topics.
    Overflow { dropped: u64, topics: Vec<Topic> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect<I> {
    /// Queue one turn in the bot's conversation. The turn ID is the idempotency
    /// key: recovery re-queues only an outstanding turn, never a new one.
    RequestTurn { bot: BotId, chat: ChatId, turn: TurnId, reasons: Vec<Reason<I>> },
    /// The budget is spent; reasons stay pending until `until`.
    BudgetExhausted { bot: BotId, until: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    WaitingTurn,
    Paused,
    Stopping,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(bound(serialize = "I: serde::Serialize", deserialize = "I: serde::Deserialize<'de>")))]
struct Bot<I> {
    definition: Definition,
    generation: u64,
    pending: Vec<Reason<I>>,
    turn: Option<(TurnId, ChatId)>,
    last_turn: Option<u64>,
    turns: VecDeque<u64>,
    timer_due: Option<u64>,
    cursors: BTreeMap<String, u64>,
    #[cfg_attr(feature = "serde", serde(default))]
    processed: BTreeMap<String, u64>,
    #[cfg_attr(feature = "serde", serde(default))]
    dropped: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    overflowed: Vec<Topic>,
    exhausted: bool,
    paused: bool,
    stopping: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    rearm: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    held: Vec<Topic>,
    #[cfg_attr(feature = "serde", serde(default))]
    wait: Option<Wait>,
}
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct Wait { topics: Vec<Topic>, until: Option<u64> }
impl<I: Info> Bot<I> {
    /// Admits a reason. Replays at or below the topic cursor are ignored. When
    /// the pending set is full a new distinct reason is counted as dropped and
    /// its topic is reported for resynchronisation in the next turn.
    fn accept(&mut self, cursor: Option<(String, u64)>, reason: Reason<I>) {
        if let Some((key, cursor)) = &cursor
            && (self.cursors.get(key).is_some_and(|seen| cursor <= seen)
                || self.processed.get(key).is_some_and(|seen| cursor <= seen))
        {
            return;
        }
        if !self.merge(&reason) {
            if self.pending.len() == PENDING {
                self.dropped = self.dropped.saturating_add(1);
                if let Reason::Info { topic, .. } = &reason
                    && !self.overflowed.contains(topic)
                {
                    self.overflowed.push(topic.clone());
                }
            } else {
                self.pending.push(reason);
            }
        }
        if let Some((key, cursor)) = cursor {
            self.cursors.insert(key, cursor);
        }
    }
    fn merge(&mut self, reason: &Reason<I>) -> bool {
        match reason {
            Reason::Info { topic, through, info } => self.pending.iter_mut().any(|pending| match pending {
                Reason::Info { topic: t, through: existing, info: held } if t == topic => {
                    let merged = held.merge(info);
                    if merged {
                        *existing = (*existing).max(*through);
                    }
                    merged
                }
                _ => false,
            }),
            Reason::Timer => self.pending.contains(&Reason::Timer),
            Reason::Continue => self.pending.contains(&Reason::Continue),
            Reason::Overflow { .. } => false,
        }
    }
    /// Whether a message came from this bot, through its current chat or the
    /// chat its outstanding turn runs in.
    fn own(&self, from: Option<&ChatId>) -> bool {
        from.is_some_and(|from| *from == self.definition.chat || self.turn.as_ref().is_some_and(|(_, chat)| chat == from))
    }
    fn recent(&self, now: u64) -> impl Iterator<Item = u64> + '_ {
        let window = self.definition.budget.window_ms;
        self.turns.iter().copied().filter(move |at| now.saturating_sub(*at) < window)
    }
    /// When pending reasons could next become a turn, if blocked only by time.
    /// `None` also covers deadlines beyond the representable range.
    fn ready_at(&self, now: u64) -> Option<u64> {
        if self.pending.is_empty() || self.turn.is_some() || self.paused || self.stopping || self.wait.is_some() || self.definition.validate().is_err() {
            return None;
        }
        let budget = self.definition.budget;
        let cooled = match self.last_turn {
            Some(at) => at.checked_add(budget.cooldown_ms)?,
            None => 0,
        };
        let reopened = if self.recent(now).count() >= budget.turns_per_window as usize {
            self.recent(now).next()?.checked_add(budget.window_ms)?
        } else {
            0
        };
        Some(cooled.max(reopened))
    }
}

/// Persistable supervisor for every bot on a node: a mailbox per bot with
/// cursors, coalescing, budgets and turn identity, generic over the
/// application's messages. Deterministic for a given sequence of events and
/// times; per-bot transitions are serialized.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(bound(serialize = "I: serde::Serialize", deserialize = "I: serde::Deserialize<'de>")))]
pub struct Supervisor<I> {
    bots: BTreeMap<BotId, Bot<I>>,
    generation: u64,
    revision: u64,
    next_turn: u64,
}
impl<I> Default for Supervisor<I> {
    fn default() -> Self {
        Self { bots: BTreeMap::new(), generation: 0, revision: 0, next_turn: 0 }
    }
}
impl<I: Info> Supervisor<I> {
    /// Evidence referenced by a pending or outstanding callback must remain readable.
    pub fn retains(&self, topic: &Topic) -> bool {
        self.bots.values().any(|bot| bot.held.contains(topic) || bot.overflowed.contains(topic) || bot.pending.iter().any(|reason| match reason {
            Reason::Info { topic: pending, .. } => pending == topic,
            Reason::Overflow { topics, .. } => topics.contains(topic),
            Reason::Timer | Reason::Continue => false,
        }))
    }
    pub fn definitions(&self) -> impl Iterator<Item = &Definition> {
        self.bots.values().map(|bot| &bot.definition)
    }
    pub fn status(&self, bot: &BotId) -> Option<Status> {
        self.bots.get(bot).map(|bot| match bot {
            Bot { stopping: true, .. } => Status::Stopping,
            Bot { turn: Some(_), .. } => Status::WaitingTurn,
            Bot { paused: true, .. } => Status::Paused,
            _ => Status::Ready,
        })
    }
    /// Whether a planned or recovered turn may still be queued: it must be the
    /// bot's outstanding turn and the bot must not be stopping.
    pub fn eligible(&self, bot: &BotId, turn: &TurnId) -> bool {
        self.bots.get(bot).is_some_and(|b| !b.stopping && b.turn.as_ref().is_some_and(|(id, _)| id == turn))
    }
    /// Dispatch rechecks pause separately from an already requested/running turn.
    /// Pausing retains the request; resuming does not allocate another turn.
    pub fn may_start(&self, bot: &BotId, turn: &TurnId) -> bool {
        self.eligible(bot, turn) && self.bots.get(bot).is_some_and(|b| !b.paused)
    }
    /// Whether retained source evidence is newer than both admission and
    /// published processing. The application can skip reoffering old records.
    pub fn unseen(&self, bot: &BotId, topic: &Topic, through: u64) -> bool {
        self.bots.get(bot).is_some_and(|b| !b.stopping
            && b.cursors.get(&topic.key()).is_none_or(|seen| through > *seen)
            && b.processed.get(&topic.key()).is_none_or(|seen| through > *seen))
    }
    /// Turns requested but not yet completed, for reconciliation after restart.
    pub fn outstanding(&self) -> impl Iterator<Item = (&BotId, &ChatId, &TurnId)> {
        self.bots.iter().filter_map(|(id, bot)| bot.turn.as_ref().map(|(turn, chat)| (id, chat, turn)))
    }
    /// The earliest time a `Tick` could produce an effect; drivers wait until
    /// then or the next event instead of polling.
    pub fn next_deadline(&self, now: Now) -> Option<Now> {
        self.bots.values().flat_map(|bot| [bot.timer_due.filter(|_| !bot.paused && bot.wait.is_none()), bot.wait.as_ref().filter(|_| !bot.paused).and_then(|wait| wait.until), bot.ready_at(now.0)]).flatten().min().map(Now)
    }
    fn define(&mut self, definition: Definition, now: u64) {
        if definition.validate().is_err() {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let timer = definition.timer();
        match self.bots.get_mut(&definition.bot) {
            Some(bot) if !bot.stopping => {
                if bot.definition.timer() != timer {
                    bot.timer_due = timer.and_then(|every| now.checked_add(every));
                }
                bot.definition = definition;
                bot.generation = generation;
            }
            Some(_) => {}
            None => {
                let bot = Bot {
                    timer_due: timer.and_then(|every| now.checked_add(every)),
                    definition,
                    generation,
                    pending: Vec::new(),
                    turn: None,
                    last_turn: None,
                    turns: VecDeque::new(),
                    cursors: BTreeMap::new(),
                    processed: BTreeMap::new(),
                    dropped: 0,
                    overflowed: Vec::new(),
                    exhausted: false,
                    paused: false,
                    stopping: false,
                    rearm: false,
                    held: Vec::new(),
                    wait: None,
                };
                self.bots.insert(bot.definition.bot.clone(), bot);
            }
        }
    }
    fn deliver(&mut self, event: Event<I>, now: u64) {
        match event {
            Event::Define(definition) => self.define(definition, now),
            Event::StartAt { bot, topic, after } => {
                if let Some(bot) = self.bots.get_mut(&bot) {
                    let cursor = bot.cursors.entry(topic.key()).or_default();
                    *cursor = (*cursor).max(after);
                }
            }
            Event::Pause(bot) => {
                if let Some(bot) = self.bots.get_mut(&bot) {
                    bot.paused = true;
                }
            }
            Event::Resume(bot) => {
                if let Some(bot) = self.bots.get_mut(&bot) {
                    bot.paused = false;
                    bot.wait = None;
                }
            }
            Event::Rearm(id) => {
                if let Some(bot) = self.bots.get_mut(&id)
                    && bot.definition.restart == Restart::Temporary && bot.turn.is_some()
                    && !bot.stopping && !bot.paused
                {
                    bot.rearm = true;
                }
            }
            Event::Continue(id) => {
                if let Some(bot)=self.bots.get_mut(&id)
                    && bot.definition.restart==Restart::Permanent && !bot.stopping {
                    bot.wait = None;
                    bot.accept(None,Reason::Continue);
                }
            }
            Event::Wait { bot:id, topics, until } => {
                if let Some(bot) = self.bots.get_mut(&id)
                    && !bot.stopping && until.is_none_or(|until| until > now)
                    && topics.len() <= 16 && topics.iter().all(|topic| bot.definition.subscribes(topic)) {
                    // An explicit dependency wait supersedes idle check-ins,
                    // including timers accumulated during the active turn.
                    bot.pending.retain(|reason| !matches!(reason, Reason::Continue | Reason::Timer));
                    bot.wait = (!bot.pending.iter().any(|reason| match reason {
                        Reason::Info{topic,info,..} => info.urgent() || topics.contains(topic),
                        Reason::Timer => false,
                        _ => false,
                    })).then_some(Wait{topics,until});
                }
            }
            Event::Processed { bot:id,topic,through } => {
                if let Some(bot)=self.bots.get_mut(&id) {
                    let cursor = bot.processed.entry(topic.key()).or_default();
                    *cursor = (*cursor).max(through);
                    bot.pending.retain(|reason| !matches!(reason,Reason::Info{topic:t,through:pending,..} if t==&topic && *pending<=through));
                    // A later ingestion watermark keeps its gap visible.
                    if bot.cursors.get(&topic.key()).is_some_and(|seen|*seen<=through) {
                        bot.overflowed.retain(|t|t!=&topic);
                        if bot.overflowed.is_empty(){bot.dropped=0;}
                    }
                }
            }
            Event::Stop(id) => {
                if let Some(bot) = self.bots.get_mut(&id) {
                    bot.stopping = true;
                    bot.pending.clear();
                    bot.dropped = 0;
                    bot.overflowed.clear();
                    bot.timer_due = None;
                    bot.wait = None;
                    if bot.turn.is_none() {
                        self.bots.remove(&id);
                    }
                }
            }
            Event::Info { topic, cursor, from, info } => {
                let key = topic.key();
                for bot in self.bots.values_mut().filter(|bot| !bot.stopping) {
                    if bot.definition.subscribes(&topic) && !bot.own(from.as_ref()) {
                        if bot.cursors.get(&key).is_none_or(|seen| cursor > *seen)
                            && bot.processed.get(&key).is_none_or(|seen| cursor > *seen)
                            && bot.wait.as_ref().is_some_and(|wait| info.urgent() || wait.topics.contains(&topic)) {
                            bot.wait = None;
                        }
                        bot.accept(Some((key.clone(), cursor)), Reason::Info { topic: topic.clone(), through: cursor, info: info.clone() });
                    }
                }
            }
            Event::TurnCompleted { bot: id, turn } => {
                if let Some(bot) = self.bots.get_mut(&id)
                    && bot.turn.as_ref().is_some_and(|(id, _)| *id == turn)
                {
                    bot.turn = None;
                    bot.held.clear();
                    if bot.stopping || (bot.definition.restart == Restart::Temporary && !bot.rearm && !bot.paused) {
                        self.bots.remove(&id);
                    } else if bot.rearm || (bot.paused && bot.definition.restart == Restart::Temporary) {
                        bot.rearm = false;
                        bot.turns.clear();
                        bot.last_turn = None;
                        bot.exhausted = false;
                    }
                }
            }
            Event::Tick => {}
        }
        for bot in self.bots.values_mut().filter(|bot| !bot.stopping && !bot.paused) {
            if bot.wait.as_ref().is_some_and(|wait| wait.until.is_some_and(|until| until <= now)) {
                bot.wait = None;
                bot.accept(None, Reason::Timer);
            }
            if let (Some(due), Some(every)) = (bot.timer_due, bot.definition.timer())
                && due <= now
            {
                if bot.wait.is_none() { bot.accept(None, Reason::Timer); }
                bot.timer_due = now.checked_add(every);
            }
        }
    }
    fn drain(&mut self, now: u64) -> Vec<Planned<BotId, Effect<I>>> {
        let mut planned = Vec::new();
        for (id, bot) in &mut self.bots {
            let Some(ready) = bot.ready_at(now) else {
                if !bot.pending.is_empty() && bot.turn.is_none() && bot.wait.is_none() && !bot.paused && !bot.stopping && !bot.exhausted {
                    bot.exhausted = true;
                    self.revision += 1;
                    planned.push(Planned {
                        stamp: Stamp { resource: id.clone(), generation: bot.generation, revision: self.revision },
                        durability: Durability::None,
                        effect: Effect::BudgetExhausted { bot: id.clone(), until: u64::MAX },
                    });
                }
                continue;
            };
            while bot.turns.len() > HISTORY {
                bot.turns.pop_front();
            }
            let effect = if ready <= now {
                self.next_turn += 1;
                let turn = TurnId(self.next_turn);
                bot.turn = Some((turn, bot.definition.chat.clone()));
                bot.last_turn = Some(now);
                bot.turns.push_back(now);
                bot.exhausted = false;
                let mut reasons = std::mem::take(&mut bot.pending);
                if bot.dropped > 0 {
                    reasons.push(Reason::Overflow { dropped: std::mem::take(&mut bot.dropped), topics: std::mem::take(&mut bot.overflowed) });
                }
                bot.held = reasons.iter().flat_map(|reason| match reason {
                    Reason::Info { topic, .. } => std::slice::from_ref(topic),
                    Reason::Overflow { topics, .. } => topics.as_slice(),
                    Reason::Timer | Reason::Continue => &[],
                }).cloned().collect();
                Effect::RequestTurn { bot: id.clone(), chat: bot.definition.chat.clone(), turn, reasons }
            } else if bot.recent(now).count() >= bot.definition.budget.turns_per_window as usize && !bot.exhausted {
                bot.exhausted = true;
                Effect::BudgetExhausted { bot: id.clone(), until: ready }
            } else {
                continue;
            };
            self.revision += 1;
            let durability = if matches!(effect, Effect::RequestTurn { .. }) { Durability::Intent } else { Durability::None };
            planned.push(Planned { stamp: Stamp { resource: id.clone(), generation: bot.generation, revision: self.revision }, durability, effect });
        }
        planned
    }
}
impl<I: Info> Machine for Supervisor<I> {
    type Resource = BotId;
    type Event = Event<I>;
    type Effect = Effect<I>;
    fn step(&mut self, event: Event<I>, now: Now) -> Vec<Planned<BotId, Effect<I>>> {
        self.deliver(event, now.0);
        self.drain(now.0)
    }
}

#[cfg(test)]
mod tests;
