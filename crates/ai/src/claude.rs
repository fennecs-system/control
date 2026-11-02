use crate::{Activity, Decoder, Event, Output, Tokens};
use serde_json::Value;
use std::io;
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
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
}
impl Claude<'_> {
    fn event(&mut self, value: &Value) -> Option<Activity> {
        match value["type"].as_str()? {
            "system" if value["subtype"] == "init" => {
                self.initialized = true;
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
                let usage = &value["usage"];
                if let (Some(input), Some(output)) = (
                    usage["input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                ) {
                    let cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    let created = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    self.tokens = Some(Tokens {
                        input_tokens: input.saturating_add(cached).saturating_add(created),
                        cached_input_tokens: cached,
                        output_tokens: output,
                    });
                }
            }
            "assistant" => {
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
                    let name = clean(std::iter::once(name.strip_prefix("mcp__control__").unwrap_or(name)), 2000);
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
            return Err(fail(&format!("{failure}.")));
        }
        if self.bad_tools {
            return Err(fail(
                "Claude started with an unexpected tool configuration and was stopped before doing work.",
            ));
        }
        if self.invalid {
            return Err(fail(
                "Claude returned an invalid result or unexpected tool configuration",
            ));
        }
        self.result.map(|text| (text, self.tokens)).ok_or_else(|| {
            fail("Claude did not return a successful final result; check login or usage limits")
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
    fn finish(self) -> io::Result<Output> {
        let (text, tokens) = self.finish_result()?;
        Ok(Output {
            text: Some(text),
            tokens,
        })
    }
}
