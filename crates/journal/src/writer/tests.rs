use super::*;
use std::sync::mpsc;

/// A sink in memory: what it wrote, scripted failures (shared by writes and
/// probes), and an optional gate that holds its first call until the test
/// opens it.
#[derive(Default)]
struct Fake {
    written: Arc<Mutex<Vec<Vec<String>>>>,
    attempts: Arc<Mutex<u32>>,
    /// Failures left before writes succeed (`u32::MAX`: forever).
    failures: u32,
    unconfirmed: bool,
    broken_log: bool,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}
impl Fake {
    fn fail(&mut self) -> Option<io::Error> {
        *self.attempts.lock().unwrap() += 1;
        (self.failures > 0).then(|| {
            if self.failures != u32::MAX {
                self.failures -= 1;
            }
            io::Error::other("injected")
        })
    }
    fn call(&mut self, written: Vec<String>) -> Result<(), Fault> {
        if let Some((entered, open)) = self.gate.take() {
            entered.send(()).unwrap();
            open.recv().unwrap();
        }
        match self.fail() {
            Some(error) if self.unconfirmed => Err(Fault::Unconfirmed(error)),
            Some(error) => Err(Fault::NotReplaced(error)),
            None => {
                self.written.lock().unwrap().push(written);
                Ok(())
            }
        }
    }
}
impl Sink for Fake {
    fn append(&mut self, lines: &[&[u8]]) -> Result<(), Fault> {
        self.call(
            lines
                .iter()
                .map(|line| String::from_utf8_lossy(line).into_owned())
                .collect(),
        )
    }
    fn compact(&mut self, snapshot: &[u8], _: &[u8]) -> Result<Log, Fault> {
        self.call(vec![format!("compact {}", String::from_utf8_lossy(snapshot))])?;
        Ok(if self.broken_log {
            Log::Broken(io::Error::other("injected"))
        } else {
            Log::Restarted
        })
    }
    fn probe(&mut self) -> io::Result<()> {
        self.fail().map_or(Ok(()), Err)
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
fn line(text: &str) -> Option<Record> {
    Some(Record::Line(text.as_bytes().to_vec()))
}
fn compact(text: &str) -> Option<Record> {
    Some(Record::Compact {
        snapshot: text.as_bytes().to_vec(),
        header: b"header".to_vec(),
    })
}
fn gated() -> (Fake, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered) = mpsc::channel();
    let (open, open_rx) = mpsc::channel();
    let fake = Fake {
        gate: Some((entered_tx, open_rx)),
        ..Fake::default()
    };
    (fake, entered, open)
}
fn until(writer: &Writer, ready: fn(&Health) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready(&writer.health()) {
        assert!(Instant::now() < deadline, "health stayed {:?}", writer.health());
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn records_queued_during_a_write_share_the_next_one() {
    let (fake, entered, open) = gated();
    let written = fake.written.clone();
    let writer = Writer::new(fake, quick);
    let first = writer.submit(line("1")).unwrap();
    // Its waiter leads the first commit, on its own thread.
    let leader = std::thread::spawn(move || first.wait().unwrap().seq());
    entered.recv().unwrap();
    let rest: Vec<Ticket> = ["2", "3", "4"]
        .into_iter()
        .map(|text| writer.submit(line(text)).unwrap())
        .collect();
    open.send(()).unwrap();
    let seqs: Vec<Seq> = std::iter::once(leader.join().unwrap())
        .chain(rest.into_iter().map(|ticket| ticket.wait().unwrap().seq()))
        .collect();
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        *written.lock().unwrap(),
        [vec!["1"], vec!["2", "3", "4"]],
        "one write for everything queued meanwhile"
    );
}

#[test]
fn a_compaction_contains_the_lines_queued_before_it() {
    let (fake, entered, open) = gated();
    let written = fake.written.clone();
    let writer = Writer::new(fake, quick);
    let first = writer.submit(line("0")).unwrap();
    let leader = std::thread::spawn(move || first.wait().is_ok());
    entered.recv().unwrap();
    let more = [line("1"), compact("2"), line("3")].map(|record| writer.submit(record).unwrap());
    open.send(()).unwrap();
    assert!(leader.join().unwrap());
    more.into_iter().for_each(|ticket| assert!(ticket.wait().is_ok()));
    assert_eq!(*written.lock().unwrap(), [vec!["0"], vec!["compact 2"], vec!["3"]]);
}

#[test]
fn a_blip_is_absorbed_by_quick_retries() {
    let fake = Fake {
        failures: QUICK_RETRIES,
        ..Fake::default()
    };
    let attempts = fake.attempts.clone();
    let writer = Writer::new(fake, quick);
    writer.submit_held(line("kept")).unwrap().wait().unwrap();
    assert_eq!(*attempts.lock().unwrap(), QUICK_RETRIES + 1);
    assert!(matches!(writer.health(), Health::Ready));
}

/// Released records already applied in memory stay queued while the disk is
/// down, new ones are refused before anything changes, and once the disk
/// recovers the queued ones are written in order and saves resume.
#[test]
fn a_failing_disk_degrades_refuses_new_writes_and_writes_queued_records_in_order_on_recovery() {
    let (mut fake, entered, open) = gated();
    fake.failures = 30;
    let written = fake.written.clone();
    let writer = Writer::new(fake, quick);
    let first = writer.submit(line("1")).unwrap();
    let leader = std::thread::spawn(move || first.wait());
    entered.recv().unwrap();
    let second = writer.submit(line("2")).unwrap();
    open.send(()).unwrap();
    assert!(
        matches!(leader.join().unwrap(), Err(Unsaved::NotYet(_))),
        "an answer, not a hang"
    );
    assert!(matches!(second.wait(), Err(Unsaved::NotYet(_))));
    assert!(matches!(writer.submit(line("refused")), Err(Submit::Unavailable(_))));
    assert!(matches!(writer.submit_held(None), Err(Submit::Unavailable(_))));
    until(&writer, |health| matches!(health, Health::Ready));
    assert_eq!(*written.lock().unwrap(), [vec!["1", "2"]], "written in order, once");
    writer.submit(line("3")).unwrap().wait().unwrap();
    assert_eq!(*written.lock().unwrap(), [vec!["1", "2"], vec!["3"]]);
}

/// A record whose caller still holds its lock is refused instead (the caller
/// rolls back), and the first save after recovery compacts.
#[test]
fn a_held_record_is_refused_on_degrading_and_the_next_save_after_recovery_compacts() {
    let fake = Fake {
        failures: QUICK_RETRIES + 2,
        ..Fake::default()
    };
    let written = fake.written.clone();
    let writer = Writer::new(fake, quick);
    assert!(matches!(
        writer.submit_held(line("refused")).unwrap().wait(),
        Err(Failure::Refused(_))
    ));
    until(&writer, |health| matches!(health, Health::Ready));
    assert!(matches!(
        writer.submit_held(line("on the old base")),
        Err(Submit::Stale)
    ));
    assert!(matches!(writer.submit_held(None), Err(Submit::Stale)));
    writer.submit_held(compact("whole")).unwrap().wait().unwrap();
    writer.submit_held(line("after")).unwrap().wait().unwrap();
    assert_eq!(*written.lock().unwrap(), [vec!["compact whole"], vec!["after"]]);
}

#[test]
fn a_disk_failing_forever_refuses_every_write_and_never_halts() {
    let fake = Fake {
        failures: u32::MAX,
        ..Fake::default()
    };
    let (written, attempts) = (fake.written.clone(), fake.attempts.clone());
    let writer = Writer::new(fake, quick);
    assert!(matches!(
        writer.submit(line("queued")).unwrap().wait(),
        Err(Unsaved::NotYet(_))
    ));
    let tried = *attempts.lock().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(*attempts.lock().unwrap() > tried, "it keeps trying");
    assert!(matches!(writer.health(), Health::Degraded(_)));
    assert!(matches!(writer.submit(compact("later")), Err(Submit::Unavailable(_))));
    assert!(written.lock().unwrap().is_empty());
    drop(writer); // returns: a degraded writer does not hold up shutdown
}

#[test]
fn an_unconfirmed_write_halts_at_once_even_when_its_caller_could_roll_back() {
    let fake = Fake {
        failures: 1,
        unconfirmed: true,
        ..Fake::default()
    };
    let attempts = fake.attempts.clone();
    let writer = Writer::new(fake, quick);
    assert!(matches!(
        writer.submit_held(line("unknown")).unwrap().wait(),
        Err(Failure::Halted(_))
    ));
    assert_eq!(*attempts.lock().unwrap(), 1, "never retried");
    assert!(matches!(writer.health(), Health::Halted(_)));
    assert!(matches!(writer.submit(line("later")), Err(Submit::Halted(_))));
}

#[test]
fn a_log_that_cannot_restart_after_a_compaction_makes_the_next_save_compact() {
    let fake = Fake {
        broken_log: true,
        ..Fake::default()
    };
    let writer = Writer::new(fake, quick);
    writer.submit(compact("durable")).unwrap().wait().unwrap();
    assert!(matches!(writer.submit(line("would be lost")), Err(Submit::Stale)));
}

#[test]
fn an_unchanged_save_waits_for_the_records_before_it_and_dropping_drains() {
    let (fake, entered, open) = gated();
    let written = fake.written.clone();
    let writer = Writer::new(fake, quick);
    let changed = writer.submit(line("changed")).unwrap();
    let unchanged = writer.submit_held(None).unwrap();
    let leader = std::thread::spawn(move || changed.wait().unwrap().seq());
    entered.recv().unwrap();
    let pending = writer.submit(line("pending")).unwrap();
    open.send(()).unwrap();
    assert_eq!(unchanged.wait().unwrap().seq(), leader.join().unwrap());
    drop(writer);
    assert!(pending.wait().is_ok(), "dropping the writer wrote it first");
    assert_eq!(*written.lock().unwrap(), [vec!["changed"], vec!["pending"]]);
}

#[test]
fn the_policy_retries_a_blip_then_degrades_with_capped_backoff_and_halts_only_when_unconfirmed() {
    let not_replaced = Fault::NotReplaced(io::Error::other("full"));
    let unconfirmed = Fault::Unconfirmed(io::Error::other("rename"));
    let verdict = |fault, failures| retry_then_degrade(&Attempt { fault, failures });
    assert_eq!(verdict(&unconfirmed, 1), Verdict::Halt);
    assert_eq!(verdict(&not_replaced, 1), Verdict::Retry(Duration::from_millis(500)));
    assert_eq!(verdict(&not_replaced, 2), Verdict::Retry(Duration::from_secs(1)));
    assert_eq!(verdict(&not_replaced, 3), Verdict::Degrade(Duration::from_millis(250)));
    assert_eq!(verdict(&not_replaced, 5), Verdict::Degrade(Duration::from_secs(1)));
    assert_eq!(verdict(&not_replaced, 1000), Verdict::Degrade(PROBE_MAX));
}

#[test]
fn a_mark_waits_for_its_released_record() {
    let (fake, entered, open) = gated();
    let writer = Writer::new(fake, quick);
    let ticket = writer.submit(line("seen")).unwrap();
    let mark = ticket.mark();
    assert!(!mark.durable());
    let reader = {
        let mark = mark.clone();
        // A reader that finds nobody writing leads the commit itself.
        std::thread::spawn(move || mark.wait())
    };
    entered.recv().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    assert!(!reader.is_finished(), "the record is not durable yet");
    open.send(()).unwrap();
    reader.join().unwrap();
    assert!(mark.durable());
    ticket.wait().unwrap();
}

#[test]
fn a_mark_does_not_wait_for_a_disk_that_is_degraded() {
    let writer = Writer::new(
        Fake {
            failures: u32::MAX,
            ..Fake::default()
        },
        quick,
    );
    let ticket = writer.submit(line("kept")).unwrap();
    let mark = ticket.mark();
    assert!(matches!(ticket.wait(), Err(Unsaved::NotYet(_))));
    mark.wait();
    assert!(!mark.durable(), "kept, written once storage recovers");
}

#[test]
fn an_unchanged_save_marks_only_released_records() {
    let (fake, entered, open) = gated();
    let writer = Writer::new(fake, quick);
    // A held record can still be refused and rolled back: nothing a reader
    // saw depends on it.
    let held = writer.submit_held(line("held")).unwrap();
    let unchanged = writer.submit(None).unwrap();
    assert!(unchanged.mark().durable());
    let leader = std::thread::spawn(move || held.wait().is_ok());
    entered.recv().unwrap();
    open.send(()).unwrap();
    assert!(leader.join().unwrap());
    unchanged.wait().unwrap();
}
