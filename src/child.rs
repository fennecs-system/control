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
mod tests;
