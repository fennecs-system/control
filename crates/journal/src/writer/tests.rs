use super::*;
use std::sync::mpsc;

/// A sink in memory: what it wrote, scripted failures, and an optional gate
/// that holds its first call until the test opens it.
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
    fn call(&mut self, written: Vec<String>) -> Result<(), Fault> {
        if let Some((entered, open)) = self.gate.take() {
            entered.send(()).unwrap();
            open.recv().unwrap();
        }
        *self.attempts.lock().unwrap() += 1;
        if self.failures > 0 {
            if self.failures != u32::MAX {
                self.failures -= 1;
            }
            let error = io::Error::other("injected");
            return Err(if self.unconfirmed {
                Fault::Unconfirmed(error)
            } else {
                Fault::NotReplaced(error)
            });
        }
        self.written.lock().unwrap().push(written);
        Ok(())
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
}

/// `stall_then_halt` with a stall short enough for a test.
fn quick(attempt: &Attempt) -> Verdict {
    match stall_then_halt(attempt) {
        Verdict::Retry(_) if attempt.failing_for >= Duration::from_millis(200) => Verdict::Halt,
        Verdict::Retry(_) => Verdict::Retry(Duration::from_millis(1)),
        verdict => verdict,
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

#[test]
fn records_queued_during_a_write_share_the_next_one() {
    let (entered_tx, entered) = mpsc::channel();
    let (open, open_rx) = mpsc::channel();
    let fake = Fake {
        gate: Some((entered_tx, open_rx)),
        ..Fake::default()
    };
    let written = fake.written.clone();
    let writer = Writer::spawn(fake, quick).unwrap();
    let first = writer.submit(line("1")).unwrap();
    entered.recv().unwrap();
    let rest: Vec<Ticket> = ["2", "3", "4"]
        .into_iter()
        .map(|text| writer.submit(line(text)).unwrap())
        .collect();
    open.send(()).unwrap();
    let seqs: Vec<Seq> = std::iter::once(first)
        .chain(rest)
        .map(|ticket| ticket.wait().unwrap().seq())
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
    let (entered_tx, entered) = mpsc::channel();
    let (open, open_rx) = mpsc::channel();
    let fake = Fake {
        gate: Some((entered_tx, open_rx)),
        ..Fake::default()
    };
    let written = fake.written.clone();
    let writer = Writer::spawn(fake, quick).unwrap();
    let tickets = [writer.submit(line("0")).unwrap()];
    entered.recv().unwrap();
    let more = [line("1"), compact("2"), line("3")].map(|record| writer.submit(record).unwrap());
    open.send(()).unwrap();
    tickets
        .into_iter()
        .chain(more)
        .for_each(|ticket| assert!(ticket.wait().is_ok()));
    assert_eq!(*written.lock().unwrap(), [vec!["0"], vec!["compact 2"], vec!["3"]]);
}

#[test]
fn a_disk_failing_a_few_times_stalls_writers_then_recovers() {
    let fake = Fake {
        failures: 3,
        ..Fake::default()
    };
    let (written, attempts) = (fake.written.clone(), fake.attempts.clone());
    let writer = Writer::spawn(fake, quick).unwrap();
    writer.submit(line("kept")).unwrap().wait().unwrap();
    assert_eq!(*attempts.lock().unwrap(), 4);
    assert_eq!(*written.lock().unwrap(), [vec!["kept"]]);
    writer.submit(line("next")).unwrap().wait().unwrap();
}

#[test]
fn a_disk_failing_forever_halts_the_writer_for_every_later_save() {
    let fake = Fake {
        failures: u32::MAX,
        ..Fake::default()
    };
    let written = fake.written.clone();
    let writer = Writer::spawn(fake, quick).unwrap();
    let started = Instant::now();
    assert!(writer.submit(line("lost")).unwrap().wait().is_err());
    assert!(started.elapsed() >= Duration::from_millis(200), "it stalled first");
    assert!(matches!(writer.submit(compact("later")), Err(Submit::Halted(_))));
    assert!(matches!(writer.submit_held(None), Err(Submit::Halted(_))));
    assert!(written.lock().unwrap().is_empty());
}

#[test]
fn a_held_record_alone_in_flight_is_refused_and_the_next_save_compacts() {
    let fake = Fake {
        failures: 1,
        ..Fake::default()
    };
    let written = fake.written.clone();
    let writer = Writer::spawn(fake, quick).unwrap();
    assert!(matches!(
        writer.submit_held(line("refused")).unwrap().wait(),
        Err(Failure::Refused(_))
    ));
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
fn an_unconfirmed_write_halts_at_once_even_when_its_caller_could_roll_back() {
    let fake = Fake {
        failures: 1,
        unconfirmed: true,
        ..Fake::default()
    };
    let attempts = fake.attempts.clone();
    let writer = Writer::spawn(fake, quick).unwrap();
    assert!(matches!(
        writer.submit_held(line("unknown")).unwrap().wait(),
        Err(Failure::Halted(_))
    ));
    assert_eq!(*attempts.lock().unwrap(), 1, "never retried");
}

#[test]
fn a_log_that_cannot_restart_after_a_compaction_makes_the_next_save_compact() {
    let fake = Fake {
        broken_log: true,
        ..Fake::default()
    };
    let writer = Writer::spawn(fake, quick).unwrap();
    writer.submit(compact("durable")).unwrap().wait().unwrap();
    assert!(matches!(writer.submit(line("would be lost")), Err(Submit::Stale)));
}

#[test]
fn an_unchanged_save_waits_for_the_records_before_it_and_dropping_drains() {
    let (entered_tx, entered) = mpsc::channel();
    let (open, open_rx) = mpsc::channel();
    let fake = Fake {
        gate: Some((entered_tx, open_rx)),
        ..Fake::default()
    };
    let written = fake.written.clone();
    let writer = Writer::spawn(fake, quick).unwrap();
    let changed = writer.submit(line("changed")).unwrap();
    let unchanged = writer.submit_held(None).unwrap();
    entered.recv().unwrap();
    let pending = writer.submit(line("pending")).unwrap();
    open.send(()).unwrap();
    assert_eq!(unchanged.wait().unwrap().seq(), changed.wait().unwrap().seq());
    drop(writer);
    assert!(pending.wait().is_ok(), "dropping the writer wrote it first");
    assert_eq!(*written.lock().unwrap(), [vec!["changed"], vec!["pending"]]);
}

#[test]
fn the_policy_refuses_only_undoable_records_and_bounds_the_stall() {
    let not_replaced = Fault::NotReplaced(io::Error::other("full"));
    let unconfirmed = Fault::Unconfirmed(io::Error::other("rename"));
    let attempt = |fault, rollback, seconds| Attempt {
        fault,
        rollback,
        failing_for: Duration::from_secs(seconds),
        failures: 3,
    };
    assert_eq!(
        stall_then_halt(&attempt(&unconfirmed, Rollback::Possible, 0)),
        Verdict::Halt
    );
    assert_eq!(
        stall_then_halt(&attempt(&not_replaced, Rollback::Possible, 0)),
        Verdict::Refuse
    );
    assert_eq!(
        stall_then_halt(&attempt(&not_replaced, Rollback::Impossible, 0)),
        Verdict::Retry(Duration::from_millis(80))
    );
    assert_eq!(
        stall_then_halt(&attempt(&not_replaced, Rollback::Impossible, 30)),
        Verdict::Halt
    );
}
