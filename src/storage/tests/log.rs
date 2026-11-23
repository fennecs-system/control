//! The snapshot plus patch log: what a save writes, and what a restart reads
//! back after clean stops, crashes, migrations and schema changes.
use super::*;
use crate::runtime::RunStatus;

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!("control-state-log-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
    fn save(&self, state: &State) {
        checkpoint_in(&self.0, state).unwrap();
    }
    fn compact(&self, state: &State) {
        persist(&self.0, encode(state, &archived(&self.0)).unwrap(), true).unwrap();
    }
    /// A restart: this process forgets its handle, then recovers from disk.
    fn restart(&self) -> State {
        published().lock().unwrap().remove(&self.0);
        archives().lock().unwrap().remove(&self.0);
        let data = read_checkpoint(&self.0).unwrap().unwrap();
        let (mut state, recovered) = recover(&self.0, data).unwrap();
        // As `load` does before anything reads runs.
        crate::history::hydrate_text(&mut state).unwrap();
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
        archives().lock().unwrap().remove(&self.0);
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
        crate::chat::send(
            &mut state,
            &chat,
            &format!("message {i} {}", "text ".repeat(50)),
            "test",
        )
        .unwrap();
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
    assert!(
        scratch.log().len() < snapshot.len() / 4,
        "{} byte patch for a {} byte state",
        scratch.log().len(),
        snapshot.len()
    );
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
    archives().lock().unwrap().remove(&scratch.0);
    let restored = scratch.restart();
    assert_eq!(
        crate::history::persisting(|| serde_json::to_value(&restored).unwrap()),
        durable
    );
    // A complete but corrupt line (zero-filled block after power loss) ends the
    // valid prefix too: it and everything after it are truncated on recovery.
    crate::chat::send(&mut state, &chat, "second", "test").unwrap();
    scratch.save(&state);
    let lines = scratch.lines();
    let mut log = fs::OpenOptions::new().append(true).open(scratch.0.join(LOG)).unwrap();
    log.write_all(b"\0\0\0\0\n").unwrap();
    log.write_all(&snapshot::commit(br#"[["s",["note"],"after"]]"#))
        .unwrap();
    log.write_all(b"\n").unwrap();
    published().lock().unwrap().remove(&scratch.0);
    archives().lock().unwrap().remove(&scratch.0);
    let data = read_checkpoint(&scratch.0).unwrap().unwrap();
    let (restored, recovered) = recover(&scratch.0, data).unwrap();
    assert!(same(&restored, &state));
    assert_eq!(
        recovered.unwrap().log.len() as usize,
        lines,
        "the corrupt tail was truncated"
    );
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
    let encoded = encode(&state, &archived(&scratch.0)).unwrap();
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
    assert!(
        scratch.log().len() < LOG_MINIMUM + (1 << 20) + 4096,
        "{} log bytes",
        scratch.log().len()
    );
    assert!(scratch.lines() < 6);
    assert!(same(&scratch.restart(), &state));
}

#[test]
fn a_monolithic_state_json_migrates_and_compaction_keeps_it_readable_by_older_builds() {
    let scratch = Scratch::new("migrate");
    let (state, _) = with_chat();
    // What builds before the patch log wrote: the state alone, no log.
    fs::write(
        scratch.0.join("state.json"),
        crate::history::persisting(|| serde_json::to_vec(&state)).unwrap(),
    )
    .unwrap();
    let restored = scratch.restart();
    assert!(same(&restored, &state));
    let snapshot = scratch.snapshot();
    assert!(
        snapshot.starts_with(br#"{"_snapshot":"#),
        "migrated to a named snapshot"
    );
    let plain: State = serde_json::from_slice(&snapshot).unwrap();
    assert!(same(&plain, &state), "older builds ignore the extra members");
    // An older build then rewrote state.json (rollback): its log is stale and ignored.
    let (mut older, chat) = with_chat();
    crate::chat::send(&mut older, &chat, "written by the older build", "test").unwrap();
    scratch.save(&restored);
    fs::write(
        scratch.0.join("state.json"),
        crate::history::persisting(|| serde_json::to_vec(&older)).unwrap(),
    )
    .unwrap();
    assert!(same(&scratch.restart(), &older));
}

#[test]
fn state_from_a_newer_schema_is_read_back_through_its_recorded_lenses() {
    let scratch = Scratch::new("lens");
    let (mut state, _) = with_chat();
    state.note = "kept".into();
    // A hypothetical next version renamed `note` to `notes` and recorded the lens.
    let mut newer = schema();
    newer.version += 1;
    newer.lenses.push(vec![lens::Lens::Rename {
        from: "note".into(),
        to: "notes".into(),
    }]);
    let mut document = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    lens::document(&lens::route(&schema(), &newer).unwrap(), &mut document);
    assert_eq!(document["notes"], "kept");
    let newer_value = serde_json::to_value(&newer).unwrap();
    let encoded = snapshot::encode(&document).unwrap();
    fs::write(
        scratch.0.join("state.json"),
        snapshot::snapshot(&encoded, "n", &newer_value),
    )
    .unwrap();
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
    archives().lock().unwrap().remove(&scratch.0);
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
    state.runtime.runs[0].status = RunStatus::Completed;
    crate::chat::send(&mut state, &chat, "a reply and a receipt", "test").unwrap();
    state.runtime.notice = "receipt r-1 recorded".into();
    state.note = "edited".into();
    scratch.save(&state);
    let after = crate::history::persisting(|| serde_json::to_value(&state).unwrap());
    let full = scratch.log();
    let line = &full[committed.len()..];
    assert!(
        line.len() > 100 && line.ends_with(b"\n") && line.iter().filter(|&&b| b == b'\n').count() == 1,
        "one line, one commit"
    );
    for cut in 0..=line.len() {
        fs::write(scratch.0.join(LOG), &full[..committed.len() + cut]).unwrap();
        let expected = if cut == line.len() { &after } else { &before };
        assert!(
            &recovered(&scratch) == expected,
            "crash after {cut} of {} bytes",
            line.len()
        );
    }
    // A complete line with damaged content (a block lost after power failure)
    // fails its checksum and is not applied.
    for at in [
        committed.len() + 40,
        full.len() / 2 + committed.len() / 2,
        full.len() - 3,
    ] {
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
    let encoded = encode(&state, &archived(&scratch.0)).unwrap();
    let nonce = snapshot::nonce();
    let next = snapshot::snapshot(&encoded, &nonce, &schema);
    let header = snapshot::header(&nonce, &schema);
    let (snapshot_path, log_path, temporary) = (
        scratch.0.join("state.json"),
        scratch.0.join(LOG),
        scratch.0.join("state.json.tmp"),
    );
    // (crash point, temporary file, published snapshot, log)
    type Files<'a> = (&'a str, Option<&'a [u8]>, &'a [u8], &'a [u8]);
    let restarted = [&header[..], b"\n"].concat();
    let steps: [Files; 6] = [
        (
            "temporary file partly written",
            Some(&next[..next.len() / 2]),
            &snapshot_before,
            &log_before,
        ),
        (
            "temporary file complete, not renamed",
            Some(&next),
            &snapshot_before,
            &log_before,
        ),
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
            .args([
                "--exact",
                "storage::tests::log::a_full_disk_fails_saves_cleanly_without_corrupting_state",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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
    let encode = |state: &State| super::encode(state, &archived(&scratch.0)).unwrap();
    assert!(
        matches!(
            persist(&scratch.0, encode(&state), false),
            Err(CheckpointError::NotReplaced(_))
        ),
        "append fails cleanly"
    );
    assert!(
        matches!(
            persist(&scratch.0, encode(&state), true),
            Err(CheckpointError::NotReplaced(_))
        ),
        "compaction fails cleanly"
    );
    assert_eq!(
        (scratch.snapshot(), scratch.log()),
        (snapshot_before, log_before),
        "no partial line or snapshot"
    );
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
    let fresh =
        |state: &State| crate::history::uncached(|| crate::history::persisting(|| serde_json::to_vec(state).unwrap()));
    let mut previous = encode(&state, &Default::default()).unwrap().bytes().to_vec();
    let changes: [&dyn Fn(&mut State); 5] = [
        &|s| s.runtime.runs[0].status = RunStatus::Completed,
        &|s| {
            s.runtime
                .runs
                .iter_mut()
                .for_each(|run| run.attempts.push(Default::default()))
        },
        &|s| *s.runtime.runs[1].result = "spilled text".into(),
        &|s| drop(crate::history::archive_run(&mut s.runtime.runs[1]).unwrap()),
        &|s| s.runtime.runs.retain(|run| run.status != RunStatus::Completed),
    ];
    for change in changes {
        change(&mut state);
        let next = encode(&state, &Default::default()).unwrap().bytes().to_vec();
        assert!(next != previous && next == fresh(&state));
        previous = next;
    }
    crate::chat::send(&mut state, &chat, "new run", "test").unwrap();
    assert_eq!(encode(&state, &Default::default()).unwrap().bytes(), fresh(&state));
}

/// `count` finished runs, oldest first, then a chat with live work.
fn with_history(count: usize) -> State {
    let mut state = State::default();
    for i in 0..count {
        state
            .runtime
            .enqueue(format!("finished task {i}"), crate::runtime::RunKind::Task, "test");
        let run = state.runtime.runs.last_mut().unwrap();
        run.status = RunStatus::Completed;
        *run.result = format!("result {i}");
    }
    state
}
fn saved_runs(scratch: &Scratch) -> usize {
    let document: serde_json::Value = crate::config::read_state_in(&scratch.0, usize::MAX >> 1)
        .unwrap()
        .unwrap();
    document["runtime"]["runs"].as_array().unwrap().len()
}
fn runs_json(state: &State) -> serde_json::Value {
    // Every run, archived or not: a view, not a save.
    serde_json::to_value(&state.runtime.runs).unwrap()
}
fn blocks(scratch: &Scratch) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(scratch.0.join("history/runs")).map_or(Vec::new(), |entries| {
        entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    });
    names.sort();
    names
}

/// Finished runs far from the focus leave the state for blocks; a restart
/// reads them back, in order, and every run is still found by id.
#[test]
fn archived_runs_leave_the_state_and_are_found_by_id_after_restart() {
    let scratch = Scratch::new("archive");
    let mut state = with_history(archive::RECENT + 200);
    // An old resumable run, an old run a recent one points at, and a running one stay hot.
    state.runtime.runs[3].status = RunStatus::Failed;
    let last = state.runtime.runs.len() - 1;
    state.runtime.runs[last].persona = Some(crate::delegation::lineage(&state.runtime.runs[5]));
    state.runtime.runs[7].status = RunStatus::Running;
    scratch.save(&state); // fills the saved bytes
    assert!(blocks(&scratch).is_empty());
    scratch.save(&state); // moves whole blocks of 64
    let archived = state.runtime.runs.iter().filter(|run| run.archived()).count();
    assert_eq!(archived, 192, "three blocks of 64 out of 197 candidates");
    assert_eq!(saved_runs(&scratch), state.runtime.runs.len() - archived);
    assert_eq!(
        blocks(&scratch),
        ["000000000000-07.jsonl", "000000000128-06.jsonl"],
        "aligned like lens::cut blocks"
    );
    for index in [3, 5, 7] {
        assert!(!state.runtime.runs[index].archived(), "run {index} stays hot");
    }
    let before = runs_json(&state);
    let restored = scratch.restart();
    assert_eq!(runs_json(&restored), before, "every run, in creation order");
    let id = &state.runtime.runs[10].id;
    assert!(restored.runtime.runs.iter().any(|run| &run.id == id && run.archived()));
    assert!(same(&restored, &state));
}

/// Changing an archived run (for example a continuation recording itself on
/// it) brings it back into the state, where it shadows its block copy.
#[test]
fn changing_an_archived_run_unarchives_it() {
    let scratch = Scratch::new("unarchive");
    let mut state = with_history(archive::RECENT + 64);
    scratch.save(&state);
    scratch.save(&state);
    assert!(state.runtime.runs[0].archived());
    let hot = saved_runs(&scratch);
    state.runtime.runs[0].status = RunStatus::Continued;
    assert!(!state.runtime.runs[0].archived(), "a mutable borrow un-archives");
    scratch.save(&state);
    assert_eq!(saved_runs(&scratch), hot + 1);
    let restored = scratch.restart();
    assert_eq!(
        restored.runtime.runs[0].status,
        RunStatus::Continued,
        "the state's copy wins over the block's"
    );
    assert_eq!(runs_json(&restored), runs_json(&state));
}

/// Two blocks of one level merge into their parent once the save that
/// references the parent commits; a block no save references is deleted.
#[test]
fn blocks_merge_and_unreferenced_blocks_are_removed_at_load() {
    let scratch = Scratch::new("archive-merge");
    let mut state = with_history(archive::RECENT + 64);
    scratch.save(&state);
    scratch.save(&state);
    assert_eq!(blocks(&scratch), ["000000000000-06.jsonl"]);
    for i in 0..64 {
        state
            .runtime
            .enqueue(format!("more {i}"), crate::runtime::RunKind::Task, "test");
        state.runtime.runs.last_mut().unwrap().status = RunStatus::Completed;
    }
    scratch.save(&state);
    scratch.save(&state);
    assert_eq!(
        blocks(&scratch),
        ["000000000000-07.jsonl"],
        "merged, children removed after commit"
    );
    // A crash after writing a block but before its save: nothing references it.
    fs::write(scratch.0.join("history/runs/000000000128-06.jsonl"), b"{}\n").unwrap();
    let restored = scratch.restart();
    assert_eq!(blocks(&scratch), ["000000000000-07.jsonl"]);
    assert_eq!(runs_json(&restored), runs_json(&state));
}

/// A failed save leaves no run marked archived and no new block behind.
#[test]
fn a_failed_save_undoes_the_archive_step() {
    let scratch = Scratch::new("archive-undo");
    let state = with_history(archive::RECENT + 64);
    scratch.save(&state);
    let mut archive = archived(&scratch.0);
    let pending = archive::step(&scratch.0, &state, &mut archive).unwrap();
    assert!(state.runtime.runs[0].archived() && blocks(&scratch).len() == 1);
    pending.undo(&state);
    assert!(state.runtime.runs.iter().all(|run| !run.archived()) && blocks(&scratch).is_empty());
    assert!(archived(&scratch.0).is_empty(), "the saved block list is unchanged");
}

fn referenced_blocks(scratch: &Scratch) -> Vec<String> {
    let document: serde_json::Value = serde_json::from_slice(&scratch.snapshot()).unwrap();
    let archive: archive::Archive = serde_json::from_value(document["_archive"].clone()).unwrap_or_default();
    let mut names: Vec<String> = archive
        .blocks
        .iter()
        .map(|span| format!("{:012}-{:02}.jsonl", span.start, span.level))
        .collect();
    names.sort();
    names
}
fn ids(state: &State) -> Vec<String> {
    state.runtime.runs.iter().map(|run| run.id.clone()).collect()
}

/// A clean stop compacts and a restart loads (hydrating journal text); neither
/// may bring archived runs back into the snapshot, and the block list survives
/// any number of restarts with every run still found by id.
#[test]
fn archived_runs_stay_out_of_the_snapshot_across_shutdowns_and_restarts() {
    let scratch = Scratch::new("archive-restart");
    let state = with_history(archive::RECENT + 200);
    scratch.save(&state);
    scratch.save(&state);
    let archived = state.runtime.runs.iter().filter(|run| run.archived()).count();
    assert_eq!(archived, 192);
    let all = ids(&state);
    let every = runs_json(&state);
    scratch.compact(&state); // clean shutdown
    let hot_bytes = scratch.snapshot().len();
    let mut last = state;
    for restart in 0..2 {
        let mut restored = scratch.restart();
        assert_eq!(
            ids(&restored),
            all,
            "restart {restart}: every run once, in creation order"
        );
        assert_eq!(runs_json(&restored), every);
        assert_eq!(
            restored.runtime.runs.iter().filter(|run| run.archived()).count(),
            archived,
            "restart {restart}"
        );
        assert_eq!(
            saved_runs(&scratch),
            all.len() - archived,
            "restart {restart}: only hot runs saved"
        );
        assert_eq!(
            referenced_blocks(&scratch),
            blocks(&scratch),
            "restart {restart}: every block referenced"
        );
        assert!(
            scratch.snapshot().len() <= hot_bytes + 64,
            "restart {restart}: the snapshot stays small"
        );
        // Work after the restart, then another clean stop.
        restored.runtime.notice = format!("restart {restart}");
        scratch.save(&restored);
        scratch.compact(&restored);
        assert_eq!(saved_runs(&scratch), all.len() - archived);
        assert_eq!(referenced_blocks(&scratch), blocks(&scratch));
        last = restored;
    }
    let found = last.runtime.runs.iter().find(|run| run.id == all[10]).unwrap();
    assert!(found.archived() && *found.result == "result 10");
}

/// The state a build that un-archived every run at load left behind: every run
/// in the snapshot and its blocks still referenced. Loading keeps the blocks,
/// the next saves archive the runs again (into new blocks while the old ones
/// still hold copies) and later restarts load each run once.
#[test]
fn a_snapshot_holding_archived_runs_recovers_without_losing_or_duplicating_runs() {
    let scratch = Scratch::new("archive-repair");
    let mut state = with_history(archive::RECENT + 200);
    scratch.save(&state);
    scratch.save(&state);
    let old_blocks = blocks(&scratch);
    let every = runs_json(&state);
    let all = ids(&state);
    for run in &mut state.runtime.runs {
        let _ = &mut **run; // what the old load did to every run
    }
    scratch.compact(&state);
    assert_eq!(saved_runs(&scratch), all.len(), "every run in the snapshot");
    assert_eq!(referenced_blocks(&scratch), old_blocks);
    let restored = scratch.restart();
    assert_eq!(blocks(&scratch), old_blocks, "referenced blocks are kept");
    assert_eq!(ids(&restored), all);
    scratch.save(&restored);
    scratch.save(&restored);
    assert_eq!(restored.runtime.runs.iter().filter(|run| run.archived()).count(), 192);
    scratch.compact(&restored);
    assert_eq!(saved_runs(&scratch), all.len() - 192);
    assert_eq!(referenced_blocks(&scratch), blocks(&scratch));
    for _ in 0..2 {
        let restored = scratch.restart();
        assert_eq!(ids(&restored), all, "each run once, in creation order");
        assert_eq!(runs_json(&restored), every);
        assert_eq!(saved_runs(&scratch), all.len() - 192);
    }
}

/// Stress reproduction of the former intermittent failure: other threads
/// spawning processes while recovery reopens the log. Before the journal waited
/// for its lock, `recover` failed with "Journal is open elsewhere" (a child
/// between fork and exec held the dropped handle's lock), typically within a
/// few iterations; recovered states were never mixed. The deterministic check
/// is `control_journal`'s
/// `reopening_waits_for_a_descriptor_inherited_by_a_spawning_child`.
#[test]
#[ignore = "stress: about 10 s per iteration in a debug build"]
fn crash_recovery_survives_concurrent_process_spawns() {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spawners: Vec<_> = (0..6)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = std::process::Command::new("true").status();
                }
            })
        })
        .collect();
    let rounds = std::env::var("CONTROL_STRESS_ROUNDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(5);
    let result = std::panic::catch_unwind(|| {
        for _ in 0..rounds {
            a_crash_at_every_byte_of_a_multi_entity_save_recovers_before_or_after_it();
        }
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for s in spawners {
        s.join().unwrap();
    }
    result.unwrap();
}
