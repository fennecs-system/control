//! Host adapter for operator-configured text endpoints. Inference stays in tools.
use control_ai::model::{Limits, Request, Response, Served};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub id: String,
    pub model: String,
    pub url: String,
    /// Operator-declared runtime/weights identity; changing it invalidates caches.
    pub revision: String,
    pub limits: Limits,
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<crate::backend::Backend>,
    /// Operator declaration that the endpoint serves OpenAI-style tool calls
    /// (llama.cpp `--jinja` with a tool-capable template). It enables agent turns.
    /// Omitted when false, so existing summary cache identities are unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tools: bool,
    /// How much a reasoning model thinks during agent turns (the local loop
    /// adapts it per step around this level); summaries and generation never
    /// think. Omitted at the default (medium), so existing identities are unchanged.
    #[serde(default, skip_serializing_if = "control_ai::model::Thinking::is_default")]
    pub thinking: control_ai::model::Thinking,
}
impl Endpoint {
    pub fn validate(&self) -> io::Result<Target> {
        self.limits.validate()?;
        if !crate::application::valid_id(&self.id)
            || !crate::usage::valid_model(&self.model)
            || self.revision.is_empty()
            || self.revision.len() > 256
            || self.revision.chars().any(char::is_control)
            || !(1..=86400).contains(&self.timeout_seconds)
        {
            return Err(io::Error::other("Invalid text model metadata"));
        }
        if let Some(backend) = self.cli {
            if backend == crate::backend::Backend::Local {
                return Err(io::Error::other(
                    "Local text models use an endpoint URL, not a CLI adapter",
                ));
            }
            if !self.url.is_empty() || self.tools {
                return Err(io::Error::other(
                    "CLI text models do not accept an endpoint URL or tool-call declaration",
                ));
            }
            return Ok(Target::Cli(backend));
        }
        let url = reqwest::Url::parse(&self.url)
            .map_err(|_| io::Error::other("Invalid model endpoint"))?;
        let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
        let local = host == "localhost"
            || match host.parse::<std::net::IpAddr>() {
                Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private(),
                Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || ip.is_unique_local(),
                _ => false,
            };
        if !local
            || !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "/v1" | "/v1/")
        {
            return Err(io::Error::other(
                "Use a private/loopback model endpoint ending in /v1 without credentials or query parameters",
            ));
        }
        Ok(Target::Endpoint(url))
    }
    /// Operator-declared identity plus what the server reported about itself.
    pub fn provenance(&self, served: &Served) -> serde_json::Value {
        serde_json::json!({"id":self.id,"model":self.model,"revision":self.revision,
            "adapter":self.cli.map_or("private-endpoint",|b|b.id()),
            "served_model":served.model,"runtime_fingerprint":served.fingerprint})
    }
}

/// One completed bounded call, with the limits actually applied.
#[derive(Debug)]
pub struct Generation {
    pub response: Response,
    pub served: Served,
    pub limits: Limits,
    pub elapsed: Duration,
}

/// One bounded request, no retries, tools, credential forwarding or cloud fallback.
pub fn call(endpoint: &Endpoint, system: &str, input: &str) -> io::Result<Response> {
    generate(endpoint, system, input, None, &|| false).map(|g| g.response)
}

/// `output_tokens` may lower, never raise, the declared output ceiling. `cancel`
/// is polled while waiting; a cancelled plain-HTTP call closes its connection so
/// the server can stop generating. CLI adapters and HTTPS are checked only
/// before the request starts and remain bounded by the declared timeout.
pub fn generate(
    endpoint: &Endpoint,
    system: &str,
    input: &str,
    output_tokens: Option<u32>,
    cancel: &dyn Fn() -> bool,
) -> io::Result<Generation> {
    let limits = endpoint.limits.narrowed(output_tokens)?;
    let target = endpoint.validate()?;
    if cancel() {
        return Err(cancelled());
    }
    let started = Instant::now();
    let mut url = match target {
        Target::Cli(backend) => {
            let narrowed = Endpoint { limits: limits.clone(), ..endpoint.clone() };
            let response = cli::call(&narrowed, backend, system, input)?;
            return Ok(Generation { response, served: Served::default(), limits, elapsed: started.elapsed() });
        }
        Target::Endpoint(url) => url,
    };
    url.set_path("/v1/chat/completions");
    let body = Request {
        model: &endpoint.model,
        system,
        input,
        limits: &limits,
    }
    .chat_completions()?;
    // Summaries and generation answer directly: no reasoning spends the
    // output allowance, whatever the server's default.
    let mut body = body;
    control_ai::model::think(&mut body, 0);
    let body = serde_json::to_vec(&body).map_err(io::Error::other)?;
    let bytes = transport::post(&url, &body, Duration::from_secs(endpoint.timeout_seconds), cancel)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Malformed model response"))?;
    let served = control_ai::model::served(&value);
    let response = control_ai::model::decode(value, &limits)?;
    Ok(Generation { response, served, limits, elapsed: started.elapsed() })
}
/// One prepared chat-completions request to a private endpoint (agent tool loop).
/// Same transport, size ceiling, timeout and cancellation as `generate`.
pub fn post_json(
    endpoint: &Endpoint,
    body: &serde_json::Value,
    cancel: &dyn Fn() -> bool,
) -> io::Result<(serde_json::Value, Served)> {
    let Target::Endpoint(mut url) = endpoint.validate()? else {
        return Err(io::Error::other("Agent tool loops need a private endpoint"));
    };
    if cancel() {
        return Err(cancelled());
    }
    url.set_path("/v1/chat/completions");
    let body = serde_json::to_vec(body).map_err(io::Error::other)?;
    let bytes = transport::post(&url, &body, Duration::from_secs(endpoint.timeout_seconds), cancel)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Malformed model response"))?;
    let served = control_ai::model::served(&value);
    Ok((value, served))
}
fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Model call cancelled; no fallback was used")
}

mod cli;
#[path = "../tools/compaction-eval/mod.rs"]
mod evaluation;
pub mod stats;
pub mod tool;
mod transport;
pub enum Target {
    Endpoint(reqwest::Url),
    Cli(crate::backend::Backend),
}
pub fn recover() -> io::Result<()> {
    cli::recover()
}

const USAGE: &str = "Use control models test MODEL_ID, control models generate MODEL_ID [--max-output-tokens N] < prompt, control models bench MODEL_ID [RUNS], control models stats MODEL_ID [--json] or control models compare MODEL_ID...";
pub fn command(args: &[String]) -> io::Result<()> {
    crate::shutdown::install()?;
    if args.first().is_some_and(|s| s == "compare") {
        return evaluation::command(&args[1..]);
    }
    let (Some(action), Some(id)) = (args.first(), args.get(1)) else {
        return Err(io::Error::other(USAGE));
    };
    let mut runtime = crate::runtime::Runtime::default();
    crate::providers::load_settings(&mut runtime)?;
    let endpoint = runtime
        .compaction
        .models
        .iter()
        .find(|m| m.id == *id)
        .ok_or_else(|| io::Error::other("Text model is not configured"))?;
    let stop = || crate::shutdown::requested();
    match (action.as_str(), &args[2..]) {
        ("test", []) => {
            let response = call(
                endpoint,
                "You are testing a text-only model endpoint. No tools are available.",
                "Reply briefly with ready.",
            )?;
            println!(
                "{}",
                serde_json::json!({"model":endpoint.id,"revision":endpoint.revision,"response_bytes":response.text.len(),"tokens":response.tokens})
            );
        }
        ("generate", rest) => {
            let output_tokens = match rest {
                [] => None,
                [flag, value] if flag == "--max-output-tokens" => Some(
                    value.parse().map_err(|_| io::Error::other("Invalid output token count"))?,
                ),
                _ => return Err(io::Error::other(USAGE)),
            };
            let mut prompt = String::new();
            std::io::stdin()
                .take(endpoint.limits.input_bytes as u64 + 1)
                .read_to_string(&mut prompt)?;
            let generation = generate(endpoint, tool::SYSTEM, &prompt, output_tokens, &stop)?;
            println!("{}", tool::report(endpoint, &generation));
        }
        ("bench", rest) => {
            let runs = match rest {
                [] => 3,
                [runs] => runs.parse().ok().filter(|n| (1..=20).contains(n)).ok_or_else(|| io::Error::other("RUNS must be 1-20"))?,
                _ => return Err(io::Error::other(USAGE)),
            };
            println!("{}", tool::bench(endpoint, runs, &stop)?);
        }
        ("stats", rest) => {
            let json = match rest {
                [] => false,
                [flag] if flag == "--json" => true,
                _ => return Err(io::Error::other(USAGE)),
            };
            let stats = stats::read(endpoint)?;
            if json {
                println!("{}", serde_json::to_string(&stats).map_err(io::Error::other)?);
            } else {
                println!("{}: {}", endpoint.id, stats::line(&stats));
            }
        }
        _ => return Err(io::Error::other(USAGE)),
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
