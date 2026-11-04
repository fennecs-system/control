//! Bot tool schemas and input decoding. Authority, approval and persistence stay
//! behind `BotOps`; a model can request a bot or pause itself, never start one.
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub enum BotCall {
    Pause,
    Request { chat_id: String, every_minutes: Option<u64>, room: bool },
    Status { chat_id: Option<String> },
}
pub trait BotOps {
    fn bot(&self, attempt: &AttemptRef<'_>, call: BotCall) -> io::Result<Value>;
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"name":"control_bot_pause","description":"Durably pause this chat's own automatic bot wakes when user direction or new evidence is required. Pending events and services are retained. End your turn afterward. The operator resumes with /bot resume; this tool cannot resume, start, or alter another bot.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
            json!({"name":"control_bot_request","description":"Ask the operator to run a chat as a persistent bot: agent + supervisor + callbacks. Keep its session active while meaningful work advances the goal; room messages, watch events and an optional timer resume it when idle. Target any chat in this conversation: this chat, the lead that started it, or any agent spawned within it (for example each agent from control_agent_spawn, including ones a helper spawned). The operator approves in the chat or with /bot allow ID, also over Discord. Yield when the approval blocks useful work; decisions are delivered as callbacks. A request grants nothing until approved.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"},"every_minutes":{"type":"integer","minimum":1,"maximum":1440},"room":{"type":"boolean","default":true}},"required":["chat_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
            json!({"name":"control_bot_status","description":"Read whether a chat in this conversation runs as a bot (sleeping, working, paused or stopping) and the state of your bot requests.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        matches!(name, "control_bot_request" | "control_bot_status" | "control_bot_pause")
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        run(cx.host, &cx.attempt, name, args)
    }
}
impl BotOps for LocalHost<'_> {
    fn bot(&self, attempt: &AttemptRef<'_>, call: BotCall) -> io::Result<Value> {
        super::execute(self.0, attempt, call)
    }
}
/// Decodes a call and runs it on whatever provides `BotOps`.
fn run(ops: &dyn BotOps, attempt: &AttemptRef<'_>, name: &str, args: &Value) -> io::Result<Value> {
    ops.bot(attempt, decode(name, args)?)
}
fn decode(name: &str, args: &Value) -> io::Result<BotCall> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pause {}
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        chat_id: String,
        every_minutes: Option<u64>,
        #[serde(default = "yes")]
        room: bool,
    }
    fn yes() -> bool {
        true
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Status {
        chat_id: Option<String>,
    }
    match name {
        "control_bot_pause" => serde_json::from_value::<Pause>(args.clone()).map(|_| BotCall::Pause).map_err(|_| io::Error::other("control_bot_pause takes no arguments")),
        "control_bot_request" => serde_json::from_value::<Request>(args.clone())
            .map(|r| BotCall::Request { chat_id: r.chat_id, every_minutes: r.every_minutes, room: r.room })
            .map_err(|_| io::Error::other("Expected chat_id, optional every_minutes (1–1440) and room")),
        "control_bot_status" => serde_json::from_value::<Status>(args.clone())
            .map(|s| BotCall::Status { chat_id: s.chat_id })
            .map_err(|_| io::Error::other("Expected an optional chat_id")),
        _ => Err(io::Error::other("Unknown bot tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Provides only bot operations: tool families need no other capability.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);
    impl BotOps for Recorder {
        fn bot(&self, attempt: &AttemptRef<'_>, call: BotCall) -> io::Result<Value> {
            let call = match call {
                BotCall::Pause => "pause".into(),
                BotCall::Request { chat_id, every_minutes, room } => format!("request {chat_id} {every_minutes:?} {room}"),
                BotCall::Status { chat_id } => format!("status {chat_id:?}"),
            };
            self.0.lock().unwrap().push(format!("{}/{} {call}", attempt.run, attempt.nonce));
            Ok(json!({"recorded":true}))
        }
    }

    #[test]
    fn bot_tools_decode_typed_calls_against_a_host_offering_only_bot_operations() {
        let host = Recorder::default();
        let attempt = AttemptRef { run: "run", generation: "gen", nonce: "n1" };
        run(&host, &attempt, "control_bot_request", &json!({"chat_id":"c-1","every_minutes":5})).unwrap();
        run(&host, &attempt, "control_bot_status", &json!({})).unwrap();
        run(&host, &attempt, "control_bot_pause", &json!({})).unwrap();
        assert!(run(&host,&attempt,"control_bot_pause",&json!({"chat_id":"c-other"})).is_err());
        assert!(run(&host, &attempt, "control_bot_request", &json!({"chat_id":"c-1","every_minutes":5,"extra":1})).is_err());
        assert!(run(&host, &attempt, "control_bot_spawn", &json!({})).is_err());
        assert_eq!(*host.0.lock().unwrap(), ["run/n1 request c-1 Some(5) true", "run/n1 status None", "run/n1 pause"]);
    }
}
