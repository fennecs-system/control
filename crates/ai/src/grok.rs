//! Grok Build's `--output-format streaming-json`: one `type`-tagged object per
//! line (`text`, `thought`, `tool_call`, `tool_call_update`, `usage`,
//! `available_commands`, `error`, `end`), as its `AcpLine` reducer writes them.
use crate::{Activity, Decoder, Event, Output, Tokens, Usage};
use serde_json::Value;
use std::io;

/// The meta-tools Grok reaches MCP servers through; its only tools here.
const META: [&str; 2] = ["search_tool", "use_tool"];
/// The MCP server name Control registers; catalog keys are `control__TOOL`.
const SERVER: &str = "control__";

#[derive(Default)]
pub struct Grok {
    /// Whether the MCP meta-tools are expected. A text call allows none.
    tools: bool,
    /// The response being streamed; the last one is the reply.
    text: String,
    messages: usize,
    calls: Vec<(String, String)>,
    reply: Option<String>,
    failure: Option<String>,
    /// The first tool outside the expected set, named for the operator.
    unexpected: Option<String>,
    invalid: bool,
    /// The `end` (or `error`) line's totals, else the per-response sum.
    reported: Option<Tokens>,
    responses: Option<Tokens>,
    model: Option<String>,
}
fn clean(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(limit)
        .collect()
}
/// Grok reports `input_tokens` uncached; Control's input includes cache reads
/// and writes, with reads also shown as cached.
fn tokens(usage: &Value) -> Option<Tokens> {
    let (input, output) = (usage["input_tokens"].as_u64()?, usage["output_tokens"].as_u64()?);
    let cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let created = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    Some(Tokens {
        input_tokens: input.saturating_add(cached).saturating_add(created),
        cached_input_tokens: cached,
        output_tokens: output,
    })
}
/// A tool id may be namespaced (`ns:use_tool`); rules match its short name.
fn short(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}
impl Grok {
    /// An agent turn, whose only tools are the MCP meta-tools.
    pub fn agent() -> Self {
        Self {
            tools: true,
            ..Self::default()
        }
    }
    /// A bounded text request: any tool is unexpected.
    pub fn text() -> Self {
        Self::default()
    }
    fn expected(&self, name: &str) -> bool {
        self.tools && (META.contains(&short(name)) || name.starts_with(SERVER))
    }
    fn reject(&mut self, name: &str, emit: &mut dyn FnMut(Event)) {
        self.unexpected.get_or_insert_with(|| clean(name, 80));
        emit(Event::RejectedTools);
    }
    /// The response so far becomes one message; a tool call or the end closes it.
    fn flush(&mut self, emit: &mut dyn FnMut(Event)) -> Option<String> {
        let text = std::mem::take(&mut self.text);
        if text.is_empty() {
            return None;
        }
        self.messages += 1;
        emit(Event::Activity(Activity {
            id: format!("grok-response-{}", self.messages),
            kind: "message".into(),
            text: clean(&text, 4000),
            output: String::new(),
            status: "running".into(),
        }));
        Some(text)
    }
    fn tool_call(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        self.flush(emit);
        let (Some(id), Some(tool)) = (value["toolCallId"].as_str(), value["toolName"].as_str()) else {
            self.invalid = true;
            return;
        };
        if !self.expected(tool) {
            return self.reject(tool, emit);
        }
        // `use_tool` names the MCP tool in its input; a deferred Control tool's
        // activity is recorded under the tool it called.
        let input = &value["rawInput"];
        let name = input["tool_name"].as_str().unwrap_or(short(tool));
        let name = name.strip_prefix(SERVER).unwrap_or(name);
        let name = if name == "control_call" {
            input["tool_input"]["tool"].as_str().unwrap_or(name)
        } else {
            name
        };
        let name = clean(name, 2000);
        if self.calls.iter().any(|(known, _)| known == id) {
            return;
        }
        if self.calls.len() >= 64 {
            self.calls.remove(0);
        }
        self.calls.push((id.to_owned(), name.clone()));
        emit(Event::Activity(Activity {
            id: id.chars().take(100).collect(),
            kind: "mcp_tool_call".into(),
            text: name,
            output: String::new(),
            status: "in_progress".into(),
        }));
    }
    fn tool_result(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        let status = match value["status"].as_str() {
            Some("completed") => "completed",
            Some("failed") => "failed",
            _ => return,
        };
        let Some(id) = value["toolCallId"].as_str() else { return };
        let Some(index) = self.calls.iter().position(|(known, _)| known == id) else {
            return;
        };
        let name = self.calls.remove(index).1;
        let parts: Vec<&str> = value["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["content"]["text"].as_str().or(c["text"].as_str()))
            .collect();
        let output = match &value["rawOutput"] {
            _ if !parts.is_empty() => clean(&parts.join("\n"), 1000),
            Value::Null => String::new(),
            Value::String(text) => clean(text, 1000),
            other => clean(&other.to_string(), 1000),
        };
        emit(Event::Activity(Activity {
            id: id.chars().take(100).collect(),
            kind: "mcp_tool_call".into(),
            text: name,
            output,
            status: status.into(),
        }));
    }
    fn end(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        if let Some(total) = tokens(&value["usage"]) {
            self.reported = Some(total);
        }
        // One model served the turn when `modelUsage` names exactly one.
        if let Some(models) = value["modelUsage"].as_object()
            && models.len() == 1
        {
            self.model = models.keys().next().map(|m| clean(m, 100)).filter(|m| !m.is_empty());
        }
        let reply = self.flush(emit);
        match value["stopReason"].as_str() {
            _ if self.reply.is_some() || self.failure.is_some() => self.invalid = true,
            Some("end_turn") => {
                self.reply = Some(reply.map(|text| text.chars().take(16000).collect()).unwrap_or_default())
            }
            reason => {
                self.failure = Some(format!(
                    "Grok ended with {}",
                    clean(reason.unwrap_or("no stop reason"), 40)
                ))
            }
        }
    }
}
impl Decoder for Grok {
    fn decode(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        match value["type"].as_str() {
            Some("text") => {
                if let Some(data) = value["data"].as_str() {
                    let room = 16000usize.saturating_sub(self.text.len());
                    self.text.extend(data.chars().take(room));
                }
            }
            Some("tool_call") => self.tool_call(value, emit),
            Some("tool_call_update") => self.tool_result(value, emit),
            Some("available_commands") => {
                let unexpected = value["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .find(|tool| !self.expected(tool))
                    .map(str::to_owned);
                if let Some(tool) = unexpected {
                    self.reject(&tool, emit);
                }
            }
            Some("usage") => {
                if let Some(response) = tokens(&value["usage"]) {
                    self.responses.get_or_insert_with(Tokens::default).add(response);
                }
            }
            Some("end") => self.end(value, emit),
            Some("error") => {
                if let Some(total) = tokens(&value["usage"]) {
                    self.reported = Some(total);
                }
                let detail = clean(value["message"].as_str().unwrap_or(""), 300);
                self.failure.get_or_insert(format!(
                    "Grok ended with an error{}{detail}",
                    if detail.is_empty() { "" } else { ": " }
                ));
            }
            Some("max_turns_reached") => {
                self.failure.get_or_insert("Grok ended with max_turns_reached".into());
            }
            // Reasoning, plans and lifecycle notices carry no result.
            Some(_) => {}
            None => self.invalid = true,
        }
    }
    fn malformed(&mut self) {
        self.invalid = true;
    }
    fn usage(&self) -> Usage {
        let model = self.model.clone();
        match self.reported {
            Some(total) => Usage {
                tokens: Some(total),
                complete: true,
                model,
            },
            None => Usage {
                tokens: self.responses,
                complete: false,
                model,
            },
        }
    }
    fn finish(self) -> io::Result<Output> {
        if let Some(tool) = &self.unexpected {
            return Err(io::Error::other(format!(
                "Grok started with an unexpected tool ({tool}) and was stopped before doing work."
            )));
        }
        if let Some(failure) = &self.failure {
            return Err(io::Error::other(format!("{failure}.")));
        }
        if self.invalid {
            return Err(io::Error::other("Grok returned an invalid event stream"));
        }
        let text = self
            .reply
            .ok_or_else(|| io::Error::other("Grok did not return a final result; check login or usage limits"))?;
        Ok(Output {
            text: Some(text),
            tokens: self.reported,
        })
    }
}
#[cfg(test)]
mod tests;
