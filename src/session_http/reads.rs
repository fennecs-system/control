//! The operator MCP tools that only read state, in two steps: `view` copies
//! what a call answers out of one critical section, and the answer is
//! finished after release: Discord actors authorized (their policy is read
//! from disk), artifact manifests and files read, the reply encoded. `/mcp`
//! serves them through `crate::read_response`; the in-process operator MCP
//! (`session_http::rpc`) runs the same steps under the lock it holds.
use crate::{State, chat, identity};
use serde_json::{Value, value::RawValue};

/// A read-only tool call, its arguments checked before locking.
pub(crate) enum ReadTool {
    Chats,
    /// The chat id argument, or why it is refused (likewise below).
    Chat(Result<String, &'static str>),
    /// Waiting questions' ages count from this reading of the clock.
    Requests(crate::design::UnixSeconds),
    Scores(Result<String, &'static str>),
    Services(Result<String, &'static str>, bool),
    Artifacts(Result<crate::artifacts::Call, &'static str>),
    /// A tool whose whole answer the view computes.
    Lookup(Lookup, String, Value),
}
/// The families answered in full under the lock.
pub(crate) enum Lookup {
    Personas,
    Lenses,
    Knowledge,
    Rooms,
}
impl ReadTool {
    /// `None` for a tool that is not a read-only one.
    pub(crate) fn of(name: &str, arguments: &Value) -> Option<Self> {
        let chat = || chat::chat_id(arguments).map(str::to_owned);
        let lookup = |family| Some(Self::Lookup(family, name.to_owned(), arguments.clone()));
        Some(match name {
            "control.chats.list" => Self::Chats,
            "control.chat.read" => Self::Chat(chat()),
            "control.requests.list" => Self::Requests(crate::design::UnixSeconds::now()),
            "control.run.scores" => Self::Scores(chat()),
            "control.services.list" => Self::Services(chat(), arguments["team"] == true),
            "control.artifacts.list" | "control.artifact.read" => {
                Self::Artifacts(crate::artifacts::Call::of(name == "control.artifacts.list", arguments))
            }
            "control.personas.list" | "control.persona.read" => return lookup(Lookup::Personas),
            "control.lenses.list" | "control.lens.read" | "control.lenses.resolve" => return lookup(Lookup::Lenses),
            "control.rooms.list" | "control.room.read" => return lookup(Lookup::Rooms),
            name if crate::knowledge_tools::reads(name) => return lookup(Lookup::Knowledge),
            _ => return None,
        })
    }
    /// Copies what the call answers; refusals that need only state come here.
    pub(crate) fn view(self, state: &State) -> Result<Viewed, &'static str> {
        Ok(match self {
            Self::Chats => Viewed::Chats(chat::Listing::of(state)),
            Self::Chat(chat) => Viewed::Value(chat.and_then(|chat| chat::detail(state, &chat))?),
            Self::Requests(now) => Viewed::Requests(chat::Waiting::of(state, now)),
            Self::Scores(chat) => Viewed::Checked(chat.and_then(|chat| crate::scores::view(state, &chat))?),
            Self::Services(chat, team) => Viewed::Services(
                crate::services::operator::list(&state.services, &state.chats, &chat?, team)
                    .map_err(|_| "Chat not found.")?,
            ),
            Self::Artifacts(call) => Viewed::Artifacts(call?.view(state)?),
            Self::Lookup(family, name, arguments) => Viewed::Value(match family {
                Lookup::Personas => crate::personas::read(&state.personas, &name, &arguments),
                Lookup::Lenses => crate::lens_library::tool(crate::lens_library::Sources::of(state), &name, &arguments),
                Lookup::Knowledge => {
                    crate::knowledge_tools::read(&state.documents, &state.collective.proposals, None, &name, &arguments)
                }
                Lookup::Rooms => crate::rooms::lookup(state, &name, &arguments),
            }?),
        })
    }
}
/// What a read-only call answers, copied out of state. Plain owned values:
/// none can reach the state.
pub(crate) enum Viewed {
    Chats(chat::Listing),
    Value(Value),
    Checked(identity::Unchecked<Value>),
    /// As `Checked`, but a refused actor reads as a missing chat.
    Services(identity::Unchecked<Value>),
    Requests(chat::Waiting),
    Artifacts(crate::artifacts::Listed),
}
impl Viewed {
    /// The tool's data, after the checks and file reads that need no state.
    pub(crate) fn value(self) -> Result<Value, &'static str> {
        match self {
            // Plain data with string keys: encoding cannot fail.
            Self::Chats(listing) => Ok(serde_json::to_value(listing).expect("a chat listing encodes")),
            Self::Value(value) => Ok(value),
            Self::Checked(value) => value.check(identity::authorize),
            Self::Services(value) => value.check(identity::authorize).map_err(|_| "Chat not found."),
            Self::Requests(waiting) => Ok(waiting.authorized()),
            Self::Artifacts(listed) => listed.answer(),
        }
    }
    /// The data encoded once, as `value(..).to_string()` writes it.
    pub(crate) fn raw(self) -> Result<Box<RawValue>, &'static str> {
        // Plain data with string keys: encoding cannot fail.
        Ok(match self {
            Self::Chats(listing) => serde_json::value::to_raw_value(&listing),
            other => serde_json::value::to_raw_value(&other.value()?),
        }
        .expect("read tool data encodes"))
    }
}
