use super::*;
use crate::backend::Agent;

fn args(spec: &RunSpec) -> Vec<String> {
    crate::engine::golden::run(spec)
}
fn directory(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("control-antigravity-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&root).unwrap();
    root
}

#[test]
fn the_login_is_chosen_by_which_private_file_exists() {
    let root = directory("login");
    assert!(login_in(&root).is_err());
    // A login directory without a saved token is not a sign-in yet.
    let token = root.join(SAVED);
    fs::create_dir_all(token.parent().unwrap()).unwrap();
    assert!(login_in(&root).is_err());
    private_file(&token, "").unwrap();
    assert_eq!(login_in(&root).unwrap(), Login::Google(token.clone()));
    private_file(&root.join("adc.json"), "{}").unwrap();
    assert_eq!(login_in(&root).unwrap(), Login::Adc(root.join("adc.json")));
    // An API key wins, but only as a private file.
    fs::write(root.join("gemini-api-key"), "k").unwrap();
    fs::set_permissions(root.join("gemini-api-key"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(login_in(&root).unwrap(), Login::Adc(root.join("adc.json")));
    fs::set_permissions(root.join("gemini-api-key"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(login_in(&root).unwrap(), Login::ApiKey(root.join("gemini-api-key")));
    // A shared login directory is refused outright.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(login_in(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn each_login_is_mounted_into_this_container_only_and_state_stays_on_a_tmpfs() {
    let root = directory("container");
    let private = root.join(PRIVATE);
    fs::create_dir(&private).unwrap();
    let token = root.join("antigravity-oauth-token");
    private_file(&token, "").unwrap();
    let joined = args_of(&Login::Google(token.clone()), &private);
    assert!(joined.contains(&format!("type=bind,src={},dst={TOKEN},readonly", token.display())));
    assert!(joined.contains(&format!(
        "type=bind,src={},dst=/home/agent/.control,readonly",
        private.display()
    )));
    assert!(joined.contains("--tmpfs /home/agent/.gemini:rw,nosuid,"));
    let key = root.join("gemini-api-key");
    private_file(&key, "k").unwrap();
    let args = args(
        &container(
            "control-run-1",
            "node",
            &Login::ApiKey(key.clone()),
            &private,
            "sha256:img",
        )
        .unwrap(),
    );
    let joined = args.join(" ");
    assert!(joined.contains(&format!("type=bind,src={},dst={KEY},readonly", key.display())));
    assert!(!joined.contains(TOKEN) && !joined.contains("GEMINI_API_KEY"));
    // The shared CLI isolation: immutable root, no capabilities, no new privileges.
    for flag in [
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pull=never",
    ] {
        assert!(args.iter().any(|a| a == flag), "{flag}");
    }
    let adc = args_of(&Login::Adc(key.clone()), &private);
    assert!(adc.contains("--env AGY_ADC_AUTH=true") && adc.contains(&format!("dst={ADC},readonly")));
    fs::remove_dir_all(root).unwrap();
}
fn args_of(login: &Login, private: &Path) -> String {
    args(&container("control-run-1", "node", login, private, "sha256:img").unwrap()).join(" ")
}

#[test]
fn every_run_denies_the_clis_own_tools_and_only_a_turn_allows_controls() {
    let google = Login::Google(PathBuf::from("/k"));
    let turn = settings(&google, true);
    assert_eq!(turn["permissions"]["allow"], json!(["mcp(control/*)"]));
    for rule in [
        "command(*)",
        "read_file(*)",
        "write_file(*)",
        "read_url(*)",
        "execute_url(*)",
        "download_url(*)",
    ] {
        assert!(
            turn["permissions"]["deny"].as_array().unwrap().contains(&json!(rule)),
            "{rule}"
        );
    }
    assert!(turn.get("modelProvider").is_none());
    let text = settings(&Login::ApiKey(PathBuf::from("/k")), false);
    assert_eq!(text["permissions"]["allow"], json!([]));
    assert_eq!(text["modelProvider"], "gemini");
    assert_eq!(
        &FLAGS[..2],
        ["--dangerously-skip-permissions", "--disable-slash-commands"]
    );
    // Every Control tool is eager, under the server name the decoder expects.
    let mcp = mcp_config();
    let server = &mcp["mcpServers"]["control"];
    assert_eq!(server["command"], "/bin/control-room-mcp");
    for name in control_context::worker_mcp_names() {
        assert_eq!(server["tools"][name]["eager"], true, "{name}");
    }
}

#[test]
fn a_turn_keeps_its_configuration_and_prompt_private_beside_the_workspace() {
    let root = directory("prepare");
    let private = prepare(
        &root,
        &Login::Google(root.clone()),
        Some(&mcp_config()),
        Some("objective"),
    )
    .unwrap();
    assert_eq!(private, root.join(PRIVATE));
    assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
    for name in ["settings.json", "mcp_config.json", "prompt"] {
        assert_eq!(
            fs::metadata(private.join(name)).unwrap().mode() & 0o777,
            0o600,
            "{name}"
        );
    }
    assert_eq!(fs::read_to_string(private.join("prompt")).unwrap(), "objective");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_known_failure_gets_the_fixed_message_and_an_unknown_one_the_clis_ending() {
    assert!(
        diagnose(None, b"Error: authentication required")
            .starts_with("Antigravity's login is missing, expired or was rejected.")
    );
    assert!(
        diagnose(
            Some("Antigravity ended with ERROR: Authentication expired. Please log back in."),
            b""
        )
        .starts_with("Antigravity's login is missing, expired or was rejected.")
    );
    // Endings the pinned CLI printed in this image with no login and with an invalid key.
    assert!(
        diagnose(
            Some("Antigravity ended with ERROR: authentication failed or timed out."),
            b""
        )
        .starts_with("Antigravity's login is missing")
    );
    assert!(diagnose(Some("Antigravity ended with ERROR: agent executor error: generating and executing: Error 400, Message: API key not valid. Please pass a valid API key., Status: INVALID_ARGUMENT."), b"").starts_with("Antigravity's login is missing"));
    assert_eq!(
        diagnose(Some("Antigravity ended with ERROR: RESOURCE_EXHAUSTED."), b""),
        "The signed-in Antigravity account reached a usage or rate limit."
    );
    assert_eq!(
        diagnose(Some("Antigravity ended with ERROR: rate limit exceeded."), b""),
        "The signed-in Antigravity account reached a usage or rate limit."
    );
    assert_eq!(
        diagnose(Some("Antigravity ended with WAITING."), b""),
        "Antigravity ended with WAITING."
    );
    assert!(diagnose(None, b"").starts_with("Antigravity stopped without success."));
}

#[test]
fn the_backend_is_selected_by_its_prefix_and_treated_like_the_shared_default() {
    assert_eq!(Backend::named("antigravity"), Some(Backend::Antigravity));
    assert!(Backend::hosted().any(|b| b == Backend::Antigravity));
    // Control's shared framing and captured turn context, not its own prompt.
    assert!(!Antigravity.own_prompt() && Antigravity.host_shell() && !Antigravity.review_mount());
    assert_eq!(
        (Antigravity.mailbox(), Antigravity.probe_workspace()),
        (Some("antigravity-mailbox"), "antigravity-isolation-probe")
    );
    assert_eq!(serde_json::to_string(&Backend::Antigravity).unwrap(), "\"antigravity\"");
}

#[test]
fn antigravity_launch_vectors_are_pinned() {
    use crate::engine::golden::{fill, pin_equivalent, run};
    let root = directory("golden");
    let private = root.join(PRIVATE);
    fs::create_dir(&private).unwrap();
    let (key, adc, token) = (
        root.join("gemini-api-key"),
        root.join("adc.json"),
        root.join("antigravity-oauth-token"),
    );
    for file in [&key, &adc, &token] {
        private_file(file, "").unwrap();
    }
    let meta = fs::symlink_metadata(&key).unwrap();
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
        model: "gemini",
    };
    pin_equivalent(
        &run(&text_container(&call, &Login::ApiKey(key.clone()), &private).unwrap()),
        &fill(TEXT_KEY, &values),
    );
    pin_equivalent(
        &run(&text_container(&call, &Login::Adc(adc.clone()), &private).unwrap()),
        &fill(TEXT_ADC, &values),
    );
    let turn = turn_container(
        "control-run-1",
        "node-1",
        &Login::Google(token.clone()),
        &private,
        &root.join(MAILBOX),
        "sha256:img",
        "gemini",
    )
    .unwrap();
    pin_equivalent(&run(&turn), &fill(TURN_GOOGLE, &values));
    let login = login_container(
        "control-antigravity-login-7",
        "node-1",
        User {
            uid: meta.uid(),
            gid: meta.gid(),
        },
        &root.join("login/gemini"),
        &root.join("login/config"),
        "sha256:img",
    )
    .unwrap();
    pin_equivalent(&run(&login), &fill(LOGIN, &values));
    let check = check_container(
        "control-antigravity-probe-7-login",
        "node-1",
        &Login::Google(token),
        &private,
        "sha256:img",
    )
    .unwrap();
    pin_equivalent(&run(&check), &fill(CHECK, &values));
    fs::remove_dir_all(root).unwrap();
}
const TEXT_KEY: &[&str] = &[
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
    "--tmpfs",
    "/home/agent/.gemini:rw,nosuid,uid={uid},gid={gid},size=256m",
    "--mount",
    "type=bind,src={root}/antigravity-private,dst=/home/agent/.control,readonly",
    "--mount",
    "type=bind,src={root}/gemini-api-key,dst=/home/agent/.control-key/gemini-api-key,readonly",
    "--mount",
    "type=bind,src={root}/workspace,dst=/workspace,readonly",
    "sha256:img",
    "control-agy",
    "text",
    "--dangerously-skip-permissions",
    "--disable-slash-commands",
    "--output-format",
    "stream-json",
    "--model",
    "gemini",
];
const TEXT_ADC: &[&str] = &[
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
    "--tmpfs",
    "/home/agent/.gemini:rw,nosuid,uid={uid},gid={gid},size=256m",
    "--mount",
    "type=bind,src={root}/antigravity-private,dst=/home/agent/.control,readonly",
    "--env",
    "AGY_ADC_AUTH=true",
    "--mount",
    "type=bind,src={root}/adc.json,dst=/home/agent/.config/gcloud/application_default_credentials.json,readonly",
    "--mount",
    "type=bind,src={root}/workspace,dst=/workspace,readonly",
    "sha256:img",
    "control-agy",
    "text",
    "--dangerously-skip-permissions",
    "--disable-slash-commands",
    "--output-format",
    "stream-json",
    "--model",
    "gemini",
];
const TURN_GOOGLE: &[&str] = &[
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
    "--tmpfs",
    "/home/agent/.gemini:rw,nosuid,uid={uid},gid={gid},size=256m",
    "--mount",
    "type=bind,src={root}/antigravity-private,dst=/home/agent/.control,readonly",
    "--mount",
    "type=bind,src={root}/antigravity-oauth-token,dst=/home/agent/.gemini/antigravity-cli/antigravity-oauth-token,readonly",
    "--mount",
    "type=bind,src={root}/antigravity-mailbox,dst=/workspace",
    "sha256:img",
    "control-agy",
    "turn",
    "--dangerously-skip-permissions",
    "--disable-slash-commands",
    "--output-format",
    "stream-json",
    "--model",
    "gemini",
];
const LOGIN: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-antigravity-login-7",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-antigravity-login-7",
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
    "--mount",
    "type=bind,src={root}/login/gemini,dst=/home/agent/.gemini",
    "--mount",
    "type=bind,src={root}/login/config,dst=/home/agent/.config/antigravity",
    "--env",
    "SSH_CONNECTION=control-login 0 container 22",
    "--tmpfs",
    "/workspace:rw,nosuid,size=1m",
    "sha256:img",
    "control-agy",
    "login",
];
const CHECK: &[&str] = &[
    "run",
    "--rm",
    "--pull=never",
    "--name",
    "control-antigravity-probe-7-login",
    "--label",
    "dev.control.node=node-1",
    "--label",
    "dev.control.attempt=control-antigravity-probe-7-login",
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
    "--tmpfs",
    "/home/agent/.gemini:rw,nosuid,uid={uid},gid={gid},size=256m",
    "--mount",
    "type=bind,src={root}/antigravity-private,dst=/home/agent/.control,readonly",
    "--mount",
    "type=bind,src={root}/antigravity-oauth-token,dst=/home/agent/.gemini/antigravity-cli/antigravity-oauth-token,readonly",
    "--tmpfs",
    "/workspace:rw,nosuid,size=1m",
    "sha256:img",
    "control-agy",
    "check",
];
