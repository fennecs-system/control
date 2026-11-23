use super::*;
use crate::backend::Agent;

fn args(command: &Command) -> Vec<String> {
    command.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
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
    let args = args(&container("control-run-1", "node", &auth).unwrap());
    let joined = args.join(" ");
    assert!(joined.contains(&format!("type=bind,src={},dst=/home/agent/.grok/auth.json,readonly", auth.display())));
    assert!(joined.contains("--tmpfs /home/agent/.grok:rw,nosuid,"));
    assert!(joined.contains("--env GROK_HOME=/home/agent/.grok"));
    // The shared CLI isolation: immutable root, no capabilities, no new privileges.
    for flag in ["--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--pull=never"] {
        assert!(args.iter().any(|a| a == flag), "{flag}");
    }
    assert!(!joined.contains("XAI_API_KEY"));
    fs::remove_dir_all(auth.parent().unwrap()).unwrap();
}

#[test]
fn every_run_removes_groks_own_tools_and_a_text_call_the_mcp_tools_too() {
    // An `mcp__` allowlist entry names no built-in tool and is never unknown.
    assert!(NO_TOOLS.starts_with("mcp__"));
    for flag in ["--always-approve", "--no-subagents", "--disable-web-search", "--no-memory", "--no-auto-update"] {
        assert!(FLAGS.contains(&flag), "{flag}");
    }
    assert_eq!(&FLAGS[..2], ["--output-format", "streaming-json"]);
    // The catalog prefix the decoder expects is this server's name.
    assert_eq!(CONFIG, "[mcp_servers.control]\ncommand = \"/bin/control-room-mcp\"\n");
}

#[test]
fn a_known_failure_gets_the_fixed_message_and_an_unknown_one_groks_own_ending() {
    assert_eq!(diagnose(Some("Grok ended with an error: Rate limit exceeded."), b""), "The signed-in Grok account reached a usage or rate limit.");
    assert_eq!(diagnose(Some("Grok ended with max_tokens."), b""), "Grok ended with max_tokens.");
    assert_eq!(diagnose(None, b"error: 401 Unauthorized"), "Grok account authentication was rejected. Refresh the host login and submit a new run.");
    assert!(diagnose(None, b"").starts_with("Grok stopped without success."));
}

#[test]
fn the_backend_is_selected_by_its_prefix_and_treated_like_the_shared_default() {
    assert_eq!(Backend::named("grok"), Some(Backend::Grok));
    assert!(Backend::hosted().any(|b| b == Backend::Grok));
    // Control's shared framing and captured turn context, not its own prompt.
    assert!(!Grok.own_prompt() && Grok.host_shell() && !Grok.review_mount());
    assert_eq!((Grok.mailbox(), Grok.probe_workspace()), (Some("grok-mailbox"), "grok-isolation-probe"));
    assert_eq!(serde_json::to_string(&Backend::Grok).unwrap(), "\"grok\"");
}
