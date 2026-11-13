
use super::*;

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
