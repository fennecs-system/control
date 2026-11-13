//! Host adapter for operator-configured text endpoints. Inference stays in tools.
use control_ai::model::{Limits, Request, Response};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    time::Duration,
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
        if let Some(backend)=self.cli {
            if !self.url.is_empty() {
                return Err(io::Error::other(
                    "CLI text models do not accept an endpoint URL",
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
}

/// One bounded request, no retries, tools, credential forwarding or cloud fallback.
pub fn call(endpoint: &Endpoint, system: &str, input: &str) -> io::Result<Response> {
    let mut url = match endpoint.validate()? {
        Target::Cli(backend) => return cli::call(endpoint, backend, system, input),
        Target::Endpoint(url) => url,
    };
    url.set_path("/v1/chat/completions");
    let body = Request {
        model: &endpoint.model,
        system,
        input,
        limits: &endpoint.limits,
    }
    .chat_completions()?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(endpoint.timeout_seconds))
        .build()
        .map_err(|_| io::Error::other("Cannot create model transport"))?;
    let response = client
        .post(url)
        .json(&body)
        .send()
        .map_err(|_| io::Error::other("Model endpoint unavailable; no fallback was used"))?;
    if !response.status().is_success() {
        return Err(io::Error::other("Model endpoint rejected the request"));
    }
    let mut bytes = Vec::new();
    response.take(131073).read_to_end(&mut bytes)?;
    if bytes.len() > 131072 {
        return Err(io::Error::other("Model response exceeds size limit"));
    }
    let value =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Malformed model response"))?;
    control_ai::model::decode(value, &endpoint.limits)
}

mod cli;
pub enum Target {
    Endpoint(reqwest::Url),
    Cli(crate::backend::Backend),
}
pub fn recover() -> io::Result<()> {
    cli::recover()
}

pub fn command(args: &[String]) -> io::Result<()> {
    let [action, id] = args else {
        return Err(io::Error::other("Use control models test MODEL_ID"));
    };
    if action != "test" {
        return Err(io::Error::other("Use control models test MODEL_ID"));
    }
    let mut runtime = crate::runtime::Runtime::default();
    crate::providers::load_settings(&mut runtime)?;
    let endpoint = runtime
        .compaction
        .models
        .iter()
        .find(|m| m.id == *id)
        .ok_or_else(|| io::Error::other("Text model is not configured"))?;
    let response = call(
        endpoint,
        "You are testing a text-only model endpoint. No tools are available.",
        "Reply briefly with ready.",
    )?;
    println!(
        "{}",
        serde_json::json!({"model":endpoint.id,"revision":endpoint.revision,"response_bytes":response.text.len(),"tokens":response.tokens})
    );
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
