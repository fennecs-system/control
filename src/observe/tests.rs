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
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some(GENERATION.into()),
        ..Default::default()
    });
    let run = run.id.clone();
    state.services.push(serde_json::from_value(json!({
        "resource":{"id":"svc-game","owner":{"node":"node-a","chat":chat},"generation":1,
            "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},"lease":[1,u64::MAX],"phase":"Requested"},
        "origin":{"run":run,"attempt":GENERATION},"actor":{"type":"local_operator"},"command":"true","uid":502,"gid":20
    })).unwrap());
    (Arc::new(Mutex::new(state)), chat, run)
}
fn attempt(run: &str) -> AttemptRef<'_> {
    AttemptRef {
        run,
        generation: GENERATION,
        nonce: "observe",
    }
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
    Ok(ProbeOutput {
        text: format!("{command} -> hp=20 tick={tick}"),
        exit_code: Some(0),
        timed_out: false,
    })
}
/// A probe that cannot finish until its test releases it, so "the caller did
/// not wait for it" is shown by the caller returning at all, not by timing.
struct Latch {
    open: std::sync::Mutex<u32>,
    opened: std::sync::Condvar,
}
impl Latch {
    const fn new() -> Self {
        Self {
            open: std::sync::Mutex::new(0),
            opened: std::sync::Condvar::new(),
        }
    }
    /// Lets one blocked (or the next) probe finish.
    fn release(&self) {
        *self.open.lock().unwrap() += 1;
        self.opened.notify_all();
    }
    fn probe(&self) -> io::Result<ProbeOutput> {
        let guard = self.open.lock().unwrap();
        let mut open = self.opened.wait_while(guard, |open| *open == 0).unwrap();
        *open -= 1;
        Ok(ProbeOutput {
            text: "slow fresh value".into(),
            exit_code: Some(0),
            timed_out: false,
        })
    }
}
/// Each test has its own latch; tests run in parallel.
static TURN_LATCH: Latch = Latch::new();
fn slow_turn(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    TURN_LATCH.probe()
}
static STEP_LATCH: Latch = Latch::new();
fn slow_step(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    STEP_LATCH.probe()
}
/// Runs `call` on another thread; a call that waited for a latched probe would
/// never return, so this fails instead of hanging.
fn returns<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(call()));
    receiver
        .recv_timeout(Duration::from_secs(60))
        .expect("returned without waiting for the probe")
}
/// Waits until the chat's background refresh has recorded its result.
fn refreshed(shared: &Arc<Mutex<State>>) {
    for _ in 0..60_000 {
        if shared.lock().unwrap().observations.refreshing.is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the background refresh never finished");
}
fn timing_out(_: &Service, _: &str, _: &str, timeout: Duration) -> io::Result<ProbeOutput> {
    Ok(ProbeOutput {
        text: "partial".into(),
        exit_code: None,
        timed_out: !timeout.is_zero(),
    })
}
fn huge(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    Ok(ProbeOutput {
        text: "é".repeat(5_000),
        exit_code: Some(0),
        timed_out: false,
    })
}
fn broken(_: &Service, _: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    Err(io::Error::other("Service is no longer running"))
}

#[test]
fn a_declared_command_ends_each_turn_and_never_enters_the_transcript_or_checkpoint() {
    let (shared, chat, run) = fixture();
    let set = declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"status --brief","refresh_seconds":0}),
        ticking,
    );
    assert_eq!(
        set["observation"]["source"],
        json!({"kind":"command","service":"svc-game","command":"status --brief"})
    );
    assert!(
        set["preview"].as_str().unwrap().contains("status --brief -> hp=20"),
        "the first value is shown at once"
    );

    let prompt = append_with(&shared, &run, "Objective:\nmine diamonds".into(), ticking);
    // Last, so the unchanged turn input stays a cacheable prefix.
    let block = prompt
        .strip_prefix("Objective:\nmine diamonds\n\n")
        .expect("the turn input comes first, unchanged");
    assert!(
        block.starts_with("[Observation: latest output of `status --brief` in svc-game, 0 s old."),
        "{block}"
    );
    assert!(block.contains("hp=20") && block.ends_with("[End observation]"));

    let state = shared.lock().unwrap();
    let saved = serde_json::to_string(&*state).unwrap();
    assert!(!saved.contains("hp=20"), "values stay out of the checkpoint");
    assert!(
        saved.contains("status --brief"),
        "the declaration is chat configuration"
    );
    assert!(state.runtime.runs.iter().all(|run| !run.objective.contains("hp=20")));
    assert!(
        state
            .chats
            .iter()
            .flat_map(|chat| &chat.messages)
            .all(|message| !message.text.contains("hp=20"))
    );
    let _ = chat;
}

#[test]
fn refresh_seconds_reuses_a_recent_value_and_zero_refreshes_every_turn() {
    let (shared, _, run) = fixture();
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"cached","refresh_seconds":3600}),
        ticking,
    );
    let first = append_with(&shared, &run, String::new(), ticking);
    let second = append_with(&shared, &run, String::new(), ticking);
    assert_eq!(
        first, second,
        "a value younger than refresh_seconds is reused without a command"
    );
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"fresh","refresh_seconds":0}),
        ticking,
    );
    let first = append_with(&shared, &run, String::new(), ticking);
    let second = append_with(&shared, &run, String::new(), ticking);
    assert_ne!(first, second, "refresh_seconds=0 reruns at every turn");
}

#[test]
fn a_slow_refresh_never_blocks_a_turn_and_falls_back_to_the_last_value_with_its_age() {
    let (shared, _, run) = fixture();
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look","refresh_seconds":0}),
        ticking,
    );
    // The probe is held until released: returning shows the wait is bounded.
    let fallback = returns({
        let (shared, run) = (Arc::clone(&shared), run.clone());
        move || text(&shared, &run, Duration::from_millis(50), slow_turn)
    });
    assert!(
        fallback.contains("look -> hp=20") && fallback.contains(" s old"),
        "{fallback}"
    );
    // The refresh finishes in the background and the next turn sees it; a
    // refresh already in flight is not started twice.
    {
        let state = shared.lock().unwrap();
        let chat = state.runtime.runs[0].chat_id.clone().unwrap();
        assert!(
            state.observations.refreshing.contains(&chat),
            "the refresh continues in the background"
        );
    }
    let again = text(&shared, &run, Duration::ZERO, |_, _, _, _| {
        panic!("a refresh in flight is not started twice")
    });
    assert!(again.contains("look -> hp=20"), "{again}");
    TURN_LATCH.release();
    refreshed(&shared);
    // Its own refresh is held again, so this shows the value that landed.
    let next = text(&shared, &run, Duration::ZERO, slow_turn);
    assert!(next.contains("slow fresh value"), "{next}");

    // A timed-out refresh keeps the last value and says so.
    let (shared, _, run) = fixture();
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look","refresh_seconds":0,"timeout_seconds":1}),
        ticking,
    );
    let kept = text(&shared, &run, Duration::from_secs(1), timing_out);
    assert!(
        kept.contains("look -> hp=20") && kept.contains("last refresh timed out after 1 s"),
        "{kept}"
    );
    // So does a failed one.
    let kept = text(&shared, &run, Duration::from_secs(1), broken);
    assert!(
        kept.contains("look -> hp=20") && kept.contains("last refresh failed: Service is no longer running"),
        "{kept}"
    );
}

#[test]
fn values_are_capped_to_max_bytes_on_a_character_boundary() {
    let (shared, _, run) = fixture();
    let set = set_with(
        &shared,
        &attempt(&run),
        input(json!({"service_id":"svc-game","command":"dump","max_bytes":100_000})),
        |_| Ok(()),
        huge,
    );
    assert!(
        set.unwrap_err().to_string().contains("max_bytes must be 256–2048"),
        "the node cap bounds declarations"
    );
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"dump","max_bytes":301}),
        huge,
    );
    let block = text(&shared, &run, Duration::from_secs(1), huge);
    let body = block.split_once('\n').unwrap().1;
    let body = body
        .strip_suffix("\n[truncated]\n[End observation]")
        .expect("marked as truncated");
    assert_eq!(body.len(), 300, "cut to the character boundary below 301 bytes");
    // The default is the node's cap.
    declare(&shared, &run, json!({"service_id":"svc-game","command":"dump"}), huge);
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .observations
            .declared
            .values()
            .next()
            .unwrap()
            .max_bytes,
        2_048
    );
}

fn failing(_: &Service, command: &str, _: &str, _: Duration) -> io::Result<ProbeOutput> {
    Ok(ProbeOutput {
        text: "partial state".into(),
        exit_code: (command == "exit-3").then_some(3),
        timed_out: false,
    })
}
#[test]
fn a_failed_command_keeps_its_output_with_the_exit_status() {
    let (shared, _, run) = fixture();
    for (command, note) in [("exit-3", "; exit 3."), ("killed", "; exit unknown.")] {
        let set = declare(
            &shared,
            &run,
            json!({"service_id":"svc-game","command":command,"refresh_seconds":0}),
            failing,
        );
        let preview = set["preview"].as_str().unwrap();
        assert!(
            preview.contains(note) && preview.contains("\npartial state\n"),
            "{preview}"
        );
    }
    // A service that is gone is not probed.
    shared.lock().unwrap().services.clear();
    let block = append_with(&shared, &run, "turn".into(), |_, _, _, _| {
        panic!("a missing service is not probed")
    });
    assert!(block.contains("service unavailable to this chat"), "{block}");
}

fn text(shared: &Arc<Mutex<State>>, run: &str, wait: Duration, probe: Prober) -> String {
    block_with(shared, run, wait, probe).unwrap().0.text
}
fn watch(shared: &Arc<Mutex<State>>, chat: &str) {
    shared.lock().unwrap().watches.push(serde_json::from_value(json!({
        "id":"w-1","name":"game events","chat":chat,"service":"svc-game","generation":1,"permanent":true,"command":"run","stopped":false,
        "recent":["line one"],"notifications":{"pending":0,"through":2,"recent":["{\"hp\":18}","{\"hp\":17}"]}
    })).unwrap());
}

#[test]
fn tool_results_carry_the_whole_observation_only_when_it_changed_for_this_attempt() {
    let (shared, _, run) = fixture();
    // The declaration's preview was shown to this attempt; an identical value
    // is a short marker, never the whole block again.
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"cached","refresh_seconds":3600}),
        ticking,
    );
    let marker = for_result_with(&shared, &run, ticking).unwrap();
    // The age is wall-clock seconds; a loaded machine may take one to get here.
    assert!(
        marker.starts_with("[Observation unchanged (age ") && marker.ends_with(" s)]"),
        "{marker}"
    );
    assert!(crate::local_agent::tokens(marker.len()) <= 12, "about ten tokens");
    // A changed value is shown whole, once.
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"fresh","refresh_seconds":0}),
        ticking,
    );
    let first = for_result_with(&shared, &run, ticking).unwrap();
    let second = for_result_with(&shared, &run, ticking).unwrap();
    assert!(
        first.starts_with("[Observation: latest output of `fresh`")
            && second.starts_with("[Observation: latest output of `fresh`")
    );
    assert_ne!(first, second, "each refresh changed the value");
    // A new attempt has seen nothing yet.
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"cached","refresh_seconds":3600}),
        ticking,
    );
    shared.lock().unwrap().runtime.runs[0]
        .attempts
        .push(crate::usage::Attempt {
            workspace_id: Some("attempt-b".into()),
            ..Default::default()
        });
    assert!(
        for_result_with(&shared, &run, ticking)
            .unwrap()
            .starts_with("[Observation: latest output of `cached`")
    );
    assert!(
        for_result_with(&shared, &run, ticking)
            .unwrap()
            .starts_with("[Observation unchanged")
    );
    // An ended run gets nothing.
    shared.lock().unwrap().runtime.runs[0].status = "completed".into();
    assert_eq!(for_result_with(&shared, &run, ticking), None);
    // The last-shown hashes are memory only.
    let saved = serde_json::to_string(&*shared.lock().unwrap()).unwrap();
    assert!(!saved.contains("hp=20") && !saved.contains("attempt-a/"), "{saved}");
}

#[test]
fn the_wake_counts_as_shown_and_local_steps_always_get_the_whole_refreshed_block() {
    let (shared, _, run) = fixture();
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look","refresh_seconds":3600}),
        ticking,
    );
    shared.lock().unwrap().observations.shown.clear();
    let wake = append_with(&shared, &run, "turn".into(), ticking);
    assert!(wake.contains("look -> hp=20"));
    assert!(
        for_result_with(&shared, &run, ticking)
            .unwrap()
            .starts_with("[Observation unchanged"),
        "the wake showed it"
    );
    // A local request is rebuilt every step and carries no earlier copy.
    let first = step_with(&shared, &run, true, ticking).unwrap();
    let again = step_with(&shared, &run, false, ticking).unwrap();
    assert_eq!(first, again, "unchanged within refresh_seconds, yet whole");
    assert!(again.contains("look -> hp=20") && again.ends_with("[End observation]"));
    // refresh_seconds=0 refreshes before every step.
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look","refresh_seconds":0}),
        ticking,
    );
    let steps: Vec<String> = (0..3)
        .map(|i| step_with(&shared, &run, i == 0, ticking).unwrap())
        .collect();
    assert!(steps[0] != steps[1] && steps[1] != steps[2], "{steps:?}");
}

#[test]
fn a_slow_probe_never_blocks_a_step_or_result() {
    let (shared, _, run) = fixture();
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look","refresh_seconds":0}),
        ticking,
    );
    // The probe is held until released: returning shows the step did not await it.
    let step = returns({
        let (shared, run) = (Arc::clone(&shared), run.clone());
        move || {
            block_with(&shared, &run, Duration::from_millis(50), slow_step)
                .unwrap()
                .0
                .text
        }
    });
    assert!(
        step.contains("look -> hp=20") && step.contains(" s old"),
        "the last value with its age: {step}"
    );
    STEP_LATCH.release();
    refreshed(&shared);
    // The background refresh landed; a result sees the new value, and the next
    // slow refresh, held again, is not awaited either.
    let result = returns({
        let (shared, run) = (Arc::clone(&shared), run.clone());
        move || result_within(&shared, &run, Duration::from_millis(50), slow_step).unwrap()
    });
    assert!(result.contains("slow fresh value"), "{result}");
}

#[test]
fn a_watch_source_attaches_its_newest_notification_without_running_anything() {
    let (shared, chat, run) = fixture();
    watch(&shared, &chat);
    declare(&shared, &run, json!({"watch_id":"w-1"}), broken);
    let never =
        |_: &Service, _: &str, _: &str, _: Duration| -> io::Result<ProbeOutput> { panic!("a watch runs nothing") };
    assert!(
        for_result_with(&shared, &run, never)
            .unwrap()
            .starts_with("[Observation unchanged")
    );
    assert!(step_with(&shared, &run, false, never).unwrap().contains("{\"hp\":17}"));
    shared.lock().unwrap().watches[0]
        .notifications
        .as_mut()
        .unwrap()
        .recent
        .push("{\"hp\":9}".into());
    let changed = for_result_with(&shared, &run, never).unwrap();
    assert!(
        changed.contains("{\"hp\":9}") && !changed.contains("{\"hp\":17}"),
        "{changed}"
    );
    assert!(
        for_result_with(&shared, &run, never)
            .unwrap()
            .starts_with("[Observation unchanged")
    );
}

#[test]
fn repeated_status_execs_without_an_observation_are_nudged_once() {
    let (shared, _, run) = fixture();
    let exec = |service: &str, command: &str| {
        polling_note(
            &shared,
            &run,
            "control_service_exec",
            &json!({"service_id":service,"command":command}),
        )
    };
    for _ in 0..3 {
        assert_eq!(exec("svc-game", "ps aux | grep server"), None);
    }
    assert_eq!(
        exec("svc-game", "tail -n 20 /var/log/game.log"),
        Some(POLLING_NOTE.to_owned()),
        "the fourth consecutive status exec"
    );
    for _ in 0..8 {
        assert_eq!(
            exec("svc-game", "curl -s localhost:8080/health"),
            None,
            "once per attempt"
        );
    }
    // Work, another service or another tool interrupt the streak.
    let (shared, _, run) = fixture();
    let exec = |service: &str, command: &str| {
        polling_note(
            &shared,
            &run,
            "control_service_exec",
            &json!({"service_id":service,"command":command}),
        )
    };
    for command in [
        "cat state.json",
        "sleep 5; ls /data",
        "cargo build",
        "pgrep -f server",
        "cat out > copy.txt",
        "ls",
    ] {
        assert_eq!(exec("svc-game", command), None, "{command}");
    }
    assert_eq!(exec("svc-other", "ls"), None);
    assert_eq!(exec("svc-game", "ls"), None);
    assert_eq!(
        polling_note(&shared, &run, "control_workspace_run", &json!({"command":"ls"})),
        None
    );
    // Nothing once the chat declares an observation.
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look"}),
        ticking,
    );
    for _ in 0..6 {
        assert_eq!(exec("svc-game", "ps"), None);
    }
}

#[test]
fn status_style_commands_only_read() {
    for command in [
        "ps aux",
        "ps aux | grep x | wc -l",
        "sleep 2 && tail -n 5 log",
        "timeout 5 curl -fsS http://localhost/health",
        "cat a 2>/dev/null",
        "ls -la 2>&1",
        "systemctl status game",
        "docker ps",
        "/usr/bin/pgrep server",
        "FOO=1 cat x",
    ] {
        assert!(status_style(command), "{command}");
    }
    for command in [
        "sleep 5",
        "python3 poll.py",
        "cat a > b",
        "echo hi >> log",
        "curl -X POST localhost/reset",
        "curl -d x localhost",
        "find . -delete",
        "docker restart game",
        "ls; rm -rf x",
        "systemctl restart game",
    ] {
        assert!(!status_style(command), "{command}");
    }
}

#[test]
fn attached_observations_are_scrubbed_from_stored_activity() {
    for (stored, kept) in [
        (
            "{\"ok\":true}\n[Observation: latest output of `look` in svc, 0 s old.]\nhp=20\n[End observation]",
            "{\"ok\":true}",
        ),
        (
            "{\"ok\":true}\nnote\n[Observation unchanged (age 4 s)]",
            "{\"ok\":true}\nnote",
        ),
        ("[Observation: latest output of `look` in svc, 0 s old.]\nhp=2", ""),
        ("plain output", "plain output"),
    ] {
        let mut text = stored.to_owned();
        scrub(&mut text);
        assert_eq!(text, kept);
    }
}

#[test]
fn declarations_saved_with_exec_results_still_load() {
    let old: Declaration = serde_json::from_value(json!({"source":{"kind":"watch","watch":"w-1"},"max_bytes":2048,"refresh_seconds":10,"timeout_seconds":5,"exec_results":true})).unwrap();
    assert!(!serde_json::to_string(&old).unwrap().contains("exec_results"));
    let (shared, _, run) = fixture();
    assert!(
        declare(
            &shared,
            &run,
            json!({"service_id":"svc-game","command":"look","exec_results":true}),
            ticking
        )["preview"]
            .is_string()
    );
}

#[test]
fn a_watch_source_reuses_its_latest_notification_without_running_anything() {
    let (shared, chat, run) = fixture();
    watch(&shared, &chat);
    let set = declare(&shared, &run, json!({"watch_id":"w-1"}), broken);
    assert!(set["preview"].as_str().unwrap().contains("watch game events (w-1)"));
    let block = append_with(&shared, &run, String::new(), broken);
    assert!(
        block.contains("{\"hp\":17}") && !block.contains("{\"hp\":18}"),
        "{block}"
    );
    // Another chat's watch is refused.
    shared.lock().unwrap().watches[0].chat = "someone-else".into();
    shared.lock().unwrap().watches[0].service = "svc-other".into();
    let refused = set_with(
        &shared,
        &attempt(&run),
        input(json!({"watch_id":"w-1"})),
        |_| Ok(()),
        broken,
    )
    .unwrap_err();
    assert!(refused.to_string().contains("Observe only a watch"));
}

#[test]
fn declarations_are_visible_clearable_and_checked() {
    let (shared, _, run) = fixture();
    assert_eq!(declare(&shared, &run, json!({}), ticking)["observation"], Value::Null);
    declare(
        &shared,
        &run,
        json!({"service_id":"svc-game","command":"look"}),
        ticking,
    );
    let shown = declare(&shared, &run, json!({}), ticking);
    assert_eq!(shown["observation"]["refresh_seconds"], DEFAULT_REFRESH_SECONDS);
    assert!(shown["preview"].as_str().unwrap().contains("look -> hp=20"));
    // A failed save leaves the previous declaration.
    let error = set_with(
        &shared,
        &attempt(&run),
        input(json!({"clear":true})),
        |_| Err(io::Error::other("disk full")),
        ticking,
    );
    assert!(error.is_err());
    assert!(declare(&shared, &run, json!({}), ticking)["observation"].is_object());
    assert_eq!(declare(&shared, &run, json!({"clear":true}), ticking)["cleared"], true);
    assert_eq!(
        append_with(&shared, &run, "input".into(), ticking),
        "input",
        "a cleared chat gets no block"
    );
    // Only services the chat can use, and one source at a time.
    for args in [
        json!({"service_id":"svc-missing","command":"look"}),
        json!({"service_id":"svc-game","command":"look","watch_id":"w"}),
        json!({"command":"look"}),
    ] {
        assert!(set_with(&shared, &attempt(&run), input(args), |_| Ok(()), ticking).is_err());
    }
}

/// Estimated observation tokens added to each CLI tool result with a 2 KB
/// watch value, unchanged or changing between results.
#[test]
#[ignore = "measurement: cargo test observe::tests::measure -- --ignored --nocapture"]
fn measure_observation_tokens_per_tool_result() {
    let value = |n: usize| {
        (0..40)
            .map(|i| format!("entity-{i:02} hp={} pos={},{},64", 20 - (n + i) % 20, n * 3 + i, i * 7))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for (label, changing) in [("unchanged", false), ("changing", true)] {
        let (shared, chat, run) = fixture();
        watch(&shared, &chat);
        shared.lock().unwrap().watches[0].notifications.as_mut().unwrap().recent = vec![value(0)];
        declare(&shared, &run, json!({"watch_id":"w-1"}), broken);
        let per_result: Vec<u32> = (1..=8)
            .map(|n| {
                if changing {
                    shared.lock().unwrap().watches[0]
                        .notifications
                        .as_mut()
                        .unwrap()
                        .recent
                        .push(value(n));
                }
                crate::local_agent::tokens(for_result_with(&shared, &run, broken).unwrap().len())
            })
            .collect();
        println!("cli {label}: observation tokens per tool result {per_result:?}");
    }
}
