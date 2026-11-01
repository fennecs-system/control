//! Trusted PID 1 for offline persistent tools. Exiting the PID namespace kills
//! descendants, including detached children, without a control-plane heartbeat.
use std::{
    process::{Command, ExitCode},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn main() -> ExitCode {
    #[cfg(not(target_os = "linux"))]
    {
        return ExitCode::from(125);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::ExitStatusExt;
        let args: Vec<_> = std::env::args().skip(1).collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let Some(remaining) = args
            .first()
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|deadline| deadline.checked_sub(now))
            .filter(|s| (1..=3600).contains(s) || args.first().is_some_and(|s|s == &u64::MAX.to_string()))
        else {
            return ExitCode::from(124);
        };
        let Some(maximum) = args
            .get(1)
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|s| (1..=3600).contains(s) || (*s == u64::MAX && args.first().is_some_and(|s|s == &u64::MAX.to_string())))
        else {
            return ExitCode::from(125);
        };
        let seconds = remaining.min(maximum);
        if args.len() != 3 || args[2].is_empty() || args[2].len() > 16000 || std::process::id() != 1
        {
            return ExitCode::from(125);
        }
        // A child must not modify its supervisor through /proc or ptrace. PID 1
        // cannot receive unhandled stop/kill signals from its own namespace.
        if unsafe { prctl(4, 0usize, 0usize, 0usize, 0usize) } != 0 {
            return ExitCode::from(125);
        }
        if unsafe { signal(15, stop) } == usize::MAX || unsafe { signal(2, stop) } == usize::MAX {
            return ExitCode::from(125);
        }
        let start = Instant::now();
        let Ok(mut child) = Command::new("bash")
            .args(["-c", &format!("exec 2>&1\n{}", args[2])])
            .spawn()
        else {
            return ExitCode::from(125);
        };
        loop {
            if STOP.load(Ordering::Relaxed) {
                return ExitCode::from(143);
            }
            if start.elapsed() >= Duration::from_secs(seconds) {
                return ExitCode::from(124);
            }
            match child.try_wait() {
                Ok(Some(status)) => return ExitCode::from(status.code().unwrap_or(1) as u8),
                Err(_) => return ExitCode::from(125),
                _ => {}
            }
            loop {
                let mut raw = 0;
                let reaped = unsafe { waitpid(-1, &mut raw, 1) };
                if reaped <= 0 {
                    break;
                }
                if reaped as u32 == child.id() {
                    return ExitCode::from(
                        std::process::ExitStatus::from_raw(raw).code().unwrap_or(1) as u8,
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
