//! The local backend: an operator-configured OpenAI-compatible endpoint whose
//! tool loop Control runs itself (`Runs::Control`). This crate holds what that
//! loop decides without the host: how worker tools become function tools, how
//! results and notes enter the transcript, the token estimate, adaptive
//! thinking and per-call metering. The loop and its host stay in the root
//! for now; docs/CRATE-BOUNDARIES.md lists what still couples them.
use control_ai::Tokens;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub mod thinking;

/// Conservative token estimate for planning (about 3 UTF-8 bytes per token).
/// Real tokenizers usually do better; the server still enforces its own limit.
pub fn tokens(bytes: usize) -> u32 {
    u32::try_from(bytes.div_ceil(3)).unwrap_or(u32::MAX)
}
/// Context reserved outside the planned turn input: the role's always-loaded
/// tool schemas (under 10 KB; the rest load on demand), schemas loaded during
/// the turn, and the room the loop needs for one tool exchange before it compacts.
pub const OVERHEAD_TOKENS: u32 = CORE_TOKENS + LOADED_TOKENS + 1024;
/// Always-loaded schemas: under 10 KB for every role, about 3.4 k tokens.
pub const CORE_TOKENS: u32 = 4096;
/// Schemas a turn loads on demand; the loop still re-checks the actual size every step.
const LOADED_TOKENS: u32 = 2048;
pub const SYSTEM: &str = "You are an agent running inside Control on a local model. Act only through the provided tools; there is no other way to read files, run commands or reach rooms. Use control_workspace_run for short shell commands in /workspace. Call tools with arguments that are a JSON object matching the schema. Tools in the prompt's \"More tools\" index are callable by their full control_ name; Control loads the schema on first use. When the request is complete or you are waiting on someone, call control_turn_handoff if it is offered; otherwise reply with a brief plain-text final report and no tool call. Write each key fact or diagnosis you establish on its own line starting Finding: and each choice on a line starting Decision:; Control keeps those lines verbatim when it summarizes older steps, and control_history_read with a query searches the original older steps.";
const RESULT_BYTES: usize = 12000;

/// Per-call usage summed over the turn. Every call counts, including one that
/// failed or was cancelled in flight, so a missing report is never zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meter {
    pub tokens: Option<Tokens>,
    /// Model calls sent.
    pub calls: usize,
    /// Calls whose response reported usage.
    pub reported: usize,
    /// Reasoning tokens within the output tokens: as reported, else estimated
    /// from the returned reasoning text.
    pub reasoning: u64,
}
impl Meter {
    pub fn add(&mut self, tokens: Option<Tokens>) {
        if let Some(t) = tokens {
            self.tokens.get_or_insert_with(Tokens::default).add(t);
            self.reported += 1;
        }
    }
    /// Reported once the turn is over if every call reported; while it runs,
    /// or when some call did not report, the sum is a lower bound.
    pub fn usage(&self, over: bool, model: Option<String>) -> control_ai::Usage {
        control_ai::Usage {
            tokens: self.tokens,
            complete: over && self.reported == self.calls,
            model,
        }
    }
}

/// Keeps a tool description's opening sentences within a small byte budget:
/// schemas are re-sent every step and compete with history in a local context.
fn brief(description: &str) -> String {
    const LIMIT: usize = 320;
    if description.len() <= LIMIT {
        return description.to_owned();
    }
    let mut end = LIMIT;
    while !description.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &description[..end];
    match cut.rfind(". ") {
        Some(at) if at > 80 => cut[..=at].to_owned(),
        _ => format!("{}…", cut.trim_end()),
    }
}
/// MCP tool definitions become chat-completions function tools with unchanged
/// names and parameter schemas; long descriptions keep their opening sentences.
pub fn functions(definitions: &[Value]) -> Vec<Value> {
    definitions
        .iter()
        .filter_map(|tool| {
            let name = tool["name"].as_str()?;
            Some(json!({"type":"function","function":{"name":name,
                "description":brief(tool["description"].as_str().unwrap_or("")),
                "parameters":if tool["inputSchema"].is_object() {tool["inputSchema"].clone()} else {json!({"type":"object","properties":{}})}}}))
        })
        .collect()
}
/// Schemas a successful `control_tool_search` returned become native
/// function tools for the rest of this turn. Calls still go through the
/// worker dispatch, which alone decides whether they may run.
pub fn load(result: &Value, tools: &mut Vec<Value>, names: &mut BTreeSet<String>) {
    if result["isError"] == true {
        return;
    }
    let found = result["structuredContent"]["data"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for tool in functions(&found) {
        if let Some(name) = tool["function"]["name"].as_str()
            && names.insert(name.to_owned())
        {
            tools.push(tool);
        }
    }
}
/// A search result as the model reads it: every found schema is now one of
/// the request's function tools, so each is named with its first sentence.
pub fn searched(mut result: Value) -> Value {
    let Some(data) = result.get_mut("structuredContent").and_then(|s| s.get_mut("data")) else {
        return result;
    };
    let Some(found) = data.get("tools").and_then(Value::as_array).filter(|t| !t.is_empty()) else {
        return result;
    };
    let lines = found
        .iter()
        .map(|tool| {
            let text = tool["description"].as_str().unwrap_or("");
            let mut end = text.find(". ").map_or(text.len(), |at| at + 1).min(160);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            json!({"name":tool["name"],"description":&text[..end]})
        })
        .collect::<Vec<_>>();
    data["tools"] = json!(lines);
    let text = data.to_string();
    // The first item is the data; host notes follow it unchanged.
    if result["content"][0]["type"] == "text" {
        result["content"][0]["text"] = json!(text);
    }
    result
}
pub fn result_text(result: &Value) -> (String, bool) {
    let failed = result["isError"] == true || result.is_null();
    let mut text: String = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if result.is_null() {
        text = "No result was returned.".into();
    }
    if text.len() > RESULT_BYTES {
        let mut end = RESULT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated by Control; request a narrower result]");
    }
    (if failed { format!("ERROR: {text}") } else { text }, failed)
}
/// Appends a host note to the newest tool result, or as a user message.
pub fn append_note(messages: &mut Vec<Value>, note: &str) {
    match messages.last_mut() {
        Some(last) if last["role"] == "tool" => {
            let text = format!("{}\n\n{note}", last["content"].as_str().unwrap_or(""));
            last["content"] = Value::String(text);
        }
        _ => messages.push(json!({"role":"user","content":note})),
    }
}
/// The serialized size of `messages` in bytes.
pub fn size(messages: &[Value]) -> usize {
    serde_json::to_vec(messages).map_or(usize::MAX, |v| v.len())
}

/// An operator-configured private endpoint. Control runs the tool loop itself;
/// the CLI backends run their own.
pub struct Local;
impl control_agent::Agent for Local {
    fn id(&self) -> &'static str {
        "local"
    }
    fn label(&self) -> &'static str {
        "Local endpoint"
    }
    fn name(&self) -> &'static str {
        "Local"
    }
    fn default_model(&self) -> &'static str {
        ""
    }
    fn default_capable(&self) -> bool {
        false
    }
    /// The Codex worker image, used only for credential-free workspace commands.
    fn image(&self) -> control_agent::ImagePin {
        control_agent::WORKER_IMAGE
    }
    fn image_hint(&self) -> &'static str {
        NO_IMAGE
    }
    fn login(&self) -> Option<control_agent::Login> {
        None
    }
    fn probe(
        &self,
        _: &dyn control_agent::Node,
        _: &dyn control_isolation::Isolation,
        _image: &str,
    ) -> Result<(), String> {
        Err(NO_IMAGE.to_owned())
    }
    fn text_run(
        &self,
        _: &dyn control_agent::Node,
        _: &dyn control_isolation::Isolation,
        _call: &control_agent::TextCall<'_>,
    ) -> std::io::Result<control_isolation::RunSpec> {
        Err(std::io::Error::other(NOT_A_CLI))
    }
    fn text(
        &self,
        _reader: &mut dyn std::io::Read,
        _max: usize,
        _emit: &mut dyn FnMut(control_ai::Event),
    ) -> std::io::Result<control_ai::Output> {
        Err(std::io::Error::other(NOT_A_CLI))
    }
    fn host_shell(&self) -> bool {
        true
    }
    fn runs(&self) -> control_agent::Runs<'_> {
        control_agent::Runs::Control
    }
}
const NO_IMAGE: &str = "local endpoints run no worker image";
const NOT_A_CLI: &str = "Local endpoints are not CLI adapters";
