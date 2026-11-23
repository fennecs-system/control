use super::*;

#[test]
#[ignore = "release startup checkpoint timing fixture; use fresh scratch config/state directories, no models"]
fn profile_checkpoint_load() {
    use std::{hint::black_box, time::Instant};
    assert!(std::env::var_os("CONTROL_STATE_DIR").is_some());
    assert!(std::env::var_os("CONTROL_CONFIG_DIR").is_some());
    let directory = crate::config::state_directory().unwrap();
    fs::create_dir_all(&directory).unwrap();
    assert!(!directory.join("state.json").exists(), "use a fresh scratch directory");
    save(&State::initial()).unwrap();
    black_box(load().unwrap());
    let bytes = fs::metadata(directory.join("state.json")).unwrap().len();
    let mut samples = Vec::new();
    for round in 0..51 {
        let start = Instant::now();
        black_box(load().unwrap());
        if round != 0 {
            samples.push(start.elapsed().as_nanos());
        }
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"case":"checkpoint_load", "state_bytes":bytes,
        "median_ns":(samples[24]+samples[25])/2, "min_ns":samples[0], "p95_ns":samples[46]})
    );
}

/// One tool call's state change in a busy swarm: room posts and agent
/// messages, watch deliveries, tool receipts on running turns, new turns and
/// finished turns, in a fixed 7:5:4:2:2 mix over six active chats.
fn swarm_step(state: &mut State, step: usize, chats: &[usize], active: &mut Vec<usize>) {
    let filler = |n: usize| format!("step {step} ").repeat(n / 8 + 1);
    match step % 20 {
        0..=6 => {
            let chat = &mut state.chats[chats[step % chats.len()]];
            chat.messages.push(crate::chat::Message {
                speaker: Default::default(),
                text: filler(400),
                run_id: None,
                reply: String::new(),
            });
            chat.revision += 1;
        }
        7..=11 if !state.watches.is_empty() => {
            let count = state.watches.len();
            let watch = &mut state.watches[step % count];
            (watch.seen, watch.lines, watch.line_seq) = (watch.seen + 1, watch.lines + 1, watch.line_seq + 1);
        }
        16..=17 => {
            state.runtime.enqueue(filler(300), RunKind::Task, "benchmark");
            let index = state.runtime.runs.len() - 1;
            state.runtime.runs[index].status = "running".into();
            state.runtime.runs[index]
                .attempts
                .push(crate::usage::Attempt::default());
            active.push(index);
        }
        18..=19 if active.len() > 6 => {
            let run = &mut state.runtime.runs[active.remove(0)];
            run.status = "completed".into();
            *run.result = filler(800);
        }
        _ => {
            let run = &mut state.runtime.runs[active[step % active.len()]];
            run.attempts
                .last_mut()
                .expect("benchmark runs have an attempt")
                .activity
                .push(crate::usage::Activity {
                    id: format!("tool-{step}"),
                    kind: "tool".into(),
                    text: filler(120),
                    output: filler(600),
                    status: "completed".into(),
                });
        }
    }
}
fn quantiles(values: &mut [u128]) -> serde_json::Value {
    values.sort_unstable();
    let at = |q: usize| values[(values.len() - 1) * q / 100];
    serde_json::json!({"p50_us": at(50) / 1000, "p99_us": at(99) / 1000, "max_us": values[values.len() - 1] / 1000})
}

/// A simulated swarm hour on a real-size state, whole-state rewrite ("before")
/// against the patch log ("after"), then cold loads and crash recovery.
#[test]
#[ignore = "release benchmark: CONTROL_PROFILE_STATE=<copy of a state.json>, CONTROL_PROFILE_DIR=<scratch dir on the measured filesystem>; optional CONTROL_PROFILE_SAVES (2000/hour) and CONTROL_PROFILE_MODE=before|after; no models"]
fn profile_swarm_hour() {
    use std::{hint::black_box, time::Instant};
    let source = std::env::var("CONTROL_PROFILE_STATE").expect("a copy of a state.json");
    let root = PathBuf::from(std::env::var("CONTROL_PROFILE_DIR").expect("a scratch directory"));
    let saves: usize = std::env::var("CONTROL_PROFILE_SAVES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let modes =
        std::env::var("CONTROL_PROFILE_MODE").map_or(vec!["before".to_owned(), "after".to_owned()], |mode| vec![mode]);
    let data = fs::read(source).unwrap();
    let start = Instant::now();
    let initial: State = serde_json::from_slice(&data).unwrap();
    println!(
        "{}",
        serde_json::json!({"case": "monolithic_parse", "state_bytes": data.len(), "ms": start.elapsed().as_secs_f64() * 1e3})
    );
    let (mut plain, mut ranged, mut cached, mut compared, mut indexed) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let base = crate::history::persisting(|| snapshot::encode(&initial)).unwrap();
    let fresh = |f: &dyn Fn()| crate::history::uncached(|| crate::history::persisting(f));
    for _ in 0..21 {
        let start = Instant::now();
        fresh(&|| drop(black_box(serde_json::to_vec(&initial).unwrap())));
        plain.push(start.elapsed().as_nanos());
        let start = Instant::now();
        fresh(&|| drop(black_box(snapshot::encode(&initial).unwrap())));
        ranged.push(start.elapsed().as_nanos());
        let start = Instant::now();
        let encoded = black_box(crate::history::persisting(|| snapshot::encode(&initial)).unwrap());
        cached.push(start.elapsed().as_nanos());
        let start = Instant::now();
        black_box(snapshot::index(encoded.bytes().to_vec()).unwrap());
        indexed.push(start.elapsed().as_nanos());
        let start = Instant::now();
        black_box(snapshot::diff(&base, &encoded));
        compared.push(start.elapsed().as_nanos());
    }
    println!(
        "{}",
        serde_json::json!({"case": "encode_stages", "to_vec": quantiles(&mut plain), "encode_with_ranges": quantiles(&mut ranged), "encode_cached": quantiles(&mut cached), "index_bytes": quantiles(&mut indexed), "diff_unchanged": quantiles(&mut compared)})
    );
    let mut by_size: Vec<usize> = (0..initial.chats.len()).collect();
    by_size.sort_by_key(|&i| std::cmp::Reverse(initial.chats[i].messages.len()));
    let chats = &by_size[..6.min(by_size.len())];
    for mode in modes {
        let directory = root.join(&mode);
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let mut state: State = serde_json::from_slice(&data).unwrap();
        let mut active = Vec::new();
        for i in 0..6 {
            swarm_step(&mut state, 16 + i * 20, chats, &mut active);
        }
        let encode = |state: &State| encode(state, &archived(&directory)).unwrap();
        persist(&directory, encode(&state), true).unwrap();
        let blocks = || -> std::collections::HashMap<PathBuf, u64> {
            fs::read_dir(directory.join("history/runs"))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| (e.path(), e.metadata().map_or(0, |m| m.len())))
                .collect()
        };
        if mode != "before" {
            // The first save fills the run cache, the second moves finished runs to blocks.
            let start = Instant::now();
            checkpoint_in(&directory, &state).unwrap();
            checkpoint_in(&directory, &state).unwrap();
            let archived_runs = state.runtime.runs.iter().filter(|run| run.archived()).count();
            println!(
                "{}",
                serde_json::json!({"case": "archive_migration", "ms": start.elapsed().as_secs_f64() * 1e3, "archived_runs": archived_runs, "hot_runs": state.runtime.runs.len() - archived_runs,
                "block_bytes": blocks().values().sum::<u64>(), "blocks": blocks().len(), "snapshot_bytes": fs::metadata(directory.join("state.json")).unwrap().len()})
            );
            persist(&directory, encode(&state), true).unwrap();
        }
        let (mut written, mut latencies, mut compactions) = (0u64, Vec::new(), 0);
        let log_length = || fs::metadata(directory.join(LOG)).map_or(0, |m| m.len());
        for step in 0..saves {
            swarm_step(&mut state, step, chats, &mut active);
            let before = log_length();
            let known = blocks();
            let start = Instant::now();
            if mode == "before" {
                // The old path: a fresh serialization (no cached runs) and a full rewrite.
                let bytes =
                    crate::history::uncached(|| crate::history::persisting(|| serde_json::to_vec(black_box(&state))))
                        .unwrap();
                write_checkpoint(&directory, &bytes).unwrap();
                written += bytes.len() as u64;
            } else {
                checkpoint_in(&directory, black_box(&state)).unwrap();
            }
            latencies.push(start.elapsed().as_nanos());
            if mode != "before" {
                written += blocks()
                    .iter()
                    .filter(|(path, _)| !known.contains_key(*path))
                    .map(|(_, size)| size)
                    .sum::<u64>();
                let after = log_length();
                if after >= before {
                    written += after - before;
                } else {
                    compactions += 1;
                    written += fs::metadata(directory.join("state.json")).unwrap().len() + after;
                }
            }
        }
        println!(
            "{}",
            serde_json::json!({
                "case": "swarm_hour", "mode": mode, "saves": saves, "final_state_bytes": crate::history::persisting(|| serde_json::to_vec(&state)).unwrap().len(),
                "bytes_written": written, "bytes_per_save": written / saves as u64, "compactions": compactions, "save": quantiles(&mut latencies),
            })
        );
        if mode == "before" {
            continue;
        }
        // Cold load of the node as the hour left it: snapshot plus log replay.
        let expected = crate::history::persisting(|| serde_json::to_value(&state)).unwrap();
        let reload = || {
            published().lock().unwrap().remove(&directory);
            let start = Instant::now();
            let (loaded, recovered) = recover(&directory, read_checkpoint(&directory).unwrap().unwrap()).unwrap();
            let adopted = adopt(&directory, recovered.unwrap());
            (loaded, adopted, start.elapsed().as_secs_f64() * 1e3)
        };
        let lines = fs::read(directory.join(LOG))
            .unwrap()
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        let (loaded, adopted, ms) = reload();
        assert!(!adopted && crate::history::persisting(|| serde_json::to_value(&loaded).unwrap()) == expected);
        println!(
            "{}",
            serde_json::json!({"case": "cold_load_replay", "log_lines": lines, "log_bytes": log_length(), "ms": ms})
        );
        // A clean restart (after shutdown compaction) adopts without writing.
        persist(&directory, encode(&state), true).unwrap();
        let (_, adopted, ms) = reload();
        assert!(adopted);
        println!("{}", serde_json::json!({"case": "cold_load_clean", "ms": ms}));
        // A crash mid-append: the torn line is dropped, earlier saves survive.
        swarm_step(&mut state, 0, chats, &mut active);
        persist(&directory, encode(&state), false).unwrap();
        let expected = crate::history::persisting(|| serde_json::to_value(&state)).unwrap();
        let mut log = fs::OpenOptions::new().append(true).open(directory.join(LOG)).unwrap();
        std::io::Write::write_all(&mut log, br#"[["a",["chats",0,"messages"],0,0,[{"text":"torn"#).unwrap();
        let (loaded, _, ms) = reload();
        assert_eq!(
            crate::history::persisting(|| serde_json::to_value(&loaded).unwrap()),
            expected
        );
        println!("{}", serde_json::json!({"case": "crash_recovery_torn_tail", "ms": ms}));
        published().lock().unwrap().remove(&directory);
    }
}
