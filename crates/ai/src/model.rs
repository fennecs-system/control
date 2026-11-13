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
                "Input exceeds declared compactor context budget; reduce leaf size or increase model limits",
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
    if choice["finish_reason"] != "stop"
        || choice["message"]["tool_calls"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        || !choice["message"]["function_call"].is_null()
    {
        return Err(io::Error::other(
            "Incomplete or tool-bearing summary rejected",
        ));
    }
    let text = choice["message"]["content"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= limits.output_bytes)
        .ok_or_else(|| io::Error::other("Missing or oversized summary text"))?
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

#[cfg(test)]
mod tests;
