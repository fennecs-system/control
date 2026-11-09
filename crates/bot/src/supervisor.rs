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
mod tests {
    use super::*;

    /// Test messages: posts that fold, counters that add, exits kept apart.
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    enum Msg {
        Post,
        Lines(u64),
        Exit(Option<i32>),
        Kind(String),
    }
    impl Info for Msg {
        fn urgent(&self) -> bool { matches!(self, Self::Exit(_)) }
        fn merge(&mut self, newer: &Self) -> bool {
            match (self, newer) {
                (Msg::Post, Msg::Post) => true,
                (Msg::Lines(held), Msg::Lines(more)) => {
                    *held = held.saturating_add(*more);
                    true
                }
                (Msg::Exit(held), Msg::Exit(code)) => {
                    *held = *code;
                    true
                }
                (Msg::Kind(held), Msg::Kind(kind)) => held == kind,
                _ => false,
            }
        }
    }
    type S = Supervisor<Msg>;
    type R = Reason<Msg>;

    fn bot(name: &str) -> BotId {
        BotId(name.into())
    }
    fn budget(cooldown_ms: u64, window_ms: u64, turns_per_window: u32) -> Budget {
        Budget { cooldown_ms, window_ms, turns_per_window }
    }
    fn define(s: &mut S, name: &str, sources: Vec<Source>, budget: Budget, now: u64) {
        let definition = Definition { bot: bot(name), chat: ChatId(format!("chat-{name}")), sources, budget, restart: Restart::Permanent };
        assert!(s.step(Event::Define(definition), Now(now)).is_empty());
    }
    fn topic(kind: &str, name: &str) -> Topic {
        Topic::new(kind, name)
    }
    fn room(name: &str) -> Source {
        Source::Topic(topic("room", name))
    }
    fn message(from: Option<&str>, cursor: u64) -> Event<Msg> {
        Event::Info { topic: topic("room", "general"), cursor, from: from.map(|name| ChatId(format!("chat-{name}"))), info: Msg::Post }
    }
    fn posted(through: u64) -> R {
        Reason::Info { topic: topic("room", "general"), through, info: Msg::Post }
    }
    fn turns(planned: &[Planned<BotId, Effect<Msg>>]) -> Vec<(&str, TurnId, &[R])> {
        planned.iter().filter_map(|p| match &p.effect {
            Effect::RequestTurn { bot, turn, reasons, .. } => Some((bot.0.as_str(), *turn, reasons.as_slice())),
            _ => None,
        }).collect()
    }
    fn done(name: &str, turn: TurnId) -> Event<Msg> {
        Event::TurnCompleted { bot: bot(name), turn }
    }

    #[test]
    fn messages_request_turns_coalesce_and_ignore_own_posts_and_replays() {
        let mut s = S::default();
        define(&mut s, "alice", vec![room("general")], budget(0, 60_000, 10), 0);
        define(&mut s, "cass", vec![room("general")], budget(0, 60_000, 10), 0);
        let first = s.step(message(Some("alice"), 1), Now(10));
        let [(name, cass_turn, reasons)] = turns(&first)[..] else { panic!("one turn") };
        assert_eq!((name, reasons), ("cass", &[posted(1)][..]));
        assert_eq!(first[0].durability, Durability::Intent);
        assert!(matches!(&first[0].effect, Effect::RequestTurn { chat, .. } if chat.0 == "chat-cass"));
        assert_eq!(s.status(&bot("cass")), Some(Status::WaitingTurn));
        let alice = turns(&s.step(message(None, 2), Now(20)))[0].1;
        assert!(s.step(message(None, 3), Now(30)).is_empty(), "both bots are mid-turn");
        assert!(s.step(message(None, 3), Now(31)).is_empty(), "replayed cursor");
        assert!(s.step(message(None, 4), Now(40)).is_empty());
        let resumed = s.step(done("cass", cass_turn), Now(50));
        assert_eq!(turns(&resumed)[0].2, [posted(4)]);
        assert!(s.step(message(None, 2), Now(60)).is_empty(), "an older cursor is a replay");
        assert!(s.eligible(&bot("alice"), &alice));
        assert_eq!(s.outstanding().count(), 2);
    }
    #[test]
    fn active_turns_are_not_restarted_by_callbacks_or_clock_ticks() {
        let mut s = S::default();
        define(&mut s,"agent",vec![room("general")],budget(0,3_600_000,60),0);
        let active=turns(&s.step(message(None,1),Now(1)))[0].1;
        for cursor in 2..100 {
            assert!(turns(&s.step(message(None,cursor),Now(cursor*10_000))).is_empty());
            assert!(turns(&s.step(Event::Tick,Now(cursor*10_000))).is_empty());
            assert!(s.eligible(&bot("agent"),&active));
        }
        s.step(Event::Processed{bot:bot("agent"),topic:topic("room","general"),through:99},Now(1_000_000));
        s.step(Event::Continue(bot("agent")),Now(1_000_001));
        let successor=s.step(done("agent",active),Now(1_000_002));
        assert_eq!(turns(&successor).len(),1);
        assert_eq!(turns(&successor)[0].2,[Reason::Continue],"processed callbacks do not create another wake");
    }
    #[test]
    fn idle_waits_coalesce_noise_and_resume_on_relevant_events_or_recheck() {
        let mut s = S::default();
        define(&mut s,"agent",vec![room("general"),room("work")],budget(0,60_000,10),0);
        let active=turns(&s.step(message(None,1),Now(1)))[0].1;
        s.step(Event::Wait{bot:bot("agent"),topics:vec![topic("room","work")],until:Some(10_000)},Now(2));
        assert!(s.step(done("agent",active),Now(3)).is_empty());
        for cursor in 2..50 {
            assert!(s.step(message(None,cursor),Now(cursor*100)).is_empty(),"irrelevant telemetry remains pending without model churn");
        }
        assert_eq!(s.next_deadline(Now(5000)),Some(Now(10_000)));
        let relevant=s.step(Event::Info{topic:topic("room","work"),cursor:1,from:None,info:Msg::Post},Now(5001));
        let next=turns(&relevant)[0].1;
        assert_eq!(turns(&relevant).len(),1);
        s.step(Event::Wait{bot:bot("agent"),topics:vec![],until:Some(6000)},Now(5002));
        s.step(done("agent",next),Now(5003));
        assert!(s.step(Event::Tick,Now(5999)).is_empty());
        let recheck=s.step(Event::Tick,Now(6000));
        assert_eq!(turns(&recheck)[0].2,[Reason::Timer]);
    }
    #[test]
    fn idle_waits_suspend_periodic_checks_but_preserve_deadlines_failures_and_pause() {
        for urgent in [false,true] {
            let mut s = S::default();
            define(&mut s,"agent",vec![room("general"),Source::Timer{every_ms:100}],budget(0,60_000,10),0);
            let active=turns(&s.step(message(None,1),Now(1)))[0].1;
            s.step(Event::Wait{bot:bot("agent"),topics:vec![],until:Some(1000)},Now(2));
            s.step(done("agent",active),Now(3));
            let wake = if urgent {
                s.step(Event::Info{topic:topic("room","general"),cursor:2,from:None,info:Msg::Exit(Some(1))},Now(4))
            } else {
                assert_eq!(s.next_deadline(Now(4)),Some(Now(1000)));
                assert!(s.step(Event::Tick,Now(100)).is_empty());
                s.step(Event::Tick,Now(1000))
            };
            let active=turns(&wake)[0].1;
            s.step(Event::Wait{bot:bot("agent"),topics:vec![],until:Some(2000)},Now(1001));
            s.step(Event::Pause(bot("agent")),Now(1002));
            s.step(done("agent",active),Now(1003));
            assert!(s.step(Event::Tick,Now(3000)).is_empty());
            assert_eq!(s.status(&bot("agent")),Some(Status::Paused));
            assert_eq!(turns(&s.step(Event::Resume(bot("agent")),Now(3001))).len(),1);
        }
    }
    #[test]
    fn event_only_wait_sleeps_for_days_and_retains_new_events_without_preemption() {
        let mut s = S::default();
        define(&mut s,"agent",vec![room("general"),room("dependency"),Source::Timer{every_ms:100}],budget(0,60_000,10),0);
        let active=turns(&s.step(message(None,1),Now(1)))[0].1;
        // A periodic check accumulated while work was active must not defeat wait.
        assert!(s.step(Event::Tick,Now(100)).is_empty());
        s.step(Event::Wait{bot:bot("agent"),topics:vec![topic("room","dependency")],until:None},Now(101));
        assert!(s.step(done("agent",active),Now(102)).is_empty());
        #[cfg(feature = "serde")]
        let mut s: S = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        for now in [300_000,86_400_000,604_800_000] {
            assert!(s.step(Event::Tick,Now(now)).is_empty());
            assert_eq!(s.next_deadline(Now(now)),None);
        }
        let event=|cursor| Event::Info{topic:topic("room","dependency"),cursor,from:None,info:Msg::Post};
        let wake=s.step(event(1),Now(604_800_001));
        assert_eq!(turns(&wake).len(),1);
        let active=turns(&wake)[0].1;
        assert!(s.step(event(2),Now(604_800_002)).is_empty());
        assert!(s.eligible(&bot("agent"),&active));
        s.step(Event::Processed{bot:bot("agent"),topic:topic("room","dependency"),through:1},Now(604_800_003));
        s.step(Event::Wait{bot:bot("agent"),topics:vec![topic("room","dependency")],until:None},Now(604_800_004));
        assert_eq!(turns(&s.step(done("agent",active),Now(604_800_005))).len(),1,"newer unprocessed callback survives");
        s.step(Event::Stop(bot("agent")),Now(604_800_006));
        assert!(s.step(Event::Tick,Now(700_000_000)).is_empty());
    }
    #[test]
    fn runnable_continuations_coalesce_obey_budget_and_do_not_resurrect_temporary_bots() {
        let mut s=S::default();
        define(&mut s,"agent",vec![room("general")],budget(0,60_000,2),0);
        let first=turns(&s.step(message(None,1),Now(1)))[0].1;
        assert!(s.step(Event::Continue(bot("agent")),Now(2)).is_empty());
        assert!(s.step(Event::Continue(bot("agent")),Now(3)).is_empty());
        s.step(message(None,2),Now(4));
        let next=s.step(done("agent",first),Now(5));
        assert_eq!(turns(&next).len(),1);
        assert_eq!(turns(&next)[0].2,[Reason::Continue,posted(2)]);
        let second=turns(&next)[0].1;
        s.step(Event::Continue(bot("agent")),Now(6));
        assert!(turns(&s.step(done("agent",second),Now(7))).is_empty(),"continuations cannot bypass the two-turn budget");
        assert_eq!(turns(&s.step(Event::Tick,Now(60_001))).len(),1);
        s.step(Event::Stop(bot("agent")),Now(60_002));
        assert!(turns(&s.step(Event::Continue(bot("agent")),Now(60_003))).is_empty());

        let mut once=S::default();
        once.step(Event::Define(Definition{bot:bot("once"),chat:ChatId("once-chat".into()),sources:vec![room("general")],budget:budget(0,60_000,10),restart:Restart::Temporary}),Now(0));
        let first=turns(&once.step(message(None,1),Now(1)))[0].1;
        once.step(Event::Continue(bot("once")),Now(2));
        assert!(once.step(done("once",first),Now(3)).is_empty());
        assert_eq!(once.status(&bot("once")),None,"temporary callbacks still need explicit rearm");
    }
    #[test]
    fn published_processing_can_precede_supervisor_ingestion_without_replaying_it() {
        let mut s=S::default();
        define(&mut s,"agent",vec![room("general")],budget(0,60_000,10),0);
        s.step(Event::Processed{bot:bot("agent"),topic:topic("room","general"),through:8},Now(1));
        for cursor in 1..=8 { assert!(s.step(message(None,cursor),Now(cursor+1)).is_empty()); }
        assert_eq!(turns(&s.step(message(None,9),Now(10)))[0].2,[posted(9)]);
    }
    #[test]
    fn committed_processing_retires_only_matching_pending_watermarks() {
        for (newest,expect_next) in [(2,false),(3,true)] {
            let mut s=S::default();
            define(&mut s,"agent",vec![room("general")],budget(0,60_000,10),0);
            let first=turns(&s.step(message(None,1),Now(1)))[0].1;
            s.step(message(None,2),Now(2));
            if newest==3 {s.step(message(None,3),Now(3));}
            s.step(Event::Processed{bot:bot("agent"),topic:topic("room","general"),through:2},Now(4));
            let next=s.step(done("agent",first),Now(5));
            assert_eq!(!turns(&next).is_empty(),expect_next,"newer events survive a racing acknowledgement");
            if expect_next {assert_eq!(turns(&next)[0].2,[posted(3)]);}
        }
    }

    #[test]
    fn temporary_bots_end_after_their_first_turn_and_redefinition_can_make_them_permanent() {
        let mut s = S::default();
        let once = Definition { bot: bot("once"), chat: ChatId("chat-once".into()), sources: vec![room("general")], budget: budget(0, 60_000, 1), restart: Restart::Temporary };
        assert!(s.step(Event::Define(once.clone()), Now(0)).is_empty());
        let turn = turns(&s.step(message(None, 1), Now(1)))[0].1;
        assert!(s.step(message(None, 2), Now(2)).is_empty());
        assert!(s.step(done("once", turn), Now(3)).is_empty(), "pending messages after the wake are not a second turn");
        assert_eq!(s.status(&bot("once")), None);
        s.step(Event::Define(once.clone()), Now(4));
        s.step(Event::Define(Definition { restart: Restart::Permanent, ..once }), Now(5));
        let turn = turns(&s.step(message(None, 3), Now(6)))[0].1;
        s.step(done("once", turn), Now(7));
        assert_eq!(s.status(&bot("once")), Some(Status::Ready));
    }

    #[test]
    fn stale_or_duplicate_completions_never_release_a_newer_turn() {
        let mut s = S::default();
        define(&mut s, "bot", vec![room("general")], budget(0, 60_000, 10), 0);
        let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
        assert!(s.step(message(None, 2), Now(2)).is_empty());
        let second = turns(&s.step(done("bot", first), Now(3)))[0].1;
        assert_ne!(first, second);
        assert!(s.step(message(None, 3), Now(4)).is_empty());
        assert!(s.step(done("bot", first), Now(5)).is_empty(), "a duplicate completion is stale");
        assert_eq!(s.status(&bot("bot")), Some(Status::WaitingTurn));
        assert!(!s.eligible(&bot("bot"), &first) && s.eligible(&bot("bot"), &second));
        assert_eq!(turns(&s.step(done("bot", second), Now(6))).len(), 1);
    }

    #[test]
    fn explicit_rearm_keeps_exactly_one_next_callback_and_stop_still_wins() {
        let mut s = S::default();
        let once = Definition { bot: bot("once"), chat: ChatId("chat-once".into()), sources: vec![room("general")], budget: budget(0, 60_000, 1), restart: Restart::Temporary };
        s.step(Event::Define(once.clone()), Now(0));
        let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
        assert!(s.retains(&Topic::new("room", "general")));
        s.step(Event::Rearm(bot("once")), Now(2));
        s.step(Event::Rearm(bot("once")), Now(2));
        assert!(s.step(message(None, 2), Now(3)).is_empty());
        let effects = s.step(done("once", first), Now(4));
        let next = turns(&effects);
        assert_eq!(next.len(), 1, "an explicit request survives the previous callback");
        assert!(s.step(done("once", next[0].1), Now(5)).is_empty());
        assert_eq!(s.status(&bot("once")), None);
        assert!(!s.retains(&Topic::new("room", "general")));

        s.step(Event::Define(once), Now(6));
        let turn = turns(&s.step(message(None,3), Now(7)))[0].1;
        s.step(Event::Rearm(bot("once")), Now(8));
        s.step(Event::Stop(bot("once")), Now(9));
        s.step(done("once",turn), Now(10));
        assert_eq!(s.status(&bot("once")), None);
    }

    #[test]
    fn a_paused_temporary_callback_keeps_its_mailbox_until_explicit_resume() {
        let mut s=S::default();
        s.step(Event::Define(Definition {bot:bot("once"),chat:ChatId("chat-once".into()),sources:vec![room("general")],budget:budget(0,60_000,1),restart:Restart::Temporary}),Now(0));
        let first=turns(&s.step(message(None,1),Now(1)))[0].1;
        s.step(Event::Pause(bot("once")),Now(2));
        s.step(message(None,2),Now(3));
        assert!(s.step(done("once",first),Now(4)).is_empty());
        assert_eq!(s.status(&bot("once")),Some(Status::Paused));
        assert_eq!(turns(&s.step(Event::Resume(bot("once")),Now(5))).len(),1);
    }

    #[test]
    fn budgets_bound_turns_survive_redefinition_and_report_exhaustion_once() {
        let mut s = S::default();
        define(&mut s, "bot", vec![room("general")], budget(1_000, 10_000, 2), 0);
        let t1 = turns(&s.step(message(None, 1), Now(0)))[0].1;
        s.step(done("bot", t1), Now(100));
        assert!(s.step(message(None, 2), Now(200)).is_empty(), "cooldown");
        assert_eq!(s.next_deadline(Now(200)), Some(Now(1_000)));
        let t2 = turns(&s.step(Event::Tick, Now(1_000)))[0].1;
        s.step(done("bot", t2), Now(1_100));
        define(&mut s, "bot", vec![room("general")], budget(1_000, 10_000, 2), 1_200);
        let exhausted = s.step(message(None, 3), Now(2_000));
        assert!(matches!(&exhausted[..], [Planned { effect: Effect::BudgetExhausted { until: 10_000, .. }, durability: Durability::None, .. }]), "redefinition keeps budget accounting");
        assert!(s.step(message(None, 4), Now(3_000)).is_empty(), "exhaustion is reported once per window");
        assert_eq!(s.next_deadline(Now(3_000)), Some(Now(10_000)));
        assert_eq!(turns(&s.step(Event::Tick, Now(10_000)))[0].2, [posted(4)]);
    }

    #[test]
    fn timers_wake_idle_bots_and_set_the_next_deadline() {
        let mut s = S::default();
        define(&mut s, "scout", vec![Source::Timer { every_ms: 5_000 }], budget(0, 60_000, 100), 1_000);
        assert_eq!(s.next_deadline(Now(1_000)), Some(Now(6_000)));
        assert!(s.step(Event::Tick, Now(5_999)).is_empty());
        let turn = turns(&s.step(Event::Tick, Now(6_000)))[0].1;
        assert!(s.step(Event::Tick, Now(11_000)).is_empty(), "a running turn defers the timer");
        assert_eq!(turns(&s.step(done("scout", turn), Now(11_500)))[0].2, [Reason::Timer]);
        assert_eq!(s.next_deadline(Now(11_500)), Some(Now(16_000)));
    }

    #[test]
    fn deadlines_beyond_the_clock_never_fire_or_loop() {
        let mut s = S::default();
        define(&mut s, "late", vec![Source::Timer { every_ms: 10 }], budget(0, 60_000, 10), u64::MAX - 5);
        assert_eq!(s.next_deadline(Now(u64::MAX - 5)), None, "the first timer is unrepresentable");
        assert!(s.step(Event::Tick, Now(u64::MAX)).is_empty());
        define(&mut s, "edge", vec![room("general")], budget(10, u64::MAX, 1), 0);
        let turn = turns(&s.step(message(None, 1), Now(u64::MAX - 1)))[0].1;
        s.step(done("edge", turn), Now(u64::MAX));
        let blocked = s.step(message(None, 2), Now(u64::MAX));
        assert!(matches!(&blocked[..], [Planned { effect: Effect::BudgetExhausted { until: u64::MAX, .. }, .. }]));
        assert!(s.step(Event::Tick, Now(u64::MAX)).is_empty());
        assert_eq!(s.next_deadline(Now(u64::MAX)), None);
    }

    #[test]
    fn messages_reach_only_subscribers_and_overflow_names_topics_to_resync() {
        let mut s = S::default();
        let service = |name: &str| topic("service", name);
        let observed = |name: &str, cursor: u64, kind: &str| Event::Info { topic: service(name), cursor, from: None, info: Msg::Kind(kind.into()) };
        define(&mut s, "alice", vec![Source::Topic(service("svc-alice"))], budget(0, 60_000, 10), 0);
        let turn = turns(&s.step(observed("svc-alice", 1, "started"), Now(0)))[0].1;
        assert!(s.step(observed("svc-cass", 2, "died"), Now(1)).is_empty());
        for cursor in 2..42 {
            assert!(s.step(observed("svc-alice", cursor, &format!("event-{}", cursor % 20)), Now(2)).is_empty());
        }
        assert!(s.step(observed("svc-alice", 41, "replayed"), Now(3)).is_empty());
        let woke = s.step(done("alice", turn), Now(4));
        let (second, reasons) = (turns(&woke)[0].1, turns(&woke)[0].2.to_vec());
        assert_eq!(reasons.len(), PENDING + 1, "the first sixteen distinct reasons plus an overflow marker");
        assert_eq!(reasons.last(), Some(&Reason::Overflow { dropped: 8, topics: vec![service("svc-alice")] }));
        assert!(reasons[..PENDING].iter().all(|r| matches!(r, Reason::Info { topic, .. } if topic.name == "svc-alice")));
        assert!(!reasons.iter().any(|r| matches!(r, Reason::Info { info: Msg::Kind(kind), .. } if kind == "replayed")));
        assert!(s.step(observed("svc-alice", 18, "event-18"), Now(5)).is_empty(), "dropped messages are acknowledged, not replayed");
        assert!(s.step(done("alice", second), Now(6)).is_empty());
    }

    #[test]
    fn application_merge_rules_decide_coalescing_per_topic() {
        let mut s = S::default();
        let watch = |name: &str| topic("watch", name);
        let info = |name: &str, cursor: u64, info: Msg| Event::Info { topic: watch(name), cursor, from: None, info };
        define(&mut s, "alice", vec![Source::Topic(watch("w-alice"))], budget(0, 60_000, 10), 0);
        assert!(s.step(info("w-cass", 5, Msg::Lines(3)), Now(1)).is_empty(), "other topics are not this bot's messages");
        let woke = s.step(info("w-alice", 10, Msg::Lines(2)), Now(2));
        let (_, turn, reasons) = turns(&woke)[0];
        assert_eq!(reasons, [Reason::Info { topic: watch("w-alice"), through: 10, info: Msg::Lines(2) }]);
        assert!(s.step(info("w-alice", 15, Msg::Lines(4)), Now(3)).is_empty());
        assert!(s.step(info("w-alice", 15, Msg::Lines(4)), Now(3)).is_empty(), "replays at or below the cursor are ignored");
        assert!(s.step(info("w-alice", 20, Msg::Lines(1)), Now(4)).is_empty());
        assert!(s.step(info("w-alice", 21, Msg::Exit(Some(137))), Now(5)).is_empty());
        assert!(s.step(info("w-alice", 22, Msg::Exit(Some(0))), Now(5)).is_empty());
        let next = s.step(done("alice", turn), Now(6));
        let (_, _, reasons) = turns(&next)[0];
        assert_eq!(reasons, [
            Reason::Info { topic: watch("w-alice"), through: 20, info: Msg::Lines(5) },
            Reason::Info { topic: watch("w-alice"), through: 22, info: Msg::Exit(Some(0)) },
        ], "lines add up; exits fold separately with the latest code");
    }

    #[test]
    fn topics_with_colons_keep_separate_cursors() {
        let mut s = S::default();
        let (left, right) = (topic("a:b", "c"), topic("a", "b:c"));
        define(&mut s, "bot", vec![Source::Topic(left.clone()), Source::Topic(right.clone())], budget(0, 60_000, 10), 0);
        let turn = turns(&s.step(Event::Info { topic: left, cursor: 5, from: None, info: Msg::Post }, Now(1)))[0].1;
        assert!(s.step(Event::Info { topic: right.clone(), cursor: 3, from: None, info: Msg::Post }, Now(2)).is_empty());
        let next = s.step(done("bot", turn), Now(3));
        assert_eq!(turns(&next)[0].2, [Reason::Info { topic: right, through: 3, info: Msg::Post }]);
    }

    #[test]
    fn pause_stop_and_validation_follow_the_command_contract() {
        let mut s = S::default();
        for definition in [
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![], budget: budget(0, 1, 1), restart: Restart::Permanent },
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![room("general")], budget: budget(0, 0, 1), restart: Restart::Permanent },
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![Source::Timer { every_ms: 0 }], budget: budget(0, 1, 1), restart: Restart::Permanent },
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![room("general")], budget: budget(0, 1, 257), restart: Restart::Permanent },
        ] {
            assert!(definition.validate().is_err());
            assert!(s.step(Event::Define(definition), Now(0)).is_empty());
        }
        assert_eq!(s.definitions().count(), 0);
        define(&mut s, "bot", vec![room("general")], budget(0, 60_000, 10), 0);
        assert!(s.step(Event::Pause(bot("bot")), Now(1)).is_empty());
        assert!(s.step(message(None, 1), Now(2)).is_empty());
        assert_eq!(s.status(&bot("bot")), Some(Status::Paused));
        let turn = turns(&s.step(Event::Resume(bot("bot")), Now(3)))[0].1;
        assert!(s.step(Event::Stop(bot("bot")), Now(4)).is_empty());
        assert_eq!(s.status(&bot("bot")), Some(Status::Stopping), "stop waits for the running turn");
        assert!(!s.eligible(&bot("bot"), &turn), "a stopping bot's turn is not re-queued");
        define(&mut s, "bot", vec![room("general")], budget(0, 60_000, 10), 5);
        assert!(s.step(message(None, 2), Now(6)).is_empty(), "a stopping bot admits nothing");
        assert!(s.step(done("bot", turn), Now(7)).is_empty());
        assert_eq!(s.status(&bot("bot")), None);
        assert_eq!(s.next_deadline(Now(8)), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn restored_state_round_trips_and_definitions_outside_the_contract_never_produce_turns() {
        let mut s = S::default();
        define(&mut s, "bot", vec![room("general")], budget(0, 60_000, 10), 0);
        let turn = turns(&s.step(message(None, 1), Now(1)))[0].1;
        for i in 0..20 {
            s.step(Event::Info { topic: topic("watch", &format!("w{i}")), cursor: 1, from: None, info: Msg::Exit(None) }, Now(2));
        }
        s.step(message(None, 2), Now(2));
        let mut restored: S = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(turns(&restored.step(done("bot", turn), Now(3)))[0].2, [posted(2)], "only subscribed topics were admitted");
        let mut snapshot = serde_json::to_value(&restored).unwrap();
        snapshot["bots"]["bot"]["definition"]["budget"]["turns_per_window"] = 1000.into();
        let mut invalid: S = serde_json::from_value(snapshot).unwrap();
        assert!(turns(&invalid.step(message(None, 9), Now(9))).is_empty(), "an invalid restored budget fails closed");
        assert_eq!(invalid.next_deadline(Now(9)), None);
    }

    #[test]
    fn outstanding_turns_keep_their_destination_stops_clear_timers_and_cursor_zero_counts() {
        let mut s = S::default();
        define(&mut s, "bot", vec![room("general"), Source::Timer { every_ms: 100 }], budget(0, 60_000, 10), 0);
        let turn = turns(&s.step(message(None, 0), Now(1)))[0].1;
        let moved = Definition { bot: bot("bot"), chat: ChatId("chat-moved".into()), sources: vec![room("general"), Source::Timer { every_ms: 100 }], budget: budget(0, 60_000, 10), restart: Restart::Permanent };
        assert!(s.step(Event::Define(moved), Now(2)).is_empty());
        assert_eq!(s.outstanding().map(|(_, chat, t)| (chat.0.as_str(), *t)).collect::<Vec<_>>(), [("chat-bot", turn)], "recovery uses the chat the turn was requested for");
        assert!(s.step(Event::Stop(bot("bot")), Now(3)).is_empty());
        assert_eq!(s.next_deadline(Now(500)), None, "a stopping bot leaves no timer deadline");
        assert!(s.step(Event::Tick, Now(500)).is_empty());
        assert!(s.step(done("bot", turn), Now(501)).is_empty());
        assert_eq!(s.status(&bot("bot")), None);
    }
}
