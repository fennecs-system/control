//! Notification delivery timing: `Watch::observe` is called under the state
//! lock for every framed event a follower reads, so it is on the hot path.
//! Run with `--release -- --ignored --nocapture`; set
//! `CONTROL_SALIENCE_PROFILE_EVENTS` for a longer sample.
use super::*;
use std::{hint::black_box, time::Instant};

fn frames(count: u64) -> Vec<Frame> {
    let payloads = (0..1024u32)
        .map(|i| {
            format!(
                r#"{{"symbol":"S{}","price":{:.3},"severity":{},"note":"synthetic tick {i}"}}"#,
                i % 4,
                100.0 + (f64::from(i) * 0.37).sin() * 3.0,
                i % 10,
            )
            .into_bytes()
        })
        .collect::<Vec<_>>();
    (1..=count)
        .map(|seq| Frame {
            seq,
            event: Event::Notification(payloads[seq as usize % 1024].clone()),
        })
        .collect()
}

fn median(label: &str, frames: &[Frame], make: impl Fn() -> Watch) {
    let mut samples = (0..9)
        .map(|_| {
            let mut watch = make();
            let start = Instant::now();
            for frame in frames {
                black_box(watch.observe(black_box(frame)));
            }
            black_box(&watch);
            start.elapsed().as_nanos() as f64 / frames.len() as f64
        })
        .collect::<Vec<_>>();
    samples.sort_by(f64::total_cmp);
    println!(
        "{label}: median {:.1} ns/event (min {:.1}, max {:.1}) over {} events x 9",
        samples[4],
        samples[0],
        samples[8],
        frames.len()
    );
}

#[test]
#[ignore = "timing fixture"]
fn notification_observe_profile() {
    let count = std::env::var("CONTROL_SALIENCE_PROFILE_EVENTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let frames = frames(count);
    median("unfiltered", &frames, || Watch {
        notification_channel: true,
        ..watch()
    });
    for (label, filter) in [
        (
            "threshold",
            serde_json::json!({"rules":[[{"kind":"threshold","field":"/severity","at_least":9}]]}),
        ),
        (
            "price (key, 2 rules)",
            serde_json::json!({"key":"/symbol","rules":[[{"kind":"change","field":"/price","percent":5}],[{"kind":"change","field":"/price","percent":2},{"kind":"rate_limit","seconds":3600}]]}),
        ),
        (
            "windows (rate, zscore, hysteresis)",
            serde_json::json!({"rules":[[{"kind":"rate","field":"/price","seconds":10,"at_least":5}],[{"kind":"zscore","field":"/price","samples":32,"at_least":3}],[{"kind":"hysteresis","field":"/price","trigger":102.9,"clear":100,"clear_seconds":1}]]}),
        ),
        (
            "debounce",
            serde_json::json!({"rules":[[{"kind":"change","field":"/price"},{"kind":"debounce","seconds":5}]]}),
        ),
    ] {
        let filter: super::salience::Filter = serde_json::from_value(filter).unwrap();
        filter.validate().unwrap();
        median(label, &frames, || Watch {
            notification_channel: true,
            salience: Some(Box::new(Salience::new(filter.clone()))),
            ..watch()
        });
    }
    // Context: the follower also appends every frame to its spool. Same
    // calls as `append`, in a scratch directory rather than Control's state.
    let path = crate::test_support::scratch("control-salience-profile").with_extension("log");
    let sample = &frames[..frames.len().min(20_000)];
    let start = Instant::now();
    for frame in sample {
        let _ = fs::metadata(&path);
        let mut file = fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        file.write_all(&frame.encode()).unwrap();
    }
    println!(
        "spool append (context): {:.1} ns/event over {} events",
        start.elapsed().as_nanos() as f64 / sample.len() as f64,
        sample.len()
    );
    let _ = fs::remove_file(path);
}
