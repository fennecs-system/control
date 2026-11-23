use super::*;
use crate::runtime::RunStatus;
#[test]
fn run_history_shows_attempts_and_measured_tokens() {
    let mut runtime = Runtime::default();
    runtime.enqueue("Count tokens".into(), RunKind::Task, "test");
    assert!(!run_record(&runtime.runs[0]).contains("attempt"));
    let tokens = crate::usage::Tokens {
        input_tokens: 120,
        cached_input_tokens: 0,
        output_tokens: 30,
    };
    runtime.runs[0].attempts = vec![
        crate::usage::Attempt {
            tokens: Some(tokens),
            ..Default::default()
        },
        crate::usage::Attempt::default(),
    ];
    assert!(
        run_record(&runtime.runs[0]).contains("2 attempt(s) · 120 input (0 cached) · 30 output tokens (1/2 measured)")
    );
    // A running attempt shows its live totals as a lower bound so far.
    runtime.runs[0].attempts.push(crate::usage::Attempt {
        status: "running".into(),
        tokens: Some(crate::usage::Tokens {
            input_tokens: 1000,
            cached_input_tokens: 900,
            output_tokens: 20,
        }),
        accounting: crate::usage::Accounting::Partial,
        ..Default::default()
    });
    assert!(
        run_record(&runtime.runs[0])
            .contains("3 attempt(s) · 1120 input (900 cached) · 50 output tokens so far (2/3 measured)")
    );
}
#[test]
fn failed_cancel_form_checkpoint_restores_target_run_and_notice() {
    for status in ["queued", "running"] {
        let mut state = crate::State::default();
        state.runtime.enqueue("Keep my work".into(), RunKind::Task, "test");
        state.runtime.runs[0].status = RunStatus::named(status);
        state.runtime.runs[0].result = "Existing progress".into();
        state.runtime.notice = "Before cancellation".into();
        let id = state.runtime.runs[0].id.clone();
        let checkpoint = RunStatusCheckpoint::capture(&state.runtime, |run| run.id == id);
        let before = serde_json::to_vec(&state).unwrap();
        state.runtime.action("/cancel-run", &HashMap::from([("id".into(), id)]));
        assert!(
            checkpoint
                .save(&mut state, |_| Err(std::io::Error::other("disk full")))
                .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }
}
#[test]
fn workers_require_content_addressed_images() {
    assert!(!pinned_image("control-worker:latest"));
    assert!(!pinned_image("control-worker@sha256:short"));
    assert!(pinned_image(&format!("sha256:{}", "a".repeat(64))));
    assert!(pinned_image(&format!("control-worker@sha256:{}", "a".repeat(64))));
}
#[test]
fn retry_does_not_repeat_completed_work() {
    let mut rt = Runtime::default();
    rt.enqueue("Improve review".into(), RunKind::SelfImprovement, "website");
    let id = rt.runs[0].id.clone();
    rt.runs[0].status = RunStatus::Completed;
    rt.action("/retry-run", &HashMap::from([("id".into(), id)]));
    assert_eq!(rt.runs[0].status, RunStatus::Completed);
}
#[test]
fn snapshots_exclude_secrets_and_symlinks() {
    let root = std::env::temp_dir().join(format!(
        "control-snapshot-{}",
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
    ));
    let source = root.join("source");
    let dest = root.join("dest");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("assets")).unwrap();
    fs::write(
        source.join("assets/discord-access.example.json"),
        include_bytes!("../../assets/discord-access.example.json"),
    )
    .unwrap();
    fs::write(source.join("discord-access.json"), "private-node-config").unwrap();
    fs::write(source.join(".env"), "fake-secret").unwrap();
    fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
    fs::create_dir_all(source.join("tests")).unwrap();
    fs::create_dir_all(source.join(".github/workflows")).unwrap();
    fs::write(source.join("tests/integration.rs"), "#[test] fn fixture() {}").unwrap();
    fs::write(source.join(".github/workflows/rust.yml"), "name: fixture").unwrap();
    fs::create_dir_all(source.join("tools/playground")).unwrap();
    fs::write(source.join("tools/playground/main.rs"), "fn main() {}").unwrap();
    fs::write(source.join("tools/.env"), "nested-secret").unwrap();
    fs::write(source.join("src/.env"), "nested-secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(source.join(".env"), source.join("src/linked.rs")).unwrap();
    snapshot(&source, &dest).unwrap();
    assert!(dest.join("src/main.rs").exists());
    assert_eq!(
        fs::read(dest.join("assets/discord-access.example.json")).unwrap(),
        include_bytes!("../../assets/discord-access.example.json")
    );
    assert!(!dest.join("discord-access.json").exists());
    assert!(dest.join("tests/integration.rs").exists());
    assert!(dest.join(".github/workflows/rust.yml").exists());
    assert!(dest.join("tools/playground/main.rs").exists());
    assert!(!dest.join("tools/.env").exists());
    assert!(!dest.join(".env").exists());
    assert!(!dest.join("src/.env").exists());
    assert!(!dest.join("src/linked.rs").exists());
    fs::remove_dir_all(root).unwrap();
}
