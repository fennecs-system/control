//! Signal handlers only set a flag; ordinary threads perform cleanup and persistence.
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_: libc::c_int) {
    request();
}

pub fn request() {
    REQUESTED.store(true, Ordering::Relaxed);
}

pub fn install() -> io::Result<()> {
    // No allocation, locks, logging or I/O run in the signal handler.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = signal_handler as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

pub fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// Interrupt long idle/backoff sleeps without doing synchronization in a handler.
pub fn pause(duration: Duration) -> bool {
    let start = Instant::now();
    while !requested() {
        let Some(left) = duration
            .checked_sub(start.elapsed())
            .filter(|d| !d.is_zero())
        else {
            return false;
        };
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
    true
}
