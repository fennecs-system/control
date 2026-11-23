//! `control_guidance`: guidance looked up when needed instead of carried in
//! every prompt. Prompts hold a short core and one index line per on-demand
//! lens; this read-only tool returns the full text of the revision the run
//! captured, never the current editable library, scoped to the run's role.
use crate::runtime::RunStatus;
use crate::tool_registry::{AttemptCall, AttemptTools};
use control_context::{Audience, Resolved, agent_policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub struct Tools;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(default)]
    topic: Option<String>,
}
const NAME: &str = "control_guidance";

/// Built-in topics for conventions every run shares, beside its lenses.
const BUILT_IN: [(&str, &str, &str); 3] = [
    (
        "session",
        "Active sessions, callbacks, handoff dispositions and turn limits.",
        agent_policy::SESSION_GUIDANCE_DETAIL,
    ),
    (
        "rooms",
        "Rooms, direct messages, claims and steering in a team.",
        agent_policy::ROOM_GUIDANCE_DETAIL,
    ),
    (
        "environment",
        "Worker images, profiles, Nix builds, toolchains and network scope.",
        crate::worker::ENVIRONMENT_GUIDANCE,
    ),
];

/// Read `topic` from `context` as `role` sees it, or list the topics.
pub(crate) fn read(context: Option<&Resolved>, role: Audience, topic: Option<&str>) -> Result<Value, &'static str> {
    let topic = topic.map(str::trim).filter(|t| !t.is_empty());
    let Some(topic) = topic else {
        let lenses = context.into_iter().flat_map(|c| c.guidance.iter()).filter(|g| g.visible(role))
            .map(|g| json!({"topic":g.id,"name":g.name,"revision":g.revision,"summary":g.summary,"inline":g.inline_for(role)}));
        let built_in = BUILT_IN
            .iter()
            .map(|(topic, summary, _)| json!({"topic":topic,"summary":summary}));
        return Ok(json!({"topics":lenses.chain(built_in).collect::<Vec<_>>()}));
    };
    if let Some(guidance) = context.and_then(|c| c.guidance_for(topic, role)) {
        return Ok(
            json!({"topic":guidance.id,"name":guidance.name,"revision":guidance.revision,"text":guidance.text,
            "note":"Captured with this run; task-scoped preferences. Current user direction and enforced policy take precedence."}),
        );
    }
    BUILT_IN
        .iter()
        .find(|(name, ..)| name.eq_ignore_ascii_case(topic))
        .map(|(name, _, text)| json!({"topic":name,"text":text}))
        .ok_or("No captured guidance for that topic. Call control_guidance without a topic to list the topics.")
}

impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({
            "name": NAME,
            "description": concat!(
                "Read guidance on demand: the full captured text of a lens named in your prompt's guidance index, ",
                "or the built-in topics session, rooms and environment. Without topic, list the topics. Read a ",
                "topic when its situation arises, not every turn. Read-only; grants nothing."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {"topic": {"type": "string", "maxLength": 80}},
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        })]
    }
    fn known(&self, name: &str) -> bool {
        name == NAME
    }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        let state = cx.host.0.lock().map_err(|_| io::Error::other("State unavailable"))?;
        let run = state
            .runtime
            .runs
            .get(cx.attempt.run)
            .filter(|r| r.status == RunStatus::Running)
            .ok_or_else(|| io::Error::other("Attempt is not active"))?;
        answer(run, args)
    }
}
/// Answer a call from the active `run`; a pure read of its captured context.
pub(crate) fn answer(run: &crate::runtime::Run, args: &Value) -> io::Result<Value> {
    let input: Input = serde_json::from_value(if args.is_null() { json!({}) } else { args.clone() })
        .map_err(|_| io::Error::other("Expected an optional topic"))?;
    read(
        (*run.context).as_ref(),
        crate::lens_library::audience(run),
        input.topic.as_deref(),
    )
    .map_err(io::Error::other)
}

#[cfg(test)]
mod tests;
