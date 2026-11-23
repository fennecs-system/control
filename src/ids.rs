//! The clock behind time-ordered identifiers.
use std::sync::atomic::{AtomicU64, Ordering};

/// Wall-clock nanoseconds, strictly increasing within this process. Ids built
/// from it (chats, runs, grants) stay time-ordered and never repeat in one
/// process, even when two are made in the same clock tick (macOS reports
/// microseconds) for different `State`s, as tests do. Process-wide registries
/// and directories keyed by these ids (folder imports, run workspaces, chat
/// histories) rely on that.
pub fn stamp() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut last = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(last + 1);
        match LAST.compare_exchange_weak(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(current) => last = current,
        }
    }
}

#[cfg(test)]
mod tests;
