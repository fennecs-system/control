//! Group commit for a store's files, led by the callers themselves.
//!
//! A caller encodes and diffs under its own lock, hands the resulting
//! [`Record`] to the [`Writer`] and gets a ticket. Waiting on the ticket, a
//! caller that finds nobody writing becomes the leader: it takes every record
//! queued so far and writes them on its own thread, lines with one write and
//! one sync. Callers that arrive meanwhile queue up and are written by the next
//! leader, so N concurrent callers share syncs instead of paying N in turn,
//! and a lone save costs its write and sync and no thread hand-off (as in
//! LevelDB's write groups). There is no batching window.
//!
//! Records are written in [`Seq`] order and a ticket resolves once its record
//! (and every earlier one) is durable. A failed write is handled as the
//! [`Policy`] says: retried, or the writer becomes degraded, or (only when what
//! is on disk is unknown) it halts. A degraded writer refuses new records, so
//! callers refuse the change before applying it; records already queued stay
//! queued and a prober thread, alive only while degraded, writes them in order
//! once the disk takes writes again. Records whose callers still hold the lock
//! they staged under are refused instead, and their callers roll back.
use std::{
    fmt, io,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
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
    /// Whether the store takes durable writes again, without changing what
    /// it holds. Asked while degraded with nothing queued.
    fn probe(&mut self) -> io::Result<()>;
}

/// A failed write, as the policy sees it.
#[derive(Debug)]
pub struct Attempt<'a> {
    pub fault: &'a Fault,
    /// Consecutive failures so far, this one included.
    pub failures: u32,
}

/// What the writer does after a failed write.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Try the same write again after the pause.
    Retry(Duration),
    /// Refuse new records until a write or probe succeeds; try again after
    /// the pause, for as long as it takes.
    Degrade(Duration),
    /// Stop writing for good: every waiter and later record gets [`Halted`].
    Halt,
}

pub type Policy = fn(&Attempt) -> Verdict;

/// Quick retries that absorb a blip before the writer degrades.
pub const QUICK_RETRIES: u32 = 2;
/// The longest pause between tries while degraded.
pub const PROBE_MAX: Duration = Duration::from_secs(5);

/// A failed write is an error, not a reason to stop: two quick retries
/// (0.5 s, then 1 s), then degraded, trying again with backoff up to
/// [`PROBE_MAX`] until the disk takes writes. Only an unconfirmed write, which
/// leaves what is on disk unknown, halts.
pub fn retry_then_degrade(attempt: &Attempt) -> Verdict {
    match attempt.fault {
        Fault::Unconfirmed(_) => Verdict::Halt,
        Fault::NotReplaced(_) if attempt.failures <= QUICK_RETRIES => {
            Verdict::Retry(Duration::from_millis(500).saturating_mul(attempt.failures))
        }
        Fault::NotReplaced(_) => Verdict::Degrade(
            Duration::from_millis(250)
                .saturating_mul(1 << (attempt.failures - QUICK_RETRIES - 1).min(5))
                .min(PROBE_MAX),
        ),
    }
}

/// The store does not take writes now; the cause is the last failure.
#[derive(Clone, Debug)]
pub struct Unavailable(Arc<str>);
impl fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "storage unavailable: {}; not saved", self.0)
    }
}
impl std::error::Error for Unavailable {}
impl From<Unavailable> for io::Error {
    fn from(unavailable: Unavailable) -> Self {
        io::Error::other(unavailable)
    }
}

/// The writer stopped after a write it could not confirm.
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

/// Whether the writer takes records.
#[derive(Clone, Debug)]
pub enum Health {
    Ready,
    Degraded(Unavailable),
    Halted(Halted),
}

/// Why a record was not queued.
#[derive(Debug)]
pub enum Submit {
    /// A line or an unchanged save would extend a base the disk does not
    /// have (an earlier record was refused, or a log could not be started):
    /// submit a compaction instead.
    Stale,
    Unavailable(Unavailable),
    Halted(Halted),
}

/// Why a [`Held`] save did not become durable.
#[derive(Debug)]
pub enum Failure {
    /// Nothing was written and nothing will be; the caller rolls back.
    Refused(io::Error),
    Halted(Halted),
}

/// Why a [`Ticket`] did not resolve as durable.
#[derive(Debug)]
pub enum Unsaved {
    /// Still queued: written once the store takes writes again.
    NotYet(Unavailable),
    Halted(Halted),
}
impl fmt::Display for Unsaved {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotYet(Unavailable(cause)) => write!(
                formatter,
                "storage unavailable: {cause}; not saved yet, saved once storage recovers"
            ),
            Self::Halted(halted) => halted.fmt(formatter),
        }
    }
}
impl std::error::Error for Unsaved {}
impl From<Unsaved> for io::Error {
    fn from(unsaved: Unsaved) -> Self {
        io::Error::other(unsaved)
    }
}

/// Whether the caller of a record can still undo it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rollback {
    /// Its caller waits under the lock it staged under.
    Possible,
    /// Its caller already let others see the change.
    Impossible,
}

struct Entry {
    seq: u64,
    record: Record,
    rollback: Rollback,
}

/// Who has the files: nobody (a waiter may take them and lead a commit), a
/// leader or the prober writing, or nobody ever again.
enum Slot {
    Idle(Box<dyn Sink>),
    Writing,
    Closed,
}

struct Queue {
    sink: Slot,
    policy: Policy,
    /// The last sequence number handed out.
    last: u64,
    pending: Vec<Entry>,
    /// Records that failed while degraded and were already visible to
    /// others: the prober writes them, in order, once the store recovers.
    retained: Vec<Entry>,
    /// Every record up to here is durable (or was refused).
    settled: u64,
    /// Records refused so far (each a failed save, so few), with their error.
    refused: Vec<(u64, io::ErrorKind, String)>,
    degraded: Option<Unavailable>,
    /// The thread that tries the store again while degraded; none otherwise.
    prober: Option<JoinHandle<()>>,
    halted: Option<Halted>,
    stale: bool,
    /// The last record queued by a caller that released its lock first:
    /// the newest change others may see before it is durable.
    visible: u64,
    closing: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    /// `Queue::settled`, readable without the lock: a reply checks it on
    /// every request and waits only while a record it may show is in flight.
    durable: AtomicU64,
    /// Signalled whenever records settle, the files come free or the state
    /// changes; waiters, the prober and a closing writer wait on it.
    changed: Condvar,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Waits until `outcome` has an answer, leading a commit whenever records
    /// are queued and nobody is writing: the waiting thread does the I/O
    /// itself, so a lone save costs its write and sync and nothing more, and
    /// everything queued meanwhile shares the next leader's sync.
    fn wait<T>(self: &Arc<Self>, outcome: impl Fn(&Queue) -> Option<T>) -> T {
        let mut queue = self.lock();
        loop {
            if let Some(answer) = outcome(&queue) {
                return answer;
            }
            queue = match queue.sink {
                Slot::Idle(_) if !queue.pending.is_empty() && queue.degraded.is_none() && queue.halted.is_none() => {
                    self.commit(queue)
                }
                _ => self.changed.wait(queue).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
    /// Takes the files and everything queued, writes it on this thread
    /// (retrying, degrading or halting as the policy says) and gives the
    /// files back.
    fn commit<'a>(self: &'a Arc<Self>, mut queue: MutexGuard<'a, Queue>) -> MutexGuard<'a, Queue> {
        let Slot::Idle(mut sink) = std::mem::replace(&mut queue.sink, Slot::Writing) else {
            return queue;
        };
        let mut taken = std::mem::take(&mut queue.pending);
        let mut failures = 0;
        while !taken.is_empty() {
            drop(queue);
            let written = write(sink.as_mut(), &taken);
            queue = self.lock();
            let fault = match written {
                Ok((written, log)) => {
                    failures = 0;
                    settle(self, &mut queue, &mut taken, written, log);
                    continue;
                }
                Err(fault) => fault,
            };
            failures += 1;
            eprintln!("state writer: save failed ({failures} time(s) in a row): {fault}");
            match (queue.policy)(&Attempt {
                fault: &fault,
                failures,
            }) {
                Verdict::Retry(pause) => {
                    drop(queue);
                    std::thread::sleep(pause);
                    queue = self.lock();
                    // Records queued meanwhile extend these; they go together.
                    taken.append(&mut queue.pending);
                }
                Verdict::Degrade(pause) => {
                    degrade(&mut queue, &mut taken, &fault);
                    queue.retained = std::mem::take(&mut taken);
                    queue.prober = probe(self, failures, pause);
                }
                Verdict::Halt => {
                    queue.halted = Some(Halted(fault.to_string().into()));
                    break;
                }
            }
        }
        queue.sink = Slot::Idle(sink);
        self.changed.notify_all();
        queue
    }
}
/// Marks the first `written` of `taken` durable.
fn settle(shared: &Shared, queue: &mut Queue, taken: &mut Vec<Entry>, written: usize, log: Log) {
    if let Log::Broken(error) = log {
        eprintln!(
            "state.log: could not start the log after a compaction ({error}); the next save rewrites the whole snapshot"
        );
        queue.stale = true;
    }
    if let Some(last) = taken.drain(..written).next_back() {
        queue.settled = last.seq;
        shared.durable.store(last.seq, Ordering::Release);
    }
}
fn refusal(queue: &Queue, seq: u64) -> Option<io::Error> {
    queue
        .refused
        .iter()
        .find(|(refused, ..)| *refused == seq)
        .map(|(_, kind, message)| io::Error::new(*kind, message.clone()))
}

/// Starts the thread that tries the store again while degraded: it writes
/// the retained records (or, with none, asks the sink whether it takes writes)
/// with the policy's backoff until that succeeds, then ends. It exists only
/// while degraded.
fn probe(shared: &Arc<Shared>, failures: u32, pause: Duration) -> Option<JoinHandle<()>> {
    let shared = shared.clone();
    std::thread::Builder::new()
        .name("state-prober".into())
        .spawn(move || prober(&shared, failures, pause))
        .inspect_err(|error| eprintln!("state writer: could not start the prober ({error}); restart to save again"))
        .ok()
}

fn prober(shared: &Shared, mut failures: u32, mut pause: Duration) {
    loop {
        let mut queue = shared.lock();
        let until = Instant::now() + pause;
        while !queue.closing
            && let Some(left) = until.checked_duration_since(Instant::now())
        {
            queue = shared
                .changed
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if queue.closing {
            return;
        }
        let Slot::Idle(mut sink) = std::mem::replace(&mut queue.sink, Slot::Writing) else {
            continue;
        };
        let mut taken = std::mem::take(&mut queue.retained);
        drop(queue);
        let written = if taken.is_empty() {
            sink.probe().map(|()| (0, Log::Restarted)).map_err(Fault::NotReplaced)
        } else {
            write(sink.as_mut(), &taken)
        };
        let mut queue = shared.lock();
        let done = match written {
            Ok((written, log)) => {
                failures = 0;
                settle(shared, &mut queue, &mut taken, written, log);
                // What is left after a compaction goes at once.
                pause = Duration::ZERO;
                taken.is_empty()
            }
            Err(fault) => {
                failures += 1;
                eprintln!("state writer: storage still unavailable ({failures} time(s)): {fault}");
                match (queue.policy)(&Attempt {
                    fault: &fault,
                    failures,
                }) {
                    Verdict::Retry(next) | Verdict::Degrade(next) => {
                        pause = next;
                        false
                    }
                    Verdict::Halt => {
                        queue.halted = Some(Halted(fault.to_string().into()));
                        true
                    }
                }
            }
        };
        queue.retained = taken;
        queue.sink = Slot::Idle(sink);
        if done && queue.halted.is_none() {
            queue.degraded = None;
            eprintln!("state writer: storage takes writes again; saves resume");
        }
        shared.changed.notify_all();
        if done {
            return;
        }
    }
}

/// A record whose caller has released (or will release) its lock before
/// waiting: others may see and extend the change, so it is never refused.
#[must_use = "a save is acknowledged only after its ticket resolves"]
pub struct Ticket {
    seq: u64,
    /// What a reader of this change waits for: never a held record, which
    /// may be refused (an unchanged save shares the last record's number).
    visible: u64,
    shared: Arc<Shared>,
}
impl Ticket {
    /// Durable, or an answer that it is not saved yet (it stays queued) or
    /// never will be (halted): never a wait for a disk that does not recover.
    pub fn wait(self) -> Result<Durable, Unsaved> {
        let seq = self.seq;
        self.shared.wait(|queue| {
            if queue.settled >= seq && refusal(queue, seq).is_none() {
                Some(Ok(Durable { seq: Seq(seq) }))
            } else if let Some(halted) = &queue.halted {
                Some(Err(Unsaved::Halted(halted.clone())))
            } else if let Some(unavailable) = &queue.degraded {
                Some(Err(Unsaved::NotYet(unavailable.clone())))
            } else {
                // Only an unchanged save can follow a refused record, and
                // refusing it needs its caller to hold the lock.
                refusal(queue, seq).map(|error| Err(Unsaved::NotYet(Unavailable(error.to_string().into()))))
            }
        })
    }
    /// What a reply that may show this change (or an earlier released one)
    /// waits on before it is written.
    pub fn mark(&self) -> Mark {
        Mark {
            seq: self.visible,
            shared: self.shared.clone(),
        }
    }
}

/// A released record others may already see. A reply that showed its change
/// waits on the mark first, so nobody is told of a change a crash could
/// still take back. Held records need none: their callers keep their lock
/// until the record is durable or rolled back.
#[derive(Clone)]
pub struct Mark {
    seq: u64,
    shared: Arc<Shared>,
}
impl Mark {
    /// Whether the record is durable: one atomic load, no lock.
    pub fn durable(&self) -> bool {
        self.shared.durable.load(Ordering::Acquire) >= self.seq
    }
    /// Waits until the record is durable, leading a commit when nobody is
    /// writing. While the writer is degraded (the record stays queued and is
    /// written on recovery) or halted (Control stops) it returns at once: a
    /// reply never waits for a disk that does not recover.
    pub fn wait(&self) {
        if self.durable() {
            return;
        }
        let seq = self.seq;
        self.shared
            .wait(|queue| (queue.settled >= seq || queue.degraded.is_some() || queue.halted.is_some()).then_some(()));
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
        let seq = self.seq;
        self.shared.wait(|queue| {
            if let Some(error) = refusal(queue, seq) {
                Some(Err(Failure::Refused(error)))
            } else if queue.settled >= seq {
                Some(Ok(Durable { seq: Seq(seq) }))
            } else {
                queue.halted.clone().map(|halted| Err(Failure::Halted(halted)))
            }
        })
    }
}

/// A store's files and the order of its records. There is no writer thread:
/// a caller waiting on its ticket writes everything queued itself when nobody
/// else is writing. Dropping it writes what is queued (unless the store is
/// degraded: that stays unsaved) and closes the files.
pub struct Writer {
    shared: Arc<Shared>,
}
impl Writer {
    pub fn new(sink: impl Sink, policy: Policy) -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue {
                    sink: Slot::Idle(Box::new(sink)),
                    policy,
                    last: 0,
                    pending: Vec::new(),
                    retained: Vec::new(),
                    settled: 0,
                    refused: Vec::new(),
                    degraded: None,
                    prober: None,
                    halted: None,
                    stale: false,
                    visible: 0,
                    closing: false,
                }),
                changed: Condvar::new(),
                durable: AtomicU64::new(0),
            }),
        }
    }
    /// Whether records are taken now: a pure read for status pages.
    pub fn health(&self) -> Health {
        let queue = self.shared.lock();
        match (&queue.halted, &queue.degraded) {
            (Some(halted), _) => Health::Halted(halted.clone()),
            (None, Some(unavailable)) => Health::Degraded(unavailable.clone()),
            (None, None) => Health::Ready,
        }
    }
    /// Queues `record` after every record before it, for a caller that lets
    /// others see its change before waiting; `None` (nothing changed) waits
    /// for those earlier records only.
    pub fn submit(&self, record: Option<Record>) -> Result<Ticket, Submit> {
        let (seq, visible) = self.enqueue(record, Rollback::Impossible)?;
        Ok(Ticket {
            seq,
            visible,
            shared: self.shared.clone(),
        })
    }
    /// [`submit`](Self::submit) for a caller that waits under the lock it
    /// staged under and rolls back if the record is refused.
    pub fn submit_held(&self, record: Option<Record>) -> Result<Held, Submit> {
        let (seq, _) = self.enqueue(record, Rollback::Possible)?;
        Ok(Held {
            seq,
            shared: self.shared.clone(),
        })
    }
    /// The record's sequence number and the newest released one after it.
    fn enqueue(&self, record: Option<Record>, rollback: Rollback) -> Result<(u64, u64), Submit> {
        let mut queue = self.shared.lock();
        if let Some(halted) = &queue.halted {
            return Err(Submit::Halted(halted.clone()));
        }
        if let Some(unavailable) = &queue.degraded {
            return Err(Submit::Unavailable(unavailable.clone()));
        }
        let Some(record) = record else {
            return if queue.stale {
                Err(Submit::Stale)
            } else {
                Ok((queue.last, queue.visible))
            };
        };
        queue.stale = match (&record, queue.stale) {
            (Record::Line(_), true) => return Err(Submit::Stale),
            (Record::Line(_), false) => false,
            (Record::Compact { .. }, _) => false,
        };
        queue.last += 1;
        let seq = queue.last;
        if rollback == Rollback::Impossible {
            queue.visible = seq;
        }
        queue.pending.push(Entry { seq, record, rollback });
        Ok((seq, queue.visible))
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let shared = &self.shared;
        let mut queue = shared.lock();
        queue.closing = true;
        shared.changed.notify_all();
        loop {
            queue = match queue.sink {
                Slot::Writing => shared.changed.wait(queue).unwrap_or_else(PoisonError::into_inner),
                Slot::Idle(_) if !queue.pending.is_empty() && queue.degraded.is_none() && queue.halted.is_none() => {
                    shared.commit(queue)
                }
                Slot::Idle(_) | Slot::Closed => break,
            };
        }
        let prober = queue.prober.take();
        drop(queue);
        if let Some(prober) = prober {
            let _ = prober.join();
        }
        // Closing the files releases the log's lock for the next owner.
        let mut queue = shared.lock();
        queue.sink = Slot::Closed;
        queue
            .halted
            .get_or_insert_with(|| Halted("the writer was closed".into()));
        shared.changed.notify_all();
    }
}

/// Enters (or stays in) the degraded state after `fault`. Records whose
/// callers still hold their lock are refused, newest first, down to the
/// newest one already visible to others: nothing queued depends on them, and
/// their callers roll back. The rest stay taken, to be written in order.
fn degrade(queue: &mut Queue, taken: &mut Vec<Entry>, fault: &Fault) {
    taken.append(&mut queue.pending);
    let kept = taken
        .iter()
        .rposition(|entry| entry.rollback == Rollback::Impossible)
        .map_or(0, |at| at + 1);
    let error = fault.error();
    let cause: Arc<str> = error.to_string().into();
    let refused: Vec<_> = taken
        .drain(kept..)
        .map(|entry| (entry.seq, error.kind(), Unavailable(cause.clone()).to_string()))
        .collect();
    if !refused.is_empty() {
        // The next save compacts: memory, base and disk agree again.
        queue.stale = true;
        queue.refused.extend(refused);
    }
    if queue.degraded.replace(Unavailable(cause)).is_none() {
        eprintln!("state writer: storage unavailable ({fault}); new saves are refused until it recovers");
    }
}

/// Writes a prefix of `rest`: everything up to and including its last
/// compaction (earlier lines are inside the snapshot), else all of its lines
/// with one append. Returns how many records it settled.
fn write(sink: &mut dyn Sink, rest: &[Entry]) -> Result<(usize, Log), Fault> {
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
