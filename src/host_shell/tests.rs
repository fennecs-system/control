use super::*;
use crate::backend::Backend;
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
    assert_eq!(fs::read(workspace.join("effect.txt")).unwrap(), b"first execution");
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
            "host_shell::tests::live_shutdown_stops_offline_workspace_command",
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
    assert_eq!(unsafe { libc::kill(probe.child.id() as libc::pid_t, libc::SIGTERM) }, 0);
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
    control_agent::cli::probe(
        &crate::agent_host::NODE,
        crate::engine::isolation(),
        &image,
        "fixture-node",
        &root,
        &format!("control-claude-probe-{}", std::process::id()),
        &control_claude::PROBE,
    )
    .unwrap();
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
fn shell_launch_vectors_are_pinned() {
    use control_isolation::golden::{fill, pin_equivalent, run};
    let root = Path::new("/golden/root");
    let values: [(&str, &str); 0] = [];
    let shell = shell_container(
        "sha256:img",
        "control-run-1-shell",
        "node-1",
        &root.join("workspace"),
        "cargo test",
        false,
        true,
        User { uid: 501, gid: 20 },
    )
    .unwrap();
    pin_equivalent(&run(&shell), &fill(SHELL, &values));
    let writable = shell_container(
        "sha256:img",
        "control-run-1-shell",
        "node-1",
        &root.join("workspace"),
        "cargo test",
        true,
        false,
        User { uid: 501, gid: 20 },
    )
    .unwrap();
    pin_equivalent(&run(&writable), &fill(SHELL_FULL_ROOT, &values));
}
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
    "/home/agent:rw,nosuid,mode=700,uid=501,gid=20,size=64m",
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
    "/home/agent:rw,nosuid,mode=700,uid=501,gid=20,size=64m",
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
