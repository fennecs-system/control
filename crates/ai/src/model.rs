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

/// One tool-loop request in the OpenAI-compatible chat format. Messages and tool
/// schemas are supplied by the host loop, which owns context-fit admission.
pub fn tool_request(model: &str, messages: &[Value], tools: &[Value], limits: &Limits) -> io::Result<Value> {
    limits.validate()?;
    Ok(json!({"model":model,"stream":false,"max_tokens":limits.output_tokens,
        "messages":messages,"tools":tools,"tool_choice":"auto"}))
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
    let text = match &message["content"] {
        Value::Null => None,
        Value::String(text) if text.len() > limits.output_bytes => {
            return Err(io::Error::other("Oversized completion text"));
        }
        Value::String(text) => Some(text.trim().to_owned()).filter(|t| !t.is_empty()),
        _ => return Err(io::Error::other("Completion content must be text")),
    };
    let mut calls = Vec::new();
    for call in message["tool_calls"].as_array().into_iter().flatten().take(64) {
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
        calls.push(Call {
            id: call["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 128).map(str::to_owned),
            name,
            arguments,
        });
    }
    let tokens = value.get("usage").and_then(|u| {
        Some(Tokens {
            input_tokens: u["prompt_tokens"].as_u64()?,
            cached_input_tokens: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
            output_tokens: u["completion_tokens"].as_u64()?,
        })
    });
    Ok(Step { text, calls, truncated: choices[0]["finish_reason"] == "length", tokens })
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
