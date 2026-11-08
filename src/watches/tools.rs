//! Watch tool schemas and input decoding. Ownership, container checks and
//! persistence stay behind `WatchOps`.
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;
use super::WakePolicy;

pub enum WatchCall {
    Start { service_id: String, name: String, command: String, permanent: bool, wake: bool, wake_policy: WakePolicy },
    Wake,
    Events { watch_id: String, after: Option<u64> },
    Status { watch_id: Option<String> },
    Stop { watch_id: String },
}
pub trait WatchOps {
    fn watch(&self, attempt: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value>;
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        let mut definitions = vec![
            json!({"name":"control_watch_start","description":"Start a supervised command in a persistent service. Output is retained as untrusted evidence under spool limits. wake_policy selects lines that admit bot turns: exit_only (default), literal prefixes, or explicit any_line. Exits and source gaps remain eligible. Workload tools classify observations: emit a distinct prefix on readiness, changed health, an actionable milestone/blocker, or a newly unknown failure needing investigation. Retain raw logs; unchanged status and repeated known warnings must not emit that prefix. Routine command completions, surveys and inspections already returned to the active worker belong in retained diagnostics; do not emit an extra coordination wake for each one. Use an actionable dependency transition when a waiting worker needs to resume. Do not promote every line containing ERROR, Failed or Exception into a callback: severity text alone does not establish changed health. Emit recovery and recurrence after a resolved failure as new transitions. The host does not interpret program protocols or deduplicate meaning. wake arms a non-bot chat once; it does not override policy or disable an existing bot. Keep working while useful work remains; yield at a real dependency. restart permanent restarts with backoff; transient runs once. Up to 8 watches per chat. Policy is captured at creation; start a replacement watch to change it.","inputSchema":{"type":"object","properties":{"service_id":{"type":"string"},"name":{"type":"string","minLength":1,"maxLength":48},"command":{"type":"string","minLength":1,"maxLength":16000},"restart":{"type":"string","enum":["permanent","transient"],"default":"permanent"},"wake":{"type":"boolean","default":true,"description":"Arm a non-bot chat once on the next policy-selected line, exit or source gap. Existing bot subscriptions are unaffected."}},"required":["service_id","name","command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
            json!({"name":"control_watch_events","description":"Read unfiltered watch evidence, at most 200 events. Default starts after this attempt's last delivered or committed cursor; explicit after selects history. Owned watches return a durable receipt: include it in control_turn_handoff.processed after processing, alongside context receipts. Only successful publication acknowledges events. Skipped, missing or truncated evidence requires explicit resynchronisation. Foreign watches remain observations without receipts.","inputSchema":{"type":"object","properties":{"watch_id":{"type":"string"},"after":{"type":"integer","minimum":0}},"required":["watch_id"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
            json!({"name":"control_watch_status","description":"Inspect watch policy, creation provenance, service generation, cursor and last exit. Creation provenance does not identify the producer of each line.","inputSchema":{"type":"object","properties":{"watch_id":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
            json!({"name":"control_watch_wake","description":"Arm this chat once for its watches' next policy-selected line, exit or source gap. This does not broaden wake policies. Keep working while independent work remains; an existing bot is already subscribed.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
            json!({"name":"control_watch_stop","description":"Stop a watch; the service ends its command.","inputSchema":{"type":"object","properties":{"watch_id":{"type":"string"}},"required":["watch_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}}),
        ];
        definitions[0]["inputSchema"]["properties"]["wake_policy"] = json!({"oneOf":[
            {"type":"object","properties":{"kind":{"type":"string","enum":["any_line","exit_only"]}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"prefixes"},"prefixes":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":128},"description":"Literal case-sensitive starts of lines; at most 128 UTF-8 bytes each, no control characters."}},"required":["kind","prefixes"],"additionalProperties":false}
        ]});
        definitions
    }
    fn known(&self, name: &str) -> bool {
        matches!(name, "control_watch_start" | "control_watch_events" | "control_watch_status" | "control_watch_stop" | "control_watch_wake")
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        run(cx.host, &cx.attempt, name, args)
    }
}
impl WatchOps for LocalHost<'_> {
    fn watch(&self, attempt: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value> {
        super::ops::execute(self.0, attempt, call)
    }
}
fn run(ops: &dyn WatchOps, attempt: &AttemptRef<'_>, name: &str, args: &Value) -> io::Result<Value> {
    ops.watch(attempt, decode(name, args)?)
}
fn default_wake_policy() -> WakePolicy { WakePolicy::ExitOnly {} }
fn decode(name: &str, args: &Value) -> io::Result<WatchCall> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Start {
        service_id: String,
        name: String,
        command: String,
        #[serde(default)]
        restart: Option<String>,
        wake: Option<bool>,
        #[serde(default = "default_wake_policy")]
        wake_policy: WakePolicy,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Events {
        watch_id: String,
        after: Option<u64>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Status {
        watch_id: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stop {
        watch_id: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wake {}
    let invalid = |message: &str| io::Error::other(message.to_owned());
    match name {
        "control_watch_start" => {
            let start = serde_json::from_value::<Start>(args.clone()).map_err(|_| invalid("Expected service_id, name, command and optional restart and wake"))?;
            let permanent = match start.restart.as_deref() {
                None | Some("permanent") => true,
                Some("transient") => false,
                _ => return Err(invalid("restart must be permanent or transient")),
            };
            if start.name.trim().is_empty() || start.name.chars().count() > 48 || start.command.trim().is_empty() || start.command.len() > 16000 {
                return Err(invalid("A watch needs a name (1–48 characters) and a command"));
            }
            Ok(WatchCall::Start { service_id: start.service_id, name: start.name, command: start.command, permanent, wake: start.wake.unwrap_or(true), wake_policy: start.wake_policy })
        }
        "control_watch_events" => serde_json::from_value::<Events>(args.clone()).map(|e| WatchCall::Events { watch_id: e.watch_id, after: e.after }).map_err(|_| invalid("Expected watch_id and optional after")),
        "control_watch_status" => serde_json::from_value::<Status>(args.clone()).map(|s| WatchCall::Status { watch_id: s.watch_id }).map_err(|_| invalid("Expected an optional watch_id")),
        "control_watch_wake" => serde_json::from_value::<Wake>(args.clone()).map(|_| WatchCall::Wake).map_err(|_| invalid("control_watch_wake takes no arguments")),
        "control_watch_stop" => serde_json::from_value::<Stop>(args.clone()).map(|s| WatchCall::Stop { watch_id: s.watch_id }).map_err(|_| invalid("Expected watch_id")),
        _ => Err(invalid("Unknown watch tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);
    impl WatchOps for Recorder {
        fn watch(&self, _: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value> {
            let call = match call {
                WatchCall::Start { name, permanent, wake, .. } => format!("start {name} {permanent} {wake}"),
                WatchCall::Events { watch_id, after } => format!("events {watch_id} {after:?}"),
                WatchCall::Status { watch_id } => format!("status {watch_id:?}"),
                WatchCall::Stop { watch_id } => format!("stop {watch_id}"),
                WatchCall::Wake => "wake".into(),
            };
            self.0.lock().unwrap().push(call);
            Ok(json!({}))
        }
    }

    #[test]
    fn watch_tools_decode_typed_calls_against_only_their_capability() {
        let host = Recorder::default();
        let attempt = AttemptRef { run: "r", generation: "g", nonce: "n" };
        for (name, args) in [
            ("control_watch_start", json!({"service_id":"svc","name":"chat","command":"node bot.js"})),
            ("control_watch_start", json!({"service_id":"svc","name":"once","command":"ls","restart":"transient"})),
            ("control_watch_events", json!({"watch_id":"w-1","after":4})),
            ("control_watch_status", json!({})),
            ("control_watch_stop", json!({"watch_id":"w-1"})),
            ("control_watch_start", json!({"service_id":"svc","name":"ready","command":"tail -F log","wake":true})),
            ("control_watch_wake", json!({})),
            ("control_watch_start", json!({"service_id":"svc","name":"quiet","command":"ls","wake":false})),
        ] {
            run(&host, &attempt, name, &args).unwrap();
        }
        assert_eq!(*host.0.lock().unwrap(), ["start chat true true", "start once false true", "events w-1 Some(4)", "status None", "stop w-1", "start ready true true", "wake", "start quiet true false"]);
        assert!(run(&host, &attempt, "control_watch_start", &json!({"service_id":"svc","name":"x","command":"ls","restart":"sometimes"})).is_err());
        assert!(run(&host, &attempt, "control_watch_start", &json!({"service_id":"svc","name":" ","command":"ls"})).is_err());
        assert!(run(&host, &attempt, "control_watch_stop", &json!({"watch_id":"w-1","extra":1})).is_err());
        assert!(run(&host, &attempt, "control_watch_wake", &json!({"watch_id":"w-1"})).is_err());
        let call = decode("control_watch_start", &json!({"service_id":"svc","name":"filtered","command":"tail -F log","wake_policy":{"kind":"prefixes","prefixes":["event:"]}})).unwrap();
        assert!(matches!(call, WatchCall::Start { wake_policy:WakePolicy::Prefixes { .. }, .. }));
        assert!(decode("control_watch_start", &json!({"service_id":"svc","name":"filtered","command":"tail -F log","wake_policy":{"kind":"prefixes","prefixes":[]}})).is_err());
    }
}
