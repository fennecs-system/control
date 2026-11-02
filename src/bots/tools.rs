//! Bot tool schemas and input decoding. Authority, approval and persistence stay
//! behind `BotOps`; a model can only request a bot, never start one.
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub enum BotCall {
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
            json!({"name":"control_bot_request","description":"Ask the operator to run a chat as a persistent bot: an agent that sleeps until events wake it (new messages in its room, and optionally a timer), then acts and ends its turn. Target this chat or a chat you spawned (directly or through your children), for example each agent from control_agent_spawn. The operator approves in the chat or with /bot allow ID, also over Discord. End your turn after requesting; you are resumed with the decisions. A request grants nothing until approved.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"},"every_minutes":{"type":"integer","minimum":1,"maximum":1440},"room":{"type":"boolean","default":true}},"required":["chat_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
            json!({"name":"control_bot_status","description":"Read whether this chat or one you spawned runs as a bot (sleeping, working, paused or stopping) and the state of your bot requests.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        matches!(name, "control_bot_request" | "control_bot_status")
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        cx.host.bots().bot(&cx.attempt, decode(name, args)?)
    }
}
fn decode(name: &str, args: &Value) -> io::Result<BotCall> {
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
        "control_bot_request" => serde_json::from_value::<Request>(args.clone())
            .map(|r| BotCall::Request { chat_id: r.chat_id, every_minutes: r.every_minutes, room: r.room })
            .map_err(|_| io::Error::other("Expected chat_id, optional every_minutes (1–1440) and room")),
        "control_bot_status" => serde_json::from_value::<Status>(args.clone())
            .map(|s| BotCall::Status { chat_id: s.chat_id })
            .map_err(|_| io::Error::other("Expected an optional chat_id")),
        _ => Err(io::Error::other("Unknown bot tool")),
    }
}
