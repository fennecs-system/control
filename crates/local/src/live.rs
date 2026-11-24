//! A local turn on the shared host. The loop's effects go only through
//! `control_agent::Host`, as every backend's do: worker tools through
//! [`Host::serve`], compaction and recall through the host's compactor and
//! summary tree, activity and usage through [`Events`]. The model is reached
//! over the turn's [`Endpoint`] with this crate's transport, as a CLI reaches
//! its own provider.
use crate::{
    ADAPTER, Meter,
    tool_loop::{self, Effects, Outcome as Finished, Policy, Poll},
    transport,
};
use control_agent::{Cancelled, Control, Endpoint, Events, Failure, Host, Outcome, Stop, Turn};
use control_ai::model::Served;
use serde_json::{Value, json};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

/// The attempt's effects for one turn. Each call is one dynamic call into the
/// host, as before the loop moved: the loop itself is generic over this type.
struct Live<'a> {
    host: &'a mut dyn Host,
    events: Arc<dyn Events>,
    /// The endpoint's chat-completions URL.
    url: reqwest::Url,
    timeout: Duration,
    started: Instant,
    /// Seconds between the attempt's admission and the loop's start.
    admitted: u64,
    served: Option<Served>,
    meter: Meter,
    /// Attempt start in nanoseconds; prefixes request nonces.
    stamp: u128,
    /// The latest step's reasoning, handed to the host if the turn is cut off.
    reasoning: Option<String>,
}
impl Live<'_> {
    /// What the server says it is: its `model`, else its runtime fingerprint.
    fn reported_model(&self) -> Option<String> {
        self.served
            .as_ref()
            .and_then(|s| s.model.clone().or_else(|| s.fingerprint.clone()))
    }
}
impl Effects for Live<'_> {
    fn tools(&mut self) -> io::Result<Vec<Value>> {
        let response = self.host.serve(
            &format!("{}-0", self.stamp),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        response["result"]["tools"].as_array().cloned().ok_or_else(|| {
            io::Error::other(
                response["error"]["message"]
                    .as_str()
                    .unwrap_or("tool listing refused")
                    .to_owned(),
            )
        })
    }
    fn call(&mut self, name: &str, arguments: &Value, nonce: &str) -> Value {
        // Unique across every attempt of the run: a numeric attempt stamp prefix.
        let nonce = format!("{}-{nonce}", self.stamp);
        let response = self.host.serve(
            &nonce,
            json!({"jsonrpc":"2.0","id":nonce,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
        );
        match response.get("result") {
            Some(result) => result.clone(),
            None => {
                json!({"content":[{"type":"text","text":response["error"]["message"].as_str().unwrap_or("Tool call refused")}],"isError":true})
            }
        }
    }
    fn poll(&mut self) -> Poll {
        if self.host.design_requested() {
            return Poll::Stop(Failure::Waiting);
        }
        if self.host.cancelled() {
            return Poll::Stop(Failure::Cancelled(Cancelled::Running));
        }
        match self
            .host
            .poll(self.started.elapsed() + Duration::from_secs(self.admitted))
        {
            Control::Continue => Poll::Continue,
            Control::Finish(summary) => Poll::Finish(summary),
            Control::TimedOut(limit) => Poll::Stop(Failure::TimedOut(limit)),
        }
    }
    fn model(&mut self, body: &Value) -> io::Result<Value> {
        let host = &*self.host;
        let cancel = || host.cancelled() || host.design_requested();
        if cancel() {
            return Err(transport::cancelled());
        }
        let body = serde_json::to_vec(body).map_err(io::Error::other)?;
        let bytes = transport::post(&self.url, &body, self.timeout, &cancel)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Malformed model response"))?;
        self.served.get_or_insert(control_ai::model::served(&value));
        Ok(value)
    }
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String> {
        self.host.summarize(evidence, words)
    }
    fn record(&mut self, kind: &str, id: &str, text: &str, output: &str, status: &str) {
        self.events.activity(control_ai::Activity {
            id: id.chars().take(100).collect(),
            kind: kind.into(),
            text: text.chars().take(4000).collect(),
            output: output.chars().take(4000).collect(),
            status: status.into(),
        });
    }
    fn continues(&mut self) -> bool {
        self.host.continues()
    }
    fn metered(&mut self, meter: &Meter) {
        self.meter = *meter;
        // Live totals: one in-memory update per model call, never a save.
        self.events.usage(&meter.usage(false, self.reported_model()));
    }
    fn observation(&mut self, first: bool) -> Option<String> {
        self.host.observation(first)
    }
    fn inbox(&mut self) -> Option<String> {
        self.host.inbox()
    }
    fn reasoned(&mut self, reasoning: &str) {
        self.reasoning = Some(reasoning.to_owned());
    }
    fn retain(
        &mut self,
        block: control_comonad::Block,
        text: &str,
        children: Option<[String; 2]>,
        sources: Vec<String>,
    ) -> Option<String> {
        // Recall is best effort: a failed write leaves the step unsearchable,
        // never the turn failed; the record says so.
        match self.host.retain(block, text, children, sources) {
            Ok(key) => Some(key),
            Err(error) => {
                self.record(
                    "compaction",
                    "local-recall",
                    &format!(
                        "Could not keep steps {}–{} for recall: {error}",
                        block.start + 1,
                        block.end()
                    ),
                    "",
                    "failed",
                );
                None
            }
        }
    }
}

const NO_ENDPOINT: &str = "The selected local endpoint is no longer configured; no fallback was used.";

/// One turn on a local endpoint. The host prepared and fenced the workspace;
/// it quiesces command jobs and publishes after a successful return.
pub(crate) fn turn(turn: Turn<'_>, host: &mut dyn Host) -> Outcome {
    if !turn.tools {
        return Failure::Blocked("Local agent turns need a live Control node.".to_owned()).into();
    }
    let (Some(endpoint), Some(compactor)) = (turn.endpoint, turn.compactor) else {
        return Failure::Blocked(NO_ENDPOINT.to_owned()).into();
    };
    let Ok(mut url) = reqwest::Url::parse(&endpoint.url) else {
        return Failure::Blocked(NO_ENDPOINT.to_owned()).into();
    };
    url.set_path("/v1/chat/completions");
    let policy = Policy {
        max_steps: turn.steps,
        thinking: endpoint.thinking,
        resumed: turn.resumed,
        ..Policy::default()
    };
    let events = host.events();
    let mut live = Live {
        host,
        events,
        url,
        timeout: endpoint.timeout,
        started: Instant::now(),
        admitted: control_agent::admitted(turn.started_unix),
        served: None,
        meter: Meter::default(),
        stamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        reasoning: None,
    };
    let result = tool_loop::run(
        &mut live,
        &endpoint.model,
        &endpoint.limits,
        compactor,
        &turn.prompt,
        &policy,
    );
    finish(live, &endpoint, result)
}

/// The turn's ending as the host records it.
fn finish(mut live: Live<'_>, endpoint: &Endpoint, result: Result<Finished, Failure>) -> Outcome {
    // Cut off (time limit, step cap, provider failure): the latest reasoning
    // goes into the host checkpoint the host writes for such a failure, so the
    // next turn continues that plan.
    if result.is_err()
        && let Some(reasoning) = live.reasoning.take()
    {
        live.host.reasoned(&reasoning);
    }
    let (meter, reported_model) = (live.meter, live.reported_model());
    if let Ok(outcome) = &result {
        let served = live.served.take().unwrap_or_default();
        let provenance =
            control_ai::model::provenance(&endpoint.id, &endpoint.model, &endpoint.revision, ADAPTER, &served);
        live.events.activity(control_ai::Activity {
            id: "local-model".into(),
            kind: "local_model".into(),
            text: summary(&provenance, outcome, meter.reasoning),
            output: String::new(),
            status: "completed".into(),
        });
    }
    // The loop has ended: stop attempt-owned tool containers, as the CLI
    // backends do when their harness exits. Persistent services are separate.
    let result = match live.host.stop(Stop::Live) {
        Ok(()) => result.map(|outcome| outcome.text),
        Err(message) => Err(Failure::Interrupted(message)),
    };
    // Whatever the ending, keep what the calls reported: exact when every sent
    // call reported (also after a failure or a cancel between steps), a lower
    // bound when one did not (cancelled in flight, no usage). No call sent
    // (refused admission, cancelled first) is not launched.
    match (meter.calls, result) {
        (0, Err(failure)) => Outcome::NotLaunched(failure),
        (0, result) => Outcome::Launched { result, usage: None },
        (_, result) => Outcome::Launched {
            result,
            usage: Some(meter.usage(true, reported_model)),
        },
    }
}

/// The turn's one-line account: endpoint, steps, compactions, time and tokens.
fn summary(provenance: &Value, outcome: &Finished, reasoning: u64) -> String {
    let seconds = outcome.elapsed.as_secs_f64().max(0.001);
    format!(
        "{} · {} model steps · {} compactions · {:.1} s{}",
        provenance,
        outcome.steps,
        outcome.compactions,
        seconds,
        outcome
            .tokens
            .map(|t| format!(
                " · {} in ({} cached) / {} out tokens{} · {:.1} output tokens/s end-to-end",
                t.input_tokens,
                t.cached_input_tokens,
                t.output_tokens,
                if reasoning > 0 {
                    format!(" ({reasoning} reasoning)")
                } else {
                    String::new()
                },
                t.output_tokens as f64 / seconds
            ))
            .unwrap_or_else(|| " · usage not reported".into())
    )
}
