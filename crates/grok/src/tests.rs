use super::*;
use std::path::PathBuf;

fn args(spec: &RunSpec) -> Vec<String> {
    control_isolation::golden::run(spec)
}
fn login(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("control-grok-{tag}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let auth = root.join("auth.json");
    cli::private_file(&auth, "{}").unwrap();
    auth
}
/// The login as the host grants it: the file and its owner.
fn granted(auth: &Path) -> Granted {
    let meta = fs::symlink_metadata(auth).unwrap();
    Granted::located(
        control_agent::Form::Session,
        auth.to_path_buf(),
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
    )
}
fn message(ended: Option<&str>, stderr: &[u8]) -> String {
    let ended = ended.map(|text| Ending {
        text: text.to_owned(),
        rejected_tools: false,
    });
    diagnose(ended, stderr).message()
}

#[test]
fn the_container_mounts_the_login_read_only_and_keeps_grok_state_on_a_tmpfs() {
    let auth = login("container");
    let args = args(&container("control-run-1", "node", &granted(&auth), "sha256:img").unwrap());
    let joined = args.join(" ");
    assert!(joined.contains(&format!(
        "type=bind,src={},dst=/home/agent/.grok/auth.json,readonly",
        auth.display()
    )));
    assert!(joined.contains("--tmpfs /home/agent/.grok:rw,nosuid,"));
    assert!(joined.contains("--env GROK_HOME=/home/agent/.grok"));
    // The shared CLI isolation: immutable root, no capabilities, no new privileges.
    for flag in [
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pull=never",
    ] {
        assert!(args.iter().any(|a| a == flag), "{flag}");
    }
    assert!(!joined.contains("XAI_API_KEY"));
    fs::remove_dir_all(auth.parent().unwrap()).unwrap();
}

#[test]
fn every_run_removes_groks_own_tools_and_a_text_call_the_mcp_tools_too() {
    // An `mcp__` allowlist entry names no built-in tool and is never unknown.
    assert!(NO_TOOLS.starts_with("mcp__"));
    for flag in [
        "--always-approve",
        "--no-subagents",
        "--disable-web-search",
        "--no-memory",
        "--no-auto-update",
    ] {
        assert!(FLAGS.contains(&flag), "{flag}");
    }
    assert_eq!(&FLAGS[..2], ["--output-format", "streaming-json"]);
    // The catalog prefix the decoder expects is this server's name.
    assert_eq!(CONFIG, "[mcp_servers.control]\ncommand = \"/bin/control-room-mcp\"\n");
}

#[test]
fn a_known_failure_gets_the_fixed_message_and_an_unknown_one_groks_own_ending() {
    assert_eq!(
        message(Some("Grok ended with an error: Rate limit exceeded."), b""),
        "The signed-in Grok account reached a usage or rate limit."
    );
    assert_eq!(
        message(Some("Grok ended with max_tokens."), b""),
        "Grok ended with max_tokens."
    );
    assert_eq!(
        message(None, b"error: 401 Unauthorized"),
        "Grok account authentication was rejected. Refresh the host login and submit a new run."
    );
    assert!(message(None, b"").starts_with("Grok stopped without success."));
}

#[test]
fn the_backend_is_selected_by_its_prefix_and_treated_like_the_shared_default() {
    // Control's shared framing and captured turn context, not its own prompt.
    assert!(Grok.own_prompt("", false).is_none() && Grok.host_shell() && !Grok.review_mount());
    assert_eq!(
        (Grok.mailbox(), Grok.probe_workspace()),
        (Some("grok-mailbox"), "grok-isolation-probe")
    );
}

#[test]
fn grok_launch_vectors_are_pinned() {
    use control_isolation::golden::{fill, pin_equivalent, run};
    let auth = login("golden");
    let root = auth.parent().unwrap().to_path_buf();
    let meta = fs::symlink_metadata(&auth).unwrap();
    let (uid, gid) = (meta.uid().to_string(), meta.gid().to_string());
    let values = [
        ("root", root.to_str().unwrap()),
        ("uid", uid.as_str()),
        ("gid", gid.as_str()),
    ];
    let workspace = root.join("workspace");
    let granted = granted(&auth);
    let call = TextCall {
        image: "sha256:img",
        name: "control-summary-1",
        workspace: &workspace,
        node: "node-1",
        model: "grok-4.7",
        login: Some(&granted),
    };
    pin_equivalent(&run(&text_container(&call, &granted).unwrap()), &fill(TEXT, &values));
    let turn = turn_container(
        "control-run-1",
        "node-1",
        &granted,
        &root.join("grok-config.toml"),
        &root.join("grok-prompt"),
        &root.join(MAILBOX),
        "sha256:img",
        "grok-4.7",
    )
    .unwrap();
    pin_equivalent(&run(&turn), &fill(TURN, &values));
    let login = login_container(
        "control-grok-login-7",
        "node-1",
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
        &root,
        "sha256:img",
    )
    .unwrap();
    pin_equivalent(&run(&login), &fill(LOGIN, &values));
    fs::remove_dir_all(root).unwrap();
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
    "{uid}:{gid}",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid={uid},gid={gid},size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--interactive",
    "--env",
    "GROK_HOME=/home/agent/.grok",
    "--tmpfs",
    "/home/agent/.grok:rw,nosuid,uid={uid},gid={gid},size=64m",
    "--mount",
    "type=bind,src={root}/auth.json,dst=/home/agent/.grok/auth.json,readonly",
    "--mount",
    "type=bind,src={root}/workspace,dst=/workspace,readonly",
    "sha256:img",
    "bash",
    "-c",
    "cat > /tmp/prompt && exec grok \"$@\" --prompt-file /tmp/prompt",
    "grok",
    "--output-format",
    "streaming-json",
    "--no-auto-update",
    "--always-approve",
    "--no-subagents",
    "--disable-web-search",
    "--no-memory",
    "--no-plan",
    "--verbatim",
    "--tools",
    "mcp__control",
    "--disallowed-tools",
    "search_tool,use_tool",
    "--model",
    "grok-4.7",
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
    "{uid}:{gid}",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid={uid},gid={gid},size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--interactive",
    "--env",
    "GROK_HOME=/home/agent/.grok",
    "--tmpfs",
    "/home/agent/.grok:rw,nosuid,uid={uid},gid={gid},size=64m",
    "--mount",
    "type=bind,src={root}/auth.json,dst=/home/agent/.grok/auth.json,readonly",
    "--mount",
    "type=bind,src={root}/grok-config.toml,dst=/home/agent/.grok/config.toml,readonly",
    "--mount",
    "type=bind,src={root}/grok-prompt,dst=/home/agent/prompt,readonly",
    "--mount",
    "type=bind,src={root}/grok-mailbox,dst=/workspace",
    "sha256:img",
    "grok",
    "--prompt-file",
    "/home/agent/prompt",
    "--output-format",
    "streaming-json",
    "--no-auto-update",
    "--always-approve",
    "--no-subagents",
    "--disable-web-search",
    "--no-memory",
    "--no-plan",
    "--verbatim",
    "--tools",
    "mcp__control",
    "--model",
    "grok-4.7",
];
const LOGIN: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-grok-login-7",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-grok-login-7",
    "--label",
    "dev.control.purpose=check",
    "--user",
    "{uid}:{gid}",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=256",
    "--memory=2g",
    "--cpus=2",
    "--tmpfs",
    "/tmp:rw,nosuid,size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,uid={uid},gid={gid},size=64m",
    "--env",
    "HOME=/home/agent",
    "--env",
    "CLAUDE_CONFIG_DIR=/home/agent/.claude",
    "--workdir",
    "/workspace",
    "--interactive",
    "--tty",
    "--env",
    "GROK_HOME=/home/agent/.grok",
    "--tmpfs",
    "/workspace:rw,nosuid,size=1m",
    "--mount",
    "type=bind,src={root},dst=/home/agent/.grok",
    "sha256:img",
    "grok",
    "login",
    "--device-auth",
];

/// An API-key account's containers: the key is one read-only secret mount,
/// `control-grok` exports it for Grok alone, and Control's MCP server starts
/// without it.
#[test]
fn an_api_key_is_mounted_and_exported_only_inside_grok() {
    let root = std::env::temp_dir().join(format!("control-grok-key-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let key = root.join(KEY_FILE);
    cli::private_file(&key, "xai-CANARY").unwrap();
    let meta = fs::symlink_metadata(&key).unwrap();
    let granted = Granted::located(
        Form::ApiKey,
        key.clone(),
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
    );
    let call = TextCall {
        image: "sha256:img",
        name: "control-summary-1",
        workspace: &root,
        node: "node-1",
        model: "grok-4.7",
        login: Some(&granted),
    };
    let text = text_container(&call, &granted).unwrap();
    let turn = turn_container(
        "control-run-1",
        "node-1",
        &granted,
        &root.join("grok-config.toml"),
        &root.join("grok-prompt"),
        &root,
        "sha256:img",
        "grok-4.7",
    )
    .unwrap();
    assert_eq!(turn.argv[0], "control-grok");
    assert!(text.argv[2].contains("exec control-grok "), "{:?}", text.argv);
    for spec in [&text, &turn] {
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
            ("grok/api-key", KEY, Access::ReadOnly, &key)
        );
        let shown = format!("{:?}{:?}", spec.argv, spec.env);
        assert!(!shown.contains("CANARY") && !shown.contains("XAI_API_KEY"), "{shown}");
    }
    assert_eq!(config_for(&granted), KEY_CONFIG);
    assert!(KEY_CONFIG.contains(r#"args = ["-u", "XAI_API_KEY", "/bin/control-room-mcp"]"#));
    assert!(Grok.private_names().contains(&KEY_FILE));
    fs::remove_dir_all(root).unwrap();
}
