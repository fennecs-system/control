//! Antigravity CLI's `--output-format stream-json`: one `event`-tagged object
//! per line, `init` then `step_update`s then one `result` per turn, as
//! documented at https://antigravity.google/docs/cli/headless#streaming-json.
use crate::{Activity, Decoder, Event, Output, Tokens, Usage};
use serde_json::Value;
use std::io;

/// Control's MCP server name. Eager MCP tools are declared to the model as
/// `mcp_<server>_<tool>` (https://antigravity.google/docs/mcp#eager-loading).
const SERVER: &str = "control";
const EAGER: &str = "mcp_control_";
/// The built-in tool that calls a lazily loaded MCP tool by server and name.
const CALL: &str = "call_mcp_tool";

#[derive(Default)]
pub struct Antigravity {
    /// Whether Control's MCP tools are expected. A text call allows none.
    tools: bool,
    /// The response being streamed in the current `agent_response` step.
    text: String,
    messages: usize,
    calls: Vec<String>,
    reply: Option<String>,
    failure: Option<String>,
    /// The first tool outside Control's, named for the operator.
    unexpected: Option<String>,
    /// The first Control tool call the CLI refused, named for the operator.
    denied: Option<String>,
    invalid: bool,
    /// The `result` totals, else the per-step sum.
    reported: Option<Tokens>,
    steps: Option<Tokens>,
    model: Option<String>,
}
fn clean(text: &str, limit: usize) -> String {
    text.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').take(limit).collect()
}
/// `input_tokens` excludes cache reads: a cached turn reports input 278,
/// cache_read 30214 and total 282 in the documented stream. Control's input
/// includes cache reads, which are also shown as cached. `output_tokens`
/// already includes thinking (output 589, thinking 551 in the same page).
fn tokens(usage: &Value) -> Option<Tokens> {
    let (input, output) = (usage["input_tokens"].as_u64()?, usage["output_tokens"].as_u64()?);
    let cached = usage["cache_read_tokens"].as_u64().unwrap_or(0);
    Some(Tokens { input_tokens: input.saturating_add(cached), cached_input_tokens: cached, output_tokens: output })
}
/// Whether a tool error is the CLI refusing the call rather than the tool failing.
fn refused(error: &Value) -> bool {
    let text = format!("{} {}", error["type"].as_str().unwrap_or(""), error["message"].as_str().unwrap_or("")).to_ascii_lowercase();
    ["permission", "denied", "not allowed", "approval"].iter().any(|word| text.contains(word))
}
impl Antigravity {
    /// An agent turn, whose only tools are Control's over MCP.
    pub fn agent() -> Self {
        Self { tools: true, ..Self::default() }
    }
    /// A bounded text request: any tool is unexpected.
    pub fn text() -> Self {
        Self::default()
    }
    /// The Control tool a step called, or `None` for any other tool.
    fn control_tool(&self, name: &str, parameters: &Value) -> Option<String> {
        if !self.tools {
            return None;
        }
        let tool = match name.strip_prefix(EAGER) {
            Some(tool) => tool,
            None if name == CALL && parameters["ServerName"].as_str() == Some(SERVER) => parameters["ToolName"].as_str()?,
            None => return None,
        };
        // `control_call` names the deferred Control tool in its arguments.
        let arguments = if name == CALL { &parameters["Arguments"] } else { parameters };
        let tool = if tool == "control_call" { arguments["tool"].as_str().unwrap_or(tool) } else { tool };
        Some(clean(tool, 2000))
    }
    fn reject(&mut self, name: &str, emit: &mut dyn FnMut(Event)) {
        self.unexpected.get_or_insert_with(|| clean(name, 80));
        emit(Event::RejectedTools);
    }
    fn flush(&mut self, emit: &mut dyn FnMut(Event)) {
        let text = std::mem::take(&mut self.text);
        if text.trim().is_empty() {
            return;
        }
        self.messages += 1;
        emit(Event::Activity(Activity {
            id: format!("antigravity-response-{}", self.messages),
            kind: "message".into(),
            text: clean(&text, 4000),
            output: String::new(),
            status: "running".into(),
        }));
    }
    fn step(&mut self, step: &Value, emit: &mut dyn FnMut(Event)) {
        let done = match step["state"].as_str() {
            Some("DONE") => true,
            Some("ACTIVE") => false,
            _ => return self.invalid = true,
        };
        if done && let Some(usage) = tokens(&step["usage"]) {
            self.steps.get_or_insert_with(Tokens::default).add(usage);
        }
        // Subagents run their own tools out of Control's sight.
        if !step["subagent_info"].is_null() {
            return self.reject("subagent", emit);
        }
        match step["step_type"].as_str() {
            Some("agent_response") => {
                if let Some(delta) = step["text_delta"].as_str() {
                    let room = 16000usize.saturating_sub(self.text.len());
                    self.text.extend(delta.chars().take(room));
                }
                if done {
                    self.flush(emit);
                }
            }
            Some("tool") => self.tool(step, done, emit),
            Some("user_input" | "checkpoint") => {}
            Some(_) => {}
            None => self.invalid = true,
        }
    }
    fn tool(&mut self, step: &Value, done: bool, emit: &mut dyn FnMut(Event)) {
        let info = &step["tool_info"];
        let Some(name) = step["tool_name"].as_str().or(info["name"].as_str()) else {
            return self.invalid = true;
        };
        let Some(tool) = self.control_tool(name, &info["parameters"]) else {
            return self.reject(name, emit);
        };
        let id = format!("antigravity-step-{}", step["step_index"].as_u64().unwrap_or(0));
        let error = &info["error"];
        if done && !error.is_null() && refused(error) {
            self.denied.get_or_insert_with(|| tool.clone());
        }
        let status = match (done, error.is_null()) {
            (false, _) => "in_progress",
            (true, true) => "completed",
            (true, false) => "failed",
        };
        let output = match (&info["output"], error["message"].as_str()) {
            (_, Some(message)) => clean(message, 1000),
            (Value::String(text), None) => clean(text, 1000),
            (Value::Null, None) => String::new(),
            (other, None) => clean(&other.to_string(), 1000),
        };
        if !done {
            if self.calls.contains(&id) {
                return;
            }
            if self.calls.len() >= 64 {
                self.calls.remove(0);
            }
            self.calls.push(id.clone());
        } else {
            self.calls.retain(|known| known != &id);
        }
        emit(Event::Activity(Activity { id, kind: "mcp_tool_call".into(), text: tool, output, status: status.into() }));
    }
    fn result(&mut self, result: &Value, emit: &mut dyn FnMut(Event)) {
        if let Some(total) = tokens(&result["usage"]) {
            self.reported = Some(total);
        }
        self.flush(emit);
        if self.reply.is_some() || self.failure.is_some() {
            return self.invalid = true;
        }
        match result["status"].as_str() {
            Some("SUCCESS") => self.reply = Some(result["response"].as_str().unwrap_or("").chars().take(16000).collect()),
            status => {
                let detail = clean(result["error"].as_str().unwrap_or(""), 300);
                let detail = detail.lines().next().unwrap_or("");
                self.failure = Some(format!("Antigravity ended with {}{}{detail}", clean(status.unwrap_or("no status"), 40), if detail.is_empty() { "" } else { ": " }));
            }
        }
    }
}
impl Decoder for Antigravity {
    fn decode(&mut self, value: &Value, emit: &mut dyn FnMut(Event)) {
        match value["event"].as_str() {
            Some("init") => {
                if let Some(model) = value["init"]["model"].as_str() {
                    self.model = Some(clean(model, 100)).filter(|m| !m.is_empty());
                }
            }
            Some("step_update") => self.step(&value["step_update"], emit),
            Some("result") => self.result(&value["result"], emit),
            // Newer CLIs may add events; they carry no result.
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
            Some(total) => Usage { tokens: Some(total), complete: true, model },
            None => Usage { tokens: self.steps, complete: false, model },
        }
    }
    fn finish(self) -> io::Result<Output> {
        if let Some(tool) = &self.unexpected {
            return Err(io::Error::other(format!("Antigravity started with an unexpected tool ({tool}) and was stopped before doing work.")));
        }
        if let Some(failure) = &self.failure {
            return Err(io::Error::other(format!("{failure}.")));
        }
        // A refused call is not a failed one: the CLI exits 0 and reports
        // success, so the turn would otherwise look complete.
        if let Some(tool) = &self.denied {
            return Err(io::Error::other(format!("Antigravity ended with a refused tool call ({tool}): its permission rules did not allow a Control tool.")));
        }
        if self.invalid {
            return Err(io::Error::other("Antigravity returned an invalid event stream"));
        }
        let text = self.reply.ok_or_else(|| io::Error::other("Antigravity did not return a final result; check login or usage limits"))?;
        Ok(Output { text: Some(text), tokens: self.reported })
    }
}
#[cfg(test)]
mod tests;
