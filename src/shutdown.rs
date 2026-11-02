//! Signal handlers only set a flag and write one byte to a pipe; an ordinary
//! watcher thread cancels the root token, and threads perform cleanup.
use control_effect::sync::{CancelToken, Wait};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    sync::{
        Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
    time::{Duration, Instant},
};

static REQUESTED: AtomicBool = AtomicBool::new(false);
static WAKE: AtomicI32 = AtomicI32::new(-1);
static INSTALLED: Mutex<bool> = Mutex::new(false);

fn root() -> &'static CancelToken {
    static ROOT: OnceLock<CancelToken> = OnceLock::new();
    ROOT.get_or_init(CancelToken::new)
}

#[cfg(target_os = "macos")]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__error() }
}
#[cfg(not(target_os = "macos"))]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

extern "C" fn signal_handler(_: libc::c_int) {
    REQUESTED.store(true, Ordering::SeqCst);
    let fd = WAKE.load(Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            let saved = *errno();
            notify(fd);
            *errno() = saved;
        }
    }
}
/// Async-signal-safe: one non-blocking write; a full pipe already holds a wake.
unsafe fn notify(fd: libc::c_int) {
    let byte = 1u8;
    unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
}
fn watch(fd: libc::c_int, token: &CancelToken) {
    let mut byte = 0u8;
    loop {
        match unsafe { libc::read(fd, (&raw mut byte).cast(), 1) } {
            1 => break,
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
    token.cancel();
}
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [read.as_raw_fd(), write.as_raw_fd()] {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let flags = unsafe { libc::fcntl(write.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, write))
}

pub fn request() {
    REQUESTED.store(true, Ordering::SeqCst);
    root().cancel();
}

pub fn install() -> io::Result<()> {
    install_signals(&[libc::SIGINT, libc::SIGTERM])
}
/// Idempotent once it succeeds. On failure the previous dispositions are
/// restored and a later call may retry. Once a handler can observe the pipe its
/// descriptors are never closed, so a late handler cannot write to a reused fd
/// and the retained read end rules out SIGPIPE.
fn install_signals(signals: &[libc::c_int]) -> io::Result<()> {
    let mut installed = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    if *installed {
        return Ok(());
    }
    let (read, write) = pipe()?;
    let read = read.into_raw_fd();
    std::thread::Builder::new()
        .name("control-shutdown".into())
        .spawn(move || watch(read, root()))
        .inspect_err(|_| unsafe {
            libc::close(read);
        })?;
    let write = write.into_raw_fd();
    WAKE.store(write, Ordering::SeqCst);
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = signal_handler as *const () as usize;
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    let mut previous = Vec::with_capacity(signals.len());
    for &signal in signals {
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
            let error = io::Error::last_os_error();
            for (signal, old) in previous.iter().rev() {
                unsafe { libc::sigaction(*signal, old, std::ptr::null_mut()) };
            }
            WAKE.store(-1, Ordering::SeqCst);
            return Err(error);
        }
        previous.push((signal, old));
    }
    *installed = true;
    Ok(())
}

pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst) || root().is_cancelled()
}

/// A token cancelled when shutdown is requested.
pub fn token() -> CancelToken {
    root().child()
}

/// Sleep for up to `duration`, returning early with `true` once shutdown is requested.
pub fn pause(duration: Duration) -> bool {
    pause_on(root(), duration) || requested()
}
fn pause_on(token: &CancelToken, duration: Duration) -> bool {
    token.wait(Some(Instant::now() + duration)) == Wait::Cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn soon() -> Option<Instant> {
        Some(Instant::now() + Duration::from_secs(5))
    }
    /// Global signal state is exercised in a fresh copy of the test binary.
    fn in_child(test: &str, role: &str) -> bool {
        if std::env::var("CONTROL_SHUTDOWN_CHILD").as_deref() == Ok(role) {
            return true;
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--test-threads=1", "--nocapture"])
            .env("CONTROL_SHUTDOWN_CHILD", role)
            .status()
            .unwrap();
        assert!(status.success(), "{test} child failed: {status}");
        false
    }
    fn handler(signal: libc::c_int) -> usize {
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) }, 0);
        current.sa_sigaction
    }

    #[test]
    fn a_wake_byte_cancels_the_token_from_an_ordinary_thread() {
        let (read, write) = pipe().unwrap();
        let token = CancelToken::new();
        let watcher = {
            let token = token.clone();
            let fd = read.as_raw_fd();
            std::thread::spawn(move || watch(fd, &token))
        };
        assert!(!token.is_cancelled());
        unsafe { notify(write.as_raw_fd()) };
        unsafe { notify(write.as_raw_fd()) };
        assert_eq!(token.wait(soon()), Wait::Cancelled);
        watcher.join().unwrap();
    }

    #[test]
    fn pause_returns_early_on_cancellation_and_waits_otherwise() {
        let token = CancelToken::new();
        let started = Instant::now();
        assert!(!pause_on(&token, Duration::from_millis(20)));
        assert!(started.elapsed() >= Duration::from_millis(20));
        let waiter = {
            let token = token.clone();
            std::thread::spawn(move || pause_on(&token, Duration::from_secs(5)))
        };
        token.cancel();
        assert!(waiter.join().unwrap());
        assert!(pause_on(&token, Duration::from_secs(5)));
    }

    #[test]
    fn the_wake_pipe_never_blocks_the_writer() {
        let (read, write) = pipe().unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..200_000 {
                unsafe { notify(write.as_raw_fd()) };
            }
            done.send(()).unwrap();
        });
        finished.recv_timeout(Duration::from_secs(10)).expect("wake write blocked");
        drop(read);
    }

    #[test]
    fn installed_handler_cancels_the_root_token_and_preserves_errno() {
        if !in_child("shutdown::tests::installed_handler_cancels_the_root_token_and_preserves_errno", "handler") {
            return;
        }
        install().unwrap();
        install().unwrap();
        assert!(!requested());
        unsafe {
            *errno() = libc::EBADF;
            libc::raise(libc::SIGTERM);
            assert_eq!(*errno(), libc::EBADF);
        }
        assert!(REQUESTED.load(Ordering::SeqCst));
        assert_eq!(root().wait(soon()), Wait::Cancelled);
        assert!(pause(Duration::from_secs(5)));
    }

    #[test]
    fn request_wakes_pause_without_installed_handlers() {
        if !in_child("shutdown::tests::request_wakes_pause_without_installed_handlers", "request") {
            return;
        }
        let waiter = std::thread::spawn(|| pause(Duration::from_secs(5)));
        request();
        assert!(waiter.join().unwrap());
        assert!(requested());
    }

    #[test]
    fn failed_installation_restores_previous_handlers_and_can_retry() {
        if !in_child("shutdown::tests::failed_installation_restores_previous_handlers_and_can_retry", "rollback") {
            return;
        }
        let before = handler(libc::SIGUSR2);
        assert!(install_signals(&[libc::SIGUSR2, libc::SIGKILL]).is_err());
        assert_eq!(handler(libc::SIGUSR2), before);
        assert_eq!(WAKE.load(Ordering::SeqCst), -1);
        install_signals(&[libc::SIGUSR2]).unwrap();
        assert_ne!(handler(libc::SIGUSR2), before);
        unsafe { libc::raise(libc::SIGUSR2) };
        assert_eq!(root().wait(soon()), Wait::Cancelled);
    }
}
