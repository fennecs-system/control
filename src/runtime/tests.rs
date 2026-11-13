
use super::*;
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
    assert!(run_record(&runtime.runs[0]).contains("2 attempt(s) · 120 input · 30 output tokens"));
}
#[test]
fn failed_cancel_form_checkpoint_restores_target_run_and_notice() {
    for status in ["queued", "running"] {
        let mut state = crate::State::default();
        state
            .runtime
            .enqueue("Keep my work".into(), RunKind::Task, "test");
        state.runtime.runs[0].status = status.into();
        state.runtime.runs[0].result = "Existing progress".into();
        state.runtime.notice = "Before cancellation".into();
        let id = state.runtime.runs[0].id.clone();
        let checkpoint = RunStatusCheckpoint::capture(&state.runtime, |run| run.id == id);
        let before = serde_json::to_vec(&state).unwrap();
        state
            .runtime
            .action("/cancel-run", &HashMap::from([("id".into(), id)]));
        assert!(
            checkpoint
                .save(&mut state, |_| Err(std::io::Error::other("disk full")))
                .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }
}
#[test]
fn engine_inspection_drains_pipes_and_rejects_partial_results() {
    let output = engine_output(
        Command::new("head").args(["-c", "131072", "/dev/zero"]),
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(output.len(), 131072);
    assert!(
        engine_output(
            Command::new("head").args(["-c", "1048577", "/dev/zero"]),
            std::time::Duration::from_secs(5),
        )
        .is_err()
    );
}
#[test]
fn engine_inspection_deadline_does_not_wait_for_process_exit() {
    let started = std::time::Instant::now();
    assert!(
        engine_output(
            Command::new("sleep").arg("5"),
            std::time::Duration::from_millis(30),
        )
        .is_err()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
#[test]
fn engine_creation_diagnostics_are_opt_in_and_bounded() {
    let missing = "/control-qa-missing-storage-probe";
    let error = engine_output_diagnostic(
        Command::new("ls").arg(missing),
        std::time::Duration::from_secs(2),
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains(missing));
    let redacted = engine_output(
        Command::new("ls").arg(missing),
        std::time::Duration::from_secs(2),
    )
    .unwrap_err();
    assert_eq!(redacted.to_string(), "Container engine request failed");
    let error = engine_output_diagnostic(
        Command::new("sh").args(["-c", "head -c 65536 /dev/zero >&2; exit 1"]),
        std::time::Duration::from_secs(2),
        true,
    )
    .unwrap_err();
    assert!(error.to_string().len() <= 4096 + "Container engine request failed: ".len());
}
#[test]
fn workers_require_content_addressed_images() {
    assert!(!pinned_image("control-worker:latest"));
    assert!(!pinned_image("control-worker@sha256:short"));
    assert!(pinned_image(&format!("sha256:{}", "a".repeat(64))));
    assert!(pinned_image(&format!(
        "control-worker@sha256:{}",
        "a".repeat(64)
    )));
}
#[test]
fn retry_does_not_repeat_completed_work() {
    let mut rt = Runtime::default();
    rt.enqueue("Improve review".into(), RunKind::SelfImprovement, "website");
    let id = rt.runs[0].id.clone();
    rt.runs[0].status = "completed".into();
    rt.action("/retry-run", &HashMap::from([("id".into(), id)]));
    assert_eq!(rt.runs[0].status, "completed");
}
#[test]
fn snapshots_exclude_secrets_and_symlinks() {
    let root = std::env::temp_dir().join(format!(
        "control-snapshot-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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
    fs::write(
        source.join("tests/integration.rs"),
        "#[test] fn fixture() {}",
    )
    .unwrap();
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
