use super::*;
use crate::runtime::RunKind;
mod profiling;

#[test]
fn reconciled_approval_checkpoint_survives_restart_without_a_duplicate_resume() {
    let directory = std::env::temp_dir().join(format!(
        "control-approved-checkpoint-{}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "original task", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    let origin = state.runtime.runs[0].id.clone();
    let decide = |state: &mut State| {
        crate::approvals::resume(
            state,
            crate::approvals::Decisions {
                chat: &chat,
                origins: &[&origin],
                heading: "Port decisions",
                lines: "approved",
                instruction: "Continue.",
                task: "original task",
                kind: RunKind::PortDecision,
                source: "test",
                grants: Vec::new(),
            },
        )
    };
    let crate::approvals::Outcome::Resumed(id) = decide(&mut state) else {
        panic!("must resume")
    };
    let candidate = crate::history::persisting(|| serde_json::to_vec(&state)).unwrap();
    let mut first = true;
    legacy_result(commit_checkpoint(&directory, &candidate, |directory| {
        if std::mem::take(&mut first) {
            Err(io::Error::other("lost directory-sync acknowledgement"))
        } else {
            sync_directory(directory)
        }
    }))
    .unwrap();
    let mut restored: State =
        serde_json::from_slice(&read_checkpoint(&directory).unwrap().unwrap()).unwrap();
    assert_eq!(restored.runtime.runs.last().unwrap().id, id);
    assert_eq!(
        restored.chats[0].messages.last().unwrap().run_id.as_deref(),
        Some(id.as_str())
    );
    let counts = (
        restored.runtime.runs.len(),
        restored.chats[0].messages.len(),
    );
    assert_eq!(decide(&mut restored), crate::approvals::Outcome::Wait);
    assert_eq!(
        (
            restored.runtime.runs.len(),
            restored.chats[0].messages.len()
        ),
        counts
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn reconciliation_confirms_exact_bytes_and_never_acknowledges_different_data() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
        "control-checkpoint-reconcile-{}",
        std::process::id()
    )));
    fs::create_dir(&root.0).unwrap();
    let published = root.0.join("state.json");
    let calls = std::cell::Cell::new(0);
    commit_checkpoint(&root.0, b"candidate", |directory| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            Err(io::Error::other("injected first sync failure"))
        } else {
            sync_directory(directory)
        }
    })
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(read_checkpoint(&root.0).unwrap().unwrap(), b"candidate");

    for different in [b"different".as_slice(), b"candidate-extra", b"short"] {
        fs::write(&published, different).unwrap();
        assert!(
            confirm_checkpoint(&root.0, b"candidate", |_| panic!(
                "mismatch must not confirm"
            ))
            .is_err()
        );
        assert_eq!(fs::read(&published).unwrap(), different);
    }
    fs::remove_file(&published).unwrap();
    assert!(read_checkpoint(&root.0).unwrap().is_none());
    std::os::unix::fs::symlink("missing-target", &published).unwrap();
    assert!(
        read_checkpoint(&root.0).is_err(),
        "a dangling symlink is not an initial state"
    );
    assert!(
        confirm_checkpoint(&root.0, b"candidate", |_| panic!("no regular checkpoint")).is_err()
    );
    fs::remove_file(&published).unwrap();
    fs::create_dir(&published).unwrap();
    assert!(read_checkpoint(&root.0).is_err());
}

#[test]
fn unreconciled_legacy_save_stops_and_fences_state_in_a_separate_process() {
    const CHILD: &str = "CONTROL_CHECKPOINT_FENCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::tests::unreconciled_legacy_save_stops_and_fences_state_in_a_separate_process", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("control-checkpoint-fence-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let state = std::sync::Mutex::new(State::default());
    let intended = std::cell::RefCell::new(Vec::new());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut state = state.lock().unwrap();
        state.runtime.enqueue(
            "accepted continuation".into(),
            RunKind::PortDecision,
            "test",
        );
        *intended.borrow_mut() = serde_json::to_vec(&*state).unwrap();
        let failure = commit_checkpoint(&directory, &intended.borrow(), |_| {
            Err(io::Error::other("persistent sync failure"))
        });
        assert!(matches!(&failure, Err(CheckpointError::Unconfirmed(_))));
        legacy_result(failure).unwrap();
    }));
    assert!(result.is_err());
    assert!(crate::shutdown::requested());
    assert!(
        state.is_poisoned(),
        "dispatch and request handlers must not reacquire state"
    );
    let mut response = Vec::new();
    crate::state_response(&mut response, &state, |_, _| {
        panic!("fenced request executed")
    });
    assert!(
        String::from_utf8(response)
            .unwrap()
            .contains("503 Service Unavailable")
    );
    assert_eq!(
        fs::read(directory.join("state.json")).unwrap(),
        *intended.borrow()
    );
    // Restart can only load bytes after syncing that file and its directory;
    // it never persists an in-memory rollback over the retained candidate.
    let restored: State =
        serde_json::from_slice(&read_checkpoint(&directory).unwrap().unwrap()).unwrap();
    assert_eq!(restored.runtime.runs.len(), 1);
    assert_eq!(restored.runtime.runs[0].status, "queued");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn checkpoint_failures_preserve_the_publication_phase() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(
        std::env::temp_dir().join(format!("control-checkpoint-phase-{}", std::process::id())),
    );
    fs::create_dir(&root.0).unwrap();
    let published = root.0.join("state.json");
    let temporary = root.0.join("state.json.tmp");
    fs::write(&published, b"old").unwrap();

    // Failure before rename leaves the published bytes intact, and never calls
    // the directory-sync boundary.
    fs::create_dir(&temporary).unwrap();
    let error =
        write_checkpoint_with_sync(&root.0, b"new", |_| panic!("not published")).unwrap_err();
    assert!(matches!(error, CheckpointError::NotReplaced(_)));
    assert_eq!(fs::read(&published).unwrap(), b"old");
    fs::remove_dir(&temporary).unwrap();

    // Rename failures are conservatively unconfirmed, even when this particular
    // error (a directory at the target) is known to leave that target intact.
    fs::remove_file(&published).unwrap();
    fs::create_dir(&published).unwrap();
    let error =
        write_checkpoint_with_sync(&root.0, b"new", |_| panic!("rename failed")).unwrap_err();
    assert!(matches!(error, CheckpointError::Unconfirmed(_)));
    assert!(published.is_dir());
    fs::remove_dir(&published).unwrap();

    // The exact production path has already renamed the real file when the
    // injected directory-sync failure is returned. Rolling back memory or
    // blindly retrying this error would assume facts the result cannot prove.
    fs::write(&published, b"old").unwrap();
    let error = write_checkpoint_with_sync(&root.0, b"new", |_| {
        assert_eq!(fs::read(&published).unwrap(), b"new");
        Err(io::Error::other("injected directory-sync failure"))
    })
    .unwrap_err();
    assert!(matches!(error, CheckpointError::Unconfirmed(_)));
    assert!(
        error
            .to_string()
            .contains("injected directory-sync failure")
    );
    assert_eq!(fs::read(&published).unwrap(), b"new");

    write_checkpoint(&root.0, b"confirmed").unwrap();
    assert_eq!(fs::read(&published).unwrap(), b"confirmed");
    assert!(!temporary.exists());
}

#[test]
fn finished_diagnostics_survive_compact_and_pretty_serialization() {
    let mut state = State::initial();
    state.runtime.enqueue("t".into(), RunKind::Task, "test");
    let run = &mut state.runtime.runs[0];
    let mut attempt = crate::usage::Attempt::new();
    attempt.activity.push(crate::usage::Activity {
        id: "workspace-diff".into(),
        kind: "file_change".into(),
        text: "1 file".into(),
        output: "x".repeat(5000),
        status: "completed".into(),
    });
    for i in 0..20 {
        attempt.activity.push(crate::usage::Activity {
            id: i.to_string(),
            kind: "command_execution".into(),
            text: "c".into(),
            output: "y".repeat(3000),
            status: "completed".into(),
        });
    }
    run.attempts.push(attempt);
    run.status = "completed".into();
    let pretty = serde_json::to_vec_pretty(&state).unwrap();
    let compact = serde_json::to_vec(&state).unwrap();
    assert!(compact.len() < pretty.len());
    assert_eq!(
        serde_json::to_vec(&serde_json::from_slice::<State>(&pretty).unwrap()).unwrap(),
        compact
    );
    let restored: State = serde_json::from_slice(&compact).unwrap();
    let activity = &restored.runtime.runs[0].attempts[0].activity;
    assert_eq!(activity.len(), 21);
    assert_eq!(activity[0].output.len(), 5000);
    assert_eq!(activity[1].id, "0");
    assert!(activity[1..].iter().all(|event| event.output.len() == 3000));
}
#[test]
fn restart_fences_running_and_cancelling_without_replaying_or_publishing() {
    let mut state = State::initial();
    for status in ["running", "cancelling", "queued", "completed"] {
        state.runtime.enqueue(status.into(), RunKind::Task, "test");
        let run = state.runtime.runs.last_mut().unwrap();
        run.status = status.into();
        let mut a = crate::usage::Attempt::new();
        a.workspace_id = Some(format!("{}-attempt-1", run.id));
        a.activity.push(crate::usage::Activity {
            id: "project-prepare".into(),
            kind: "workspace".into(),
            text: "Preparing workspace".into(),
            output: String::new(),
            status: "in_progress".into(),
        });
        run.attempts.push(a);
    }
    assert!(interrupt_active(&mut state));
    assert_eq!(
        state
            .runtime
            .runs
            .iter()
            .map(|r| r.status.as_str())
            .collect::<Vec<_>>(),
        vec!["interrupted", "interrupted", "queued", "completed"]
    );
    assert_eq!(state.runtime.runs[1].attempts[0].status, "interrupted");
    assert_eq!(
        crate::history::activity(
            state.runtime.runs[1].chat_id.as_deref(),
            &state.runtime.runs[1].attempts[0]
        )[0]
        .status,
        "interrupted",
        "recorded in the journal"
    );
    assert_eq!(
        state.runtime.runs[2].attempts[0].activity[0].status,
        "in_progress"
    );
    assert!(!interrupt_active(&mut state));
    assert!(
        state
            .runtime
            .runs
            .iter()
            .all(|r| r.attempts[0].artifact.is_none())
    );
}
#[test]
#[ignore = "release benchmark: use a scratch CONTROL_STATE_DIR on the filesystem being measured; no models"]
fn profile_durable_save_stages() {
    use std::{hint::black_box, time::Instant};
    // Run this test in its own process. Never point it at an existing node.
    let directory = crate::config::state_directory().unwrap();
    assert!(std::env::var_os("CONTROL_STATE_DIR").is_some());
    assert!(
        !directory.join("state.json").exists(),
        "use a fresh scratch directory"
    );
    fs::create_dir_all(&directory).unwrap();
    let samples = |values: &mut Vec<u128>| {
        values.sort_unstable();
        serde_json::json!({"median_ns":values[values.len()/2], "p95_ns":values[(values.len()-1)*95/100]})
    };
    for handoffs in [false, true] {
        let mut state = State::initial();
        for count in [1, 100, 1000] {
            while state.runtime.runs.len() < count {
                state.runtime.enqueue(
                    "Synthetic completed task".into(),
                    RunKind::Task,
                    "benchmark",
                );
                let run = state.runtime.runs.last_mut().unwrap();
                run.status = "completed".into();
                *run.result = "A bounded synthetic result.\n".repeat(16);
                if handoffs {
                    let mut mailbox = control_context::turn_mailbox::Mailbox::default();
                    mailbox
                        .deliver(
                            "receipt".into(),
                            control_context::turn_mailbox::Delivery {
                                ranges: vec![],
                                gap: false,
                                request: "s".repeat(8000),
                            },
                        )
                        .unwrap();
                    run.attempts.push(crate::usage::Attempt {
                        turn_mailbox: Some(Box::new(mailbox)).into(),
                        ..Default::default()
                    });
                    crate::history::archive_run(run).unwrap();
                }
            }
            let bytes = crate::history::persisting(|| serde_json::to_vec(&state)).unwrap();
            let mut raw = fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(directory.join(format!("sync-{handoffs}-{count}")))
                .unwrap();
            // Establish file size and directory entry before the warm samples.
            raw.write_all(&bytes).unwrap();
            raw.sync_all().unwrap();
            fs::File::open(&directory).unwrap().sync_all().unwrap();
            save(&state).unwrap();
            let (mut encode, mut sync, mut atomic, mut complete) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for round in 0..31 {
                // Rotate stage order to avoid systematically favouring one path
                // under changing filesystem load. Every write still fsyncs.
                for offset in 0..4 {
                    let stage = (round + offset) % 4;
                    let start = Instant::now();
                    match stage {
                        0 => {
                            black_box(
                                crate::history::persisting(|| {
                                    serde_json::to_vec(black_box(&state))
                                })
                                .unwrap(),
                            );
                        }
                        1 => {
                            use std::io::{Seek, SeekFrom};
                            raw.seek(SeekFrom::Start(0)).unwrap();
                            raw.write_all(black_box(&bytes)).unwrap();
                            raw.sync_all().unwrap();
                        }
                        2 => {
                            write_checkpoint(&directory, black_box(&bytes)).unwrap();
                        }
                        _ => {
                            save(black_box(&state)).unwrap();
                        }
                    }
                    [&mut encode, &mut sync, &mut atomic, &mut complete][stage]
                        .push(start.elapsed().as_nanos());
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "case":"durable_save_stages", "runs":count, "finished_handoffs":handoffs, "state_bytes":bytes.len(), "samples":31,
                    "serialization":samples(&mut encode), "overwrite_fsync":samples(&mut sync),
                    "atomic_replace_file_and_directory_fsync":samples(&mut atomic), "storage_save":samples(&mut complete),
                })
            );
        }
    }
}
#[test]
#[ignore = "manual benchmark: state lock held by a save at 1/3/10 MB, idle and under fsync load; no containers or models"]
fn profile_checkpoint_by_state_size() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    use std::time::{Duration, Instant};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct Background(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);
    impl Drop for Background {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            let failed = self
                .1
                .drain(..)
                .map(std::thread::JoinHandle::join)
                .collect::<Vec<_>>()
                .iter()
                .any(Result::is_err);
            assert!(
                !failed || std::thread::panicking(),
                "benchmark background thread failed"
            );
        }
    }
    let root =
        Cleanup(std::env::temp_dir().join(format!("control-checkpoint-{}", std::process::id())));
    let load_root = root.0.join("load");
    fs::create_dir_all(&load_root).unwrap();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let median = |mut samples: Vec<Duration>| {
        samples.sort();
        ms(samples[samples.len() / 2])
    };
    for megabytes in [1usize, 3, 10] {
        let mut state = State::initial();
        for n in 0..megabytes * 4 {
            state
                .runtime
                .enqueue(format!("run {n}"), RunKind::Task, "test");
            let mut attempt = crate::usage::Attempt::new();
            for i in 0..64 {
                let mut output = format!("{n}-{i} ");
                output.extend(std::iter::repeat_n('o', 4000 - output.len()));
                attempt.activity.push(crate::usage::Activity {
                    id: i.to_string(),
                    kind: "command_execution".into(),
                    text: "cargo test".into(),
                    output,
                    status: "completed".into(),
                });
            }
            let run = state.runtime.runs.last_mut().unwrap();
            run.attempts.push(attempt);
            run.status = "completed".into();
        }
        let size = serde_json::to_vec(&state).unwrap().len();
        assert!(
            (megabytes << 20) * 9 / 10 <= size && size <= (megabytes << 20) * 3 / 2,
            "fixture is {size} bytes"
        );
        let state = Arc::new(Mutex::new(state));
        for loaded in [false, true] {
            let mut background = Background(Arc::new(AtomicBool::new(false)), Vec::new());
            let syncs = Arc::new(AtomicU64::new(0));
            if loaded {
                let (stop, counter, load_root) =
                    (background.0.clone(), syncs.clone(), load_root.clone());
                let (ready, started) = std::sync::mpsc::sync_channel(1);
                background.1.push(std::thread::spawn(move || {
                    let mut n = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let mut file =
                            fs::File::create(load_root.join((n % 4096).to_string())).unwrap();
                        file.write_all(&[b'l'; 1024]).unwrap();
                        file.sync_all().unwrap();
                        n += 1;
                        counter.store(n, Ordering::Relaxed);
                        if n == 1 {
                            let _ = ready.send(());
                        }
                    }
                }));
                started
                    .recv_timeout(Duration::from_secs(10))
                    .expect("fsync load did not start");
            }
            let waits = Arc::new(Mutex::new(Vec::new()));
            {
                let (stop, state, waits) = (background.0.clone(), state.clone(), waits.clone());
                background.1.push(std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        drop(state.lock().unwrap());
                        waits.lock().unwrap().push(started.elapsed());
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }));
            }
            let before = syncs.load(Ordering::Relaxed);
            let (mut serialize, mut held) = (Vec::new(), Vec::new());
            for _ in 0..7 {
                let guard = state.lock().unwrap();
                let started = Instant::now();
                let data = serde_json::to_vec(&*guard).unwrap();
                serialize.push(started.elapsed());
                write_checkpoint(&root.0, &data).unwrap();
                held.push(started.elapsed());
                drop(guard);
                std::thread::sleep(Duration::from_millis(5));
            }
            let progressed = syncs.load(Ordering::Relaxed) - before;
            assert!(
                !loaded || progressed > 0,
                "fsync load made no progress while sampling"
            );
            drop(background);
            let worst = waits
                .lock()
                .unwrap()
                .iter()
                .max()
                .copied()
                .unwrap_or_default();
            println!(
                "state_bytes={size} fsync_load={loaded} load_syncs={progressed} serialize_ms={:.1} lock_held_ms={:.1} reader_max_wait_ms={:.1}",
                median(serialize),
                median(held),
                ms(worst)
            );
        }
    }
}
