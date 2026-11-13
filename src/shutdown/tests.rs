
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
    assert_eq!(
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) },
        0
    );
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
    finished
        .recv_timeout(Duration::from_secs(10))
        .expect("wake write blocked");
    drop(read);
}

#[test]
fn installed_handler_cancels_the_root_token_and_preserves_errno() {
    if !in_child(
        "shutdown::tests::installed_handler_cancels_the_root_token_and_preserves_errno",
        "handler",
    ) {
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
    if !in_child(
        "shutdown::tests::request_wakes_pause_without_installed_handlers",
        "request",
    ) {
        return;
    }
    let waiter = std::thread::spawn(|| pause(Duration::from_secs(5)));
    request();
    assert!(waiter.join().unwrap());
    assert!(requested());
}

#[test]
fn failed_installation_restores_previous_handlers_and_can_retry() {
    if !in_child(
        "shutdown::tests::failed_installation_restores_previous_handlers_and_can_retry",
        "rollback",
    ) {
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
