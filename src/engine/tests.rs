
use super::*;

#[test]
fn storage_quota_follows_the_driver_and_backing_filesystem() {
    assert!(quota_capable("overlay2 [[\"Backing Filesystem\",\"xfs\"],[\"Supports d_type\",\"true\"]]"));
    assert!(!quota_capable("overlay2 [[\"Backing Filesystem\",\"extfs\"]]"));
    assert!(!quota_capable("overlayfs [[\"driver-type\",\"io.containerd.snapshotter.v1\"]]"));
    assert!(quota_capable("btrfs null\n"));
    assert!(!quota_capable(""));
}

#[test]
fn stats_and_processes_parse_engine_output_and_stay_bounded() {
    let stats = parse_stats("abc\t1.25%\t200MiB / 2GiB\t12\nbad line\n");
    assert_eq!(stats.len(), 1);
    let sample = &stats["abc"];
    assert_eq!(
        (
            sample.cpu.as_str(),
            sample.memory.as_str(),
            sample.pids.as_str()
        ),
        ("1.25%", "200MiB / 2GiB", "12")
    );
    let processes = parse_top(
        "PID    USER   COMMAND\n1      502    control-service-runner 0 0 bash -lc 'sleep infinity'\n77     502    java -Xmx1G -jar server.jar nogui\n90 502 sleep\n",
        2,
    );
    assert_eq!(processes.len(), 2);
    assert_eq!(
        (
            processes[1].pid.as_str(),
            processes[1].user.as_str(),
            processes[1].command.as_str()
        ),
        ("77", "502", "java -Xmx1G -jar server.jar nogui")
    );
    assert_eq!(parse_top("PID USER COMMAND\n5 502\n", 5)[0].command, "");
}

#[test]
fn only_input_exec_forwards_stdin() {
    let arguments = |command: Command| command.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
    let plain = arguments(exec("c1", "7:8", Some("/workspace"), &[("A", "b")], &["tar", "-x"]));
    let input = arguments(exec_input("c1", "7:8", Some("/workspace"), &[("A", "b")], &["tar", "-x"]));
    assert!(!plain.contains(&"--interactive".to_owned()));
    let expected = ["--user", "7:8", "--env", "A=b", "--workdir", "/workspace", "c1", "tar", "-x"];
    assert!(plain.ends_with(&expected.map(str::to_owned)));
    assert!(input.ends_with(&expected.map(str::to_owned)));
    assert_eq!(input.iter().position(|a| a == "--interactive"), input.iter().position(|a| a == "exec").map(|i| i + 1));
}
