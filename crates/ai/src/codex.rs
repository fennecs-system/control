use crate::{Activity, Decoder, Event, Output, Tokens};
use serde_json::Value;
use std::io;
pub fn activity(value: &serde_json::Value) -> Option<Activity> {
    let event = value["type"].as_str()?;
    if !["item.started", "item.updated", "item.completed"].contains(&event) {
        return None;
    }
    let item = &value["item"];
    let kind = item["type"].as_str()?;
    let text = match kind {
        "command_execution" => item["command"].as_str()?,
        "mcp_tool_call" => item["tool"].as_str()?,
        "file_change" => "File changes",
        "agent_message" => item["text"].as_str()?,
        _ => return None,
    };
    Some(Activity {
        id: item["id"].as_str()?.chars().take(100).collect(),
        kind: if kind == "agent_message" {
            "message"
        } else {
            kind
        }
        .into(),
        text: text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .take(if kind == "agent_message" { 4000 } else { 2000 })
            .collect(),
        output: item["aggregated_output"]
            .as_str()
            .or_else(|| item["error"]["message"].as_str())
            .map_or_else(
                || {
                    item["result"]["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["text"].as_str())
                        .collect()
                },
                |text| vec![text],
            )
            .iter()
            .enumerate()
            .flat_map(|(i, part)| (i > 0).then_some('\n').into_iter().chain(part.chars()))
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .take(if kind == "mcp_tool_call" { 1000 } else { 4000 })
            .collect(),
        status: item["status"]
            .as_str()
            .unwrap_or(if event == "item.completed" {
                "completed"
            } else {
                "running"
            })
            .chars()
            .take(40)
            .collect(),
    })
}
fn record(value: &serde_json::Value, total: &mut Option<Tokens>) {
    if value["type"] != "turn.completed" {
        return;
    }
    let usage = &value["usage"];
    let (Some(input_tokens), Some(cached_input_tokens), Some(output_tokens)) = (
        usage["input_tokens"].as_u64(),
        usage["cached_input_tokens"].as_u64(),
        usage["output_tokens"].as_u64(),
    ) else {
        return;
    };
    if cached_input_tokens > input_tokens {
        return;
    }
    total.get_or_insert_with(Tokens::default).add(Tokens {
        input_tokens,
        cached_input_tokens,
        output_tokens,
    });
}

#[derive(Default)]
pub struct Codex {
    tokens: Option<Tokens>,
}
impl Decoder for Codex {
    fn decode(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        record(value, &mut self.tokens);
        if let Some(activity) = activity(value) {
            emit(Event::Activity(activity));
        }
    }
    // Codex stdout can contain non-JSON diagnostics; only structured records count.
    fn malformed(&mut self) {}
    fn accepts_partial_record(&self) -> bool {
        true
    }
    fn finish(self) -> io::Result<Output> {
        Ok(Output {
            text: None,
            tokens: self.tokens,
        })
    }
}
