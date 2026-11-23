use super::*;

/// Every field rendered, in the backend's one order and spelling.
#[test]
fn a_full_spec_renders_one_exact_vector() {
    let owner = User { uid: 501, gid: 20 };
    let spec = RunSpec {
        name: "control-run-1".into(),
        image: "sha256:img".into(),
        entrypoint: Some("/bin/bash".into()),
        user: Some(owner),
        workdir: Some("/workspace".into()),
        limits: Limits { pids: Some(256), memory: Some(Size::Gib(2)), memory_swap: Some(Size::Mib(2048)), cpus: Some(2) },
        seccomp: Some("/policy.json".into()),
        proc: Proc::Unmasked,
        network: Network::None,
        logs: Logs::Local { max_size: Size::Mib(1), max_files: 2 },
        input: Input::Terminal,
        ..RunSpec::default()
    }
    .label("dev.control.node", "n")
    .env("HOME", "/home/agent")
    .tmpfs(Tmpfs::new("/home/agent", Size::Mib(64)).owned(owner))
    .tmpfs(Tmpfs { exec: Exec::Deny, nodev: true, mode: Some(0o1777), ..Tmpfs::new("/tmp", Size::Gib(1)) })
    .bind("/w", "/workspace", Access::ReadWrite)
    .secret("codex", "/a.json", "/home/agent/a.json", Access::ReadOnly)
    .args(["-c", "true"]);
    let spec = RunSpec { mounts: [spec.mounts.clone(), vec![Mount { source: Source::Volume("vol".into()), target: "/data".into(), access: Access::ReadOnly }]].concat(), ..spec };
    let expected = [
        "run", "--interactive", "--tty", "--rm", "--pull=never", "--name", "control-run-1", "--label", "dev.control.node=n",
        "--network=none", "--user", "501:20", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
        "--security-opt", "seccomp=/policy.json", "--security-opt=systempaths=unconfined", "--pids-limit=256", "--memory=2g",
        "--memory-swap=2048m", "--cpus=2", "--log-driver=local", "--log-opt=max-size=1m", "--log-opt=max-file=2",
        "--tmpfs", "/home/agent:rw,nosuid,uid=501,gid=20,size=64m", "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,mode=1777,size=1g",
        "--mount", "type=bind,src=/w,dst=/workspace", "--mount", "type=volume,source=vol,target=/data,readonly",
        "--mount", "type=bind,src=/a.json,dst=/home/agent/a.json,readonly", "--workdir", "/workspace", "--env", "HOME=/home/agent",
        "--entrypoint=/bin/bash", "sha256:img", "-c", "true",
    ];
    assert_eq!(Docker::run_arguments(&spec).unwrap(), expected);
}

#[test]
fn the_smallest_spec_is_still_confined_and_removed() {
    let spec = RunSpec { name: "n".into(), image: "i".into(), root: Access::ReadWrite, ..RunSpec::default() };
    assert_eq!(Docker::run_arguments(&spec).unwrap(), ["run", "--rm", "--pull=never", "--name", "n", "--cap-drop=ALL", "--security-opt=no-new-privileges", "i"]);
    assert!(Docker::run_arguments(&RunSpec::default()).unwrap().contains(&"--read-only".to_owned()));
}

#[test]
fn unsafe_host_paths_are_refused() {
    for source in ["relative", "/with,comma", "/with\nnewline"] {
        assert!(Docker::run_arguments(&RunSpec::default().bind(source, "/workspace", Access::ReadOnly)).is_err(), "{source:?}");
        assert!(Docker::run_arguments(&RunSpec::default().secret("s", source, "/s", Access::ReadOnly)).is_err(), "{source:?}");
    }
    let seccomp = RunSpec { seccomp: Some("relative.json".into()), ..RunSpec::default() };
    assert!(Docker::run_arguments(&seccomp).is_err());
}

#[test]
fn a_writable_secret_is_mounted_writable() {
    let spec = RunSpec::default().secret("token", "/t", "/home/agent/t", Access::ReadWrite);
    assert!(Docker::run_arguments(&spec).unwrap().contains(&"type=bind,src=/t,dst=/home/agent/t".to_owned()));
}
