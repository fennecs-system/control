
use super::*;
use std::process::{Command, Stdio};
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
                crate::engine::isolation(),
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
    probe(crate::engine::isolation(), &image, "fixture-node", &root).unwrap();
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
        capture_with(bad.as_bytes(), control_ai::claude::Claude::new(&control_context::worker_mcp_names()), "Claude", |_| {}, |_| {}, &flag).0
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

#[test]
fn claude_launch_vectors_are_pinned() {
    use crate::engine::golden::{fill, pin_equivalent, run};
    let (root, auth) = (Path::new("/golden/root"), Path::new("/golden/auth/.credentials.json"));
    let tools = control_context::worker_mcp_names().into_iter().map(|name| format!("mcp__control__{name}")).collect::<Vec<_>>().join(",");
    let values = [("tools", tools.as_str())];
    let text = text_container("sha256:img", "control-summary-1", &root.join("workspace"), "node-1", "sonnet", auth, User { uid: 501, gid: 20 }).unwrap();
    pin_equivalent(&run(&text), &fill(TEXT, &values));
    let shell = shell_container("sha256:img", "control-run-1-shell", "node-1", &root.join("workspace"), "cargo test", false, true, User { uid: 501, gid: 20 }).unwrap();
    pin_equivalent(&run(&shell), &fill(SHELL, &values));
    let writable = shell_container("sha256:img", "control-run-1-shell", "node-1", &root.join("workspace"), "cargo test", true, false, User { uid: 501, gid: 20 }).unwrap();
    pin_equivalent(&run(&writable), &fill(SHELL_FULL_ROOT, &values));
    let turn = turn_container("control-run-1", "node-1", User { uid: 501, gid: 20 }, auth, &root.join("claude-mailbox"), "sha256:img", "opus").unwrap();
    pin_equivalent(&run(&turn), &fill(TURN, &values));
    let login = login_container("control-claude-login-7", "node-1", User { uid: 501, gid: 20 }, Path::new("/golden/auth"), "sha256:img").unwrap();
    pin_equivalent(&run(&login), &fill(LOGIN, &values));
}
const TEXT: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-summary-1",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-summary-1",
    "--label",
    "dev.control.purpose=check",
    "--user",
    "501:20",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid=501,gid=20,size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--interactive",
    "--tmpfs",
    "/home/agent/.claude:rw,nosuid,uid=501,gid=20,size=32m",
    "--mount",
    "type=bind,src=/golden/auth/.credentials.json,dst=/home/agent/.claude/.credentials.json,readonly",
    "--mount",
    "type=bind,src=/golden/root/workspace,dst=/workspace,readonly",
    "sha256:img",
    "claude",
    "--print",
    "--output-format",
    "stream-json",
    "--verbose",
    "--no-session-persistence",
    "--restricted",
    "--tools",
    "",
    "--disable-slash-commands",
    "--setting-sources",
    "",
    "--strict-mcp-config",
    "--mcp-config",
    "{\"mcpServers\":{}}",
    "--permission-mode",
    "dontAsk",
    "--model",
    "sonnet",
];
const SHELL: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-run-1-shell",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-run-1-shell",
    "--label",
    "dev.control.purpose=run",
    "--user",
    "501:20",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid=501,gid=20,size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--network=none",
    "--mount",
    "type=bind,src=/golden/root/workspace,dst=/workspace",
    "sha256:img",
    "bash",
    "-c",
    "cargo test",
];
const SHELL_FULL_ROOT: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-run-1-shell",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-run-1-shell",
    "--label",
    "dev.control.purpose=run",
    "--user",
    "501:20",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid=501,gid=20,size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--read-only=false",
    "--network=none",
    "--mount",
    "type=bind,src=/golden/root/workspace,dst=/workspace,readonly",
    "sha256:img",
    "bash",
    "-c",
    "cargo test",
];
const TURN: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-run-1",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-run-1",
    "--label",
    "dev.control.purpose=run",
    "--user",
    "501:20",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid=501,gid=20,size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--interactive",
    "--tmpfs",
    "/home/agent/.claude:rw,nosuid,uid=501,gid=20,size=32m",
    "--mount",
    "type=bind,src=/golden/auth/.credentials.json,dst=/home/agent/.claude/.credentials.json,readonly",
    "--mount",
    "type=bind,src=/golden/root/claude-mailbox,dst=/workspace",
    "sha256:img",
    "claude",
    "--print",
    "--output-format",
    "stream-json",
    "--verbose",
    "--no-session-persistence",
    "--restricted",
    "--tools",
    "",
    "--disable-slash-commands",
    "--setting-sources",
    "",
    "--strict-mcp-config",
    "--mcp-config",
    "{\"mcpServers\":{\"control\":{\"command\":\"/bin/control-room-mcp\"}}}",
    "--permission-mode",
    "dontAsk",
    "--allowedTools",
    "mcp__control__control_agent_message,mcp__control__control_agent_result,mcp__control__control_agent_spawn,mcp__control__control_agent_spawn_blocking,mcp__control__control_agent_spawn_cancel,mcp__control__control_agent_spawn_status,mcp__control__control_bot_pause,mcp__control__control_bot_request,mcp__control__control_bot_status,mcp__control__control_browser_capture,mcp__control__control_call,mcp__control__control_chat,mcp__control__control_design_ask,mcp__control__control_github_open_pr,mcp__control__control_github_read,mcp__control__control_github_write,mcp__control__control_guidance,mcp__control__control_history_read,mcp__control__control_intel_brief,mcp__control__control_knowledge_propose,mcp__control__control_knowledge_read,mcp__control__control_knowledge_search,mcp__control__control_model_generate,mcp__control__control_observe_set,mcp__control__control_operator_notify,mcp__control__control_package_read,mcp__control__control_packages_search,mcp__control__control_port_request,mcp__control__control_port_status,mcp__control__control_port_stop,mcp__control__control_preview_start,mcp__control__control_preview_stop,mcp__control__control_project_request,mcp__control__control_recovery,mcp__control__control_room_ack,mcp__control__control_room_claim,mcp__control__control_room_list,mcp__control__control_room_notifications,mcp__control__control_room_plan,mcp__control__control_room_post,mcp__control__control_room_read,mcp__control__control_room_watch,mcp__control__control_rust_bench,mcp__control__control_rust_run,mcp__control__control_score_probe,mcp__control__control_search_path,mcp__control__control_service_attach,mcp__control__control_service_copy,mcp__control__control_service_exec,mcp__control__control_service_exec_status,mcp__control__control_service_start,mcp__control__control_service_status,mcp__control__control_service_stop,mcp__control__control_solve,mcp__control__control_tool_search,mcp__control__control_turn_context,mcp__control__control_turn_handoff,mcp__control__control_watch_events,mcp__control__control_watch_start,mcp__control__control_watch_status,mcp__control__control_watch_stop,mcp__control__control_watch_wake,mcp__control__control_web_fetch,mcp__control__control_web_search,mcp__control__control_workspace_cancel,mcp__control__control_workspace_run,mcp__control__control_workspace_start,mcp__control__control_workspace_status",
    "--model",
    "opus",
];
const LOGIN: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-claude-login-7",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-claude-login-7",
    "--label",
    "dev.control.purpose=check",
    "--user",
    "501:20",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid=501,gid=20,size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--interactive",
    "--tty",
    "--tmpfs",
    "/workspace:rw,nosuid,size=1m",
    "--mount",
    "type=bind,src=/golden/auth,dst=/home/agent/.claude",
    "sha256:img",
    "claude",
    "auth",
    "login",
];
