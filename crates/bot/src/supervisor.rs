use control_effect::machine::{Durability, Machine, Now, Planned, Stamp};
use std::collections::{BTreeMap, VecDeque};

/// Coalesced reasons a sleeping bot keeps before its next turn.
const PENDING: usize = 16;

macro_rules! id {
    ($name:ident, $inner:ty) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(transparent))]
        pub struct $name(pub $inner);
    };
}
id!(BotId, String);
id!(ChatId, String);
id!(ServiceId, String);
id!(TurnId, u64);
impl Copy for TurnId {}

/// What a bot listens to: room messages for coordination with other agents,
/// events observed on services it manages, and timers while idle.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Source {
    Room { room: String },
    Service { service: ServiceId },
    Timer { every_ms: u64 },
}

/// Bounds automatic turns: a minimum gap between turns and a cap per window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Budget {
    pub cooldown_ms: u64,
    pub window_ms: u64,
    pub turns_per_window: u32,
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
}
impl Definition {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sources.is_empty() {
            return Err("A bot needs at least one event source");
        }
        if self.budget.window_ms == 0 || self.budget.turns_per_window == 0 {
            return Err("A budget needs a positive window and turn count");
        }
        if self.sources.iter().any(|source| matches!(source, Source::Timer { every_ms: 0 })) {
            return Err("Timers need a positive interval");
        }
        Ok(())
    }
    fn timer(&self) -> Option<u64> {
        self.sources.iter().filter_map(|source| match source {
            Source::Timer { every_ms } => Some(*every_ms),
            _ => None,
        }).min()
    }
}

/// Admitted inputs. Commands come only from authorized routes; a room message
/// saying "stop" is data. Cursors are per source and increase; replays at or
/// below a bot's cursor are ignored. Every `RequestTurn` is answered by
/// `TurnCompleted` with its turn ID, including when the turn could not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Define(Definition),
    Pause(BotId),
    Resume(BotId),
    Stop(BotId),
    RoomMessage { room: String, from: Option<ChatId>, cursor: u64 },
    ResourceObserved { service: ServiceId, cursor: u64, kind: String },
    TurnCompleted { bot: BotId, turn: TurnId },
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Reason {
    Room { room: String, through: u64 },
    Service { service: ServiceId, kind: String, through: u64 },
    Timer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Queue one turn in the bot's conversation. The turn ID is the idempotency
    /// key: recovery re-queues only an outstanding turn, never a new one.
    RequestTurn { bot: BotId, chat: ChatId, turn: TurnId, reasons: Vec<Reason> },
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct Bot {
    definition: Definition,
    generation: u64,
    pending: Vec<Reason>,
    turn: Option<(TurnId, ChatId)>,
    last_turn: Option<u64>,
    turns: VecDeque<u64>,
    timer_due: Option<u64>,
    cursors: BTreeMap<String, u64>,
    exhausted: bool,
    paused: bool,
    stopping: bool,
}
impl Bot {
    /// Advances the source cursor; false for a replayed or older event.
    fn admit(&mut self, key: String, cursor: u64) -> bool {
        if self.cursors.get(&key).is_some_and(|seen| cursor <= *seen) {
            return false;
        }
        self.cursors.insert(key, cursor);
        true
    }
    fn add(&mut self, reason: Reason) {
        let merged = match &reason {
            Reason::Room { room, through } => self.pending.iter_mut().find_map(|r| match r {
                Reason::Room { room: other, through: existing } if other == room => Some(existing),
                _ => None,
            }).map(|existing| *existing = (*existing).max(*through)),
            Reason::Service { service, kind, through } => self.pending.iter_mut().find_map(|r| match r {
                Reason::Service { service: s, kind: k, through: existing } if s == service && k == kind => Some(existing),
                _ => None,
            }).map(|existing| *existing = (*existing).max(*through)),
            Reason::Timer => self.pending.contains(&Reason::Timer).then_some(()),
        };
        if merged.is_some() {
            return;
        }
        if self.pending.len() == PENDING {
            self.pending.remove(0);
        }
        self.pending.push(reason);
    }
    fn recent(&self, now: u64) -> impl Iterator<Item = u64> + '_ {
        let window = self.definition.budget.window_ms;
        self.turns.iter().copied().filter(move |at| now.saturating_sub(*at) < window)
    }
    /// When pending reasons could next become a turn, if blocked only by time.
    /// `None` also covers deadlines beyond the representable range.
    fn ready_at(&self, now: u64) -> Option<u64> {
        if self.pending.is_empty() || self.turn.is_some() || self.paused || self.stopping {
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

/// Persistable supervisor for every bot on a node. Deterministic for a given
/// sequence of events and times; per-bot transitions are serialized.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Supervisor {
    bots: BTreeMap<BotId, Bot>,
    generation: u64,
    revision: u64,
    next_turn: u64,
}
impl Supervisor {
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
    /// Turns requested but not yet completed, for reconciliation after restart.
    pub fn outstanding(&self) -> impl Iterator<Item = (&BotId, &ChatId, &TurnId)> {
        self.bots.iter().filter_map(|(id, bot)| bot.turn.as_ref().map(|(turn, chat)| (id, chat, turn)))
    }
    /// The earliest time a `Tick` could produce an effect; drivers wait until
    /// then or the next event instead of polling.
    pub fn next_deadline(&self, now: Now) -> Option<Now> {
        self.bots.values().flat_map(|bot| [bot.timer_due, bot.ready_at(now.0)]).flatten().min().map(Now)
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
                    exhausted: false,
                    paused: false,
                    stopping: false,
                };
                self.bots.insert(bot.definition.bot.clone(), bot);
            }
        }
    }
    fn deliver(&mut self, event: Event, now: u64) {
        match event {
            Event::Define(definition) => self.define(definition, now),
            Event::Pause(bot) => {
                if let Some(bot) = self.bots.get_mut(&bot) {
                    bot.paused = true;
                }
            }
            Event::Resume(bot) => {
                if let Some(bot) = self.bots.get_mut(&bot) {
                    bot.paused = false;
                }
            }
            Event::Stop(id) => {
                if let Some(bot) = self.bots.get_mut(&id) {
                    bot.stopping = true;
                    bot.pending.clear();
                    bot.timer_due = None;
                    if bot.turn.is_none() {
                        self.bots.remove(&id);
                    }
                }
            }
            Event::RoomMessage { room, from, cursor } => {
                for bot in self.bots.values_mut().filter(|bot| !bot.stopping) {
                    let listens = bot.definition.sources.iter().any(|source| matches!(source, Source::Room { room: r } if *r == room));
                    if listens && from.as_ref() != Some(&bot.definition.chat) && bot.admit(format!("room:{room}"), cursor) {
                        bot.add(Reason::Room { room: room.clone(), through: cursor });
                    }
                }
            }
            Event::ResourceObserved { service, cursor, kind } => {
                for bot in self.bots.values_mut().filter(|bot| !bot.stopping) {
                    let manages = bot.definition.sources.iter().any(|source| matches!(source, Source::Service { service: s } if *s == service));
                    if manages && bot.admit(format!("service:{}", service.0), cursor) {
                        bot.add(Reason::Service { service: service.clone(), kind: kind.clone(), through: cursor });
                    }
                }
            }
            Event::TurnCompleted { bot: id, turn } => {
                if let Some(bot) = self.bots.get_mut(&id)
                    && bot.turn.as_ref().is_some_and(|(id, _)| *id == turn)
                {
                    bot.turn = None;
                    if bot.stopping {
                        self.bots.remove(&id);
                    }
                }
            }
            Event::Tick => {}
        }
        for bot in self.bots.values_mut().filter(|bot| !bot.stopping) {
            if let (Some(due), Some(every)) = (bot.timer_due, bot.definition.timer())
                && due <= now
            {
                bot.add(Reason::Timer);
                bot.timer_due = now.checked_add(every);
            }
        }
    }
    fn drain(&mut self, now: u64) -> Vec<Planned<BotId, Effect>> {
        let mut planned = Vec::new();
        for (id, bot) in &mut self.bots {
            let Some(ready) = bot.ready_at(now) else {
                if !bot.pending.is_empty() && bot.turn.is_none() && !bot.paused && !bot.stopping && !bot.exhausted {
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
            let window = bot.definition.budget.window_ms;
            bot.turns.retain(|at| now.saturating_sub(*at) < window);
            let effect = if ready <= now {
                self.next_turn += 1;
                let turn = TurnId(self.next_turn);
                bot.turn = Some((turn, bot.definition.chat.clone()));
                bot.last_turn = Some(now);
                bot.turns.push_back(now);
                bot.exhausted = false;
                Effect::RequestTurn { bot: id.clone(), chat: bot.definition.chat.clone(), turn, reasons: std::mem::take(&mut bot.pending) }
            } else if bot.turns.len() >= bot.definition.budget.turns_per_window as usize && !bot.exhausted {
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
impl Machine for Supervisor {
    type Resource = BotId;
    type Event = Event;
    type Effect = Effect;
    fn step(&mut self, event: Event, now: Now) -> Vec<Planned<BotId, Effect>> {
        self.deliver(event, now.0);
        self.drain(now.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot(name: &str) -> BotId {
        BotId(name.into())
    }
    fn budget(cooldown_ms: u64, window_ms: u64, turns_per_window: u32) -> Budget {
        Budget { cooldown_ms, window_ms, turns_per_window }
    }
    fn define(s: &mut Supervisor, name: &str, sources: Vec<Source>, budget: Budget, now: u64) {
        let definition = Definition { bot: bot(name), chat: ChatId(format!("chat-{name}")), sources, budget };
        assert!(s.step(Event::Define(definition), Now(now)).is_empty());
    }
    fn room(name: &str) -> Source {
        Source::Room { room: name.into() }
    }
    fn message(from: Option<&str>, cursor: u64) -> Event {
        Event::RoomMessage { room: "general".into(), from: from.map(|name| ChatId(format!("chat-{name}"))), cursor }
    }
    fn turns(planned: &[Planned<BotId, Effect>]) -> Vec<(&str, TurnId, &[Reason])> {
        planned.iter().filter_map(|p| match &p.effect {
            Effect::RequestTurn { bot, turn, reasons, .. } => Some((bot.0.as_str(), *turn, reasons.as_slice())),
            _ => None,
        }).collect()
    }
    fn done(name: &str, turn: TurnId) -> Event {
        Event::TurnCompleted { bot: bot(name), turn }
    }

    #[test]
    fn room_messages_request_turns_coalesce_and_ignore_own_posts_and_replays() {
        let mut s = Supervisor::default();
        define(&mut s, "alice", vec![room("general")], budget(0, 60_000, 10), 0);
        define(&mut s, "cass", vec![room("general")], budget(0, 60_000, 10), 0);
        let first = s.step(message(Some("alice"), 1), Now(10));
        let [(name, cass_turn, reasons)] = turns(&first)[..] else { panic!("one turn") };
        assert_eq!((name, reasons), ("cass", &[Reason::Room { room: "general".into(), through: 1 }][..]));
        assert_eq!(first[0].durability, Durability::Intent);
        assert!(matches!(&first[0].effect, Effect::RequestTurn { chat, .. } if chat.0 == "chat-cass"));
        assert_eq!(s.status(&bot("cass")), Some(Status::WaitingTurn));
        let alice = turns(&s.step(message(None, 2), Now(20)))[0].1;
        assert!(s.step(message(None, 3), Now(30)).is_empty(), "both bots are mid-turn");
        assert!(s.step(message(None, 3), Now(31)).is_empty(), "replayed cursor");
        assert!(s.step(message(None, 4), Now(40)).is_empty());
        let resumed = s.step(done("cass", cass_turn), Now(50));
        assert_eq!(turns(&resumed)[0].2, [Reason::Room { room: "general".into(), through: 4 }]);
        assert!(s.step(message(None, 2), Now(60)).is_empty(), "an older cursor is a replay");
        assert!(s.eligible(&bot("alice"), &alice));
        assert_eq!(s.outstanding().count(), 2);
    }

    #[test]
    fn stale_or_duplicate_completions_never_release_a_newer_turn() {
        let mut s = Supervisor::default();
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
    fn budgets_bound_turns_survive_redefinition_and_report_exhaustion_once() {
        let mut s = Supervisor::default();
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
        assert_eq!(turns(&s.step(Event::Tick, Now(10_000)))[0].2, [Reason::Room { room: "general".into(), through: 4 }]);
    }

    #[test]
    fn timers_wake_idle_bots_and_set_the_next_deadline() {
        let mut s = Supervisor::default();
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
        let mut s = Supervisor::default();
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
    fn service_events_reach_only_bots_managing_that_service_and_stay_bounded() {
        let mut s = Supervisor::default();
        let manages = |service: &str| Source::Service { service: ServiceId(service.into()) };
        let observed = |service: &str, cursor: u64, kind: &str| Event::ResourceObserved { service: ServiceId(service.into()), cursor, kind: kind.into() };
        define(&mut s, "alice", vec![manages("svc-alice")], budget(0, 60_000, 10), 0);
        let turn = turns(&s.step(observed("svc-alice", 1, "started"), Now(0)))[0].1;
        assert!(s.step(observed("svc-cass", 2, "died"), Now(1)).is_empty());
        for cursor in 2..42 {
            assert!(s.step(observed("svc-alice", cursor, &format!("event-{}", cursor % 20)), Now(2)).is_empty());
        }
        assert!(s.step(observed("svc-alice", 41, "replayed"), Now(3)).is_empty());
        let woke = s.step(done("alice", turn), Now(4));
        let reasons = turns(&woke)[0].2;
        assert_eq!(reasons.len(), PENDING);
        assert!(reasons.iter().all(|r| matches!(r, Reason::Service { service, .. } if service.0 == "svc-alice")));
        assert!(!reasons.iter().any(|r| matches!(r, Reason::Service { kind, .. } if kind == "replayed")));
    }

    #[test]
    fn pause_stop_and_validation_follow_the_command_contract() {
        let mut s = Supervisor::default();
        for definition in [
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![], budget: budget(0, 1, 1) },
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![room("general")], budget: budget(0, 0, 1) },
            Definition { bot: bot("x"), chat: ChatId("c".into()), sources: vec![Source::Timer { every_ms: 0 }], budget: budget(0, 1, 1) },
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

    #[test]
    fn outstanding_turns_keep_their_destination_stops_clear_timers_and_cursor_zero_counts() {
        let mut s = Supervisor::default();
        define(&mut s, "bot", vec![room("general"), Source::Timer { every_ms: 100 }], budget(0, 60_000, 10), 0);
        let turn = turns(&s.step(message(None, 0), Now(1)))[0].1;
        let moved = Definition { bot: bot("bot"), chat: ChatId("chat-moved".into()), sources: vec![room("general"), Source::Timer { every_ms: 100 }], budget: budget(0, 60_000, 10) };
        assert!(s.step(Event::Define(moved), Now(2)).is_empty());
        assert_eq!(s.outstanding().map(|(_, chat, t)| (chat.0.as_str(), *t)).collect::<Vec<_>>(), [("chat-bot", turn)], "recovery uses the chat the turn was requested for");
        assert!(s.step(Event::Stop(bot("bot")), Now(3)).is_empty());
        assert_eq!(s.next_deadline(Now(500)), None, "a stopping bot leaves no timer deadline");
        assert!(s.step(Event::Tick, Now(500)).is_empty());
        assert!(s.step(done("bot", turn), Now(501)).is_empty());
        assert_eq!(s.status(&bot("bot")), None);
    }
}
