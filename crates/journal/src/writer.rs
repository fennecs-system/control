//! One thread owns a store's files; callers stage records and wait for them.
//!
//! A caller encodes and diffs under its own lock, hands the resulting
//! [`Record`] to the [`Writer`] and gets a ticket. The writer drains every
//! record queued since its last write and makes them durable together: lines
//! with one write and one sync, so N waiting callers share one sync instead of
//! paying N in turn. There is no batching window: a record that arrives while
//! the disk is idle is written at once.
//!
//! Records are written in [`Seq`] order and a ticket resolves only once its
//! record (and every earlier one) is durable. A failed write is retried or
//! given up as the [`Policy`] decides; a record can be refused (nothing
//! written, the caller rolls back) only while it is the one record in flight
//! and its caller still holds the lock it staged under, since otherwise a later
//! record may already extend it.
use std::{
    fmt, io,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// A record's place in the order the writer makes records durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seq(u64);

/// Proof that every record up to `seq` is durable; only the writer makes one.
#[derive(Debug)]
pub struct Durable {
    seq: Seq,
}
impl Durable {
    pub fn seq(&self) -> Seq {
        self.seq
    }
}

/// What a store writes for one save.
pub enum Record {
    /// One committed log line (without its newline).
    Line(Vec<u8>),
    /// A whole snapshot, then a fresh log starting with `header`. Lines queued
    /// before it are contained in it and are not written.
    Compact { snapshot: Vec<u8>, header: Vec<u8> },
}

/// Why a write failed, by whether it may have reached the disk.
#[derive(Debug)]
pub enum Fault {
    /// Failed before publishing (serialization, configuration, a temporary
    /// file, or an append rolled back cleanly): the files are as before.
    NotReplaced(io::Error),
    /// A rename was attempted but not durably acknowledged (even a rename
    /// error counts: a directory-sync error can leave the replacement
    /// visible), or a failed append could not be rolled back.
    Unconfirmed(io::Error),
}
impl Fault {
    fn error(&self) -> &io::Error {
        match self {
            Self::NotReplaced(error) | Self::Unconfirmed(error) => error,
        }
    }
}
impl fmt::Display for Fault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReplaced(error) => write!(formatter, "checkpoint not replaced: {error}"),
            Self::Unconfirmed(error) => write!(formatter, "checkpoint unconfirmed: {error}"),
        }
    }
}
impl std::error::Error for Fault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error())
    }
}

/// Whether a compaction's log can take the lines after it.
#[derive(Debug)]
pub enum Log {
    Restarted,
    /// The snapshot is durable but its log could not be started: the next
    /// save must compact again.
    Broken(io::Error),
}

/// The files a writer owns.
pub trait Sink: Send + 'static {
    /// Appends `lines` durably, all or none.
    fn append(&mut self, lines: &[&[u8]]) -> Result<(), Fault>;
    /// Publishes `snapshot` durably, then starts its log with `header`.
    fn compact(&mut self, snapshot: &[u8], header: &[u8]) -> Result<Log, Fault>;
}

/// Whether the caller of a failed record can still undo it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rollback {
    /// The only record in flight, and its caller holds the lock it staged under.
    Possible,
    /// Later records may extend it, or its caller already let others see it.
    Impossible,
}

/// A failed write, as the policy sees it.
#[derive(Debug)]
pub struct Attempt<'a> {
    pub fault: &'a Fault,
    pub rollback: Rollback,
    /// Since the first failure of this batch.
    pub failing_for: Duration,
    /// Failures of this batch so far, this one included.
    pub failures: u32,
}

/// What the writer does after a failed write.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Give the error to the record's caller, which rolls back. Only honoured
    /// with [`Rollback::Possible`]; otherwise it halts.
    Refuse,
    Retry(Duration),
    /// Stop writing for good: every waiter and later record gets [`Halted`].
    Halt,
}

pub type Policy = fn(&Attempt) -> Verdict;

/// How long a failing disk may stall writers before the writer gives up.
pub const STALL: Duration = Duration::from_secs(30);

/// Today's contract, plus a bounded stall: an unconfirmed write halts at once;
/// a record its caller can undo is refused as before; anything else (a record
/// others may extend) is retried with backoff for up to [`STALL`], then halts.
pub fn stall_then_halt(attempt: &Attempt) -> Verdict {
    match (attempt.fault, attempt.rollback) {
        (Fault::Unconfirmed(_), _) => Verdict::Halt,
        (Fault::NotReplaced(_), Rollback::Possible) => Verdict::Refuse,
        (Fault::NotReplaced(_), Rollback::Impossible) if attempt.failing_for >= STALL => Verdict::Halt,
        (Fault::NotReplaced(_), Rollback::Impossible) => Verdict::Retry(
            Duration::from_millis(10)
                .saturating_mul(1 << attempt.failures.min(7))
                .min(Duration::from_secs(1)),
        ),
    }
}

/// The writer stopped after a write it could not complete or confirm.
#[derive(Clone, Debug)]
pub struct Halted(Arc<str>);
impl fmt::Display for Halted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the state writer stopped: {}", self.0)
    }
}
impl std::error::Error for Halted {}
impl From<Halted> for io::Error {
    fn from(halted: Halted) -> Self {
        io::Error::other(halted)
    }
}

/// Why a record was not queued.
#[derive(Debug)]
pub enum Submit {
    /// A line or an unchanged save would extend a base the disk does not
    /// have (an earlier record was refused, or a log could not be started):
    /// submit a compaction instead.
    Stale,
    Halted(Halted),
}

/// Why a [`Held`] save did not become durable.
#[derive(Debug)]
pub enum Failure {
    /// Nothing was written; the caller rolls back.
    Refused(io::Error),
    Halted(Halted),
}

struct Entry {
    seq: u64,
    record: Record,
    rollback: Rollback,
}

#[derive(Default)]
struct Queue {
    /// The last sequence number handed out.
    last: u64,
    pending: Vec<Entry>,
    /// Every record up to here is durable or was refused.
    settled: u64,
    /// Records refused so far (each a failed save, so few), with their error.
    refused: Vec<(u64, io::ErrorKind, String)>,
    halted: Option<Halted>,
    stale: bool,
    closing: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    work: Condvar,
    settled: Condvar,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Waits until `seq` is settled, then says how.
    fn wait(&self, seq: u64) -> Result<Durable, Failure> {
        let mut queue = self.lock();
        loop {
            if let Some((_, kind, message)) = queue.refused.iter().find(|(refused, ..)| *refused == seq) {
                return Err(Failure::Refused(io::Error::new(*kind, message.clone())));
            }
            if queue.settled >= seq {
                return Ok(Durable { seq: Seq(seq) });
            }
            if let Some(halted) = &queue.halted {
                return Err(Failure::Halted(halted.clone()));
            }
            queue = self.settled.wait(queue).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// A record whose caller has released (or will release) its lock before
/// waiting: others may see and extend the change, so it is never refused.
#[must_use = "a save is acknowledged only after its ticket resolves"]
pub struct Ticket {
    seq: u64,
    shared: Arc<Shared>,
}
impl Ticket {
    pub fn wait(self) -> Result<Durable, Halted> {
        self.shared.wait(self.seq).map_err(|failure| match failure {
            Failure::Halted(halted) => halted,
            // Only an unchanged save waiting on another caller's record can
            // see a refusal, and refusals need that caller to hold the lock.
            Failure::Refused(error) => Halted(error.to_string().into()),
        })
    }
}
/// A record whose caller waits under the lock it staged under, and so can
/// roll back if the record is refused.
#[must_use = "a save is acknowledged only after it resolves"]
pub struct Held {
    seq: u64,
    shared: Arc<Shared>,
}
impl Held {
    pub fn wait(self) -> Result<Durable, Failure> {
        self.shared.wait(self.seq)
    }
}
/// The thread that owns a sink. Dropping it writes what is queued, then joins.
pub struct Writer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl Writer {
    pub fn spawn(sink: impl Sink, policy: Policy) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            work: Condvar::new(),
            settled: Condvar::new(),
        });
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("state-writer".into())
                .spawn(move || run(&shared, sink, policy))?
        };
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    /// Queues `record` after every record before it, for a caller that lets
    /// others see its change before waiting; `None` (nothing changed) waits
    /// for those earlier records only.
    pub fn submit(&self, record: Option<Record>) -> Result<Ticket, Submit> {
        let seq = self.enqueue(record, Rollback::Impossible)?;
        Ok(Ticket {
            seq,
            shared: self.shared.clone(),
        })
    }
    /// [`submit`](Self::submit) for a caller that waits under the lock it
    /// staged under and rolls back if the record is refused.
    pub fn submit_held(&self, record: Option<Record>) -> Result<Held, Submit> {
        let seq = self.enqueue(record, Rollback::Possible)?;
        Ok(Held {
            seq,
            shared: self.shared.clone(),
        })
    }
    fn enqueue(&self, record: Option<Record>, rollback: Rollback) -> Result<u64, Submit> {
        let mut queue = self.shared.lock();
        if let Some(halted) = &queue.halted {
            return Err(Submit::Halted(halted.clone()));
        }
        let Some(record) = record else {
            return if queue.stale {
                Err(Submit::Stale)
            } else {
                Ok(queue.last)
            };
        };
        queue.stale = match (&record, queue.stale) {
            (Record::Line(_), true) => return Err(Submit::Stale),
            (Record::Line(_), false) => false,
            (Record::Compact { .. }, _) => false,
        };
        queue.last += 1;
        let seq = queue.last;
        queue.pending.push(Entry { seq, record, rollback });
        self.shared.work.notify_one();
        Ok(seq)
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.shared.lock().closing = true;
        self.shared.work.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, mut sink: impl Sink, policy: Policy) {
    loop {
        let batch = {
            let mut queue = shared.lock();
            while queue.pending.is_empty() && !queue.closing {
                queue = shared.work.wait(queue).unwrap_or_else(PoisonError::into_inner);
            }
            if queue.pending.is_empty() {
                return;
            }
            std::mem::take(&mut queue.pending)
        };
        if write_batch(shared, &mut sink, policy, &batch).is_err() {
            return;
        }
    }
}

/// Writes `batch`, retrying as `policy` says, and settles its records.
/// `Err` once the writer halted.
fn write_batch(shared: &Shared, sink: &mut impl Sink, policy: Policy, batch: &[Entry]) -> Result<(), Halted> {
    let mut rest = batch;
    let mut failures = 0;
    let mut first_failure = None;
    while let Some(last) = rest.last() {
        let fault = match write(sink, rest) {
            Ok((written, log)) => {
                let mut queue = shared.lock();
                if let Log::Broken(error) = log {
                    eprintln!(
                        "state.log: could not start the log after a compaction ({error}); the next save rewrites the whole snapshot"
                    );
                    queue.stale = true;
                }
                queue.settled = rest[written - 1].seq;
                shared.settled.notify_all();
                rest = &rest[written..];
                continue;
            }
            Err(fault) => fault,
        };
        failures += 1;
        let failing_for = first_failure.get_or_insert_with(Instant::now).elapsed();
        eprintln!("state writer: save failed ({failures} time(s)): {fault}");
        let mut queue = shared.lock();
        // Refusable only while nothing newer was queued: a newer record may
        // extend this one. Checked and acted on under one lock with `submit`.
        let rollback = match rest {
            [only] if only.rollback == Rollback::Possible && queue.pending.is_empty() && queue.last == only.seq => {
                Rollback::Possible
            }
            _ => Rollback::Impossible,
        };
        match policy(&Attempt {
            fault: &fault,
            rollback,
            failing_for,
            failures,
        }) {
            Verdict::Retry(pause) => {
                drop(queue);
                std::thread::sleep(pause);
            }
            Verdict::Refuse if rollback == Rollback::Possible => {
                let error = fault.error();
                queue.refused.push((last.seq, error.kind(), error.to_string()));
                queue.settled = last.seq;
                // The next save compacts: memory, base and disk agree again.
                queue.stale = true;
                shared.settled.notify_all();
                return Ok(());
            }
            Verdict::Refuse | Verdict::Halt => {
                let halted = Halted(fault.to_string().into());
                queue.halted = Some(halted.clone());
                shared.settled.notify_all();
                return Err(halted);
            }
        }
    }
    Ok(())
}

/// Writes a prefix of `rest`: everything up to and including its last
/// compaction (earlier lines are inside the snapshot), else all of its lines
/// with one append. Returns how many records it settled.
fn write(sink: &mut impl Sink, rest: &[Entry]) -> Result<(usize, Log), Fault> {
    let compaction = rest
        .iter()
        .enumerate()
        .rev()
        .find_map(|(at, entry)| match &entry.record {
            Record::Compact { snapshot, header } => Some((at, snapshot, header)),
            Record::Line(_) => None,
        });
    match compaction {
        Some((at, snapshot, header)) => sink.compact(snapshot, header).map(|log| (at + 1, log)),
        None => {
            let lines: Vec<&[u8]> = rest
                .iter()
                .filter_map(|entry| match &entry.record {
                    Record::Line(line) => Some(line.as_slice()),
                    Record::Compact { .. } => None,
                })
                .collect();
            sink.append(&lines).map(|()| (rest.len(), Log::Restarted))
        }
    }
}

#[cfg(test)]
#[path = "writer/tests.rs"]
mod tests;
