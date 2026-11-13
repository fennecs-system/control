
use super::*;
use std::{sync::atomic::AtomicUsize, thread, time::Duration};

fn soon() -> Option<Instant> {
    Some(Instant::now() + Duration::from_secs(5))
}
/// Returns once a worker has registered, i.e. passed its fast path and is
/// about to wait on (or already waiting on) its condition variable.
fn blocked(token: &CancelToken, waiters: usize) {
    let started = Instant::now();
    while token.registrations() < waiters {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "worker never blocked: {} of {waiters}",
            token.registrations()
        );
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
    assert!(matches!(
        tx.try_send(Counted(dropped.clone())),
        Err(SendError::Closed(_))
    ));

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
    assert_eq!(
        rx.recv(Some(started + Duration::from_millis(20)), &token),
        Recv::Elapsed
    );
    assert!(started.elapsed() >= Duration::from_millis(20));
    tx.try_send(1).unwrap();
    assert_eq!(
        tx.send(2, Some(Instant::now() + Duration::from_millis(20)), &token),
        Err(SendError::Elapsed(2))
    );
    assert_eq!(
        token.wait(Some(Instant::now() + Duration::from_millis(5))),
        Wait::Elapsed
    );
    assert_eq!(rx.recv(Some(Instant::now()), &token), Recv::Item(1));
    assert_eq!(rx.recv(Some(Instant::now()), &token), Recv::Elapsed);
    assert_eq!(
        Changes::new().wait_newer(0, Some(Instant::now()), &token),
        Newer::Elapsed
    );
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
    assert_eq!(
        changes.wait_newer(seen, Some(Instant::now()), &token),
        Newer::Changed(seen + 1)
    );
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
    assert_eq!(
        sum,
        (0..4)
            .map(|p| (0..2_000).map(|i| p * 10_000 + i).sum::<u64>())
            .sum()
    );

    for _ in 0..200 {
        let (_tx, rx) = channel::<u8>(1);
        let token = CancelToken::new();
        let waiting = token.clone();
        let receiver = thread::spawn(move || rx.recv(soon(), &waiting));
        token.cancel();
        assert_eq!(receiver.join().unwrap(), Recv::Cancelled);
    }
}
