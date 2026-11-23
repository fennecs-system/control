use super::*;
use crate::backend::Agent;

fn args(spec: &RunSpec) -> Vec<String> {
    crate::engine::golden::run(spec)
}
fn login(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("control-grok-{tag}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let auth = root.join("auth.json");
    private(&auth, "{}").unwrap();
    auth
}

#[test]
fn the_container_mounts_the_login_read_only_and_keeps_grok_state_on_a_tmpfs() {
    let auth = login("container");
    let args = args(&container("control-run-1", "node", &auth, "sha256:img").unwrap());
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
        diagnose(Some("Grok ended with an error: Rate limit exceeded."), b""),
        "The signed-in Grok account reached a usage or rate limit."
    );
    assert_eq!(
        diagnose(Some("Grok ended with max_tokens."), b""),
        "Grok ended with max_tokens."
    );
    assert_eq!(
        diagnose(None, b"error: 401 Unauthorized"),
        "Grok account authentication was rejected. Refresh the host login and submit a new run."
    );
    assert!(diagnose(None, b"").starts_with("Grok stopped without success."));
}

#[test]
fn the_backend_is_selected_by_its_prefix_and_treated_like_the_shared_default() {
    assert_eq!(Backend::named("grok"), Some(Backend::Grok));
    assert!(Backend::hosted().any(|b| b == Backend::Grok));
    // Control's shared framing and captured turn context, not its own prompt.
    assert!(!Grok.own_prompt() && Grok.host_shell() && !Grok.review_mount());
    assert_eq!(
        (Grok.mailbox(), Grok.probe_workspace()),
        (Some("grok-mailbox"), "grok-isolation-probe")
    );
    assert_eq!(serde_json::to_string(&Backend::Grok).unwrap(), "\"grok\"");
}

#[test]
fn grok_launch_vectors_are_pinned() {
    use crate::engine::golden::{fill, pin_equivalent, run};
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
    let call = crate::backend::TextCall {
        image: "sha256:img",
        name: "control-summary-1",
        workspace: &workspace,
        node: "node-1",
        model: "grok-4.7",
    };
    pin_equivalent(&run(&text_container(&call, &auth).unwrap()), &fill(TEXT, &values));
    let turn = turn_container(
        "control-run-1",
        "node-1",
        &auth,
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
