use crate::{Activity, Decoder, Event, Output, Tokens, Usage};
use serde_json::Value;
use std::io;
#[derive(Default)]
pub struct Claude<'a> {
    allowed_tools: &'a [&'a str],
    pub tokens: Option<Tokens>,
    pub result: Option<String>,
    invalid: bool,
    initialized: bool,
    /// Set as soon as the init event lists a tool outside the allow-list.
    bad_tools: bool,
    /// Subtype and error text of an unsuccessful result event, for the user.
    failure: Option<String>,
    tools: Vec<(String, String)>,
    /// The result event's totals, also when the result is an error.
    reported: Option<Tokens>,
    /// Per-call usage of finished API calls, and of the call still streaming
    /// (one call's content blocks arrive as consecutive assistant events
    /// repeating its message id and usage).
    calls: Option<Tokens>,
    call: Option<(String, Tokens)>,
    model: Option<String>,
}
/// Claude counts cache reads and writes outside `input_tokens`; Control's
/// input includes them, with reads also shown as cached.
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
fn model(value: &Value) -> Option<String> {
    value.as_str().filter(|m| !m.is_empty()).map(|m| m.chars().filter(|c| !c.is_control()).take(100).collect())
}
impl Claude<'_> {
    fn event(&mut self, value: &Value) -> Option<Activity> {
        match value["type"].as_str()? {
            "system" if value["subtype"] == "init" => {
                self.initialized = true;
                if let Some(model) = model(&value["model"]) {
                    self.model = Some(model);
                }
                let allowed = self.allowed_tools;
                if !value["tools"].as_array().is_some_and(|tools| {
                    tools.iter().all(|t| {
                        t.as_str().is_some_and(|s| {
                            s == "EndConversation"
                                || s.strip_prefix("mcp__control__")
                                    .is_some_and(|name| allowed.contains(&name))
                        })
                    })
                }) {
                    self.invalid = true;
                    self.bad_tools = true;
                }
            }
            "result" => {
                if let Some(total) = tokens(&value["usage"]) {
                    self.reported = Some(total);
                }
                if value["is_error"] != false || value["subtype"] != "success" {
                    let detail: String = value["result"]
                        .as_str()
                        .or(value["error"].as_str())
                        .unwrap_or("")
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(300)
                        .collect();
                    self.failure = Some(format!(
                        "Claude ended with {}{}{}",
                        value["subtype"]
                            .as_str()
                            .unwrap_or("an error")
                            .chars()
                            .take(40)
                            .collect::<String>(),
                        if detail.is_empty() { "" } else { ": " },
                        detail
                    ));
                }
                if self.result.is_some()
                    || value["is_error"] != false
                    || value["subtype"] != "success"
                    || !self.initialized
                {
                    self.invalid = true;
                    return None;
                }
                let Some(text) = value["result"].as_str() else {
                    self.invalid = true;
                    return None;
                };
                self.result = Some(text.chars().take(16000).collect());
                self.tokens = tokens(&value["usage"]);
            }
            "assistant" => {
                self.call_usage(&value["message"]);
                let mut text = String::new();
                let mut remaining = 4000usize;
                let mut first = true;
                for block in value["message"]["content"].as_array()? {
                    if block["type"] == "text"
                        && let Some(part) = block["text"].as_str()
                    {
                        if !first && remaining > 0 {
                            text.push('\n');
                            remaining -= 1;
                        }
                        first = false;
                        for c in part
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                            .take(remaining)
                        {
                            text.push(c);
                            remaining -= 1;
                        }
                        if remaining == 0 {
                            break;
                        }
                    }
                }
                if !text.is_empty() {
                    return Some(Activity {
                        id: value["message"]["id"]
                            .as_str()
                            .unwrap_or("claude-response")
                            .chars()
                            .take(100)
                            .collect(),
                        kind: "message".into(),
                        text,
                        output: String::new(),
                        status: "running".into(),
                    });
                }
            }
            _ => {}
        }
        None
    }
    fn call_usage(&mut self, message: &Value) {
        if let Some(model) = model(&message["model"]) {
            self.model = Some(model);
        }
        let (Some(id), Some(usage)) = (message["id"].as_str(), tokens(&message["usage"])) else { return };
        match &mut self.call {
            // Later blocks of the same call carry its latest counts.
            Some((current, latest)) if current == id => *latest = usage,
            call => {
                if let Some((_, finished)) = call.replace((id.to_owned(), usage)) {
                    self.calls.get_or_insert_with(Tokens::default).add(finished);
                }
            }
        }
    }
    fn tools(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        fn clean<'a>(parts: impl Iterator<Item = &'a str>, limit: usize) -> String {
            parts
                .enumerate()
                .flat_map(|(i, part)| (i > 0).then_some('\n').into_iter().chain(part.chars()))
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                .take(limit)
                .collect()
        }
        let user = match value["type"].as_str() {
            Some("assistant") => false,
            Some("user") => true,
            _ => return,
        };
        for block in value["message"]["content"].as_array().into_iter().flatten() {
            let (id, name, output, status) = match (user, block["type"].as_str()) {
                (false, Some("tool_use")) => {
                    let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) else { continue };
                    let name = name.strip_prefix("mcp__control__").unwrap_or(name);
                    // A deferred tool's activity is recorded under the tool it called.
                    let name = if name == "control_call" { block["input"]["tool"].as_str().unwrap_or(name) } else { name };
                    let name = clean(std::iter::once(name), 2000);
                    if self.tools.len() >= 64 { self.tools.remove(0); }
                    self.tools.push((id.to_owned(), name.clone()));
                    (id, name, String::new(), "in_progress")
                }
                (true, Some("tool_result")) => {
                    let Some(id) = block["tool_use_id"].as_str() else { continue };
                    let Some(index) = self.tools.iter().position(|(known, _)| known == id) else { continue };
                    let name = self.tools.remove(index).1;
                    let content = &block["content"];
                    let output = match content.as_str() {
                        Some(text) => clean(std::iter::once(text), 1000),
                        None => clean(content.as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()), 1000),
                    };
                    (id, name, output, if block["is_error"] == true { "failed" } else { "completed" })
                }
                _ => continue,
            };
            emit(Event::Activity(Activity {
                id: id.chars().take(100).collect(),
                kind: "mcp_tool_call".into(),
                text: name,
                output,
                status: status.into(),
            }));
        }
    }
    fn finish_result(self) -> io::Result<(String, Option<Tokens>)> {
        if let Some(failure) = &self.failure {
            return Err(io::Error::other(format!("{failure}.")));
        }
        if self.bad_tools {
            return Err(io::Error::other(
                "Claude started with an unexpected tool configuration and was stopped before doing work.",
            ));
        }
        if self.invalid {
            return Err(io::Error::other(
                "Claude returned an invalid result or unexpected tool configuration",
            ));
        }
        self.result.map(|text| (text, self.tokens)).ok_or_else(|| {
            io::Error::other("Claude did not return a successful final result; check login or usage limits")
        })
    }
}

impl<'a> Claude<'a> {
    pub fn new(allowed_tools: &'a [&'a str]) -> Self {
        Self {
            allowed_tools,
            ..Self::default()
        }
    }
}
impl Decoder for Claude<'_> {
    fn decode(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        if let Some(activity) = self.event(value) {
            emit(Event::Activity(activity));
        }
        self.tools(value, emit);
        if self.bad_tools {
            emit(Event::RejectedTools);
        }
    }
    fn malformed(&mut self) {
        self.invalid = true;
    }
    /// The result event's totals when it arrived (success or error), else the
    /// sum of per-call usage: a lower bound for a turn stopped before its result.
    fn usage(&self) -> Usage {
        let model = self.model.clone();
        if let Some(total) = self.reported {
            return Usage { tokens: Some(total), complete: true, model };
        }
        let mut calls = self.calls;
        if let Some((_, current)) = &self.call {
            calls.get_or_insert_with(Tokens::default).add(*current);
        }
        Usage { tokens: calls, complete: false, model }
    }
    fn finish(self) -> io::Result<Output> {
        let (text, tokens) = self.finish_result()?;
        Ok(Output {
            text: Some(text),
            tokens,
        })
    }
}
