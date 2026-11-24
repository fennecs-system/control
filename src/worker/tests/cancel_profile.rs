//! Live `/cancel all` on a large node: the real dispatcher, worker threads,
//! durable checkpoints and commit path, with a credential-free worker that
//! runs until cancelled. A presentation thread re-renders on each change, as
//! the browser does. No model, container, personal state or credential.
use super::*;
use crate::runtime::RunStatus;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn cpu() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// Polls cancellation like a provider worker, then commits through the real path.
fn execute(_: &dyn control_isolation::Isolation, state: Store, task: Task) {
    while !crate::lifecycle::cancelled(Some((&state, &task.0))) {
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = commit(
        &mut state.guard().unwrap(),
        &task.0,
        task.3,
        task.4,
        Err((control_agent::Status::Cancelled, "Cancelled.".into())),
        false,
        storage::save,
    );
}

fn node(unrelated: usize, runs: usize, tree: usize) -> (State, String) {
    let mut state = State::default();
    state.runtime.execution.max_parallel_runs = crate::scheduler::MAX_PARALLEL;
    let root = crate::chat::create(&mut state);
    state.chats[0].room = Some("general".into());
    let mut chats = vec![root.clone()];
    for index in 0..tree + unrelated {
        let chat = crate::chat::create(&mut state);
        let session = state.chats.last_mut().unwrap();
        session.room = Some("general".into());
        if index < tree {
            session.parent_chat = Some(root.clone());
        }
        chats.push(chat);
    }
    // Finished history first, as on a long-lived node.
    for chat in &chats[tree + 1..] {
        for turn in 0..runs {
            crate::chat::send(
                &mut state,
                chat,
                &format!("Turn {turn}: {}", "context ".repeat(32)),
                "test",
            )
            .unwrap();
            let run = state.runtime.runs.last_mut().unwrap();
            run.status = RunStatus::Completed;
            run.emitted = Some(RunStatus::Completed);
            run.result = "done ".repeat(64).into();
        }
    }
    for index in 0..4_000 {
        state.collective.posts.push(crate::rooms::Post {
            room: "general".into(),
            text: "room traffic ".repeat(16),
            chat: Some(chats[index % chats.len()].clone()),
            run: None,
            key: format!("k{index}"),
            delivery: Default::default(),
        });
    }
    for chat in &chats[1..=tree] {
        crate::chat::send(&mut state, chat, "Join the game", "test").unwrap();
        state.runtime.runs.last_mut().unwrap().status = RunStatus::Completed;
        state.runtime.runs.last_mut().unwrap().emitted = Some(RunStatus::Completed);
        crate::bots::command(&mut state, chat, "on").unwrap();
    }
    for chat in &chats[..=tree] {
        crate::chat::send(&mut state, chat, "Play", "test").unwrap();
    }
    (state, root)
}

#[test]
#[ignore = "run alone with CONTROL_STATE_DIR and CONTROL_CONFIG_DIR under a scratch control-cancel-profile directory; requests process-wide shutdown"]
fn live_cancel_all_profile() {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            crate::shutdown::request();
        }
    }
    let _stop = Stop;
    assert!(
        std::env::var("CONTROL_STATE_DIR")
            .unwrap()
            .contains("control-cancel-profile")
    );
    assert!(crate::backend::Backend::Codex.image().is_none() && crate::backend::Backend::Claude.image().is_none());
    storage::initialize().unwrap();
    let unrelated = std::env::var("CONTROL_PROFILE_CHATS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let (initial, root) = node(unrelated, 48, 6);
    storage::save(&initial).unwrap();
    let bytes = fs::metadata(crate::config::state("state.json").unwrap()).unwrap().len();
    let state = crate::store::Store::new(initial);
    let threads = start_with(state.clone(), crate::engine::isolation(), execute);
    let active = |s: &State| s.runtime.runs.iter().filter(|r| r.status.occupies_slot()).count();
    let started = Instant::now();
    while active(&state.guard().unwrap()) < 7 {
        assert!(started.elapsed() < Duration::from_secs(60), "turns did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    // A connected page re-renders the transcript and navigation per change.
    let (done, renders) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let presenter = {
        let (state, root, done, renders) = (state.clone(), root.clone(), done.clone(), renders.clone());
        std::thread::spawn(move || {
            let changes = storage::changes().clone();
            let token = crate::shutdown::token();
            let mut seen = changes.generation();
            while !done.load(Ordering::Relaxed) {
                let _ = changes.wait_newer(seen, Some(Instant::now() + Duration::from_millis(100)), &token);
                let now = changes.generation();
                if now == seen {
                    continue;
                }
                seen = now;
                let s = state.guard().unwrap();
                std::hint::black_box((
                    crate::chat::transcript(&s, &root).ok(),
                    crate::navigation::Picker::of(&s.chats, &root).render(),
                ));
                renders.fetch_add(1, Ordering::Relaxed);
            }
        })
    };
    std::thread::sleep(Duration::from_secs(1));
    let (before, generation, started) = (cpu(), storage::changes().generation(), Instant::now());
    {
        let mut s = state.guard().unwrap();
        crate::session_http::change_with_save(
            &mut s,
            "control.chat.send",
            &serde_json::json!({"id":root,"message":"/cancel all"}),
            storage::save,
        )
        .unwrap();
    }
    wake();
    while active(&state.guard().unwrap()) > 0 {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "cancellation did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let settled = started.elapsed();
    // Let the dispatcher's follow-up passes run.
    std::thread::sleep(Duration::from_secs(3));
    let (cpu_s, checkpoints) = (cpu() - before, storage::changes().generation() - generation);
    done.store(true, Ordering::Relaxed);
    presenter.join().unwrap();
    let s = state.guard().unwrap();
    println!(
        "{}",
        serde_json::json!({"workload":"live_cancel_all","chats":s.chats.len(),"runs":s.runtime.runs.len(),
        "state_bytes":bytes,"cancelled":s.runtime.runs.iter().filter(|r| r.status == RunStatus::Cancelled).count(),
        "settled_ms":settled.as_millis(),"checkpoints":checkpoints,"renders":renders.load(Ordering::Relaxed),
        "cpu_s_including_3s_tail":(cpu_s * 1000.0).round() / 1000.0})
    );
    drop(s);
    crate::shutdown::request();
    threads.join().unwrap();
}
