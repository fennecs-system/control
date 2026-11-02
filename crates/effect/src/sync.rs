use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
/// Waits until notified or the deadline passes. `None` means the deadline had
/// already passed; callers re-check their predicate after every wake.
fn wait_until<'a, T>(changed: &Condvar, guard: MutexGuard<'a, T>, deadline: Option<Instant>) -> Option<MutexGuard<'a, T>> {
    match deadline {
        None => Some(changed.wait(guard).unwrap_or_else(PoisonError::into_inner)),
        Some(deadline) => {
            let now = Instant::now();
            (now < deadline).then(|| changed.wait_timeout(guard, deadline - now).unwrap_or_else(PoisonError::into_inner).0)
        }
    }
}
/// Woken by cancellation. Implementations notify after taking their own lock,
/// so a waiter that registered and then checked the token under that lock
/// cannot miss the wake.
trait Wake: Send + Sync {
    fn wake(&self);
}

struct Token {
    cancelled: AtomicBool,
    signal: Mutex<()>,
    changed: Condvar,
    listeners: Mutex<Vec<Weak<dyn Wake>>>,
    children: Mutex<Vec<Weak<Token>>>,
    parent: Option<Arc<Token>>,
}
impl Drop for Token {
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        while let Some(node) = parent {
            parent = Arc::into_inner(node).and_then(|mut token| token.parent.take());
        }
    }
}
/// One waiter's interest in cancellation, removed when the wait ends.
struct Registration<'a> {
    token: &'a Token,
    listener: Weak<dyn Wake>,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let mut listeners = lock(&self.token.listeners);
        if let Some(index) = listeners.iter().position(|l| Weak::ptr_eq(l, &self.listener)) {
            listeners.swap_remove(index);
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    Cancelled,
    Elapsed,
}
/// Cooperative cancellation shared by clones. Children are cancelled with any
/// ancestor, including through dropped intermediate tokens, but never cancel it.
#[derive(Clone)]
pub struct CancelToken(Arc<Token>);
impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}
impl CancelToken {
    pub fn new() -> Self {
        Self::with_parent(None)
    }
    fn with_parent(parent: Option<Arc<Token>>) -> Self {
        Self(Arc::new(Token {
            cancelled: AtomicBool::new(false),
            signal: Mutex::new(()),
            changed: Condvar::new(),
            listeners: Mutex::new(Vec::new()),
            children: Mutex::new(Vec::new()),
            parent,
        }))
    }
    pub fn child(&self) -> Self {
        let child = Self::with_parent(Some(self.0.clone()));
        let mut children = lock(&self.0.children);
        children.retain(|c| c.strong_count() > 0);
        children.push(Arc::downgrade(&child.0));
        drop(children);
        if self.is_cancelled() {
            child.cancel();
        }
        child
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }
    pub fn cancel(&self) {
        let mut pending = vec![self.0.clone()];
        while let Some(node) = pending.pop() {
            if node.cancelled.swap(true, Ordering::SeqCst) {
                continue;
            }
            drop(lock(&node.signal));
            node.changed.notify_all();
            let listeners = std::mem::take(&mut *lock(&node.listeners));
            listeners.iter().filter_map(Weak::upgrade).for_each(|listener| listener.wake());
            pending.extend(std::mem::take(&mut *lock(&node.children)).iter().filter_map(Weak::upgrade));
        }
    }
    pub fn wait(&self, deadline: Option<Instant>) -> Wait {
        let mut guard = lock(&self.0.signal);
        loop {
            if self.is_cancelled() {
                return Wait::Cancelled;
            }
            match wait_until(&self.0.changed, guard, deadline) {
                Some(next) => guard = next,
                None => return Wait::Elapsed,
            }
        }
    }
    /// Call without holding the listener's lock, then re-check the token under
    /// it before waiting. A cancelled token keeps no registration.
    fn register(&self, listener: Weak<dyn Wake>) -> Option<Registration<'_>> {
        if self.is_cancelled() {
            return None;
        }
        lock(&self.0.listeners).push(listener.clone());
        Some(Registration { token: &self.0, listener })
    }
    #[cfg(test)]
    fn registrations(&self) -> usize {
        lock(&self.0.listeners).len()
    }
}

struct Queue<T> {
    items: VecDeque<T>,
    capacity: usize,
    senders: usize,
    receiving: bool,
}
struct Shared<T> {
    queue: Mutex<Queue<T>>,
    readable: Condvar,
    writable: Condvar,
}
impl<T: Send> Wake for Shared<T> {
    fn wake(&self) {
        drop(lock(&self.queue));
        self.readable.notify_all();
        self.writable.notify_all();
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum SendError<T> {
    Full(T),
    Closed(T),
    Cancelled(T),
    Elapsed(T),
}
impl<T> SendError<T> {
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(item) | Self::Closed(item) | Self::Cancelled(item) | Self::Elapsed(item) => item,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Recv<T> {
    Item(T),
    Closed,
    Cancelled,
    Elapsed,
}
pub struct Sender<T>(Arc<Shared<T>>);
pub struct Receiver<T>(Arc<Shared<T>>);
/// Bounded multi-producer, single-consumer queue. Operations complete at once
/// when they can; cancellation and deadlines only end a wait. Queued items are
/// delivered before `Closed`, and dropped with the receiver.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert!(capacity > 0, "channel capacity must be positive");
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue { items: VecDeque::with_capacity(capacity), capacity, senders: 1, receiving: true }),
        readable: Condvar::new(),
        writable: Condvar::new(),
    });
    (Sender(shared.clone()), Receiver(shared))
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        lock(&self.0.queue).senders += 1;
        Self(self.0.clone())
    }
}
impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut queue = lock(&self.0.queue);
        queue.senders -= 1;
        if queue.senders == 0 {
            drop(queue);
            self.0.readable.notify_all();
        }
    }
}
impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let items = {
            let mut queue = lock(&self.0.queue);
            queue.receiving = false;
            std::mem::take(&mut queue.items)
        };
        self.0.writable.notify_all();
        drop(items);
    }
}
impl<T: Send + 'static> Sender<T> {
    pub fn try_send(&self, item: T) -> Result<(), SendError<T>> {
        let mut queue = lock(&self.0.queue);
        if !queue.receiving {
            return Err(SendError::Closed(item));
        }
        if queue.items.len() >= queue.capacity {
            return Err(SendError::Full(item));
        }
        queue.items.push_back(item);
        drop(queue);
        self.0.readable.notify_one();
        Ok(())
    }
    pub fn send(&self, item: T, deadline: Option<Instant>, token: &CancelToken) -> Result<(), SendError<T>> {
        let (mut registration, mut registered) = (None, false);
        let mut queue = lock(&self.0.queue);
        loop {
            if !queue.receiving {
                return Err(SendError::Closed(item));
            }
            if queue.items.len() < queue.capacity {
                queue.items.push_back(item);
                drop(queue);
                self.0.readable.notify_one();
                return Ok(());
            }
            if token.is_cancelled() {
                return Err(SendError::Cancelled(item));
            }
            if !registered {
                drop(queue);
                let listener: Weak<Shared<T>> = Arc::downgrade(&self.0);
                (registration, registered) = (token.register(listener), true);
                queue = lock(&self.0.queue);
                continue;
            }
            match wait_until(&self.0.writable, queue, deadline) {
                Some(next) => queue = next,
                None => {
                    drop(registration);
                    return Err(SendError::Elapsed(item));
                }
            }
        }
    }
}
impl<T: Send + 'static> Receiver<T> {
    pub fn recv(&self, deadline: Option<Instant>, token: &CancelToken) -> Recv<T> {
        let (mut _registration, mut registered) = (None, false);
        let mut queue = lock(&self.0.queue);
        loop {
            if let Some(item) = queue.items.pop_front() {
                drop(queue);
                self.0.writable.notify_one();
                return Recv::Item(item);
            }
            if queue.senders == 0 {
                return Recv::Closed;
            }
            if token.is_cancelled() {
                return Recv::Cancelled;
            }
            if !registered {
                drop(queue);
                let listener: Weak<Shared<T>> = Arc::downgrade(&self.0);
                (_registration, registered) = (token.register(listener), true);
                queue = lock(&self.0.queue);
                continue;
            }
            match wait_until(&self.0.readable, queue, deadline) {
                Some(next) => queue = next,
                None => return Recv::Elapsed,
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Newer {
    Changed(u64),
    Cancelled,
    Elapsed,
}
/// A generation counter readers wait on instead of polling for state changes.
pub struct Changes {
    generation: Mutex<u64>,
    changed: Condvar,
}
impl Wake for Changes {
    fn wake(&self) {
        drop(lock(&self.generation));
        self.changed.notify_all();
    }
}
impl Changes {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { generation: Mutex::new(0), changed: Condvar::new() })
    }
    pub fn generation(&self) -> u64 {
        *lock(&self.generation)
    }
    pub fn bump(&self) -> u64 {
        let mut generation = lock(&self.generation);
        *generation += 1;
        let current = *generation;
        drop(generation);
        self.changed.notify_all();
        current
    }
    pub fn wait_newer(self: &Arc<Self>, seen: u64, deadline: Option<Instant>, token: &CancelToken) -> Newer {
        let (mut _registration, mut registered) = (None, false);
        let mut generation = lock(&self.generation);
        loop {
            if *generation > seen {
                return Newer::Changed(*generation);
            }
            if token.is_cancelled() {
                return Newer::Cancelled;
            }
            if !registered {
                drop(generation);
                let listener: Weak<Changes> = Arc::downgrade(self);
                (_registration, registered) = (token.register(listener), true);
                generation = lock(&self.generation);
                continue;
            }
            match wait_until(&self.changed, generation, deadline) {
                Some(next) => generation = next,
                None => return Newer::Elapsed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::AtomicUsize,
        thread,
        time::Duration,
    };

    fn soon() -> Option<Instant> {
        Some(Instant::now() + Duration::from_secs(5))
    }
    /// Returns once a worker has registered, i.e. passed its fast path and is
    /// about to wait on (or already waiting on) its condition variable.
    fn blocked(token: &CancelToken, waiters: usize) {
        let started = Instant::now();
        while token.registrations() < waiters {
            assert!(started.elapsed() < Duration::from_secs(5), "worker never blocked: {} of {waiters}", token.registrations());
            thread::yield_now();
        }
    }

    #[test]
    fn cancellation_wakes_blocked_receivers_senders_and_readers() {
        let (_tx, rx) = channel::<u32>(1);
        let token = CancelToken::new();
        let waiting = token.clone();
        let receiver = thread::spawn(move || rx.recv(soon(), &waiting));
        blocked(&token, 1);
        token.cancel();
        assert_eq!(receiver.join().unwrap(), Recv::Cancelled);

        let (tx, rx) = channel::<u32>(1);
        tx.try_send(1).unwrap();
        let token = CancelToken::new();
        let waiting = token.clone();
        let sender = thread::spawn(move || tx.send(2, soon(), &waiting));
        blocked(&token, 1);
        token.cancel();
        assert_eq!(sender.join().unwrap(), Err(SendError::Cancelled(2)));
        assert_eq!(rx.recv(None, &CancelToken::new()), Recv::Item(1));

        let changes = Changes::new();
        let token = CancelToken::new();
        let reader = {
            let (changes, token) = (changes.clone(), token.clone());
            thread::spawn(move || changes.wait_newer(0, soon(), &token))
        };
        blocked(&token, 1);
        token.cancel();
        assert_eq!(reader.join().unwrap(), Newer::Cancelled);

        let token = CancelToken::new();
        let waiting = token.clone();
        let waiter = thread::spawn(move || waiting.wait(soon()));
        token.cancel();
        assert_eq!(waiter.join().unwrap(), Wait::Cancelled);
    }

    #[test]
    fn closing_either_side_wakes_the_other_and_releases_queued_items() {
        let (tx, rx) = channel(2);
        let token = CancelToken::new();
        tx.try_send(1).unwrap();
        tx.clone().try_send(2).unwrap();
        assert_eq!(tx.try_send(3), Err(SendError::Full(3)));
        drop(tx);
        assert_eq!(rx.recv(soon(), &token), Recv::Item(1));
        assert_eq!(rx.recv(soon(), &token), Recv::Item(2));
        assert_eq!(rx.recv(soon(), &token), Recv::Closed);

        let (tx, rx) = channel::<u8>(1);
        let waiting = token.clone();
        let receiver = thread::spawn(move || rx.recv(soon(), &waiting));
        blocked(&token, 1);
        drop(tx);
        assert_eq!(receiver.join().unwrap(), Recv::Closed);

        let (tx, rx) = channel(1);
        tx.try_send("queued").unwrap();
        let waiting = token.clone();
        let sender = thread::spawn(move || tx.send("blocked", soon(), &waiting));
        blocked(&token, 1);
        drop(rx);
        assert_eq!(sender.join().unwrap(), Err(SendError::Closed("blocked")));

        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = channel(2);
        tx.try_send(Counted(dropped.clone())).ok().unwrap();
        tx.try_send(Counted(dropped.clone())).ok().unwrap();
        drop(rx);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert!(matches!(tx.try_send(Counted(dropped.clone())), Err(SendError::Closed(_))));

        let cancelled = CancelToken::new();
        cancelled.cancel();
        let (tx, rx) = channel(1);
        assert_eq!(tx.send(7, soon(), &cancelled), Ok(()));
        assert_eq!(rx.recv(soon(), &cancelled), Recv::Item(7));
        assert_eq!(rx.recv(soon(), &cancelled), Recv::Cancelled);
        assert_eq!(cancelled.registrations(), 0);
    }

    #[test]
    fn deadlines_end_waits_and_registrations_end_with_them() {
        let token = CancelToken::new();
        let (tx, rx) = channel::<u8>(1);
        let started = Instant::now();
        assert_eq!(rx.recv(Some(started + Duration::from_millis(20)), &token), Recv::Elapsed);
        assert!(started.elapsed() >= Duration::from_millis(20));
        tx.try_send(1).unwrap();
        assert_eq!(tx.send(2, Some(Instant::now() + Duration::from_millis(20)), &token), Err(SendError::Elapsed(2)));
        assert_eq!(token.wait(Some(Instant::now() + Duration::from_millis(5))), Wait::Elapsed);
        assert_eq!(rx.recv(Some(Instant::now()), &token), Recv::Item(1));
        assert_eq!(rx.recv(Some(Instant::now()), &token), Recv::Elapsed);
        assert_eq!(Changes::new().wait_newer(0, Some(Instant::now()), &token), Newer::Elapsed);
        assert_eq!(token.registrations(), 0);

        let (tx, rx) = channel::<u8>(1);
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let (tx, token) = (tx.clone(), token.clone());
                thread::spawn(move || tx.send(9, soon(), &token))
            })
            .collect();
        let queued = usize::from(tx.try_send(0).is_ok());
        blocked(&token, 2);
        for _ in 0..3 + queued {
            assert!(matches!(rx.recv(soon(), &token), Recv::Item(_)));
        }
        waiters.into_iter().for_each(|w| w.join().unwrap().unwrap());
        assert_eq!(token.registrations(), 0);
    }

    #[test]
    fn descendants_follow_ancestors_through_dropped_intermediates_but_never_cancel_them() {
        let parent = CancelToken::new();
        let child = parent.child();
        let grandchild = child.child();
        let sibling = parent.child();
        sibling.cancel();
        assert!(!parent.is_cancelled() && !child.is_cancelled());
        drop(child);
        parent.cancel();
        assert!(grandchild.is_cancelled());
        assert!(parent.child().is_cancelled());

        let root = CancelToken::new();
        let mut leaf = root.child();
        for _ in 0..100_000 {
            leaf = leaf.child();
        }
        root.cancel();
        assert!(leaf.is_cancelled());
        drop(root);
        drop(leaf);
    }

    #[test]
    fn changes_wake_readers_on_new_generations() {
        let changes = Changes::new();
        let token = CancelToken::new();
        let seen = changes.generation();
        let reader = {
            let (changes, token) = (changes.clone(), token.clone());
            thread::spawn(move || changes.wait_newer(seen, soon(), &token))
        };
        blocked(&token, 1);
        assert_eq!(changes.bump(), seen + 1);
        assert_eq!(reader.join().unwrap(), Newer::Changed(seen + 1));
        assert_eq!(changes.wait_newer(seen, Some(Instant::now()), &token), Newer::Changed(seen + 1));
    }

    #[test]
    fn no_lost_wakeups_under_contention() {
        let (tx, rx) = channel::<u64>(1);
        let producers: Vec<_> = (0..4)
            .map(|p| {
                let tx = tx.clone();
                thread::spawn(move || {
                    let token = CancelToken::new();
                    for i in 0..2_000 {
                        tx.send(p * 10_000 + i, soon(), &token).unwrap();
                    }
                })
            })
            .collect();
        drop(tx);
        let token = CancelToken::new();
        let (mut count, mut sum) = (0u64, 0u64);
        loop {
            match rx.recv(soon(), &token) {
                Recv::Item(value) => {
                    count += 1;
                    sum += value;
                }
                Recv::Closed => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        producers.into_iter().for_each(|p| p.join().unwrap());
        assert_eq!(count, 8_000);
        assert_eq!(sum, (0..4).map(|p| (0..2_000).map(|i| p * 10_000 + i).sum::<u64>()).sum());

        for _ in 0..200 {
            let (_tx, rx) = channel::<u8>(1);
            let token = CancelToken::new();
            let waiting = token.clone();
            let receiver = thread::spawn(move || rx.recv(soon(), &waiting));
            token.cancel();
            assert_eq!(receiver.join().unwrap(), Recv::Cancelled);
        }
    }
}
