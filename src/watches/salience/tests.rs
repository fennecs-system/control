use super::*;
use serde_json::json;

fn salience(filter: Value) -> Salience {
    let filter: Filter = serde_json::from_value(filter).unwrap();
    filter.validate().unwrap();
    Salience::new(filter)
}
/// Feeds `(seconds, payload)` events and returns the woken sequence numbers,
/// releasing due holds first as `Watch::observe_at` does.
fn feed(salience: &mut Salience, events: &[(f64, Value)]) -> Vec<u64> {
    let mut woke = Vec::new();
    for (index, (at, payload)) in events.iter().enumerate() {
        let (seq, now) = (index as u64 + 1, millis(*at));
        salience.release(now, |seq, _| woke.push(seq));
        if salience.admits(seq, payload.to_string().as_bytes(), now) { woke.push(seq); }
    }
    woke
}
fn values(field: &str, series: &[(f64, f64)]) -> Vec<(f64, Value)> {
    series.iter().map(|&(t, v)| (t, json!({field.trim_start_matches('/'): v}))).collect()
}

#[test]
fn threshold_passes_bounds_and_serves_as_severity_passthrough() {
    let mut s = salience(json!({"rules":[[{"kind":"threshold","field":"/severity","at_least":5}],[{"kind":"threshold","field":"/hp","at_most":4}]]}));
    let woke = feed(&mut s, &[(0.0, json!({"severity":2,"hp":10})), (1.0, json!({"severity":5,"hp":10})), (2.0, json!({"severity":1,"hp":4}))]);
    assert_eq!(woke, [2, 3]);
    assert_eq!((s.counters.woke, s.counters.retained), (2, 1));
}

#[test]
fn change_compares_with_the_last_woken_value_not_the_last_seen() {
    let mut s = salience(json!({"rules":[[{"kind":"change","field":"/price","percent":2}]]}));
    // 1.5% steps: each is small, but they accumulate past 2% from the baseline.
    let woke = feed(&mut s, &values("/price", &[(0.0, 100.0), (1.0, 101.5), (2.0, 103.0), (3.0, 104.0), (4.0, 100.5)]));
    assert_eq!(woke, [1, 3, 5], "first value wakes; then moves of at least 2% from 100 and 103");
    let mut s = salience(json!({"rules":[[{"kind":"change","field":"/x","absolute":0.5}]]}));
    assert_eq!(feed(&mut s, &values("/x", &[(0.0, 0.0), (1.0, 0.4), (2.0, -0.6), (3.0, -0.5)])), [1, 3]);
}

#[test]
fn change_without_amounts_deduplicates_any_value_including_long_digested_text() {
    let mut s = salience(json!({"rules":[[{"kind":"change","field":"/hash"}]]}));
    let long = "x".repeat(500);
    let woke = feed(&mut s, &[(0.0, json!({"hash":"a"})), (1.0, json!({"hash":"a"})), (2.0, json!({"hash":long})), (3.0, json!({"hash":long})), (4.0, json!({"hash":{"n":1}}))]);
    assert_eq!(woke, [1, 3, 5]);
    let state = serde_json::to_string(&s).unwrap();
    assert!(!state.contains(&long) && state.len() < 1024, "long values are stored as digests: {state}");
}

#[test]
fn rate_measures_change_per_second_across_its_window() {
    let mut s = salience(json!({"rules":[[{"kind":"rate","field":"/hp","seconds":10,"at_least":2}]]}));
    // Slow drift, then a fall of 12 within a second (14 over the 6 s window).
    let woke = feed(&mut s, &values("/hp", &[(0.0, 20.0), (5.0, 19.0), (9.0, 18.0), (10.0, 17.0), (11.0, 5.0)]));
    assert_eq!(woke, [5]);
    // At high event rates, decimated samples still span the window.
    let mut s = salience(json!({"rules":[[{"kind":"rate","field":"/v","seconds":10,"at_least":1}]]}));
    let series = (0..1000).map(|i| (f64::from(i) * 0.01, f64::from(i) * 0.005)).collect::<Vec<_>>();
    assert!(feed(&mut s, &values("/v", &series)).is_empty(), "0.5/s never reaches 1/s");
    assert!(s.partitions[0].stages[0].samples.len() <= RATE_SAMPLES);
}

#[test]
fn zscore_flags_anomalies_after_warming_up_its_window() {
    let mut s = salience(json!({"rules":[[{"kind":"zscore","field":"/latency","samples":8,"at_least":3}]]}));
    let mut series = [10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 50.0, 10.0].iter().enumerate().map(|(i, &v)| (i as f64, v)).collect::<Vec<_>>();
    assert_eq!(feed(&mut s, &values("/latency", &series)), [8]);
    assert!(s.partitions[0].stages[0].samples.len() <= 8);
    // A flat signal: any departure is anomalous; a short history is not judged.
    let mut s = salience(json!({"rules":[[{"kind":"zscore","field":"/v","samples":5,"at_least":3}]]}));
    series = vec![(0.0, 99.0), (1.0, 1.0), (2.0, 1.0), (3.0, 1.0), (4.0, 1.0), (5.0, 1.0), (6.0, 2.0)];
    assert_eq!(feed(&mut s, &values("/v", &series)), [7]);
}

/// Run -128's reflex jitter: a threat score oscillating around one threshold
/// toggled flee/fight every tick. Hysteresis fires once and commits.
#[test]
fn hysteresis_fires_once_and_rearms_only_after_a_sustained_clear() {
    let mut s = salience(json!({"rules":[[{"kind":"hysteresis","field":"/threat","trigger":0.8,"clear":0.5,"clear_seconds":3}]]}));
    let jitter = [(0.0, 0.2), (1.0, 0.81), (1.1, 0.79), (1.2, 0.82), (1.3, 0.6), (1.4, 0.85),
        (2.0, 0.4), (3.0, 0.9), // cleared for 1 s only: still committed
        (4.0, 0.3), (5.0, 0.4), (8.0, 0.45), // cleared from 4 s through 8 s
        (8.5, 0.95)];
    assert_eq!(feed(&mut s, &values("/threat", &jitter)), [2, 12]);
    // Low side: trigger below clear (health), and a clear that lasted until the next sample.
    let mut s = salience(json!({"rules":[[{"kind":"hysteresis","field":"/hp","trigger":4,"clear":10,"clear_seconds":2}]]}));
    assert_eq!(feed(&mut s, &values("/hp", &[(0.0, 3.0), (1.0, 12.0), (5.0, 2.0), (5.5, 11.0), (6.0, 3.0)])), [1, 3]);
}

#[test]
fn debounce_wakes_with_the_latest_event_after_quiet_and_runs_later_stages_then() {
    let mut s = salience(json!({"rules":[[{"kind":"change","field":"/hash"},{"kind":"debounce","seconds":5}]]}));
    let events = [(0.0, json!({"hash":"a"})), (1.0, json!({"hash":"b"})), (2.0, json!({"hash":"c"})), (9.0, json!({"hash":"c"}))];
    assert_eq!(feed(&mut s, &events), [3], "flapping changes settle into one wake with the latest");
    assert_eq!(s.holding(), 0);
    // A quiet routine still wakes: the supervisor pass releases due holds.
    let mut s = salience(json!({"rules":[[{"kind":"debounce","seconds":2},{"kind":"rate_limit","seconds":60}]]}));
    assert!(!s.admits(1, b"{}", 0));
    let mut woke = Vec::new();
    s.release(1_999, |seq, _| woke.push(seq));
    assert!(woke.is_empty() && s.holding() == 1);
    s.release(2_000, |seq, _| woke.push(seq));
    assert_eq!((woke.as_slice(), s.counters.woke), ([1].as_slice(), 1));
    // The rate limit after the debounce is evaluated at release.
    assert!(!s.admits(2, b"{}", 3_000));
    let mut released = 0;
    s.release(5_000, |_, _| released += 1);
    assert_eq!((released, s.counters.woke, s.counters.retained), (0, 1, 2));
}

/// Price monitoring: notify on a >2% move at most hourly, unless the move is >5%.
#[test]
fn rules_compose_rate_limits_with_a_passthrough() {
    let hour = 3600.0;
    let mut s = salience(json!({"key":"/symbol","rules":[
        [{"kind":"change","field":"/price","percent":5}],
        [{"kind":"change","field":"/price","percent":2},{"kind":"rate_limit","seconds":hour}],
    ]}));
    let tick = |t: f64, symbol: &str, price: f64| (t, json!({"symbol":symbol,"price":price}));
    let woke = feed(&mut s, &[
        tick(0.0, "A", 100.0), tick(0.0, "B", 50.0), // first sightings
        tick(60.0, "A", 103.0),                       // 3%: within the hour
        tick(120.0, "A", 106.0),                      // 6%: passthrough
        tick(hour + 120.0, "A", 108.2),               // 2.1% from 106, after an hour
        tick(hour + 130.0, "B", 50.5),                // 1%: B's own baseline
    ]);
    assert_eq!(woke, [1, 2, 4, 5]);
    assert_eq!(s.partitions.len(), 2);
}

#[test]
fn stage_order_decides_what_later_stages_see() {
    let series = values("/v", &[(0.0, 20.0), (1.0, 0.0), (2.0, 20.0)]);
    let hysteresis = json!({"kind":"hysteresis","field":"/v","trigger":15,"clear":5});
    let threshold = json!({"kind":"threshold","field":"/v","at_least":10});
    let mut signal_first = salience(json!({"rules":[[hysteresis.clone(), threshold.clone()]]}));
    assert_eq!(feed(&mut signal_first, &series), [1, 3], "hysteresis saw the clear and re-armed");
    let mut gate_first = salience(json!({"rules":[[threshold, hysteresis]]}));
    assert_eq!(feed(&mut gate_first, &series), [1], "the threshold hid the clear from hysteresis");
    assert_eq!(gate_first.counters.retained, 2);
    // Delivery stages compare with the last wake from any rule.
    let mut s = salience(json!({"rules":[[{"kind":"threshold","field":"/v","at_least":100}],[{"kind":"rate_limit","seconds":10}]]}));
    assert_eq!(feed(&mut s, &values("/v", &[(0.0, 1.0), (5.0, 100.0), (12.0, 1.0), (16.0, 1.0)])), [1, 2, 4]);
}

#[test]
fn unreadable_payloads_wake_by_default_but_can_be_retained() {
    let mut s = salience(json!({"rules":[[{"kind":"threshold","field":"/price","at_least":1}]]}));
    let woke = feed(&mut s, &[(0.0, json!({"task_id":"build-54","outcome":"failed"})), (1.0, json!({"price":"n/a"})), (2.0, json!({"price":0}))]);
    assert_eq!(woke, [1, 2], "completion notices without the field still wake");
    assert!(s.admits(9, b"not json", 3_000));
    let mut s = salience(json!({"rules":[[{"kind":"threshold","field":"/price","at_least":1}]],"unmatched":"retain"}));
    assert!(!s.admits(1, b"not json", 0));
    assert_eq!(s.counters.retained, 1);
}

#[test]
fn keys_are_bounded_and_eviction_spares_pending_holds() {
    let mut s = salience(json!({"key":"/id","rules":[[{"kind":"debounce","seconds":100}]]}));
    for id in 0..20u64 { s.admits(id + 1, json!({"id": id}).to_string().as_bytes(), id); }
    assert_eq!((s.partitions.len(), s.holding()), (MAX_KEYS, MAX_KEYS));
    assert!(serde_json::to_string(&s).unwrap().len() < 4096);
}

#[test]
fn state_survives_restart_mid_hysteresis_and_mid_debounce() {
    let mut s = salience(json!({"rules":[
        [{"kind":"hysteresis","field":"/threat","trigger":0.8,"clear":0.5,"clear_seconds":3}],
        [{"kind":"change","field":"/page"},{"kind":"debounce","seconds":10}],
    ]}));
    assert!(s.admits(1, br#"{"threat":0.9}"#, 0));
    assert!(!s.admits(2, br#"{"threat":0.2,"page":"v2"}"#, 1_000));
    let mut restored: Salience = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(restored, s);
    assert!(!restored.admits(3, br#"{"threat":0.95}"#, 2_000), "still committed after restart");
    let mut woke = Vec::new();
    restored.release(11_000, |seq, preview| woke.push((seq, preview.to_owned())));
    assert_eq!(woke, [(2, r#"{"threat":0.2,"page":"v2"}"#.to_owned())]);
}

#[test]
fn field_lookup_matches_json_pointer_semantics() {
    let value = json!({"a":{"b":[10,{"c":3}]},"x/y":1,"m~n":2,"":4});
    for pointer in ["", "/a", "/a/b/0", "/a/b/1/c", "/a/b/01", "/a/b/+1", "/a/b/2", "/x~1y", "/m~0n", "/", "/a/b/1/c/d", "/missing"] {
        assert_eq!(lookup(&value, pointer), value.pointer(pointer), "{pointer}");
    }
    let mut s = salience(json!({"key":"/m/0","rules":[[{"kind":"threshold","field":"/data/levels/1/hp","at_most":4}]]}));
    assert!(s.admits(1, br#"{"m":["zombie"],"data":{"levels":[{"hp":20},{"hp":3}]}}"#, 0));
    assert_eq!(s.partitions[0].key, "zombie");
}

#[test]
fn invalid_filters_are_rejected_before_a_watch_starts() {
    for filter in [
        json!({"rules":[]}),
        json!({"rules":[[]]}),
        json!({"rules":[[{"kind":"threshold","field":"/x"}]]}),
        json!({"rules":[[{"kind":"threshold","field":"x","at_least":1}]]}),
        json!({"rules":[[{"kind":"change","field":"/x","percent":-1}]]}),
        json!({"rules":[[{"kind":"debounce","seconds":0}]]}),
        json!({"rules":[[{"kind":"rate_limit","seconds":1e9}]]}),
        json!({"rules":[[{"kind":"hysteresis","field":"/x","trigger":1,"clear":1}]]}),
        json!({"rules":[[{"kind":"zscore","field":"/x","samples":4,"at_least":3}]]}),
        json!({"rules":[[{"kind":"zscore","field":"/x","samples":64,"at_least":3},{"kind":"rate","field":"/x","seconds":1,"at_least":1}]]}),
        json!({"rules":vec![json!([{"kind":"debounce","seconds":1}]); 5]}),
        json!({"rules":[vec![json!({"kind":"debounce","seconds":1}); 7]]}),
    ] {
        let parsed = serde_json::from_value::<Filter>(filter.clone());
        assert!(parsed.map_or(true, |f| f.validate().is_err()), "{filter}");
    }
    for filter in [json!({"rules":[[{"kind":"unknown"}]]}), json!({"rules":[[{"kind":"debounce","seconds":1,"extra":1}]]}), json!({"rules":[[]],"extra":1})] {
        assert!(serde_json::from_value::<Filter>(filter.clone()).is_err(), "{filter}");
    }
}
