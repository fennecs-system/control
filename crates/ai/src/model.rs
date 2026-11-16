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
    let text = choice["message"]["content"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= limits.output_bytes)
        .ok_or_else(|| io::Error::other("Missing or oversized completion text"))?
        .to_owned();
    let tokens = value.get("usage").and_then(|u| {
        Some(Tokens {
            input_tokens: u["prompt_tokens"].as_u64()?,
            cached_input_tokens: u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
            output_tokens: u["completion_tokens"].as_u64()?,
        })
    });
    Ok(Response { text, tokens })
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
