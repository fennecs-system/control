//! Writes that release the state lock before waiting for the disk
//! (`Store::commit`): what their callers hear when storage fails, that the
//! dispatcher is woken only once the change is durable, and that a reply
//! showing a pending change waits for it. The disk is a scripted writer.
use super::*;
use crate::storage::Saving;
use crate::store::{Commit, Staging};
use control_journal::writer::{Attempt, Fault, Log, Record, Sink, Verdict, Writer, retry_then_degrade};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// A disk a test scripts: failures before writes succeed, a gate that holds
/// its first write until the test opens it, and what it wrote.
#[derive(Default)]
struct Disk {
    written: Arc<Mutex<Vec<String>>>,
    failures: Arc<AtomicUsize>,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}
impl Disk {
    fn fail(&self) -> bool {
        self.failures
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
            .is_ok()
    }
    fn call(&mut self, what: String) -> Result<(), Fault> {
        if let Some((entered, open)) = self.gate.take() {
            entered.send(()).unwrap();
            open.recv().unwrap();
        }
        if self.fail() {
            return Err(Fault::NotReplaced(std::io::Error::other("disk full")));
        }
        self.written.lock().unwrap().push(what);
        Ok(())
    }
}
impl Sink for Disk {
    fn append(&mut self, lines: &[&[u8]]) -> Result<(), Fault> {
        self.call(format!("{} line(s)", lines.len()))
    }
    fn compact(&mut self, _: &[u8], _: &[u8]) -> Result<Log, Fault> {
        self.call("compact".into()).map(|()| Log::Restarted)
    }
    fn probe(&mut self) -> std::io::Result<()> {
        if self.fail() {
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(())
        }
    }
}
/// `retry_then_degrade` with pauses short enough for a test.
fn quick(attempt: &Attempt) -> Verdict {
    match retry_then_degrade(attempt) {
        Verdict::Retry(_) => Verdict::Retry(Duration::from_millis(1)),
        Verdict::Degrade(_) => Verdict::Degrade(Duration::from_millis(2)),
        Verdict::Halt => Verdict::Halt,
    }
}
/// Stages one line on `writer`, as `storage::submit` does on the node's.
fn stage(writer: &Writer) -> impl FnOnce(&State, Staging) -> std::io::Result<Saving> + '_ {
    move |_, _| {
        writer
            .submit(Some(Record::Line(b"{}".to_vec())))
            .map(Saving::staged)
            .map_err(|refused| std::io::Error::other(format!("{refused:?}")))
    }
}
/// Refuses before staging, as `storage::submit` does while storage is unavailable.
fn refused(_: &State, _: Staging) -> std::io::Result<Saving> {
    Err(std::io::Error::other("storage unavailable: disk full; not saved"))
}
fn until(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn with_chat() -> (Store, String) {
    let mut state = State::default();
    let id = chat::create(&mut state);
    (Store::new(state), id)
}
fn message(text: &str) -> HashMap<String, String> {
    HashMap::from([("message".to_owned(), text.to_owned())])
}

#[test]
fn a_chat_send_whose_save_fails_after_release_is_kept_and_saved_on_recovery_without_a_wake() {
    let (store, id) = with_chat();
    // Three failures degrade the writer (two quick retries); the prober
    // fails once more, then writes the kept record.
    let disk = Disk {
        failures: Arc::new(AtomicUsize::new(4)),
        ..Disk::default()
    };
    let written = disk.written.clone();
    let writer = Writer::new(disk, quick);
    let woke = AtomicBool::new(false);
    let committed = session_http::send_commit(&store, &id, "", None, &message("Keep this"), stage(&writer), || {
        woke.store(true, Ordering::SeqCst)
    })
    .unwrap();
    let Commit::NotYet(_, unsaved) = committed else {
        panic!("a failure after the release keeps the change");
    };
    let reply = String::from_utf8(session_http::not_saved_yet("", &unsaved.to_string())).unwrap();
    assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
    assert!(
        reply.contains("not saved yet, saved once storage recovers"),
        "the caller is told it is not saved yet: {reply}"
    );
    assert!(!woke.load(Ordering::SeqCst), "no wake before durability");
    store
        .read(|state| {
            assert_eq!(state.chats[0].messages.last().unwrap().text, "Keep this");
            assert!(
                state
                    .runtime
                    .runs
                    .iter()
                    .any(|run| run.status == runtime::RunStatus::Queued),
                "others may already see the queued turn, so it stays"
            );
        })
        .unwrap();
    until(|| written.lock().unwrap().len() == 1);
    assert!(
        !woke.load(Ordering::SeqCst),
        "the wake was forgone; the dispatcher's scan finds the queued turn"
    );
}

#[test]
fn a_chat_send_wakes_the_dispatcher_only_once_it_is_durable() {
    let (store, id) = with_chat();
    let (entered, entered_rx) = mpsc::channel();
    let (open, open_rx) = mpsc::channel();
    let writer = Writer::new(
        Disk {
            gate: Some((entered, open_rx)),
            ..Disk::default()
        },
        quick,
    );
    let woke = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let sender = scope.spawn(|| {
            session_http::send_commit(&store, &id, "", None, &message("Go"), stage(&writer), || {
                woke.store(true, Ordering::SeqCst)
            })
        });
        entered_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!woke.load(Ordering::SeqCst), "the disk has not taken it yet");
        assert!(store.try_guard().is_some(), "the lock is released while waiting");
        open.send(()).unwrap();
        assert!(matches!(sender.join().unwrap(), Ok(Commit::Saved(_))));
    });
    assert!(woke.load(Ordering::SeqCst));
}

#[test]
fn a_chat_send_storage_refuses_before_release_is_undone() {
    let (store, id) = with_chat();
    let committed = session_http::send_commit(&store, &id, "", None, &message("Lost"), refused, || {
        panic!("nothing to wake")
    })
    .unwrap();
    assert!(matches!(committed, Commit::NotSaved(_)));
    store
        .read(|state| {
            assert!(state.chats[0].messages.is_empty());
            assert!(state.runtime.runs.is_empty());
        })
        .unwrap();
}

#[test]
fn a_task_the_caller_was_told_failed_leaves_no_queued_run() {
    let store = Store::new(State::default());
    let task = HashMap::from([("objective".to_owned(), "Never runs".to_owned())]);
    let committed = posts::commit(&store, posts::Post::Task, &task, refused, || panic!("nothing to wake")).unwrap();
    assert!(matches!(committed, Commit::NotSaved(_)));
    store
        .read(|state| {
            assert!(state.runtime.runs.is_empty(), "the refused task is not queued");
            assert!(state.runtime.notice.is_empty());
        })
        .unwrap();
}

#[test]
fn a_retry_storage_refuses_is_undone_and_one_it_kept_says_not_saved_yet() {
    let mut state = State::default();
    state
        .runtime
        .enqueue("Failed once".into(), runtime::RunKind::Task, "test");
    state.runtime.runs[0].status = runtime::RunStatus::Failed;
    *state.runtime.runs[0].result = "It broke.".into();
    let id = state.runtime.runs[0].id().as_str().to_owned();
    let store = Store::new(state);
    let retry = HashMap::from([("id".to_owned(), id)]);
    let committed = posts::commit(&store, posts::Post::Retry, &retry, refused, || {}).unwrap();
    assert!(matches!(committed, Commit::NotSaved(_)));
    store
        .read(|state| {
            assert_eq!(state.runtime.runs[0].status, runtime::RunStatus::Failed);
            assert_eq!(&*state.runtime.runs[0].result, "It broke.");
        })
        .unwrap();
    let disk = Disk {
        failures: Arc::new(AtomicUsize::new(3)),
        ..Disk::default()
    };
    let written = disk.written.clone();
    let writer = Writer::new(disk, quick);
    let committed = posts::commit(&store, posts::Post::Retry, &retry, stage(&writer), || {}).unwrap();
    assert!(matches!(committed, Commit::NotYet((), _)));
    store
        .read(|state| assert_eq!(state.runtime.runs[0].status, runtime::RunStatus::Queued))
        .unwrap();
    until(|| written.lock().unwrap().len() == 1);
}

#[test]
fn a_room_post_storage_refuses_is_undone() {
    let store = Store::new(State::default());
    let post = HashMap::from([
        ("channel".to_owned(), "research".to_owned()),
        ("message".to_owned(), "Lost".to_owned()),
    ]);
    let committed = posts::commit(&store, posts::Post::Room, &post, refused, || {}).unwrap();
    assert!(matches!(committed, Commit::NotSaved(_)));
    store.read(|state| assert!(state.collective.posts.is_empty())).unwrap();
}

#[test]
fn a_reply_that_showed_a_pending_change_waits_until_it_is_durable() {
    let (store, id) = with_chat();
    let (entered, entered_rx) = mpsc::channel();
    let (open, open_rx) = mpsc::channel();
    let writer = Writer::new(
        Disk {
            gate: Some((entered, open_rx)),
            ..Disk::default()
        },
        quick,
    );
    let replied = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let sender = scope
            .spawn(|| session_http::send_commit(&store, &id, "", None, &message("Seen early"), stage(&writer), || {}));
        // The sender leads the write and is held at the disk: its message is
        // visible to others and not durable.
        entered_rx.recv().unwrap();
        let reader = scope.spawn(|| {
            let mut reply = Vec::new();
            read_response(
                &mut reply,
                &store,
                &session_http::CHAT_PAGES,
                session_http::ChatPage::Transcript(id.clone()),
            );
            replied.store(true, Ordering::SeqCst);
            String::from_utf8(reply).unwrap()
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !replied.load(Ordering::SeqCst),
            "the reply waits for the change it showed"
        );
        open.send(()).unwrap();
        assert!(reader.join().unwrap().contains("Seen early"));
        assert!(matches!(sender.join().unwrap(), Ok(Commit::Saved(_))));
    });
    // Nothing in flight: a reply has nothing to wait for.
    let ((), seen) = store.read_seen(|_| ()).unwrap();
    assert!(seen.is_clear());
}
