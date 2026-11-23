use super::*;
use control_ai::Tokens;

fn capture(reader: impl Read, update: impl FnMut(control_ai::Activity)) -> io::Result<(String, Option<Tokens>)> {
    cli::capture_with(
        reader,
        control_ai::claude::Claude::new(&control_context::worker_mcp_names()),
        "Claude",
        update,
        |_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .0
}
fn message(stderr: &[u8]) -> String {
    diagnose(stderr).message(&control_agent::SignIn::from("control claude login".to_owned()))
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
        cli::capture_with(
            bad.as_bytes(),
            control_ai::claude::Claude::new(&control_context::worker_mcp_names()),
            "Claude",
            |_| {},
            |_| {},
            &flag
        )
        .0
        .unwrap_err()
        .to_string()
        .contains("tool configuration")
    );
    assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    assert!(message(b"429 rate limit reached").contains("rate limit"));
    assert!(message(b"401 unauthorized").contains("Claude account"));
    assert!(message(b"secret-ish noise").starts_with("Claude stopped"));
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
            bytes.replace("mcp__control__control_workspace_run", "Bash").as_bytes(),
            |_| {}
        )
        .is_err()
    );
    assert!(
        capture(
            bytes.replace("\"is_error\":false", "\"is_error\":true").as_bytes(),
            |_| {}
        )
        .is_err()
    );
    assert!(capture(b"not json\n".as_slice(), |_| {}).is_err());
    assert!(capture(bytes.trim_end().as_bytes(), |_| {}).is_err());
}

#[test]
fn claude_launch_vectors_are_pinned() {
    use control_isolation::golden::{fill, pin_equivalent, run};
    let root = Path::new("/golden/root");
    let auth = Granted::located(
        control_agent::Form::Session,
        "/golden/auth/.credentials.json".into(),
        User { uid: 501, gid: 20 },
    );
    let tools = control_context::worker_mcp_names()
        .into_iter()
        .map(|name| format!("mcp__control__{name}"))
        .collect::<Vec<_>>()
        .join(",");
    let values = [("tools", tools.as_str())];
    let text = text_container(
        "sha256:img",
        "control-summary-1",
        &root.join("workspace"),
        "node-1",
        "sonnet",
        &auth,
    )
    .unwrap();
    pin_equivalent(&run(&text), &fill(TEXT, &values));
    let turn = turn_container(
        "control-run-1",
        "node-1",
        &auth,
        &root.join("claude-mailbox"),
        "sha256:img",
        "opus",
    )
    .unwrap();
    pin_equivalent(&run(&turn), &fill(TURN, &values));
    let login = login_container(
        "control-claude-login-7",
        "node-1",
        User { uid: 501, gid: 20 },
        Path::new("/golden/auth"),
        "sha256:img",
    )
    .unwrap();
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
    "/home/agent:rw,nosuid,mode=700,uid=501,gid=20,size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid=501,gid=20,size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid=501,gid=20,size=64m",
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

/// A turn and a text call run as the login's owner and carry the login
/// read-only beneath the CLI's private home, and nothing else secret.
#[test]
fn the_login_is_read_only_beneath_the_private_home_of_the_cli_uid() {
    let owner = User { uid: 501, gid: 20 };
    let root = Path::new("/golden/root");
    for (form, target) in [(Form::Session, CREDENTIALS), (Form::ApiKey, KEY)] {
        let auth = Granted::located(form, "/golden/auth/login".into(), owner);
        let turn = turn_container("control-run-1", "node-1", &auth, &root.join("m"), "sha256:img", "opus").unwrap();
        let text = text_container(
            "sha256:img",
            "control-summary-1",
            &root.join("w"),
            "node-1",
            "sonnet",
            &auth,
        )
        .unwrap();
        for spec in [turn, text] {
            assert_eq!(spec.user, Some(owner));
            assert_eq!(cli::secrets_private(&spec), Ok(()));
            let targets: Vec<_> = spec.secrets.iter().map(|s| (s.target.as_str(), s.access)).collect();
            assert_eq!(targets, [(target, Access::ReadOnly)], "{form:?}");
        }
    }
}

/// An API-key account's turn and text call: the key is one read-only secret
/// mount, and the only flag that refers to it names the file, not the key.
#[test]
fn an_api_key_is_mounted_and_read_by_claudes_own_helper() {
    let root = std::env::temp_dir().join(format!("control-claude-key-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let key = root.join(KEY_FILE);
    cli::private_file(&key, "sk-ant-CANARY").unwrap();
    let meta = fs::symlink_metadata(&key).unwrap();
    let granted = Granted::located(
        Form::ApiKey,
        key.clone(),
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
    );
    assert_eq!(KEY_SETTINGS, format!(r#"{{"apiKeyHelper":"cat {KEY}"}}"#));
    let specs = [
        turn_container("control-run-1", "node", &granted, &root, "sha256:img", "sonnet").unwrap(),
        text_container("sha256:img", "control-summary-1", &root, "node", "sonnet", &granted).unwrap(),
    ];
    for spec in specs {
        let [secret] = spec.secrets.as_slice() else {
            panic!("one secret: {:?}", spec.secrets)
        };
        assert_eq!(
            (
                secret.name.as_str(),
                secret.target.as_str(),
                secret.access,
                &secret.source
            ),
            ("claude/api-key", KEY, Access::ReadOnly, &key)
        );
        let at = spec.argv.iter().position(|word| word == "--settings").unwrap();
        assert_eq!(spec.argv[at + 1], KEY_SETTINGS);
        assert!(spec.argv.contains(&"--restricted".to_owned()));
        let shown = format!("{:?}{:?}", spec.argv, spec.env);
        assert!(
            !shown.contains("CANARY") && !shown.contains("ANTHROPIC_API_KEY"),
            "{shown}"
        );
    }
    // A sign-in keeps its own file and no settings flag.
    let session = Granted::located(Form::Session, key.clone(), granted.owner());
    let spec = turn_container("control-run-1", "node", &session, &root, "sha256:img", "sonnet").unwrap();
    assert_eq!(spec.secrets[0].target, "/home/agent/.claude/.credentials.json");
    assert!(!spec.argv.contains(&"--settings".to_owned()));
    assert!(Claude.private_names().contains(&KEY_FILE));
    fs::remove_dir_all(root).unwrap();
}
