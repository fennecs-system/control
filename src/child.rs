//! Wait for a child process without polling, and kill it without risking a
//! reused PID: the waiter observes exit with `WNOWAIT`, marks the child exited
//! while it is still an unreaped zombie, and only then reaps it.
use control_effect::sync::{CancelToken, Receiver, Recv, channel};
use std::{
    io,
    process::{Child, ExitStatus},
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

pub struct Watched {
    pid: libc::pid_t,
    exited: Arc<Mutex<bool>>,
    exit: Receiver<io::Result<ExitStatus>>,
}

type Task = Box<dyn FnOnce() + Send>;

pub fn watch(child: Child) -> io::Result<Watched> {
    watch_with(child, observe_exit, |task| {
        std::thread::Builder::new().name("control-child".into()).spawn(task).map(drop)
    })
}

/// The waiter thread starts before it owns the child, so a failed spawn still
/// leaves the caller able to kill and reap it.
fn watch_with(
    mut child: Child,
    observe: fn(libc::pid_t) -> io::Result<()>,
    spawn: impl FnOnce(Task) -> io::Result<()>,
) -> io::Result<Watched> {
    let pid = libc::pid_t::try_from(child.id()).map_err(io::Error::other)?;
    let exited = Arc::new(Mutex::new(false));
    let (sender, exit) = channel(1);
    let (hand_over, handed) = std::sync::mpsc::channel::<Child>();
    let marked = exited.clone();
    let task: Task = Box::new(move || {
        let Ok(mut child) = handed.recv() else { return };
        let observed = observe(pid);
        let lost = observed.as_ref().is_err_and(|error| error.raw_os_error() == Some(libc::ECHILD));
        if observed.is_err() && !lost {
            // Still our unreaped child, so its PID is reserved: stop it before reaping.
            let _ = child.kill();
        }
        *marked.lock().unwrap_or_else(PoisonError::into_inner) = true;
        let status = match observed {
            Ok(()) => child.wait(),
            Err(error) if lost => {
                // Someone else reaped it; the PID may be reused, so never signal or wait.
                std::mem::forget(child);
                Err(error)
            }
            Err(error) => child.wait().and(Err(error)),
        };
        let _ = sender.try_send(status);
    });
    if let Err(error) = spawn(task) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    hand_over.send(child).map_err(|_| io::Error::other("Child waiter stopped before taking ownership"))?;
    Ok(Watched { pid, exited, exit })
}

/// Block until the child has exited, leaving it unreaped. Any error other than
/// an interruption means exit cannot be observed safely.
fn observe_exit(pid: libc::pid_t) -> io::Result<()> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

impl Watched {
    /// The exit status once the child has exited and been reaped.
    pub fn wait(&self, deadline: Option<Instant>, token: &CancelToken) -> Recv<io::Result<ExitStatus>> {
        self.exit.recv(deadline, token)
    }
    /// SIGKILL the child unless exit was observed or ownership was lost. The
    /// PID stays reserved until the waiter reaps after marking exit.
    pub fn kill(&self) {
        let exited = self.exited.lock().unwrap_or_else(PoisonError::into_inner);
        if !*exited {
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
        }
    }
}
/// A dropped watch, including during unwinding, never leaves its child running;
/// the waiter thread still reaps it.
impl Drop for Watched {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
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
        assert!(matches!(watched.wait(Some(Instant::now() + Duration::from_millis(20)), &CancelToken::new()), Recv::Elapsed));
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
        let error = watch_with(child, observe_exit, |_| Err(io::Error::other("no threads"))).err().unwrap();
        assert_eq!(error.to_string(), "no threads");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child must be gone and reaped");
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn lost_ownership_reports_an_error_and_disables_kill() {
        fn lost(_: libc::pid_t) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::ECHILD))
        }
        let watched = watch_with(Command::new("sleep").arg("0").spawn().unwrap(), lost, |task| {
            std::thread::spawn(task);
            Ok(())
        })
        .unwrap();
        assert_eq!(status(&watched).unwrap_err().raw_os_error(), Some(libc::ECHILD));
        assert!(*watched.exited.lock().unwrap());
        watched.kill();
        assert_eq!(observe_exit(1).unwrap_err().raw_os_error(), Some(libc::ECHILD));
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
            assert!(Instant::now() < deadline, "child still running or unreaped after drop");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn other_observation_errors_still_stop_reap_and_release_output() {
        fn broken(_: libc::pid_t) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::EINVAL))
        }
        let mut child = Command::new("sh").args(["-c", "exec sleep 30"]).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || std::io::Read::read_to_end(&mut stdout, &mut Vec::new()));
        let watched = watch_with(child, broken, |task| {
            std::thread::spawn(task);
            Ok(())
        })
        .unwrap();
        assert_eq!(status(&watched).unwrap_err().raw_os_error(), Some(libc::EINVAL));
        reader.join().unwrap().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child must be stopped and reaped");
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }
}
