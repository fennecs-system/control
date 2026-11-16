use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const GENERATION: &str = "attempt-a";

/// One chat with a running attempt and a service it owns.
fn fixture() -> (Arc<Mutex<State>>, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Play the game", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt { workspace_id: Some(GENERATION.into()), ..Default::default() });
    let run = run.id.clone();
    state.services.push(serde_json::from_value(json!({
        "resource":{"id":"svc-game","owner":{"node":"node-a","chat":chat},"generation":1,
            "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},"lease":[1,u64::MAX],"phase":"Requested"},
        "origin":{"run":run,"attempt":GENERATION},"actor":{"type":"local_operator"},"command":"true","uid":502,"gid":20
    })).unwrap());
    (Arc::new(Mutex::new(state)), chat, run)
}
fn attempt(run: &str) -> AttemptRef<'_> {
    AttemptRef { run, generation: GENERATION, nonce: "observe" }
}
fn input(args: Value) -> Input {
    serde_json::from_value(args).unwrap()
}
fn declare(shared: &Arc<Mutex<State>>, run: &str, args: Value, probe: Prober) -> Value {
    set_with(shared, &attempt(run), input(args), |_| Ok(()), probe).unwrap()
}

static TICKS: AtomicU64 = AtomicU64::new(0);
fn ticking(_: &Service, command: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    let tick = TICKS.fetch_add(1, Ordering::SeqCst);
    Ok(ProbeOutput { text: format!("{command} -> hp=20 tick={tick}"), exit_code: Some(0), timed_out: false })
}
fn slow(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    std::thread::sleep(Duration::from_millis(300));
    Ok(ProbeOutput { text: "slow fresh value".into(), exit_code: Some(0), timed_out: false })
}
fn timing_out(_: &Service, _: &str, _: &str, timeout: Duration) -> io::Result<ProbeOutput> {
    Ok(ProbeOutput { text: "partial".into(), exit_code: None, timed_out: !timeout.is_zero() })
}
fn huge(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    Ok(ProbeOutput { text: "é".repeat(5_000), exit_code: Some(0), timed_out: false })
}
fn broken(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    Err(io::Error::other("Service is no longer running"))
}

#[test]
fn a_declared_command_tops_each_turn_and_never_enters_the_transcript_or_checkpoint() {
    let (shared, chat, run) = fixture();
    let set = declare(&shared, &run, json!({"service_id":"svc-game","command":"status --brief","refresh_seconds":0}), ticking);
    assert_eq!(set["observation"]["source"], json!({"kind":"command","service":"svc-game","command":"status --brief"}));
    assert!(set["preview"].as_str().unwrap().contains("status --brief -> hp=20"), "the first value is shown at once");

    let prompt = prepend_with(&shared, &run, "Objective:\nmine diamonds".into(), ticking);
    let (block, rest) = prompt.split_once("\n\n").unwrap();
    assert!(block.starts_with("[Observation: latest output of `status --brief` in svc-game, 0 s old."), "{block}");
    assert!(block.contains("hp=20") && block.ends_with("[End observation]"));
    assert_eq!(rest, "Objective:\nmine diamonds", "the turn input follows unchanged");

    let state = shared.lock().unwrap();
    let saved = serde_json::to_string(&*state).unwrap();
    assert!(!saved.contains("hp=20"), "values stay out of the checkpoint");
    assert!(saved.contains("status --brief"), "the declaration is chat configuration");
    assert!(state.runtime.runs.iter().all(|run| !run.objective.contains("hp=20")));
    assert!(state.chats.iter().flat_map(|chat| &chat.messages).all(|message| !message.text.contains("hp=20")));
    let _ = chat;
}

#[test]
fn refresh_seconds_reuses_a_recent_value_and_zero_refreshes_every_turn() {
    let (shared, _, run) = fixture();
    declare(&shared, &run, json!({"service_id":"svc-game","command":"cached","refresh_seconds":3600}), ticking);
    let first = prepend_with(&shared, &run, String::new(), ticking);
    let second = prepend_with(&shared, &run, String::new(), ticking);
    assert_eq!(first, second, "a value younger than refresh_seconds is reused without a command");
    declare(&shared, &run, json!({"service_id":"svc-game","command":"fresh","refresh_seconds":0}), ticking);
    let first = prepend_with(&shared, &run, String::new(), ticking);
    let second = prepend_with(&shared, &run, String::new(), ticking);
    assert_ne!(first, second, "refresh_seconds=0 reruns at every turn");
}

#[test]
fn a_slow_refresh_never_blocks_a_turn_and_falls_back_to_the_last_value_with_its_age() {
    let (shared, _, run) = fixture();
    declare(&shared, &run, json!({"service_id":"svc-game","command":"look","refresh_seconds":0}), ticking);
    let started = std::time::Instant::now();
    let fallback = block_with(&shared, &run, Duration::from_millis(50), false, slow).unwrap();
    assert!(started.elapsed() < Duration::from_millis(250), "the wait is bounded, not the command");
    assert!(fallback.contains("look -> hp=20") && fallback.contains(" s old"), "{fallback}");
    // The refresh finishes in the background and the next turn sees it; a
    // refresh already in flight is not started twice.
    {
        let state = shared.lock().unwrap();
        let chat = state.runtime.runs[0].chat_id.clone().unwrap();
        assert!(state.observations.refreshing.contains(&chat), "the refresh continues in the background");
    }
    std::thread::sleep(Duration::from_millis(400));
    let next = block_with(&shared, &run, Duration::from_millis(50), false, slow).unwrap();
    assert!(next.contains("slow fresh value"), "{next}");

    // A timed-out refresh keeps the last value and says so.
    let (shared, _, run) = fixture();
    declare(&shared, &run, json!({"service_id":"svc-game","command":"look","refresh_seconds":0,"timeout_seconds":1}), ticking);
    let kept = block_with(&shared, &run, Duration::from_secs(1), false, timing_out).unwrap();
    assert!(kept.contains("look -> hp=20") && kept.contains("last refresh timed out after 1 s"), "{kept}");
    // So does a failed one.
    let kept = block_with(&shared, &run, Duration::from_secs(1), false, broken).unwrap();
    assert!(kept.contains("look -> hp=20") && kept.contains("last refresh failed: Service is no longer running"), "{kept}");
}

#[test]
fn values_are_capped_to_max_bytes_on_a_character_boundary() {
    let (shared, _, run) = fixture();
    let set = set_with(&shared, &attempt(&run), input(json!({"service_id":"svc-game","command":"dump","max_bytes":100_000})), |_| Ok(()), huge);
    assert!(set.unwrap_err().to_string().contains("max_bytes must be 256–2048"), "the node cap bounds declarations");
    declare(&shared, &run, json!({"service_id":"svc-game","command":"dump","max_bytes":301}), huge);
    let block = block_with(&shared, &run, Duration::from_secs(1), false, huge).unwrap();
    let body = block.split_once('\n').unwrap().1;
    let body = body.strip_suffix("\n[truncated]\n[End observation]").expect("marked as truncated");
    assert_eq!(body.len(), 300, "cut to the character boundary below 301 bytes");
    // The default is the node's cap.
    declare(&shared, &run, json!({"service_id":"svc-game","command":"dump"}), huge);
    assert_eq!(shared.lock().unwrap().observations.declared.values().next().unwrap().max_bytes, 2_048);
}

fn failing(_: &Service, command: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    Ok(ProbeOutput { text: "partial state".into(), exit_code: (command == "exit-3").then_some(3), timed_out: false })
}
#[test]
fn a_failed_command_keeps_its_output_with_the_exit_status() {
    let (shared, _, run) = fixture();
    for (command, note) in [("exit-3", "; exit 3."), ("killed", "; exit unknown.")] {
        let set = declare(&shared, &run, json!({"service_id":"svc-game","command":command,"refresh_seconds":0}), failing);
        let preview = set["preview"].as_str().unwrap();
        assert!(preview.contains(note) && preview.contains("\npartial state\n"), "{preview}");
    }
    // A service that is gone is not probed.
    shared.lock().unwrap().services.clear();
    let block = prepend_with(&shared, &run, "turn".into(), |_, _, _, _| panic!("a missing service is not probed"));
    assert!(block.contains("service unavailable to this chat"), "{block}");
}

#[test]
fn exec_results_carry_the_observation_only_when_declared() {
    let (shared, _, run) = fixture();
    declare(&shared, &run, json!({"service_id":"svc-game","command":"look"}), ticking);
    let mut value = json!({"status":"completed","output":"ok"});
    attach_with(&shared, &run, &mut value, ticking);
    assert!(value.get("observation").is_none(), "opt-in");
    declare(&shared, &run, json!({"service_id":"svc-game","command":"look","exec_results":true}), ticking);
    attach_with(&shared, &run, &mut value, ticking);
    assert!(value["observation"].as_str().unwrap().contains("look -> hp=20"));
    assert_eq!(value["output"], "ok");
}

#[test]
fn a_watch_source_reuses_its_latest_notification_without_running_anything() {
    let (shared, chat, run) = fixture();
    shared.lock().unwrap().watches.push(serde_json::from_value(json!({
        "id":"w-1","name":"game events","chat":chat,"service":"svc-game","generation":1,"permanent":true,"command":"run","stopped":false,
        "recent":["line one"],"notifications":{"pending":0,"through":2,"recent":["{\"hp\":18}","{\"hp\":17}"]}
    })).unwrap());
    let set = declare(&shared, &run, json!({"watch_id":"w-1"}), broken);
    assert!(set["preview"].as_str().unwrap().contains("watch game events (w-1)"));
    let block = prepend_with(&shared, &run, String::new(), broken);
    assert!(block.contains("{\"hp\":17}") && !block.contains("{\"hp\":18}"), "{block}");
    // Another chat's watch is refused.
    shared.lock().unwrap().watches[0].chat = "someone-else".into();
    shared.lock().unwrap().watches[0].service = "svc-other".into();
    let refused = set_with(&shared, &attempt(&run), input(json!({"watch_id":"w-1"})), |_| Ok(()), broken).unwrap_err();
    assert!(refused.to_string().contains("Observe only a watch"));
}

#[test]
fn declarations_are_visible_clearable_and_checked() {
    let (shared, _, run) = fixture();
    assert_eq!(declare(&shared, &run, json!({}), ticking)["observation"], Value::Null);
    declare(&shared, &run, json!({"service_id":"svc-game","command":"look"}), ticking);
    let shown = declare(&shared, &run, json!({}), ticking);
    assert_eq!(shown["observation"]["refresh_seconds"], DEFAULT_REFRESH_SECONDS);
    assert!(shown["preview"].as_str().unwrap().contains("look -> hp=20"));
    // A failed save leaves the previous declaration.
    let error = set_with(&shared, &attempt(&run), input(json!({"clear":true})), |_| Err(io::Error::other("disk full")), ticking);
    assert!(error.is_err());
    assert!(declare(&shared, &run, json!({}), ticking)["observation"].is_object());
    assert_eq!(declare(&shared, &run, json!({"clear":true}), ticking)["cleared"], true);
    assert_eq!(prepend_with(&shared, &run, "input".into(), ticking), "input", "a cleared chat gets no block");
    // Only services the chat can use, and one source at a time.
    for args in [json!({"service_id":"svc-missing","command":"look"}), json!({"service_id":"svc-game","command":"look","watch_id":"w"}), json!({"command":"look"})] {
        assert!(set_with(&shared, &attempt(&run), input(args), |_| Ok(()), ticking).is_err());
    }
}
