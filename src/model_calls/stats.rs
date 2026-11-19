//! Read-only llama.cpp server statistics for a configured private endpoint:
//! the Prometheus `/metrics` text (llama-server `--metrics`) and `/slots`.
//! Read only on demand, by `control models stats NAME` or while Manage → Models
//! is open; nothing polls in the background and nothing is stored.
use super::{Endpoint, Target, transport};
use serde::Serialize;
use serde_json::Value;
use std::{io, time::Duration};

/// Each statistics request is short; a busy server still answers these.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Parsing bounds: lines read, metric name length and slots counted.
const LINES: usize = 1024;
const NAME_BYTES: usize = 128;
const SLOTS: usize = 256;

/// Unlabelled llama.cpp counters and gauges Control reads; others are ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Metrics {
    /// Prompt tokens evaluated (excluding prefix-cache reuse) and their seconds.
    pub prompt_tokens: Option<f64>,
    pub prompt_seconds: Option<f64>,
    /// Prompt tokens served from a slot's cached prefix.
    pub prompt_tokens_cached: Option<f64>,
    pub predicted_tokens: Option<f64>,
    pub predicted_seconds: Option<f64>,
    pub requests_processing: Option<f64>,
    pub requests_deferred: Option<f64>,
}
impl Metrics {
    /// Parses Prometheus text exposition, bounded to `LINES` lines. Comments,
    /// labelled samples, unknown names and non-finite or negative values are
    /// skipped; a malformed line never fails the whole read.
    pub fn parse(text: &str) -> Self {
        let mut metrics = Self::default();
        for line in text.lines().take(LINES) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let (Some(name), Some(value)) = (fields.next(), fields.next()) else { continue };
            if name.len() > NAME_BYTES || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':') {
                continue;
            }
            let Some(value) = value.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0) else { continue };
            let slot = match name.strip_prefix("llamacpp:").unwrap_or(name) {
                "prompt_tokens_total" => &mut metrics.prompt_tokens,
                "prompt_seconds_total" => &mut metrics.prompt_seconds,
                "prompt_tokens_cached_total" => &mut metrics.prompt_tokens_cached,
                "tokens_predicted_total" => &mut metrics.predicted_tokens,
                "tokens_predicted_seconds_total" => &mut metrics.predicted_seconds,
                "requests_processing" => &mut metrics.requests_processing,
                "requests_deferred" => &mut metrics.requests_deferred,
                _ => continue,
            };
            slot.get_or_insert(value);
        }
        metrics
    }
    /// Average prompt throughput since the server started, tokens/s.
    pub fn prompt_rate(&self) -> Option<f64> {
        rate(self.prompt_tokens, self.prompt_seconds)
    }
    /// Average generation throughput since the server started, tokens/s.
    pub fn generation_rate(&self) -> Option<f64> {
        rate(self.predicted_tokens, self.predicted_seconds)
    }
    /// Share of prompt tokens reused from a cached prefix, 0–1.
    pub fn cache_share(&self) -> Option<f64> {
        let cached = self.prompt_tokens_cached?;
        let total = cached + self.prompt_tokens?;
        (total > 0.0).then(|| cached / total)
    }
    /// Share of the server's busy time spent processing prompts (prefill)
    /// rather than generating, 0–1, since start. A high share means prompts
    /// are re-processed instead of reused from the prefix cache.
    pub fn prefill_share(&self) -> Option<f64> {
        let prefill = self.prompt_seconds?;
        let busy = prefill + self.predicted_seconds?;
        (busy > 0.0).then(|| prefill / busy)
    }
}
fn rate(tokens: Option<f64>, seconds: Option<f64>) -> Option<f64> {
    let seconds = seconds.filter(|s| *s > 0.0)?;
    Some(tokens? / seconds)
}

/// What the server's slots hold: the latest prompt plus generated tokens per
/// slot stays in its KV cache for prefix reuse, so `context_tokens` of
/// `context_capacity` approximates KV cache use.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Slots {
    pub total: usize,
    pub busy: usize,
    pub context_tokens: u64,
    pub context_capacity: u64,
}
impl Slots {
    /// Parses llama-server's `/slots` array, bounded to `SLOTS` entries.
    pub fn parse(value: &Value) -> Option<Self> {
        let slots = value.as_array()?;
        let mut parsed = Self::default();
        for slot in slots.iter().take(SLOTS) {
            let count = |v: &Value| v.as_u64().unwrap_or(0).min(1 << 32);
            parsed.total += 1;
            parsed.busy += usize::from(slot["is_processing"].as_bool() == Some(true));
            let held = count(&slot["n_prompt_tokens"]) + count(&slot["next_token"][0]["n_decoded"]);
            let capacity = count(&slot["n_ctx"]);
            parsed.context_tokens += if capacity > 0 { held.min(capacity) } else { held };
            parsed.context_capacity += capacity;
        }
        Some(parsed)
    }
}

/// One endpoint's statistics. An absent part carries the reason instead.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Stats {
    pub model: String,
    pub url: String,
    pub reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slots: Option<Slots>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slots_error: Option<String>,
}

/// Reads `/metrics` and `/slots` once each, with short timeouts and the model
/// transport's response ceiling. CLI adapters have no server to read.
pub fn read(endpoint: &Endpoint) -> io::Result<Stats> {
    let Target::Endpoint(mut url) = endpoint.validate()? else {
        return Err(io::Error::other("CLI text models have no inference server statistics"));
    };
    let mut stats = Stats { model: endpoint.id.clone(), url: endpoint.url.clone(), ..Stats::default() };
    url.set_path("/metrics");
    match transport::get(&url, TIMEOUT) {
        Ok(body) => {
            stats.reachable = true;
            stats.metrics = Some(Metrics::parse(&String::from_utf8_lossy(&body)));
        }
        Err(error) => stats.metrics_error = Some(explain(&error, "--metrics", &mut stats.reachable)),
    }
    url.set_path("/slots");
    match transport::get(&url, TIMEOUT) {
        Ok(body) => {
            stats.reachable = true;
            match serde_json::from_slice(&body).ok().as_ref().and_then(Slots::parse) {
                Some(slots) => stats.slots = Some(slots),
                None => stats.slots_error = Some("malformed /slots response".into()),
            }
        }
        Err(error) => stats.slots_error = Some(explain(&error, "--slots", &mut stats.reachable)),
    }
    Ok(stats)
}
/// A short reason for a failed read. Any HTTP status, such as llama-server's
/// 501 for an endpoint it was started without, means the server answered.
fn explain(error: &io::Error, flag: &str, answered: &mut bool) -> String {
    let text = error.to_string();
    *answered |= text.contains("(HTTP ");
    if text.contains("(HTTP 501)") || text.contains("(HTTP 404)") {
        format!("off (restart the server with {flag}; nix run .#inference adds it)")
    } else if error.kind() == io::ErrorKind::TimedOut {
        "timed out".into()
    } else {
        text.chars().take(160).collect()
    }
}

fn tokens(count: u64) -> String {
    if count >= 1000 { format!("{:.1}k", count as f64 / 1000.0) } else { count.to_string() }
}
/// The compact one-line summary shown on the model's card and by the CLI.
pub fn line(stats: &Stats) -> String {
    if !stats.reachable {
        return format!("not answering at {}", stats.url);
    }
    let mut parts = Vec::new();
    match &stats.metrics {
        Some(m) => {
            let speed = |rate: Option<f64>| rate.map_or_else(|| "–".to_owned(), |r| format!("{r:.1}"));
            parts.push(format!("prompt {} tok/s · generation {} tok/s (avg since start)", speed(m.prompt_rate()), speed(m.generation_rate())));
            if let (Some(processing), Some(deferred)) = (m.requests_processing, m.requests_deferred) {
                parts.push(format!("{processing:.0} processing, {deferred:.0} queued"));
            }
            if let Some(share) = m.cache_share() {
                parts.push(format!("prefix cache {:.0}% of prompt tokens", share * 100.0));
            }
            if let (Some(prefilled), Some(share)) = (m.prompt_tokens, m.prefill_share()) {
                parts.push(format!("prefilled {} tokens, {:.0}% of busy time", tokens(prefilled as u64), share * 100.0));
            }
        }
        None => parts.push(format!("metrics {}", stats.metrics_error.as_deref().unwrap_or("unavailable"))),
    }
    match &stats.slots {
        Some(s) => {
            parts.push(format!("slots {}/{} busy", s.busy, s.total));
            if s.context_capacity > 0 {
                parts.push(format!("KV {}/{} tokens ({:.0}%)", tokens(s.context_tokens), tokens(s.context_capacity),
                    s.context_tokens as f64 * 100.0 / s.context_capacity as f64));
            }
        }
        None => parts.push(format!("slots {}", stats.slots_error.as_deref().unwrap_or("unavailable"))),
    }
    parts.join(" · ")
}

/// Live lines for every configured private endpoint, read in parallel, as
/// escaped HTML fragments keyed by model name (the page's `data-live` slots).
pub fn live(endpoints: &[Endpoint]) -> serde_json::Map<String, Value> {
    std::thread::scope(|scope| {
        let reads = endpoints.iter().filter(|e| e.cli.is_none()).map(|endpoint| {
            (endpoint.id.clone(), scope.spawn(|| read(endpoint)))
        }).collect::<Vec<_>>();
        reads.into_iter().map(|(id, read)| {
            let text = match read.join() {
                Ok(Ok(stats)) => line(&stats),
                Ok(Err(error)) => error.to_string(),
                Err(_) => "statistics unavailable".into(),
            };
            (id, Value::String(format!("<span class='model-stats-line'>{}</span>", crate::escape(&text))))
        }).collect()
    })
}

#[cfg(test)]
mod tests;
