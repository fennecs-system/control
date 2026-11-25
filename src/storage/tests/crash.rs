//! A process that dies anywhere in a save recovers every acknowledged save and
//! nothing that was never applied: acknowledged ⊆ recovered ⊆ applied.
use super::{log::same, *};

const CHILD: &str = "CONTROL_STATE_CRASH_CHILD";
const DIRECTORY: &str = "CONTROL_STATE_CRASH_DIRECTORY";

/// Where the child dies during the writer's second append.
#[derive(Clone, Copy, Debug)]
enum Crash {
    /// Applied in memory and queued, nothing written.
    Applied,
    /// Half of the batch written.
    Torn,
    /// The whole batch written, never synced.
    Written,
    /// Written and synced, never acknowledged.
    Synced,
}
impl Crash {
    const ALL: [Self; 4] = [Self::Applied, Self::Torn, Self::Written, Self::Synced];
    fn name(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Torn => "torn",
            Self::Written => "written",
            Self::Synced => "synced",
        }
    }
}

/// The real files, dying at `crash` on the second append.
struct Dying {
    files: Files,
    crash: Crash,
    appends: usize,
}
impl Sink for Dying {
    fn append(&mut self, lines: &[&[u8]]) -> Result<(), CheckpointError> {
        self.appends += 1;
        if self.appends < 2 {
            return self.files.append(lines);
        }
        let bytes: Vec<u8> = lines
            .iter()
            .flat_map(|line| line.iter().chain(b"\n"))
            .copied()
            .collect();
        let unsynced = |bytes: &[u8]| {
            fs::OpenOptions::new()
                .append(true)
                .open(self.files.directory.join(LOG))
                .and_then(|mut log| log.write_all(bytes))
                .unwrap()
        };
        match self.crash {
            Crash::Applied => {}
            Crash::Torn => unsynced(&bytes[..bytes.len() / 2]),
            Crash::Written => unsynced(&bytes),
            Crash::Synced => self.files.append(lines)?,
        }
        std::process::abort();
    }
    fn compact(&mut self, snapshot: &[u8], header: &[u8]) -> Result<Log, CheckpointError> {
        self.files.compact(snapshot, header)
    }
    fn probe(&mut self) -> io::Result<()> {
        self.files.probe()
    }
}

/// Save `n` of the child's fixed sequence.
fn step(state: &mut State, n: usize) {
    *state.note = format!("save {n}");
    state.messages.push(format!("message {n}"));
}
fn record(directory: &Path, name: &str, n: usize) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(name))
        .unwrap();
    writeln!(file, "{n}").unwrap();
    file.sync_all().unwrap();
}
fn recorded(directory: &Path, name: &str) -> usize {
    fs::read_to_string(directory.join(name))
        .unwrap_or_default()
        .lines()
        .map(|n| n.parse().unwrap())
        .max()
        .unwrap_or(0)
}

/// The child: one compacted save, then saves through a dying writer until it
/// dies. `released` stages several saves before waiting, as release-before-wait
/// callers do, so the dying append holds a batch.
fn child(directory: &Path, crash: Crash, released: bool) {
    let mut state = State::default();
    checkpoint_in(directory, &state).unwrap();
    {
        let mut all = published().lock().unwrap();
        let Published {
            writer,
            log_bytes,
            base,
        } = all.remove(directory).unwrap();
        drop(writer);
        let files = Files::new(directory, Some(FileJournal::open(&directory.join(LOG)).unwrap()));
        let writer = Writer::new(
            Dying {
                files,
                crash,
                appends: 0,
            },
            writer::retry_then_degrade,
        );
        all.insert(
            directory.to_owned(),
            Published {
                writer,
                log_bytes,
                base,
            },
        );
    }
    // The writer dies during its second append, well before the last round.
    for round in 0..100 {
        if released {
            let tickets: Vec<(usize, Ticket)> = (1..=3)
                .map(|i| {
                    let n = round * 3 + i;
                    step(&mut state, n);
                    record(directory, "applied", n);
                    let encoded = encode(&state, &archived(directory)).unwrap();
                    (n, stage(directory, encoded, false, Writer::submit).unwrap())
                })
                .collect();
            for (n, ticket) in tickets {
                ticket.wait().unwrap();
                record(directory, "acknowledged", n);
            }
        } else {
            step(&mut state, round + 1);
            record(directory, "applied", round + 1);
            checkpoint_in(directory, &state).unwrap();
            record(directory, "acknowledged", round + 1);
        }
    }
    panic!("the dying writer never died");
}

fn crash_and_recover(test: &str, released: bool) {
    if let (Ok(crash), Some(directory)) = (std::env::var(CHILD), std::env::var_os(DIRECTORY)) {
        let crash = Crash::ALL.into_iter().find(|c| c.name() == crash).unwrap();
        return child(Path::new(&directory), crash, released);
    }
    for crash in Crash::ALL {
        let directory = crate::test_support::scratch(&format!("control-state-crash-{}", crash.name()));
        fs::create_dir_all(&directory).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--test-threads=1", "--nocapture"])
            .env(CHILD, crash.name())
            .env(DIRECTORY, &directory)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{crash:?}: the child must die mid-save");
        let (applied, acknowledged) = (recorded(&directory, "applied"), recorded(&directory, "acknowledged"));
        let (state, _) = recover(&directory, read_checkpoint(&directory).unwrap().unwrap()).unwrap();
        archives().lock().unwrap().remove(&directory);
        let recovered = state.messages.len();
        let mut expected = State::default();
        (1..=recovered).for_each(|n| step(&mut expected, n));
        assert!(
            same(&state, &expected),
            "{crash:?}: recovered a state the child applied"
        );
        assert!(
            acknowledged <= recovered && recovered <= applied,
            "{crash:?}: acknowledged {acknowledged}, recovered {recovered}, applied {applied}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !released {
            // One record per append: the dying one is save 2.
            let durable = matches!(crash, Crash::Written | Crash::Synced);
            assert_eq!(
                (acknowledged, recovered, applied),
                (1, 1 + usize::from(durable), 2),
                "{crash:?}"
            );
        }
        fs::remove_dir_all(&directory).unwrap();
    }
}
#[test]
fn a_crash_anywhere_in_a_held_save_keeps_acknowledged_saves_and_no_unapplied_ones() {
    crash_and_recover(
        "storage::tests::crash::a_crash_anywhere_in_a_held_save_keeps_acknowledged_saves_and_no_unapplied_ones",
        false,
    );
}

#[test]
fn a_crash_anywhere_in_a_batch_of_released_saves_keeps_acknowledged_saves_and_no_unapplied_ones() {
    crash_and_recover(
        "storage::tests::crash::a_crash_anywhere_in_a_batch_of_released_saves_keeps_acknowledged_saves_and_no_unapplied_ones",
        true,
    );
}
