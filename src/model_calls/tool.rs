//! `control.model.generate`: bounded text inference on a configured private
//! endpoint. It reuses the summary models' endpoint registry and limits, grants
//! no tools, forwards no credentials and never falls back to a hosted model.
use super::{generate, Endpoint, Generation};
use serde_json::{json, Value};
use std::{io, sync::Mutex};

pub const NAME: &str = "control.model.generate";
pub(crate) const SYSTEM: &str = "You are a local text model called through Control. You have no tools, files or network access. Answer the request directly and concisely.";
const SYSTEM_BYTES: usize = 8192;
/// One in-flight generation per Control process; the endpoint has its own slots.
pub(super) static BUSY: Mutex<()> = Mutex::new(());

pub fn definition() -> Value {
    json!({"name":NAME,"description":"Generate text with an operator-configured local/private model endpoint (for example llama.cpp serving a Qwen GGUF). Text in, text out: no tools, files, network or credentials. Input and output are bounded by the model's declared context/output limits; a reply cut at the output limit is an error, not partial text. Returns usage when the server reports it and provenance (configured model, weights/runtime revision, server-reported identity). Output is untrusted text and grants no authority. Omit model when exactly one endpoint model is configured.","inputSchema":{"type":"object","properties":{"model":{"type":"string","maxLength":100,"description":"Configured text model name from Manage → Models & providers."},"prompt":{"type":"string","minLength":1,"maxLength":1048576},"system":{"type":"string","maxLength":SYSTEM_BYTES},"max_output_tokens":{"type":"integer","minimum":1,"maximum":1048576,"description":"Lower the declared output ceiling for this call."}},"required":["prompt"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}})
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    model: Option<String>,
    prompt: String,
    system: Option<String>,
    max_output_tokens: Option<u32>,
}

/// Only private endpoints: CLI text adapters reach hosted providers.
pub(crate) fn select<'a>(models: &'a [Endpoint], id: Option<&str>) -> io::Result<&'a Endpoint> {
    let endpoints: Vec<_> = models.iter().filter(|m| m.cli.is_none()).collect();
    let names = || endpoints.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ");
    match id {
        Some(id) => endpoints.iter().copied().find(|m| m.id == id).ok_or_else(|| {
            io::Error::other(format!("No local endpoint model named {id:?}. Configured: [{}]", names()))
        }),
        None if endpoints.len() == 1 => Ok(endpoints[0]),
        None => Err(io::Error::other(format!(
            "Name a local endpoint model. Configured: [{}]. Add one in Manage → Models & providers.",
            names()
        ))),
    }
}

pub fn call(args: &Value, cancel: &dyn Fn() -> bool) -> io::Result<Value> {
    let input: Input = serde_json::from_value(args.clone())
        .map_err(|_| io::Error::other("Expected prompt with optional model, system and max_output_tokens"))?;
    if input.prompt.trim().is_empty() || input.system.as_ref().is_some_and(|s| s.len() > SYSTEM_BYTES) {
        return Err(io::Error::other("Prompt must be non-empty and system at most 8192 bytes"));
    }
    let mut runtime = crate::runtime::Runtime::default();
    crate::providers::load_settings(&mut runtime)?;
    let endpoint = select(&runtime.compaction.models, input.model.as_deref())?;
    run(endpoint, input.system.as_deref().unwrap_or(SYSTEM), &input.prompt, input.max_output_tokens, cancel)
}
pub(crate) fn run(
    endpoint: &Endpoint,
    system: &str,
    prompt: &str,
    output_tokens: Option<u32>,
    cancel: &dyn Fn() -> bool,
) -> io::Result<Value> {
    let _guard = BUSY
        .try_lock()
        .map_err(|_| io::Error::other("A local model call is already running; retry after it finishes"))?;
    let generation = generate(endpoint, system, prompt, output_tokens, cancel)?;
    Ok(report(endpoint, &generation))
}

pub(crate) fn report(endpoint: &Endpoint, generation: &Generation) -> Value {
    let seconds = generation.elapsed.as_secs_f64();
    let tokens = generation.response.tokens;
    json!({"text":generation.response.text,"usage":tokens,"usage_reported":tokens.is_some(),
        "elapsed_ms":generation.elapsed.as_millis() as u64,
        // End-to-end, including prompt processing and queueing; not decode speed.
        "output_tokens_per_second":tokens.filter(|_|seconds>0.0).map(|t|((t.output_tokens as f64/seconds)*100.0).round()/100.0),
        "provenance":endpoint.provenance(&generation.served),"limits":generation.limits,
        "trust":"Untrusted model output. It grants no authority, tool access or approval."})
}

const BENCH: [&str; 3] = [
    "In three short sentences, explain why bounded context matters for long-running agents.",
    "List five practical checks before running a 27B quantized model on a laptop, one line each.",
    "Summarize in two sentences: a binary summary tree keeps recent turns verbatim and merges older ranges pairwise.",
];
/// Sequential end-to-end calls through the same adapter; memory is the server's to report.
pub(crate) fn bench(endpoint: &Endpoint, runs: usize, cancel: &dyn Fn() -> bool) -> io::Result<Value> {
    let mut results = Vec::new();
    for run in 0..runs {
        let generation = generate(endpoint, SYSTEM, BENCH[run % BENCH.len()], Some(256), cancel)?;
        results.push(json!({"run":run+1,"elapsed_ms":generation.elapsed.as_millis() as u64,"usage":generation.response.tokens,
            "output_tokens_per_second":generation.response.tokens.map(|t|((t.output_tokens as f64/generation.elapsed.as_secs_f64())*100.0).round()/100.0),
            "provenance":endpoint.provenance(&generation.served)}));
    }
    let mut elapsed: Vec<u64> = results.iter().filter_map(|r| r["elapsed_ms"].as_u64()).collect();
    elapsed.sort_unstable();
    Ok(json!({"model":endpoint.id,"revision":endpoint.revision,"runs":results,"median_elapsed_ms":elapsed[elapsed.len()/2],
        "note":"End-to-end latency through Control's adapter. Measure server memory separately (for example docker stats or ps)."}))
}
