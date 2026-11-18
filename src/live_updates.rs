//! Disposable transport hints. Readers re-read authorised views; these neither
//! persist state nor wake agents, and never carry conversation contents.
use control_effect::sync::{CancelToken, Changes, Newer};
use std::{
    io::{self, Write},
    net::TcpStream,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn changes() -> &'static Arc<Changes> {
    static CHANGES: OnceLock<Arc<Changes>> = OnceLock::new();
    CHANGES.get_or_init(Changes::new)
}
pub(crate) fn notify() {
    // With no connected presentation clients there is nothing to invalidate.
    // New subscribers always receive an initial fresh-read hint.
    if SUBSCRIBERS.load(Ordering::Acquire) != 0 {
        changes().bump();
    }
}

/// Progress that lives only in memory (a folder copy's entry count) has no
/// checkpoint to announce it. Readers are hinted at most this often, so a long
/// copy shows progress without one re-read per entry.
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// Hints readers about in-memory progress when `last` is unset or at least
/// [`PROGRESS_INTERVAL`] old; returns whether it did and records the time.
pub(crate) fn progress(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.saturating_duration_since(last) < PROGRESS_INTERVAL) {
        return false;
    }
    *last = Some(now);
    notify();
    true
}

static SUBSCRIBERS: AtomicUsize = AtomicUsize::new(0);
const MAX_SUBSCRIBERS: usize = 16;
struct Subscriber;
impl Subscriber {
    fn acquire() -> Option<Self> {
        SUBSCRIBERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_SUBSCRIBERS).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Subscriber {
    fn drop(&mut self) {
        SUBSCRIBERS.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(crate) fn serve(stream: &mut TcpStream) -> io::Result<()> {
    let Some(_subscriber) = Subscriber::acquire() else {
        return control_http::respond(
            stream,
            "503 Service Unavailable",
            "text/plain",
            b"Live connection limit reached",
            "Retry-After: 5\r\n",
            control_http::Policy::Page,
        );
    };
    let token = crate::shutdown::token();
    stream_updates(stream, changes(), &token)
}
fn stream_updates(
    stream: &mut TcpStream,
    changes: &Arc<Changes>,
    token: &CancelToken,
) -> io::Result<()> {
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n")?;
    let mut seen = changes.generation();
    // Reconnects always refresh. Reading the generation before writing means a
    // concurrent update cannot be lost between the initial hint and the wait.
    stream.write_all(b"data: changed\n\n")?;
    loop {
        match changes.wait_newer(seen, Some(Instant::now() + Duration::from_secs(15)), token) {
            Newer::Changed(generation) => {
                seen = generation;
                stream.write_all(b"data: changed\n\n")?;
            }
            Newer::Elapsed => stream.write_all(b": keepalive\n\n")?,
            Newer::Cancelled => return Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) fn fixture_stream(stream: &mut TcpStream, token: &CancelToken) -> io::Result<()> {
    let _subscriber =
        Subscriber::acquire().ok_or_else(|| io::Error::other("fixture stream limit"))?;
    stream_updates(stream, changes(), token)
}

/// A connected reader for tests outside this module, and the hint generation it
/// would observe.
#[cfg(test)]
pub(crate) fn fixture_subscriber() -> Option<impl Sized> {
    Subscriber::acquire()
}
#[cfg(test)]
pub(crate) fn generation() -> u64 {
    changes().generation()
}

#[cfg(test)]
mod tests;
