use super::*;

fn args(spec: &RunSpec) -> Vec<String> {
    control_isolation::golden::run(spec)
}
/// A login as the host grants it: the file and its owner.
fn granted(form: Form, path: &Path) -> Granted {
    let meta = fs::symlink_metadata(path).unwrap();
    Granted::located(
        form,
        path.to_path_buf(),
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
fn directory(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("control-antigravity-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&root).unwrap();
    root
}

#[test]
fn each_login_is_mounted_into_this_container_only_and_state_stays_on_a_tmpfs() {
    let root = directory("container");
    let private = root.join(PRIVATE);
    fs::create_dir(&private).unwrap();
    let token = root.join("antigravity-oauth-token");
    cli::private_file(&token, "").unwrap();
    let joined = args_of(&granted(Form::Session, &token), &private);
    assert!(joined.contains(&format!("type=bind,src={},dst={TOKEN},readonly", token.display())));
    assert!(joined.contains(&format!(
        "type=bind,src={},dst=/home/agent/.control,readonly",
        private.display()
    )));
    assert!(joined.contains("--tmpfs /home/agent/.gemini:rw,nosuid,"));
    let key = root.join("gemini-api-key");
    cli::private_file(&key, "k").unwrap();
    let args = args(
        &container(
            "control-run-1",
            "node",
            &granted(Form::ApiKey, &key),
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
    let adc = args_of(&granted(Form::CloudDefault, &key), &private);
    assert!(adc.contains("--env AGY_ADC_AUTH=true") && adc.contains(&format!("dst={ADC},readonly")));
    fs::remove_dir_all(root).unwrap();
}
fn args_of(login: &Granted, private: &Path) -> String {
    args(&container("control-run-1", "node", login, private, "sha256:img").unwrap()).join(" ")
}

#[test]
fn every_run_denies_the_clis_own_tools_and_only_a_turn_allows_controls() {
    let turn = settings(Form::Session, true);
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
    let text = settings(Form::ApiKey, false);
    assert_eq!(text["permissions"]["allow"], json!([]));
    assert_eq!(text["modelProvider"], "gemini");
    // Unattended without auto-approval: an action no rule allows is refused.
    assert!(!FLAGS.contains(&"--dangerously-skip-permissions"));
    assert_eq!(FLAGS[0], "--disable-slash-commands");
    // Every Control tool is eager, under the server name the decoder expects.
    let mcp = mcp_config(Form::Session);
    let server = &mcp["mcpServers"]["control"];
    assert_eq!(server["command"], "/bin/control-room-mcp");
    for name in control_context::worker_mcp_names() {
        assert_eq!(server["tools"][name]["eager"], true, "{name}");
    }
    // With a key, the server starts without the variable the entrypoint exports.
    let keyed = mcp_config(Form::ApiKey);
    let server = &keyed["mcpServers"]["control"];
    assert_eq!(
        (&server["command"], &server["args"]),
        (
            &json!("/bin/env"),
            &json!(["-u", "GEMINI_API_KEY", "/bin/control-room-mcp"])
        )
    );
    assert_eq!(server["tools"], mcp["mcpServers"]["control"]["tools"]);
}

#[test]
fn a_turn_keeps_its_configuration_and_prompt_private_beside_the_workspace() {
    let root = directory("prepare");
    let private = prepare(
        &root,
        &granted(Form::Session, &root),
        Some(&mcp_config(Form::Session)),
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
        message(None, b"Error: authentication required")
            .starts_with("Antigravity's login is missing, expired or was rejected.")
    );
    assert!(
        message(
            Some("Antigravity ended with ERROR: Authentication expired. Please log back in."),
            b""
        )
        .starts_with("Antigravity's login is missing, expired or was rejected.")
    );
    // Endings the pinned CLI printed in this image with no login and with an invalid key.
    assert!(
        message(
            Some("Antigravity ended with ERROR: authentication failed or timed out."),
            b""
        )
        .starts_with("Antigravity's login is missing")
    );
    assert!(message(Some("Antigravity ended with ERROR: agent executor error: generating and executing: Error 400, Message: API key not valid. Please pass a valid API key., Status: INVALID_ARGUMENT."), b"").starts_with("Antigravity's login is missing"));
    assert_eq!(
        message(Some("Antigravity ended with ERROR: RESOURCE_EXHAUSTED."), b""),
        "The signed-in Antigravity account reached a usage or rate limit."
    );
    assert_eq!(
        message(Some("Antigravity ended with ERROR: rate limit exceeded."), b""),
        "The signed-in Antigravity account reached a usage or rate limit."
    );
    assert_eq!(
        message(Some("Antigravity ended with WAITING."), b""),
        "Antigravity ended with WAITING."
    );
    assert!(message(None, b"").starts_with("Antigravity stopped without success."));
}

#[test]
fn the_backend_is_selected_by_its_prefix_and_treated_like_the_shared_default() {
    // Control's shared framing and captured turn context, not its own prompt.
    assert!(Antigravity.own_prompt("", false).is_none() && Antigravity.host_shell() && !Antigravity.review_mount());
    assert_eq!(
        (Antigravity.mailbox(), Antigravity.probe_workspace()),
        (Some("antigravity-mailbox"), "antigravity-isolation-probe")
    );
}

#[test]
fn antigravity_launch_vectors_are_pinned() {
    use control_isolation::golden::{fill, pin_equivalent, run};
    let root = directory("golden");
    let private = root.join(PRIVATE);
    fs::create_dir(&private).unwrap();
    let (key, adc, token) = (
        root.join("gemini-api-key"),
        root.join("adc.json"),
        root.join("antigravity-oauth-token"),
    );
    for file in [&key, &adc, &token] {
        cli::private_file(file, "").unwrap();
    }
    let meta = fs::symlink_metadata(&key).unwrap();
    let (uid, gid) = (meta.uid().to_string(), meta.gid().to_string());
    let values = [
        ("root", root.to_str().unwrap()),
        ("uid", uid.as_str()),
        ("gid", gid.as_str()),
    ];
    let workspace = root.join("workspace");
    let granted_key = granted(Form::ApiKey, &key);
    let call = TextCall {
        image: "sha256:img",
        name: "control-summary-1",
        workspace: &workspace,
        node: "node-1",
        model: "gemini",
        login: Some(&granted_key),
    };
    pin_equivalent(
        &run(&text_container(&call, &granted_key, &private).unwrap()),
        &fill(TEXT_KEY, &values),
    );
    pin_equivalent(
        &run(&text_container(&call, &granted(Form::CloudDefault, &adc), &private).unwrap()),
        &fill(TEXT_ADC, &values),
    );
    let turn = turn_container(
        "control-run-1",
        "node-1",
        &granted(Form::Session, &token),
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
        &granted(Form::Session, &token),
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
    "/home/agent:rw,nosuid,mode=700,uid={uid},gid={gid},size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid={uid},gid={gid},size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid={uid},gid={gid},size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid={uid},gid={gid},size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid={uid},gid={gid},size=64m",
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

/// A rejected tool is the typed failure even when the ending's wording
/// resembles a provider cause (a tool named after a login error).
#[test]
fn a_rejected_tool_is_always_an_unexpected_tool_failure() {
    let text = "stopped: unexpected tool (authentication required / resource_exhausted)";
    let ended = Some(Ending {
        text: text.to_owned(),
        rejected_tools: true,
    });
    assert_eq!(
        diagnose(ended, b"rate limit"),
        Failure::UnexpectedTools(text.to_owned())
    );
}

/// Each form of login runs as its owner and is carried read-only beneath the
/// CLI's private home, and nothing else secret.
#[test]
fn every_login_form_is_read_only_beneath_the_private_home_of_the_cli_uid() {
    let root = directory("private");
    let private = root.join(PRIVATE);
    fs::create_dir(&private).unwrap();
    for (form, target) in [(Form::ApiKey, KEY), (Form::Session, TOKEN), (Form::CloudDefault, ADC)] {
        let file = root.join("login");
        cli::private_file(&file, "").unwrap();
        let login = granted(form, &file);
        let turn = turn_container(
            "control-run-1",
            "node-1",
            &login,
            &private,
            &root.join(MAILBOX),
            "sha256:img",
            "gemini",
        )
        .unwrap();
        assert_eq!(turn.user, Some(login.owner()));
        assert_eq!(cli::secrets_private(&turn), Ok(()), "{form:?}");
        let targets: Vec<_> = turn.secrets.iter().map(|s| (s.target.as_str(), s.access)).collect();
        assert_eq!(targets, [(target, Access::ReadOnly)], "{form:?}");
    }
}
