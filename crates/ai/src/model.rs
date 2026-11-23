//! Bounded text-call contract. No tools, transport, credentials or executor.
use crate::Tokens;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub context_tokens: u32,
    pub output_tokens: u32,
    pub input_bytes: usize,
    pub output_bytes: usize,
}
impl Limits {
    pub fn validate(&self) -> io::Result<()> {
        if self.context_tokens < 1024
            || self.output_tokens == 0
            || self.output_tokens.saturating_add(512) >= self.context_tokens
            || self.input_bytes == 0
            || self.input_bytes > 1_048_576
            || self.output_bytes == 0
            || self.output_bytes > 65_536
        {
            return Err(io::Error::other("Invalid model context/output limits"));
        }
        Ok(())
    }
    /// A per-call output ceiling that can only lower the declared one.
    pub fn narrowed(&self, output_tokens: Option<u32>) -> io::Result<Self> {
        self.validate()?;
        match output_tokens {
            None => Ok(self.clone()),
            Some(0) => Err(io::Error::other("Requested output tokens must be positive")),
            Some(requested) => Ok(Self {
                output_tokens: requested.min(self.output_tokens),
                ..self.clone()
            }),
        }
    }
    /// Conservative byte bound, not a tokenizer or a claimed token count.
    pub fn input_budget(&self) -> usize {
        self.input_bytes.min(
            self.context_tokens
                .saturating_sub(self.output_tokens.saturating_add(512)) as usize,
        )
    }
}

pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub input: &'a str,
    pub limits: &'a Limits,
}
impl Request<'_> {
    pub fn chat_completions(&self) -> io::Result<Value> {
        self.limits.validate()?;
        if self.system.len().saturating_add(self.input.len()) > self.limits.input_budget() {
            return Err(io::Error::other(
                "Input exceeds the model's declared context budget; reduce the input or increase model limits",
            ));
        }
        Ok(
            json!({"model":self.model,"stream":false,"max_tokens":self.limits.output_tokens,
            "messages":[{"role":"system","content":self.system},{"role":"user","content":self.input}]}),
        )
    }
}

/// One completion's usage. OpenAI-style `usage` first; llama.cpp's `timings`
/// supply the prefix-cache hit (`cache_n`) when `prompt_tokens_details` is
/// absent, and the whole count when `usage` is. Cached tokens are part of input.
/// `None`: the server reported nothing, which is unknown rather than zero.
pub fn usage(value: &Value) -> Option<Tokens> {
    let timings = &value["timings"];
    let cache = timings["cache_n"].as_u64();
    let u = &value["usage"];
    if let (Some(input), Some(output)) = (u["prompt_tokens"].as_u64(), u["completion_tokens"].as_u64()) {
        let cached = u["prompt_tokens_details"]["cached_tokens"].as_u64().or(cache).unwrap_or(0);
        return Some(Tokens { input_tokens: input, cached_input_tokens: cached.min(input), output_tokens: output });
    }
    let processed = timings["prompt_n"].as_u64()?;
    let cached = cache.unwrap_or(0);
    Some(Tokens {
        input_tokens: processed.saturating_add(cached),
        cached_input_tokens: cached,
        output_tokens: timings["predicted_n"].as_u64()?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub text: String,
    pub tokens: Option<Tokens>,
}
pub fn decode(value: Value, limits: &Limits) -> io::Result<Response> {
    let choices = value["choices"]
        .as_array()
        .filter(|v| v.len() == 1)
        .ok_or_else(|| io::Error::other("Expected one text completion"))?;
    let choice = &choices[0];
    if choice["message"]["tool_calls"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
        || !choice["message"]["function_call"].is_null()
    {
        return Err(io::Error::other(
            "Tool-bearing text completion rejected; no tools are available",
        ));
    }
    if choice["finish_reason"] == "length" {
        return Err(io::Error::other(
            "Output reached the declared token limit; partial text was discarded",
        ));
    }
    if choice["finish_reason"] != "stop" {
        return Err(io::Error::other("Incomplete text completion rejected"));
    }
    // A think block left inline is never part of the answer.
    let text = choice["message"]["content"]
        .as_str()
        .map(|s| if s.contains("</think>") { split_reasoning(s).1.trim_start() } else { s })
        .filter(|s| !s.trim().is_empty() && s.len() <= limits.output_bytes)
        .ok_or_else(|| io::Error::other("Missing or oversized completion text"))?
        .to_owned();
    let tokens = usage(&value);
    Ok(Response { text, tokens })
}

/// One tool-loop request in the OpenAI-compatible chat format. Messages and tool
/// schemas are supplied by the host loop, which owns context-fit admission.
pub fn tool_request(model: &str, messages: &[Value], tools: &[Value], limits: &Limits) -> io::Result<Value> {
    limits.validate()?;
    Ok(json!({"model":model,"stream":false,"max_tokens":limits.output_tokens,
        "messages":messages,"tools":tools,"tool_choice":"auto"}))
}

/// How much a reasoning model (Qwen3-family chat templates on llama.cpp) may
/// think before it answers. Each level is a per-request token budget; `Off`
/// asks the template not to think at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
}
impl Thinking {
    pub const ALL: [Self; 5] = [Self::Off, Self::Minimal, Self::Low, Self::Medium, Self::High];
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.name() == name)
    }
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    /// Reasoning tokens a request at this level may spend.
    pub fn tokens(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Minimal => 128,
            Self::Low => 512,
            Self::Medium => 1536,
            Self::High => 4096,
        }
    }
    /// One level more, up to `High`; `Off` stays off.
    pub fn up(self) -> Self {
        match self {
            Self::Off => Self::Off,
            Self::Minimal => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium | Self::High => Self::High,
        }
    }
    /// One level less, down to `Minimal`; `Off` stays off.
    pub fn down(self) -> Self {
        match self {
            Self::Off => Self::Off,
            Self::Minimal | Self::Low => Self::Minimal,
            Self::Medium => Self::Low,
            Self::High => Self::Medium,
        }
    }
    /// The budget for one request whose declared output limit is
    /// `output_tokens`. Reasoning shares that allowance with the answer, so it
    /// gets at most half and the reply (tool-call arguments included) always
    /// keeps the other half. Under `MIN_BUDGET` thinking is off.
    pub fn budget(self, output_tokens: u32) -> u32 {
        let budget = self.tokens().min(output_tokens / 2);
        if budget < MIN_BUDGET { 0 } else { budget }
    }
}
/// The smallest reasoning budget worth opening a think block for.
pub const MIN_BUDGET: u32 = 64;

/// Sets llama.cpp's per-request reasoning controls (server b11669,
/// `tools/server/server-common.cpp`) on a chat-completions body:
/// `chat_template_kwargs.enable_thinking` switches the template's thinking on
/// or off, `reasoning_budget_tokens` bounds it (0 ends it at once; the server
/// forces the end tag when the budget runs out), and `reasoning_format:
/// "deepseek"` returns it as `message.reasoning_content`, apart from the
/// answer. Servers without these fields ignore them.
pub fn think(body: &mut Value, budget: u32) {
    body["chat_template_kwargs"] = json!({"enable_thinking": budget > 0});
    body["reasoning_format"] = json!("deepseek");
    body["reasoning_budget_tokens"] = json!(budget);
}
/// Separates a think block left inline in `content` (a server returning raw
/// `<think>…</think>` text) from the answer after it.
pub fn split_reasoning(content: &str) -> (Option<&str>, &str) {
    match content.find("</think>") {
        Some(end) => {
            let head = content[..end].trim_start();
            let thought = head.strip_prefix("<think>").unwrap_or(head).trim();
            ((!thought.is_empty()).then_some(thought), &content[end + "</think>".len()..])
        }
        None => (None, content),
    }
}
/// Reasoning tokens the server reports separately, when it does
/// (`usage.completion_tokens_details.reasoning_tokens`). They are already
/// counted in the output tokens.
pub fn reasoning_tokens(value: &Value) -> Option<u64> {
    value["usage"]["completion_tokens_details"]["reasoning_tokens"].as_u64()
}
/// A requested tool call. Arguments that are not a JSON object stay an error
/// for the loop to report back to the model; they are never executed.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Result<Value, String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub text: Option<String>,
    pub calls: Vec<Call>,
    /// The reply hit the output limit; its text and calls are not trustworthy.
    pub truncated: bool,
    pub tokens: Option<Tokens>,
    /// What the model thought before answering (`reasoning_content`, or an
    /// inline think block taken out of `text`). Never part of `text`.
    pub reasoning: Option<String>,
}
/// Decodes one assistant step. Only an unusable envelope is an error here;
/// malformed calls are returned for the loop's bounded repair policy.
pub fn decode_step(value: &Value, limits: &Limits) -> io::Result<Step> {
    let choices = value["choices"]
        .as_array()
        .filter(|v| v.len() == 1)
        .ok_or_else(|| io::Error::other("Expected one completion choice"))?;
    let message = &choices[0]["message"];
    if !message.is_object() {
        return Err(io::Error::other("Completion has no message"));
    }
    let mut reasoning = ["reasoning_content", "reasoning"]
        .iter()
        .find_map(|field| message[*field].as_str())
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_owned);
    let text = match &message["content"] {
        Value::Null => None,
        Value::String(content) => {
            let (inline, text) = split_reasoning(content);
            if text.len() > limits.output_bytes {
                return Err(io::Error::other("Oversized completion text"));
            }
            if reasoning.is_none() {
                reasoning = inline.map(str::to_owned);
            }
            Some(text.trim().to_owned()).filter(|t| !t.is_empty())
        }
        _ => return Err(io::Error::other("Completion content must be text")),
    };
    let calls = message["tool_calls"].as_array().into_iter().flatten().take(64).map(|call| {
        let function = &call["function"];
        let name = function["name"].as_str().unwrap_or("").chars().take(128).collect();
        let arguments = match &function["arguments"] {
            Value::String(raw) if raw.trim().is_empty() => Ok(json!({})),
            Value::String(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(value @ Value::Object(_)) => Ok(value),
                Ok(_) => Err("arguments must be a JSON object".to_owned()),
                Err(error) => Err(format!("arguments are not valid JSON ({error})")),
            },
            value @ Value::Object(_) => Ok(value.clone()),
            Value::Null => Ok(json!({})),
            _ => Err("arguments must be a JSON object".to_owned()),
        };
        Call {
            id: call["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 128).map(str::to_owned),
            name,
            arguments,
        }
    }).collect();
    Ok(Step { text, calls, truncated: choices[0]["finish_reason"] == "length", tokens: usage(value), reasoning })
}

/// Runtime identity the server reports about itself. It is untrusted metadata
/// recorded beside the operator-declared revision, never a substitute for it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Served {
    pub model: Option<String>,
    pub fingerprint: Option<String>,
}
pub fn served(value: &Value) -> Served {
    let field = |name: &str| {
        value[name]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .map(str::to_owned)
    };
    Served {
        model: field("model"),
        fingerprint: field("system_fingerprint"),
    }
}

#[cfg(test)]
mod tests;
