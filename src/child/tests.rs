
use super::*;
use std::{process::Command, time::Duration};

fn soon() -> Option<Instant> {
    Some(Instant::now() + Duration::from_secs(10))
}
fn status(watched: &Watched) -> io::Result<ExitStatus> {
    match watched.wait(soon(), &CancelToken::new()) {
        Recv::Item(status) => status,
        other => panic!("no exit status: {other:?}"),
    }
}

#[test]
fn exit_is_delivered_without_polling_and_kill_after_exit_is_a_no_op() {
    let watched = watch(Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap()).unwrap();
    assert_eq!(status(&watched).unwrap().code(), Some(7));
    watched.kill();
    assert!(*watched.exited.lock().unwrap());
}

#[test]
fn kill_stops_a_running_child_and_deadlines_and_cancellation_end_waits() {
    let watched = watch(Command::new("sleep").arg("30").spawn().unwrap()).unwrap();
    assert!(matches!(
        watched.wait(
            Some(Instant::now() + Duration::from_millis(20)),
            &CancelToken::new()
        ),
        Recv::Elapsed
    ));
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(watched.wait(soon(), &cancelled), Recv::Cancelled));
    watched.kill();
    assert_eq!(status(&watched).unwrap().code(), None);
}

#[test]
fn a_failed_waiter_spawn_kills_and_reaps_the_child() {
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id() as libc::pid_t;
    let error = watch_with(child, observe_exit, |_| Err(io::Error::other("no threads")))
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "no threads");
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "child must be gone and reaped"
    );
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn lost_ownership_reports_an_error_and_disables_kill() {
    fn lost(_: libc::pid_t) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ECHILD))
    }
    let watched = watch_with(
        Command::new("sleep").arg("0").spawn().unwrap(),
        lost,
        |task| {
            std::thread::spawn(task);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        status(&watched).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(*watched.exited.lock().unwrap());
    watched.kill();
    assert_eq!(
        observe_exit(1).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn dropping_a_watch_while_unwinding_kills_and_reaps_the_child() {
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id() as libc::pid_t;
    let unwound = std::panic::catch_unwind(move || {
        let _watched = watch(child).unwrap();
        panic!("supervisor failed");
    });
    assert!(unwound.is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "child still running or unreaped after drop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn other_observation_errors_still_stop_reap_and_release_output() {
    fn broken(_: libc::pid_t) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EINVAL))
    }
    let mut child = Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id() as libc::pid_t;
    let mut stdout = child.stdout.take().unwrap();
    let reader =
        std::thread::spawn(move || std::io::Read::read_to_end(&mut stdout, &mut Vec::new()));
    let watched = watch_with(child, broken, |task| {
        std::thread::spawn(task);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        status(&watched).unwrap_err().raw_os_error(),
        Some(libc::EINVAL)
    );
    reader.join().unwrap().unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "child must be stopped and reaped"
    );
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}
