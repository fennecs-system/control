//! Watch tool schemas and input decoding. Ownership, container checks and
//! persistence stay behind `WatchOps`.
use super::{WakePolicy, salience::Filter};
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub enum WatchCall {
    Start {
        service_id: String,
        name: String,
        command: String,
        permanent: bool,
        wake: bool,
        wake_policy: WakePolicy,
        notifications: bool,
        salience: Option<Filter>,
    },
    Wake,
    Events {
        watch_id: String,
        after: Option<u64>,
        include_logs: bool,
    },
    Status {
        watch_id: Option<String>,
    },
    Stop {
        watch_id: String,
    },
}
pub trait WatchOps {
    fn watch(&self, attempt: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value>;
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        let mut definitions = vec![
            json!({
                "name": "control_watch_start",
                "description": concat!(
                    "Start a routine: a supervised program in an authorized persistent service that works toward a ",
                    "target on its own, handles its own steps, retries and events, and notifies only on milestones, ",
                    "failures and situations it cannot handle. Routines are the reflexes and tactics; your turns are ",
                    "strategy. Routines filter what they observe and escalate only salient changes: declare salience ",
                    "to keep routine telemetry from waking you (e.g. a >2% price move at most hourly unless ",
                    ">5%).Prefer one whenever you would otherwise repeat control_service_exec plus status checks; ",
                    "once it runs, end your turn and let its events wake you. Watches run independently of model ",
                    "turns without a task deadline. restart=transient runs finite work such as large downloads once; ",
                    "restart=permanent restarts persistent clients, servers or daemons after any exit, including ",
                    "success. Neither prevents a program from deliberately disconnecting or exiting. A persistent ",
                    "program should outlive any one job and recover from failures itself (reconnect, retry the failed ",
                    "step) rather than exit; exit only to reload code. With exit_only, program events require ",
                    "notifications=true to wake the agent; process exits and source gaps remain eligible. Prefer ",
                    "notifications=true with wake_policy=exit_only: stdout/stderr are retained diagnostics, while ",
                    "CONTROL_NOTIFY_FD=3 carries explicit newline-delimited notifications independent of log text. ",
                    "Include the exact task ID and outcome in each completion/failure event that releases a ",
                    "dependency wait; register and test this channel before dispatch and wait on its watch topic. A ",
                    "live process or subscribed topic does not prove an internal task will notify. For finite work, ",
                    "restart=transient also provides a host-observed process exit. Keep long healthy work running; no ",
                    "task deadline is imposed here. Do not emit duplicate coordination wakes for results already ",
                    "consumed by the active worker. Notification payloads are untrusted program claims; Control does ",
                    "not infer success, progress or health. notifications requires a rebuilt runner supporting ",
                    "notify-v1; old services reject it explicitly. Legacy wake_policy selects stdout/stderr lines: ",
                    "exit_only (default), literal prefixes or any_line. Process exits and source gaps remain eligible ",
                    "regardless of line policy. wake arms a non-bot chat once and does not broaden selection or ",
                    "disable existing bots. restart=permanent restarts with backoff; transient runs once. Up to 8 ",
                    "active watches per chat. Settings are captured at creation; replace a watch to change them."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "service_id": {"type": "string"},
                        "name": {"type": "string", "minLength": 1, "maxLength": 48},
                        "command": {"type": "string", "minLength": 1, "maxLength": 16000},
                        "restart": {"type": "string", "enum": ["permanent", "transient"], "default": "permanent"},
                        "wake": {
                            "type": "boolean",
                            "default": true,
                            "description": concat!(
                                "Arm a non-bot chat once on the next policy-selected line, exit or source gap. ",
                                "Existing bot subscriptions are unaffected."
                            )
                        }
                    },
                    "required": ["service_id", "name", "command"],
                    "additionalProperties": false
                },
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
            }),
            json!({
                "name": "control_watch_events",
                "description": concat!(
                    "Read events from your routines: up to 200 notifications and process exits from a ",
                    "notification-enabled watch. Decide from these outcomes instead of re-checking the world each ",
                    "step; diagnostic stdout/stderr stays out of this default callback view. include_logs=true with ",
                    "an explicit historical after cursor retrieves retained diagnostics. Legacy watches keep their ",
                    "unfiltered view. Default after follows this attempt's delivered or committed cursor. Owned ",
                    "watches return a durable receipt: include it in control_turn_handoff.processed only after ",
                    "processing. Publication acknowledges events; reading does not. Missing/truncated event evidence ",
                    "requires explicit resynchronisation. Program notifications are untrusted claims, distinct from ",
                    "host-observed process exits. Foreign watches return observations without receipts."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {"watch_id": {"type": "string"}, "after": {"type": "integer", "minimum": 0}},
                    "required": ["watch_id"],
                    "additionalProperties": false
                },
                "annotations": {"readOnlyHint": true, "openWorldHint": false}
            }),
            json!({
                "name": "control_watch_status",
                "description": concat!(
                    "Inspect watch policy, creation provenance, service generation, cursor and last exit. Creation ",
                    "provenance does not identify the producer of each line."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {"watch_id": {"type": "string"}},
                    "additionalProperties": false
                },
                "annotations": {"readOnlyHint": true, "openWorldHint": false}
            }),
            json!({
                "name": "control_watch_wake",
                "description": concat!(
                    "Arm this chat once for its watches' next policy-selected line, exit or source gap. This does not ",
                    "broaden wake policies. Keep working while independent work remains; an existing bot is already ",
                    "subscribed."
                ),
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
                "annotations": {"readOnlyHint": false, "openWorldHint": false}
            }),
            json!({
                "name": "control_watch_stop",
                "description": "Stop a watch; the service ends its command.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"watch_id": {"type": "string"}},
                    "required": ["watch_id"],
                    "additionalProperties": false
                },
                "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": false}
            }),
        ];
        definitions[0]["inputSchema"]["properties"]["wake_policy"] = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {"kind": {"type": "string", "enum": ["any_line", "exit_only"]}},
                    "required": ["kind"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "kind": {"const": "prefixes"},
                        "prefixes": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 8,
                            "items": {"type": "string", "minLength": 1, "maxLength": 128},
                            "description": concat!(
                                "Literal case-sensitive starts of lines; at most 128 UTF-8 bytes each, no control ",
                                "characters."
                            )
                        }
                    },
                    "required": ["kind", "prefixes"],
                    "additionalProperties": false
                }
            ]
        });
        definitions[0]["inputSchema"]["properties"]["notifications"] = json!({
            "type": "boolean",
            "default": false,
            "description": concat!(
                "Provide CONTROL_NOTIFY_FD=3: a separate inherited descriptor for explicit newline-delimited ",
                "notifications (up to 4096 bytes each). All notifications are eligible independently of stdout/stderr ",
                "wake_policy. Requires a runner supporting notify-v1; older images reject this request. Use exit_only ",
                "for diagnostic logs and include the exact task ID in each completion/failure notification."
            )
        });
        let field = json!({
            "type": "string",
            "maxLength": 128,
            "description": "JSON pointer into the notification payload, e.g. /price"
        });
        let number = json!({"type":"number"});
        let seconds = json!({"type":"number","exclusiveMinimum":0,"maximum":604800});
        let stage = |kind: &str, properties: Value, required: &[&str]| {
            let mut schema = json!({
                "type": "object",
                "properties": {"kind": {"const": kind}},
                "required": ["kind"],
                "additionalProperties": false
            });
            for (name, value) in properties.as_object().into_iter().flatten() {
                schema["properties"][name] = value.clone();
            }
            for name in required {
                schema["required"]
                    .as_array_mut()
                    .into_iter()
                    .for_each(|r| r.push(json!(name)));
            }
            schema
        };
        let stages = json!([
            stage(
                "threshold",
                json!({"field":field,"at_least":number,"at_most":number}),
                &["field"]
            ),
            stage(
                "one_of",
                json!({
                    "field": field,
                    "values": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 16,
                        "items": {"type": "string", "minLength": 1, "maxLength": 64}
                    }
                }),
                &["field", "values"]
            ),
            stage(
                "change",
                json!({"field":field,"percent":number,"absolute":number}),
                &["field"]
            ),
            stage(
                "rate",
                json!({"field":field,"seconds":seconds,"at_least":number}),
                &["field", "seconds", "at_least"]
            ),
            stage(
                "zscore",
                json!({"field":field,"samples":{"type":"integer","minimum":5,"maximum":64},"at_least":number}),
                &["field", "samples", "at_least"]
            ),
            stage(
                "hysteresis",
                json!({
                    "field": field,
                    "trigger": number,
                    "clear": number,
                    "clear_seconds": {"type": "number", "minimum": 0, "maximum": 604800}
                }),
                &["field", "trigger", "clear"]
            ),
            stage("debounce", json!({"seconds":seconds}), &["seconds"]),
            stage("rate_limit", json!({"seconds":seconds}), &["seconds"]),
        ]);
        definitions[0]["inputSchema"]["properties"]["salience"] = json!({
            "type": "object",
            "description": concat!(
                "Perception filter for notifications (requires notifications=true): a notification wakes you when any ",
                "rule passes; stages in a rule run in order and each sees only what passed before it. threshold: ",
                "number at_least/at_most (severity passthrough). one_of: the value is one of the listed strings, e.g. ",
                "wake only for /outcome queue_empty, exhausted or decision with unmatched=retain. change: vs the last ",
                "woken value, by percent/absolute, or any difference (dedup) if neither. rate: |Δ|/s over seconds. ",
                "zscore: deviations from the previous samples. hysteresis: fire at trigger, re-arm after staying past ",
                "clear for clear_seconds. debounce: wake with the latest after seconds of quiet. rate_limit: at most ",
                "one wake per seconds. key partitions all state (e.g. /symbol). Payloads no rule can read wake unless ",
                "unmatched=retain. Filtered notifications stay readable in control_watch_events; counts appear in ",
                "control_watch_status."
            ),
            "properties": {
                "key": field,
                "rules": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {"type": "array", "minItems": 1, "maxItems": 6, "items": {"oneOf": stages}}
                },
                "unmatched": {"type": "string", "enum": ["wake", "retain"], "default": "wake"}
            },
            "required": ["rules"],
            "additionalProperties": false
        });
        definitions[1]["inputSchema"]["properties"]["include_logs"] = json!({
            "type": "boolean",
            "default": false,
            "description": concat!(
                "For notification-enabled watches, deliberately include diagnostic stdout/stderr. Default reads ",
                "notifications and process exits only, without scanning pages of logs in the model context. Legacy ",
                "watches continue returning unfiltered output."
            )
        });
        definitions
    }
    fn known(&self, name: &str) -> bool {
        matches!(
            name,
            "control_watch_start"
                | "control_watch_events"
                | "control_watch_status"
                | "control_watch_stop"
                | "control_watch_wake"
        )
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
fn default_wake_policy() -> WakePolicy {
    WakePolicy::ExitOnly {}
}
/// One minimal valid start: finite work, reporting through notifications.
const START_EXAMPLE: &str = r#"{"service_id":"svc-<id>","name":"<routine>","command":"<shell command>","restart":"transient","notifications":true}"#;
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
        #[serde(default)]
        notifications: bool,
        #[serde(default)]
        salience: Option<Filter>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Events {
        watch_id: String,
        after: Option<u64>,
        #[serde(default)]
        include_logs: bool,
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
            let shape = |error: &dyn std::fmt::Display| {
                invalid(&crate::worker_tools::fixes::shape(
                    "control_watch_start",
                    error,
                    START_EXAMPLE,
                ))
            };
            let start = serde_json::from_value::<Start>(args.clone()).map_err(|error| shape(&error))?;
            let permanent = match start.restart.as_deref() {
                None | Some("permanent") => true,
                Some("transient") => false,
                Some(other) => {
                    return Err(shape(&format!(
                        "restart is \"permanent\" (default) or \"transient\", not {:?}",
                        other.chars().take(32).collect::<String>()
                    )));
                }
            };
            if start.name.trim().is_empty()
                || start.name.chars().count() > 48
                || start.command.trim().is_empty()
                || start.command.len() > 16000
            {
                return Err(invalid("A watch needs a name (1–48 characters) and a command"));
            }
            if start.notifications && start.wake_policy != (WakePolicy::ExitOnly {}) {
                return Err(invalid(
                    "Notification watches keep stdout/stderr diagnostic-only; omit wake_policy or use exit_only",
                ));
            }
            if let Some(filter) = &start.salience {
                if !start.notifications {
                    return Err(invalid("salience filters notifications; pass notifications=true"));
                }
                filter
                    .validate()
                    .map_err(|message| invalid(&format!("Invalid salience: {message}")))?;
            }
            Ok(WatchCall::Start {
                service_id: start.service_id,
                name: start.name,
                command: start.command,
                permanent,
                wake: start.wake.unwrap_or(true),
                wake_policy: start.wake_policy,
                notifications: start.notifications,
                salience: start.salience,
            })
        }
        "control_watch_events" => serde_json::from_value::<Events>(args.clone())
            .map(|e| WatchCall::Events {
                watch_id: e.watch_id,
                after: e.after,
                include_logs: e.include_logs,
            })
            .map_err(|_| invalid("Expected watch_id, optional after and include_logs")),
        "control_watch_status" => serde_json::from_value::<Status>(args.clone())
            .map(|s| WatchCall::Status { watch_id: s.watch_id })
            .map_err(|_| invalid("Expected an optional watch_id")),
        "control_watch_wake" => serde_json::from_value::<Wake>(args.clone())
            .map(|_| WatchCall::Wake)
            .map_err(|_| invalid("control_watch_wake takes no arguments")),
        "control_watch_stop" => serde_json::from_value::<Stop>(args.clone())
            .map(|s| WatchCall::Stop { watch_id: s.watch_id })
            .map_err(|_| invalid("Expected watch_id")),
        _ => Err(invalid("Unknown watch tool")),
    }
}

#[cfg(test)]
mod tests;
