use crate::{Activity, Decoder, Event, Output, Tokens, Usage};
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
        // A deferred tool's activity is recorded under the tool it called.
        "mcp_tool_call" => match item["tool"].as_str()? {
            "control_call" => item["arguments"]["tool"].as_str().unwrap_or("control_call"),
            tool => tool,
        },
        "file_change" => "File changes",
        "agent_message" => item["text"].as_str()?,
        _ => return None,
    };
    Some(Activity {
        id: item["id"].as_str()?.chars().take(100).collect(),
        kind: if kind == "agent_message" { "message" } else { kind }.into(),
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
    /// Codex reports usage only in `turn.completed`, the whole turn's total.
    /// A turn stopped or failed before it reports nothing: unknown.
    fn usage(&self) -> Usage {
        Usage {
            tokens: self.tokens,
            complete: self.tokens.is_some(),
            model: None,
        }
    }
    fn finish(self) -> io::Result<Output> {
        Ok(Output {
            text: None,
            tokens: self.tokens,
        })
    }
}

/// A text request's reply: one final agent message and no tool records.
pub struct Text {
    inner: Codex,
    text: Option<String>,
    bad: bool,
    max: usize,
}
impl Text {
    /// A reply of at most `max` bytes.
    pub fn new(max: usize) -> Self {
        Self {
            inner: Codex::default(),
            text: None,
            bad: false,
            max,
        }
    }
}
impl Decoder for Text {
    fn decode(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        self.inner.decode(value, &mut |_| {});
        if value["type"].as_str().is_some_and(|s| s.starts_with("item.")) {
            match value["item"]["type"].as_str() {
                Some("agent_message") if value["type"] == "item.completed" => {
                    self.text = value["item"]["text"]
                        .as_str()
                        .filter(|text| text.len() <= self.max)
                        .map(str::to_owned);
                    if self.text.is_none() {
                        self.bad = true;
                        emit(Event::RejectedTools);
                    }
                }
                Some("agent_message" | "reasoning") => {}
                _ => {
                    self.bad = true;
                    emit(Event::RejectedTools);
                }
            }
        }
        if value["type"] == "error" || value["type"] == "turn.failed" {
            self.bad = true;
        }
    }
    fn malformed(&mut self) {
        self.bad = true;
    }
    fn finish(self) -> io::Result<Output> {
        if self.bad {
            return Err(io::Error::other("CLI summary contained invalid/tool records"));
        }
        let mut output = self.inner.finish()?;
        output.text = self.text;
        Ok(output)
    }
}
