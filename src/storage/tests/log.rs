//! The snapshot plus patch log: what a save writes, and what a restart reads
//! back after clean stops, crashes, migrations and schema changes.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!("control-state-log-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
    fn save(&self, state: &State) {
        persist(&self.0, encode(state).unwrap(), false).unwrap();
    }
    fn compact(&self, state: &State) {
        persist(&self.0, encode(state).unwrap(), true).unwrap();
    }
    /// A restart: this process forgets its handle, then recovers from disk.
    fn restart(&self) -> State {
        published().lock().unwrap().remove(&self.0);
        let data = read_checkpoint(&self.0).unwrap().unwrap();
        let (state, recovered) = recover(&self.0, data).unwrap();
        if !adopt(&self.0, recovered.unwrap()) {
            self.compact(&state);
        }
        state
    }
    fn snapshot(&self) -> Vec<u8> {
        fs::read(self.0.join("state.json")).unwrap()
    }
    fn log(&self) -> Vec<u8> {
        fs::read(self.0.join(LOG)).unwrap()
    }
    fn lines(&self) -> usize {
        self.log().iter().filter(|&&b| b == b'\n').count()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        published().lock().unwrap().remove(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn same(a: &State, b: &State) -> bool {
    let json = |s| crate::history::persisting(|| serde_json::to_value(s).unwrap());
    json(a) == json(b)
}
fn with_chat() -> (State, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    for i in 0..20 {
        crate::chat::send(&mut state, &chat, &format!("message {i} {}", "text ".repeat(50)), "test").unwrap();
    }
    (state, chat)
}

#[test]
fn saves_append_only_the_change_and_a_restart_reads_them_back() {
    let scratch = Scratch::new("append");
    let (mut state, chat) = with_chat();
    scratch.save(&state);
    let snapshot = scratch.snapshot();
    assert_eq!(scratch.lines(), 1, "a first save compacts: snapshot and header");
    crate::chat::send(&mut state, &chat, "one more", "test").unwrap();
    scratch.save(&state);
    scratch.save(&state);
    assert_eq!(scratch.lines(), 2, "an unchanged save writes nothing");
    assert_eq!(scratch.snapshot(), snapshot, "the snapshot is not rewritten");
    assert!(scratch.log().len() < snapshot.len() / 4, "{} byte patch for a {} byte state", scratch.log().len(), snapshot.len());
    let restored = scratch.restart();
    assert!(same(&restored, &state));
    // The restart compacted the replayed log into a fresh snapshot.
    assert_eq!(scratch.lines(), 1);
    let plain: State = serde_json::from_slice(&scratch.snapshot()).unwrap();
    assert!(same(&plain, &state), "a compacted snapshot alone is the whole state");
}

#[test]
fn a_clean_restart_adopts_the_snapshot_without_writing() {
    let scratch = Scratch::new("adopt");
    let (state, _) = with_chat();
    scratch.compact(&state);
    let snapshot = scratch.snapshot();
    let restored = scratch.restart();
    assert!(same(&restored, &state));
    assert_eq!(scratch.snapshot(), snapshot, "same snapshot, same nonce");
}

#[test]
fn a_torn_or_corrupt_log_tail_is_dropped_and_earlier_saves_survive() {
    use std::io::Write;
    let scratch = Scratch::new("torn");
    let (mut state, chat) = with_chat();
    scratch.save(&state);
    crate::chat::send(&mut state, &chat, "durable", "test").unwrap();
    scratch.save(&state);
    let durable = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    // A crash mid-append leaves a line without its newline.
    let mut log = fs::OpenOptions::new().append(true).open(scratch.0.join(LOG)).unwrap();
    log.write_all(br#"[["s",["note"],"never ack"#).unwrap();
    published().lock().unwrap().remove(&scratch.0);
    let restored = scratch.restart();
    assert_eq!(crate::history::persisting(|| serde_json::to_value(&restored).unwrap()), durable);
    // A complete but corrupt line (zero-filled block after power loss) ends the
    // valid prefix too: it and everything after it are truncated on recovery.
    crate::chat::send(&mut state, &chat, "second", "test").unwrap();
    scratch.save(&state);
    let lines = scratch.lines();
    let mut log = fs::OpenOptions::new().append(true).open(scratch.0.join(LOG)).unwrap();
    log.write_all(b"\0\0\0\0\n").unwrap();
    log.write_all(&snapshot::commit(br#"[["s",["note"],"after"]]"#)).unwrap();
    log.write_all(b"\n").unwrap();
    published().lock().unwrap().remove(&scratch.0);
    let data = read_checkpoint(&scratch.0).unwrap().unwrap();
    let (restored, recovered) = recover(&scratch.0, data).unwrap();
    assert!(same(&restored, &state));
    assert_eq!(recovered.unwrap().log.len() as usize, lines, "the corrupt tail was truncated");
}

#[test]
fn a_crash_between_snapshot_and_log_reset_ignores_the_old_log() {
    let scratch = Scratch::new("compaction-crash");
    let (mut state, chat) = with_chat();
    scratch.save(&state);
    crate::chat::send(&mut state, &chat, "in the log", "test").unwrap();
    scratch.save(&state);
    // Compaction published a new snapshot, then the process died before the
    // log was reset: the log's header names the previous snapshot.
    crate::chat::send(&mut state, &chat, "in the new snapshot", "test").unwrap();
    let encoded = encode(&state).unwrap();
    let schema = serde_json::to_value(schema()).unwrap();
    write_checkpoint(&scratch.0, &snapshot::snapshot(&encoded, &snapshot::nonce(), &schema)).unwrap();
    assert_eq!(scratch.lines(), 2);
    assert!(same(&scratch.restart(), &state));
}

#[test]
fn the_log_compacts_once_it_outgrows_the_snapshot() {
    let scratch = Scratch::new("outgrow");
    let mut state = State::default();
    scratch.save(&state);
    for i in 0..6 {
        state.note = format!("{i}").repeat(1 << 20);
        scratch.save(&state);
    }
    assert!(scratch.log().len() < LOG_MINIMUM + (1 << 20) + 4096, "{} log bytes", scratch.log().len());
    assert!(scratch.lines() < 6);
    assert!(same(&scratch.restart(), &state));
}

#[test]
fn a_monolithic_state_json_migrates_and_compaction_keeps_it_readable_by_older_builds() {
    let scratch = Scratch::new("migrate");
    let (state, _) = with_chat();
    // What builds before the patch log wrote: the state alone, no log.
    fs::write(scratch.0.join("state.json"), crate::history::persisting(|| serde_json::to_vec(&state)).unwrap()).unwrap();
    let restored = scratch.restart();
    assert!(same(&restored, &state));
    let snapshot = scratch.snapshot();
    assert!(snapshot.starts_with(br#"{"_snapshot":"#), "migrated to a named snapshot");
    let plain: State = serde_json::from_slice(&snapshot).unwrap();
    assert!(same(&plain, &state), "older builds ignore the extra members");
    // An older build then rewrote state.json (rollback): its log is stale and ignored.
    let (mut older, chat) = with_chat();
    crate::chat::send(&mut older, &chat, "written by the older build", "test").unwrap();
    scratch.save(&restored);
    fs::write(scratch.0.join("state.json"), crate::history::persisting(|| serde_json::to_vec(&older)).unwrap()).unwrap();
    assert!(same(&scratch.restart(), &older));
}

#[test]
fn state_from_a_newer_schema_is_read_back_through_its_recorded_lenses() {
    let scratch = Scratch::new("lens");
    let (mut state, _) = with_chat();
    state.note = "kept".into();
    // A hypothetical version 2 renamed `note` to `notes` and recorded the lens.
    let newer = Schema { version: 2, lenses: vec![vec![], vec![lens::Lens::Rename { from: "note".into(), to: "notes".into() }]] };
    let mut document = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    lens::document(&lens::route(&schema(), &newer).unwrap(), &mut document);
    assert_eq!(document["notes"], "kept");
    let newer_value = serde_json::to_value(&newer).unwrap();
    let encoded = snapshot::encode(&document).unwrap();
    fs::write(scratch.0.join("state.json"), snapshot::snapshot(&encoded, "n", &newer_value)).unwrap();
    // Its log changed `notes`; this build reads it as `note`.
    let mut log = snapshot::header("n", &newer_value);
    log.push(b'\n');
    log.extend(snapshot::commit(br#"[["s",["notes"],"edited by the newer build"]]"#));
    log.push(b'\n');
    fs::write(scratch.0.join(LOG), log).unwrap();
    let restored = scratch.restart();
    assert_eq!(restored.note, "edited by the newer build");
    assert_eq!(restored.chats.len(), 1);
}

/// What a restart recovers from the files as they are.
fn recovered(scratch: &Scratch) -> serde_json::Value {
    published().lock().unwrap().remove(&scratch.0);
    let (state, _) = recover(&scratch.0, read_checkpoint(&scratch.0).unwrap().unwrap()).unwrap();
    crate::history::persisting(|| serde_json::to_value(&state).unwrap())
}

/// One save that changes a chat, a run, a new run and two top-level members
/// is one transaction: a crash after any byte of it recovers all or none.
#[test]
fn a_crash_at_every_byte_of_a_multi_entity_save_recovers_before_or_after_it() {
    let scratch = Scratch::new("transaction");
    let (mut state, chat) = with_chat();
    scratch.save(&state);
    let committed = scratch.log();
    let before = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    state.runtime.runs[0].status = "completed".into();
    crate::chat::send(&mut state, &chat, "a reply and a receipt", "test").unwrap();
    state.runtime.notice = "receipt r-1 recorded".into();
    state.note = "edited".into();
    scratch.save(&state);
    let after = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    let full = scratch.log();
    let line = &full[committed.len()..];
    assert!(line.len() > 100 && line.ends_with(b"\n") && line.iter().filter(|&&b| b == b'\n').count() == 1, "one line, one commit");
    for cut in 0..=line.len() {
        fs::write(scratch.0.join(LOG), &full[..committed.len() + cut]).unwrap();
        let expected = if cut == line.len() { &after } else { &before };
        assert!(&recovered(&scratch) == expected, "crash after {cut} of {} bytes", line.len());
    }
    // A complete line with damaged content (a block lost after power failure)
    // fails its checksum and is not applied.
    for at in [committed.len() + 40, full.len() / 2 + committed.len() / 2, full.len() - 3] {
        let mut damaged = full.clone();
        damaged[at] ^= 0x01;
        fs::write(scratch.0.join(LOG), &damaged).unwrap();
        assert!(recovered(&scratch) == before, "damage at byte {at}");
    }
}

/// Compaction writes the snapshot to a temporary file, syncs and renames it,
/// syncs the directory, then restarts the log. A crash after any of those
/// steps recovers the same committed state.
#[test]
fn a_crash_at_every_step_of_compaction_recovers_the_committed_state() {
    let scratch = Scratch::new("compaction-steps");
    let (mut state, chat) = with_chat();
    scratch.save(&state);
    crate::chat::send(&mut state, &chat, "committed in the log", "test").unwrap();
    scratch.save(&state);
    let committed = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    let (snapshot_before, log_before) = (scratch.snapshot(), scratch.log());
    let schema = serde_json::to_value(schema()).unwrap();
    let encoded = encode(&state).unwrap();
    let nonce = snapshot::nonce();
    let next = snapshot::snapshot(&encoded, &nonce, &schema);
    let header = snapshot::header(&nonce, &schema);
    let (snapshot_path, log_path, temporary) = (scratch.0.join("state.json"), scratch.0.join(LOG), scratch.0.join("state.json.tmp"));
    // (crash point, temporary file, published snapshot, log)
    type Files<'a> = (&'a str, Option<&'a [u8]>, &'a [u8], &'a [u8]);
    let restarted = [&header[..], b"\n"].concat();
    let steps: [Files; 6] = [
        ("temporary file partly written", Some(&next[..next.len() / 2]), &snapshot_before, &log_before),
        ("temporary file complete, not renamed", Some(&next), &snapshot_before, &log_before),
        ("renamed, old log", None, &next, &log_before),
        ("log emptied", None, &next, b""),
        ("log header torn", None, &next, &header[..header.len() / 2]),
        ("log restarted", None, &next, &restarted),
    ];
    for (step, partial, published, log) in steps {
        let _ = fs::remove_file(&temporary);
        if let Some(partial) = partial {
            fs::write(&temporary, partial).unwrap();
        }
        fs::write(&snapshot_path, published).unwrap();
        fs::write(&log_path, log).unwrap();
        assert!(recovered(&scratch) == committed, "crash at: {step}");
    }
}

/// A full disk (here a file-size limit, in a child process because the limit
/// is per process): the append and the compaction both fail as not replaced,
/// leave no partial line or snapshot, and the next save after space returns
/// succeeds from the same base.
#[test]
fn a_full_disk_fails_saves_cleanly_without_corrupting_state() {
    const CHILD: &str = "CONTROL_STATE_LOG_FULL_DISK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::tests::log::a_full_disk_fails_saves_cleanly_without_corrupting_state", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let scratch = Scratch::new("full-disk");
    let (mut state, chat) = with_chat();
    scratch.save(&state);
    let (snapshot_before, log_before) = (scratch.snapshot(), scratch.log());
    let before = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    let limit = |bytes: u64| unsafe {
        let mut current = std::mem::zeroed::<libc::rlimit>();
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut current), 0);
        current.rlim_cur = bytes.min(current.rlim_max);
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &current), 0);
    };
    unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
    limit(log_before.len() as u64 + 16);
    crate::chat::send(&mut state, &chat, &"does not fit ".repeat(100), "test").unwrap();
    let encode = |state: &State| super::encode(state).unwrap();
    assert!(matches!(persist(&scratch.0, encode(&state), false), Err(CheckpointError::NotReplaced(_))), "append fails cleanly");
    assert!(matches!(persist(&scratch.0, encode(&state), true), Err(CheckpointError::NotReplaced(_))), "compaction fails cleanly");
    assert_eq!((scratch.snapshot(), scratch.log()), (snapshot_before, log_before), "no partial line or snapshot");
    assert!(recovered(&scratch) == before);
    limit(libc::RLIM_INFINITY);
    scratch.save(&state);
    assert!(same(&scratch.restart(), &state), "saves resume once space returns");
}

/// Runs remember their saved bytes; every way of changing a run forgets them.
/// `encode` itself asserts the cached encoding equals a fresh one.
#[test]
fn cached_runs_are_reencoded_after_any_mutable_access() {
    let (mut state, chat) = with_chat();
    let fresh = |state: &State| crate::history::uncached(|| crate::history::persisting(|| serde_json::to_vec(state).unwrap()));
    let mut previous = encode(&state).unwrap().bytes().to_vec();
    let changes: [&dyn Fn(&mut State); 5] = [
        &|s| s.runtime.runs[0].status = "completed".into(),
        &|s| s.runtime.runs.iter_mut().for_each(|run| run.attempts.push(Default::default())),
        &|s| *s.runtime.runs[1].result = "spilled text".into(),
        &|s| drop(crate::history::archive_run(&mut s.runtime.runs[1]).unwrap()),
        &|s| s.runtime.runs.retain(|run| run.status != "completed"),
    ];
    for change in changes {
        change(&mut state);
        let next = encode(&state).unwrap().bytes().to_vec();
        assert!(next != previous && next == fresh(&state));
        previous = next;
    }
    crate::chat::send(&mut state, &chat, "new run", "test").unwrap();
    assert_eq!(encode(&state).unwrap().bytes(), fresh(&state));
}
