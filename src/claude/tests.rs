
use super::*;
#[test]
fn command_admission_survives_reload_without_replaying_an_effect() {
    let root = std::env::temp_dir().join(format!("control-claude-intent-{}", std::process::id()));
    fs::create_dir_all(root.join("workspace")).unwrap();
    let workspace = root.join("workspace");
    claim(&workspace, "request-1").unwrap();
    fs::write(workspace.join("effect.txt"), b"first execution").unwrap();
    // A new caller after an ambiguous acknowledgement still finds durable intent.
    assert!(claim(&workspace, "request-1").is_err());
    assert_eq!(
        fs::read(workspace.join("effect.txt")).unwrap(),
        b"first execution"
    );
    claim(&workspace, "request-2").unwrap();
    fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "requires pinned Claude image and local engine; subprocess signal test, no credentials or model calls"]
fn live_shutdown_stops_offline_workspace_command() {
    const CHILD: &str = "CONTROL_QA_SHELL_SIGNAL_CHILD";
    if let Ok(root) = std::env::var(CHILD) {
        let root = Path::new(&root);
        let name = std::env::var("CONTROL_QA_SHELL_SIGNAL_NAME").unwrap();
        crate::shutdown::install().unwrap();
        let result = shell(
            &Backend::Claude.image().unwrap(),
            &name,
            "shutdown-fixture",
            root,
            "printf ready > ready; sleep 90",
        );
        assert!(crate::shutdown::requested());
        assert!(result.is_err());
        return;
    }
    let image = Backend::Claude.image().expect("pinned Claude image");
    let name = format!("control-shell-shutdown-{}", std::process::id());
    let root = std::env::temp_dir().join(&name);
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    struct Probe {
        child: std::process::Child,
        name: String,
        image: String,
        root: std::path::PathBuf,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = crate::lifecycle::stop_resource(
                &self.name,
                "shutdown-fixture",
                &self.image,
                &self.root,
            );
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "claude::tests::live_shutdown_stops_offline_workspace_command",
            "--ignored",
            "--exact",
        ])
        .env(CHILD, &root)
        .env("CONTROL_QA_SHELL_SIGNAL_NAME", &name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut probe = Probe {
        child,
        name,
        image,
        root,
    };
    let start = Instant::now();
    while !probe.root.join("ready").is_file() {
        assert!(
            probe.child.try_wait().unwrap().is_none(),
            "probe exited before its command started"
        );
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "offline command did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        unsafe { libc::kill(probe.child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let start = Instant::now();
    loop {
        if let Some(status) = probe.child.try_wait().unwrap() {
            assert!(status.success(), "offline shutdown check failed");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "offline command did not stop"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let names = crate::engine::query(&[
        "ps",
        "--all",
        "--format",
        "{{.Names}}",
        "--filter",
        &format!("name={}", probe.name),
    ])
    .unwrap();
    assert!(names.trim().is_empty());
}
#[test]
#[ignore = "requires the pinned Claude image and a Linux container engine; no credentials or model calls"]
fn live_offline_workspace_isolated_and_persistent_across_processes() {
    let image = Backend::Claude.image().expect("Configure claude-image-id");
    let root = std::env::temp_dir().join(format!("control-claude-offline-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    probe(&image, "fixture-node", &root).unwrap();
    let workspace = root.join("project");
    fs::create_dir(&workspace).unwrap();
    let first = shell(
        &image,
        "control-claude-offline-first",
        "fixture-node",
        &workspace,
        "printf 1 > counter.txt; command -v rustc; command -v rg; command -v fd",
    )
    .unwrap();
    assert_eq!(first["exit_code"], 0);
    let next = shell(&image,"control-claude-offline-next","fixture-node",&workspace,"set -eu; test \"$(cat counter.txt)\" = 1; printf 2 > counter.txt; test ! -e /home/agent/.claude/.credentials.json; test ! -e /var/run/docker.sock; test -z \"${ANTHROPIC_API_KEY-}\"; test -z \"${CLAUDE_CODE_OAUTH_TOKEN-}\"; head -c 40000 /dev/zero | tr '\\0' x").unwrap();
    assert_eq!(next["exit_code"], 0);
    assert_eq!(next["stdout"].as_str().unwrap().len(), 12000);
    assert_eq!(fs::read(workspace.join("counter.txt")).unwrap(), b"2");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn failures_name_the_result_subtype_and_bad_tools_abort_early() {
    let init = "{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"mcp__control__control_workspace_run\"]}\n";
    let failed = format!(
        "{init}{{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"result\":\"Reached max turns\"}}\n"
    );
    let error = capture(failed.as_bytes(), |_| {}).unwrap_err().to_string();
    assert!(error.contains("error_max_turns") && error.contains("Reached max turns"));
    let flag = std::sync::atomic::AtomicBool::new(false);
    let bad = init.replace("mcp__control__control_workspace_run", "Bash");
    assert!(
        capture_with(bad.as_bytes(), |_| {}, &flag)
            .unwrap_err()
            .to_string()
            .contains("tool configuration")
    );
    assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    assert!(diagnose(b"429 rate limit reached").contains("rate limit"));
    assert!(diagnose(b"401 unauthorized").contains("Claude account"));
    assert!(diagnose(b"secret-ish noise").starts_with("Claude stopped"));
}
#[test]
fn stream_requires_valid_init_and_final_and_normalizes_caches() {
    let bytes = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"mcp__control__control_workspace_run\"]}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":5}}\n"
    );
    let (text, tokens) = capture(bytes.as_bytes(), |_| {}).unwrap();
    assert_eq!(text, "done");
    assert_eq!(
        tokens.unwrap(),
        Tokens {
            input_tokens: 45,
            cached_input_tokens: 30,
            output_tokens: 2
        }
    );
    assert!(
        capture(
            bytes
                .replace("mcp__control__control_workspace_run", "Bash")
                .as_bytes(),
            |_| {}
        )
        .is_err()
    );
    assert!(
        capture(
            bytes
                .replace("\"is_error\":false", "\"is_error\":true")
                .as_bytes(),
            |_| {}
        )
        .is_err()
    );
    assert!(capture(b"not json\n".as_slice(), |_| {}).is_err());
    assert!(capture(bytes.trim_end().as_bytes(), |_| {}).is_err());
}
