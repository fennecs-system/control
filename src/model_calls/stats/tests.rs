use super::*;
use std::io::{BufRead, Write};

/// Recorded from the pinned llama.cpp (b11669-620d222) started by
/// `nix run .#inference` flags with `--metrics`, after two short requests.
const METRICS: &str = include_str!("metrics-sample.txt");
/// `/slots` from the same server (one slot, ctx 1024).
const SLOTS: &str = include_str!("slots-sample.json");

/// A mock server answering `/metrics` then `/slots` with the given status/body.
fn serve(replies: [(&'static str, &'static str, &'static str); 2]) -> (Endpoint, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for (path, (status, mime, body)) in ["/metrics", "/slots"].into_iter().zip(replies) {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, format!("GET {path} HTTP/1.1\r\n"));
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" { break; }
                assert!(!line.to_ascii_lowercase().starts_with("authorization"));
            }
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let mut endpoint = crate::model_calls::tests::endpoint();
    endpoint.url = format!("http://{address}/v1");
    (endpoint, server)
}

#[test]
fn recorded_llama_metrics_parse_into_rates_cache_share_and_queue() {
    let metrics = Metrics::parse(METRICS);
    assert_eq!(metrics, Metrics {
        prompt_tokens: Some(59.0),
        prompt_seconds: Some(12.4125),
        prompt_tokens_cached: Some(51.0),
        predicted_tokens: Some(8.0),
        predicted_seconds: Some(2.36842),
        requests_processing: Some(0.0),
        requests_deferred: Some(0.0),
    });
    assert!((metrics.prompt_rate().unwrap() - 4.753).abs() < 0.01);
    assert!((metrics.generation_rate().unwrap() - 3.378).abs() < 0.01);
    assert!((metrics.cache_share().unwrap() - 51.0 / 110.0).abs() < 1e-9);
    assert!((metrics.prefill_share().unwrap() - 12.4125 / (12.4125 + 2.36842)).abs() < 1e-9);
}

#[test]
fn metrics_parsing_is_bounded_and_skips_malformed_lines() {
    let text = concat!(
        "# TYPE llamacpp:prompt_tokens_total counter\n",
        "llamacpp:prompt_tokens_total{slot=\"0\"} 999\n", // labelled: skipped
        "llamacpp:prompt_tokens_total NaN\n",
        "llamacpp:prompt_tokens_total -4\n",
        "llamacpp:prompt_tokens_total\n",
        "llamacpp:prompt$tokens_total 5\n",
        "llamacpp:prompt_tokens_total 100 1700000000000\n", // optional timestamp
        "llamacpp:prompt_tokens_total 7\n",                 // first sample wins
        "llamacpp:prompt_seconds_total 0\n",
        "llamacpp:requests_deferred +Inf\n",
    );
    let metrics = Metrics::parse(text);
    assert_eq!(metrics.prompt_tokens, Some(100.0));
    assert_eq!(metrics.prompt_rate(), None, "no rate without elapsed seconds");
    assert_eq!(metrics.requests_deferred, None);
    assert_eq!(metrics.cache_share(), None);
    // Only the first LINES lines are read and an overlong name is ignored.
    let mut long = "x 1\n".repeat(LINES);
    long.push_str("llamacpp:requests_processing 3\n");
    assert_eq!(Metrics::parse(&long).requests_processing, None);
    let name = format!("llamacpp:{} 1\n", "a".repeat(NAME_BYTES));
    assert_eq!(Metrics::parse(&name), Metrics::default());
}

#[test]
fn recorded_slots_report_busy_slots_and_context_held() {
    let slots = Slots::parse(&serde_json::from_str(SLOTS).unwrap()).unwrap();
    assert_eq!(slots, Slots { total: 1, busy: 0, context_tokens: 58, context_capacity: 1024 });
    let busy = serde_json::json!([
        {"n_ctx": 16384, "is_processing": true, "n_prompt_tokens": 9000, "next_token": [{"n_decoded": 120}]},
        {"n_ctx": 16384, "is_processing": false, "n_prompt_tokens": 99999},
        "junk",
    ]);
    assert_eq!(Slots::parse(&busy).unwrap(), Slots { total: 3, busy: 1, context_tokens: 9120 + 16384, context_capacity: 32768 });
    assert_eq!(Slots::parse(&serde_json::json!({"error": "x"})), None);
}

#[test]
fn reads_a_live_server_and_formats_the_cli_line() {
    let (endpoint, server) = serve([("200 OK", "text/plain; version=0.0.4", METRICS), ("200 OK", "application/json", SLOTS)]);
    let stats = read(&endpoint).unwrap();
    server.join().unwrap();
    assert!(stats.reachable);
    assert_eq!(line(&stats), "prompt 4.8 tok/s · generation 3.4 tok/s (avg since start) · 0 processing, 0 queued · prefix cache 46% of prompt tokens · prefilled 59 tokens, 84% of busy time · slots 0/1 busy · KV 58/1.0k tokens (6%)");
    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["metrics"]["prompt_tokens_cached"], 51.0);
    assert_eq!(json["slots"]["context_capacity"], 1024);
    assert!(json.get("metrics_error").is_none());
}

#[test]
fn a_server_without_metrics_still_reports_slots_and_how_to_enable_them() {
    // What the running service answers before it is restarted with --metrics.
    let refusal = r#"{"error":{"code":501,"message":"This server does not support metrics endpoint. Start it with `--metrics`","type":"not_supported_error"}}"#;
    let (endpoint, server) = serve([("501 Not Implemented", "application/json", refusal), ("200 OK", "application/json", SLOTS)]);
    let stats = read(&endpoint).unwrap();
    server.join().unwrap();
    assert!(stats.reachable);
    assert_eq!(line(&stats), "metrics off (restart the server with --metrics; nix run .#inference adds it) · slots 0/1 busy · KV 58/1.0k tokens (6%)");
}

#[test]
fn an_endpoint_that_does_not_answer_is_reported_once() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut endpoint = crate::model_calls::tests::endpoint();
    endpoint.url = format!("http://{address}/v1");
    let stats = read(&endpoint).unwrap();
    assert!(!stats.reachable);
    assert_eq!(line(&stats), format!("not answering at http://{address}/v1"));
    // The page's fragments are escaped HTML keyed by model name.
    let fragments = live(std::slice::from_ref(&endpoint));
    assert_eq!(fragments[&endpoint.id], format!("<span class='model-stats-line'>not answering at http://{address}/v1</span>"));
}

#[test]
fn cli_adapters_have_no_server_statistics() {
    let mut endpoint = crate::model_calls::tests::endpoint();
    (endpoint.cli, endpoint.url) = (Some(crate::backend::Backend::Codex), String::new());
    assert!(read(&endpoint).unwrap_err().to_string().contains("no inference server statistics"));
    assert!(live(&[endpoint]).is_empty());
}
