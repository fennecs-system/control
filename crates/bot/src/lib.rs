//! Persistent, callback-driven bots: agents that manage a collection of
//! services and respond to events, rather than answering one prompt per turn.
//! The supervisor turns admitted events into bounded turn requests. Pure: no
//! clocks, I/O or locks. The application persists the supervisor together with
//! the intents it emits and interprets them through ordinary conversation turns.
//! See docs/research/BOT-CONTROL.md for the contract this implements a slice of.
pub mod supervisor;
pub use supervisor::{BotId, Budget, ChatId, Definition, Effect, Event, Reason, ServiceId, Source, Status, Supervisor, TurnId, WatchId};
