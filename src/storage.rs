pub(crate) mod archive;

use crate::State;
use crate::config::{STATE_LOG as LOG, state_schema as schema};
use crate::runtime::RunStatus;
use control_effect::sync::Changes;
pub use control_journal::writer::Health;
use control_journal::{
    FileJournal, Journal,
    lens::{self, Schema},
    snapshot::{self, Part, Tree},
    writer::{self, Durable, Failure, Log, Mark, Record, Sink, Submit, Ticket, Unsaved, Writer},
};
use std::{
    collections::{HashMap, hash_map::Entry},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

/// Bumped after every durable checkpoint so supervisors can wait for state
/// changes instead of polling.
pub fn changes() -> &'static Arc<Changes> {
    static CHANGES: OnceLock<Arc<Changes>> = OnceLock::new();
    CHANGES.get_or_init(Changes::new)
}

pub fn initialize() -> io::Result<()> {
    let directory = crate::config::state_directory().map_err(io::Error::other)?;
    fs::create_dir_all(&directory)?;
    if !fs::symlink_metadata(&directory)?.is_dir() {
        return Err(io::Error::other("State directory must be a directory, not a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let identity = directory.join("node-id");
    if !identity.exists() {
        use std::os::unix::fs::OpenOptionsExt;
        let mut bytes = [0; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
            .map_err(|_| io::Error::other("Could not generate node identity"))?;
        let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(identity)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::File::open(&directory)?.sync_all()?;
    }
    crate::config::node_id().map_err(io::Error::other)?;
    Ok(())
}
pub fn lock() -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = libc::O_NOFOLLOW;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(flags)
        .open(crate::config::state("state.lock").map_err(io::Error::other)?)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Invalid state lock"));
    }
    file.try_lock()
        .map_err(|_| io::Error::other("Another Control server is using this state directory"))?;
    Ok(file)
}
pub fn load() -> io::Result<State> {
    let directory = crate::config::state_directory().map_err(io::Error::other)?;
    // A previous process may have stopped with an unconfirmed rename. Confirm
    // the file and directory before recovering journals or dispatching anything.
    let (mut state, published) = match read_checkpoint(&directory)? {
        Some(data) => recover(&directory, data)?,
        None => (State::initial(), None),
    };
    let renamed = crate::providers::load_settings(&mut state.runtime)?;
    let mut changed = crate::providers::repoint(&mut state.chats, &renamed);
    if !renamed.is_empty() {
        // Persist the merged library so model.json shows one entry per model.
        crate::providers::save_settings(&state.runtime)?;
    }
    state.runtime.accounts = crate::providers::accounts::read()?;
    state.runtime.execution = crate::scheduler::load_limits()?;
    // Journal lines from a save that never happened are dropped, then
    // journal-backed run text is read back before anything uses it.
    crate::history::recover(&state)?;
    crate::history::hydrate_text(&mut state)?;
    crate::turn_context::recover(&state)?;
    changed |= crate::knowledge::migrate(&mut state);
    changed |= state.lens_library.merge_defaults();
    changed |= state.personas.merge_defaults();
    changed |= interrupt_active(&mut state);
    // Without a current snapshot to adopt as the diff base, the first save
    // compacts: replayed logs, older schemas and migrations all end here.
    let adopted = published.is_some_and(|published| adopt(&directory, published));
    if changed || !adopted {
        save(&mut state)?;
    }
    Ok(state)
}

/// A log may grow to the snapshot's size (at least this) before compaction:
/// replay stays bounded by twice the snapshot, and compaction at most doubles
/// the bytes the log itself wrote.
const LOG_MINIMUM: usize = 4 << 20;

/// The published state this process extends: the writer that owns its files
/// (the log exclusively locked) and the encoding the next diff starts from.
struct Published {
    writer: Writer,
    log_bytes: usize,
    base: Option<Tree>,
}
/// Keyed by state directory: one in a server, several in tests.
fn published() -> &'static Mutex<HashMap<PathBuf, Published>> {
    static PUBLISHED: OnceLock<Mutex<HashMap<PathBuf, Published>>> = OnceLock::new();
    PUBLISHED.get_or_init(Mutex::default)
}

/// What `recover` read, kept until `load` decides whether the snapshot can be
/// adopted as the diff base without rewriting it.
struct Recovered {
    data: Vec<u8>,
    nonce: Option<String>,
    log: FileJournal,
    /// Whether the log was this snapshot's, with nothing to replay, and the
    /// snapshot is in this build's schema.
    clean: bool,
}

/// The snapshot with its log's valid prefix replayed (translated through
/// lenses when an older or newer build wrote it). A torn or corrupt tail is
/// truncated away: those saves never returned success.
fn recover(directory: &Path, data: Vec<u8>) -> io::Result<(State, Option<Recovered>)> {
    #[derive(serde::Deserialize)]
    struct Identity {
        #[serde(rename = "_snapshot")]
        nonce: Option<String>,
        #[serde(rename = "_schema", default)]
        schema: Schema,
        #[serde(rename = "_archive", default)]
        archive: archive::Archive,
    }
    let identity: Identity = serde_json::from_slice(&data).map_err(io::Error::other)?;
    let mut log = FileJournal::open(&directory.join(LOG))?;
    let lines = (0..log.len())
        .map(|i| log.read(i))
        .collect::<io::Result<Option<Vec<_>>>>()?
        .unwrap_or_default();
    let found = identity
        .nonce
        .as_deref()
        .and_then(|nonce| snapshot::patches(lines, nonce));
    let ours = schema();
    if let Some((_, patches)) = &found {
        let committed = patches.len() as u64 + 1;
        if committed < log.len() {
            eprintln!(
                "state.log: dropping {} uncommitted line(s) after the last complete save",
                log.len() - committed
            );
            log.truncate(committed)?;
        }
        if !patches.is_empty() {
            eprintln!(
                "state.log: replaying {} save(s) made since the last compaction",
                patches.len()
            );
        }
    }
    let clean = identity.schema == ours && found.as_ref().is_some_and(|(_, patches)| patches.is_empty());
    let (mut state, archived) = if clean || (found.is_none() && identity.schema == ours) {
        (
            serde_json::from_slice(&data).map_err(io::Error::other)?,
            identity.archive,
        )
    } else {
        let mut document: serde_json::Value = serde_json::from_slice(&data).map_err(io::Error::other)?;
        snapshot::identify(&mut document);
        lens::document(&lens::route(&identity.schema, &ours)?, &mut document);
        if let Some((written, patches)) = found {
            let route = lens::route(&written, &ours)?;
            for patch in patches {
                snapshot::apply(
                    &mut document,
                    patch.into_iter().filter_map(|edit| lens::edit(&route, edit)).collect(),
                )?;
            }
        }
        let archived = document
            .get("_archive")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(io::Error::other)?
            .unwrap_or_default();
        (serde_json::from_value(document).map_err(io::Error::other)?, archived)
    };
    // Archived runs come back beside the hot ones before anything reads runs.
    archive::load(directory, &mut state, &archived)?;
    archives()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(directory.to_owned(), archived.into());
    Ok((
        state,
        Some(Recovered {
            data,
            nonce: identity.nonce,
            log,
            clean,
        }),
    ))
}

/// Adopts the published snapshot as the diff base, so a clean restart writes
/// nothing. The base is indexed from the file's bytes, not re-encoded: a diff
/// against it turns exactly that document into the next state, whether or not
/// this build would have written the same bytes for it.
fn adopt(directory: &Path, recovered: Recovered) -> bool {
    let Recovered {
        data,
        nonce: Some(nonce),
        log,
        clean: true,
    } = recovered
    else {
        return false;
    };
    let schema = serde_json::to_value(schema()).expect("schemas serialize");
    let Some(encoded) = snapshot::base(&data, &nonce, &schema) else {
        return false;
    };
    let log_bytes = (0..log.len())
        .filter_map(|i| log.read(i).ok().flatten())
        .map(|line| line.len() + 1)
        .sum();
    let writer = Writer::new(Files::new(directory, Some(log)), writer::retry_then_degrade);
    published().lock().unwrap_or_else(PoisonError::into_inner).insert(
        directory.to_owned(),
        Published {
            writer,
            log_bytes,
            base: Some(Tree::leaf(encoded)),
        },
    );
    true
}

/// Writes the whole state as a fresh snapshot and starts an empty log, so
/// `state.json` alone is complete. Used on shutdown: a stopped node can be
/// read (or rolled back to an older build) from the snapshot by itself.
pub fn compact(state: &State) -> io::Result<()> {
    let directory = crate::config::state_directory().map_err(io::Error::other)?;
    let encoded = encode(state, &archived(&directory)).map_err(io::Error::other)?;
    legacy_result(persist(&directory, encoded, true))?;
    changes().bump();
    crate::live_updates::notify();
    Ok(())
}

fn interrupt_active(state: &mut State) -> bool {
    let mut changed = false;
    for run in state.runtime.runs.iter_mut() {
        if run.status.occupies_slot() {
            changed = true;
            run.status = RunStatus::Interrupted;
            run.result = "The control plane stopped during this run. Inspect the container before retrying.".into();
            if let Some(attempt) = run.attempts.last_mut() {
                attempt.status = crate::usage::AttemptStatus::Interrupted;
                attempt
                    .termination
                    .get_or_insert(crate::attempt_control::Termination::Restarted);
                for activity in &mut attempt.activity {
                    if activity.kind == "workspace" && activity.status == "in_progress" {
                        activity.status = "interrupted".into();
                    }
                }
                for job in &mut attempt.command_jobs {
                    if job.status.is_active() {
                        job.status = crate::command_jobs::JobStatus::Interrupted;
                        if let Some(activity) = attempt.activity.iter_mut().find(|a| a.id == format!("job-{}", job.id))
                        {
                            activity.status = "interrupted".into();
                        }
                    }
                }
            }
            // A run a crash interrupted is finished: its text and activity leave
            // state for its journal like any other (on failure they stay).
            let _ = crate::history::archive_run(run);
            if let Some(session) = state.chats.iter_mut().find(|c| Some(&c.id) == run.chat_id.as_ref()) {
                session.revision = session.revision.saturating_add(1);
            }
        }
    }
    changed
}
pub fn save(state: &mut State) -> io::Result<()> {
    legacy_result(checkpoint(state))
}

/// A save staged under the caller's lock, waited for after releasing it.
#[must_use = "the change is durable only once `wait` returns"]
pub struct Saving(Ticket);
impl Saving {
    /// A save staged on a writer a test scripts.
    #[cfg(test)]
    pub(crate) fn staged(ticket: Ticket) -> Self {
        Self(ticket)
    }
    /// What a reply that may show this change waits on.
    pub(crate) fn mark(&self) -> Mark {
        self.0.mark()
    }
    /// Waits until the change is durable. An error says the change is not
    /// saved yet (storage is unavailable; it stays queued and is saved when
    /// storage recovers) or never will be (the writer stopped and Control is
    /// stopping with it).
    pub fn wait(self) -> Result<Durable, Unsaved> {
        match self.0.wait() {
            Ok(durable) => {
                changes().bump();
                crate::live_updates::notify();
                Ok(durable)
            }
            Err(Unsaved::Halted(halted)) => {
                crate::shutdown::request();
                Err(Unsaved::Halted(halted))
            }
            Err(not_yet) => Err(not_yet),
        }
    }
}

/// Stages `state` like [`save`] but returns before it is durable, so the
/// caller can release its lock first and then [`Saving::wait`]. Others may
/// see and extend the change before then, so it is never refused once staged:
/// if storage fails it stays queued until storage recovers. An error here
/// (storage already unavailable among them) means nothing was staged and the
/// caller rolls back as after `save`. Only the store's driver stages this
/// way (`Staging`), so every change others may see before it is durable
/// sets the reply gate (`Store::commit`). Runs are not archived here: undoing
/// that on a refusal needs the state.
pub(crate) fn submit(state: &State, _: crate::store::Staging) -> io::Result<Saving> {
    let directory = crate::config::state_directory().map_err(io::Error::other)?;
    let encoded = encode(state, &archived(&directory)).map_err(io::Error::other)?;
    legacy_result(stage(&directory, encoded, false, Writer::submit)).map(Saving)
}

fn legacy_result<T>(result: Result<T, CheckpointError>) -> io::Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(CheckpointError::NotReplaced(error)) => Err(error),
        Err(CheckpointError::Unconfirmed(error)) => {
            // Legacy callers roll back on ordinary errors. Unwinding while they
            // own the state mutex makes it unavailable; shutdown stops workers.
            // No caller may overwrite the uncertain disk state with a rollback.
            crate::shutdown::request();
            panic!("Checkpoint outcome is unconfirmed; stopping Control: {error}");
        }
    }
}

// Whether publication was attempted matters to an effect interpreter deciding
// whether rollback is safe. `Unconfirmed` requires reconciliation before retry;
// it does not mean the new state is absent. Legacy `save` callers must stop
// rather than erase an unconfirmed outcome into a rollback-safe error.
pub use control_journal::writer::Fault as CheckpointError;

/// The state's saved encoding as a tree of parts (`snapshot::Part`): each
/// top-level field, chat, room post and run keeps its encoding until it is
/// next borrowed mutably, so this costs what changed, not the state's size.
/// Debug builds and tests check every encoding against a fresh serialization
/// (and `edits` every patch against a diff of the joined documents): the
/// parts can only change the cost, never the bytes. Archived runs are left
/// out; `archive` lists their blocks.
fn encode(state: &State, archive: &Part<archive::Archive>) -> serde_json::Result<Tree> {
    let saved = archive::Saved { archive, state };
    let tree = crate::history::persisting(|| snapshot::split(&saved))?;
    #[cfg(debug_assertions)]
    {
        let fresh = crate::history::persisting(|| serde_json::to_vec(&saved))?;
        assert!(
            fresh[..] == tree.bytes()[..],
            "the state's parts differ from a fresh encoding"
        );
    }
    Ok(tree)
}

/// The edits from `base` to `tree`, reading only the parts they do not share.
fn edits(base: &Tree, tree: &Tree) -> Option<Vec<u8>> {
    let patch = snapshot::diff_trees(base, tree);
    #[cfg(debug_assertions)]
    {
        let joined = |tree: &Tree| snapshot::index(tree.bytes().into_owned()).expect("trees join to JSON");
        assert!(
            patch == snapshot::diff(&joined(base), &joined(tree)),
            "the patch differs from a diff of the whole documents"
        );
    }
    patch
}

/// The archive blocks the state in `directory` references, by directory like
/// `published` but never dropped on a failed save: the runs marked archived in
/// memory are only in those blocks.
fn archives() -> &'static Mutex<HashMap<PathBuf, Part<archive::Archive>>> {
    static ARCHIVES: OnceLock<Mutex<HashMap<PathBuf, Part<archive::Archive>>>> = OnceLock::new();
    ARCHIVES.get_or_init(Mutex::default)
}
/// A copy that shares the kept list's encoding (made here, once per change),
/// so a save that drops its copy does not leave the next to encode it again.
fn archived(directory: &Path) -> Part<archive::Archive> {
    archives()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(directory)
        .map(|archive| {
            let _ = snapshot::split(archive);
            archive.clone()
        })
        .unwrap_or_default()
}

/// Per state directory, the runtime's encoding when the last archive step
/// found nothing to move. What a step moves depends only on the runtime, so
/// while the runtime keeps that encoding the step is skipped instead of
/// scanning every run again.
fn settled() -> &'static Mutex<HashMap<PathBuf, Tree>> {
    static SETTLED: OnceLock<Mutex<HashMap<PathBuf, Tree>>> = OnceLock::new();
    SETTLED.get_or_init(Mutex::default)
}
fn settled_in(directory: &Path, runtime: &Tree) -> bool {
    settled()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(directory)
        .is_some_and(|settled| settled.same(runtime))
}

/// Synchronous, phase-aware checkpoint boundary. Success includes file and
/// directory sync; only success wakes state-change subscribers. The caller
/// waits under its lock (writing itself when nobody else is); [`submit`] is
/// the variant that waits after releasing it.
///
/// A checkpoint persists only what changed since the last one: one line of
/// `state.log`, appended and synced, holding the patch from the previous
/// encoding to this one (nothing at all when they are equal). When the log
/// outgrows the snapshot, or no base is known yet, it compacts instead: a full
/// atomic `state.json` replacement followed by a fresh log. Either way the
/// change is durable before this returns, so receipts and idempotency records
/// keep their guarantees.
///
/// Finished runs far from the focus move to archive blocks first
/// (`storage::archive`); the blocks are durable before this save, which
/// commits them, references them.
pub fn checkpoint(state: &mut State) -> Result<(), CheckpointError> {
    let directory =
        crate::config::state_directory().map_err(|error| CheckpointError::NotReplaced(io::Error::other(error)))?;
    checkpoint_in(&directory, state)?;
    changes().bump();
    crate::live_updates::notify();
    Ok(())
}
fn checkpoint_in(directory: &Path, state: &mut State) -> Result<(), CheckpointError> {
    // Refused before the archive step moves anything; the caller rolls back.
    if let Health::Degraded(unavailable) = health_in(directory) {
        return Err(CheckpointError::NotReplaced(unavailable.into()));
    }
    let directory = directory.to_owned();
    let mut archive = archived(&directory);
    let before = state.runtime.saved().cloned();
    let pending = if before.as_ref().is_some_and(|runtime| settled_in(&directory, runtime)) {
        archive::Pending::default()
    } else {
        archive::step(&directory, state, &mut archive).map_err(CheckpointError::NotReplaced)?
    };
    let saved = encode(state, &archive)
        .map_err(|error| CheckpointError::NotReplaced(io::Error::other(error)))
        .and_then(|encoded| persist(&directory, encoded, false));
    match saved {
        Ok(()) => {
            archives()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(directory.clone(), archive);
            // Nothing moved, and every hot run was saved when the step looked.
            if let Some(runtime) = before.filter(|before| state.runtime.saved().is_some_and(|now| now.same(before))) {
                settled()
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(directory, runtime);
            }
            pending.commit();
        }
        Err(CheckpointError::NotReplaced(error)) => {
            pending.undo(state);
            return Err(CheckpointError::NotReplaced(error));
        }
        // The save may be published: keep its blocks; restart decides.
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Persists `encoded` in `directory` and waits for it under the caller's lock,
/// which lets a refused save roll back.
fn persist(directory: &Path, encoded: Tree, compact: bool) -> Result<(), CheckpointError> {
    match stage(directory, encoded, compact, Writer::submit_held)?.wait() {
        Ok(_) => Ok(()),
        Err(Failure::Refused(error)) => Err(CheckpointError::NotReplaced(error)),
        Err(Failure::Halted(halted)) => Err(CheckpointError::Unconfirmed(halted.into())),
    }
}

/// Whether this node's state takes saves now: ready, degraded (saves are
/// refused with the cause until storage recovers; reads go on) or halted. A
/// pure read for status pages and for refusing a change before applying it.
pub fn health() -> Health {
    crate::config::state_directory().map_or(Health::Ready, |directory| health_in(&directory))
}
fn health_in(directory: &Path) -> Health {
    published()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(directory)
        .map_or(Health::Ready, |current| current.writer.health())
}

/// A record the writer did not queue, as the legacy phases: unavailable
/// storage is an ordinary error the caller rolls back; a halted writer is an
/// unconfirmed checkpoint.
fn refused(refusal: Submit) -> CheckpointError {
    match refusal {
        Submit::Unavailable(unavailable) => CheckpointError::NotReplaced(unavailable.into()),
        Submit::Halted(halted) => CheckpointError::Unconfirmed(halted.into()),
        Submit::Stale => CheckpointError::NotReplaced(io::Error::other("a compaction is never stale")),
    }
}

/// Hands `encoded` to `directory`'s writer with `submit`: nothing new when
/// unchanged, else one patch line, else (when `compact`, with no base yet,
/// once the log outgrows the snapshot, or when the disk lost the base) a
/// compaction. The base advances here, under the caller's lock, so the next
/// save diffs against this one whether or not it is durable yet.
fn stage<T>(
    directory: &Path,
    encoded: Tree,
    compact: bool,
    submit: fn(&Writer, Option<Record>) -> Result<T, Submit>,
) -> Result<T, CheckpointError> {
    let mut all = published().lock().unwrap_or_else(PoisonError::into_inner);
    if !compact
        && let Some(current) = all.get_mut(directory)
        && let Some(base) = &current.base
    {
        let patch = edits(base, &encoded);
        let outgrown = patch
            .as_ref()
            .is_some_and(|patch| current.log_bytes + patch.len() >= base.len().max(LOG_MINIMUM));
        if !outgrown {
            let line = patch.map(|patch| snapshot::commit(&patch));
            let bytes = line.as_ref().map_or(0, |line| line.len() + 1);
            match submit(&current.writer, line.map(Record::Line)) {
                Ok(staged) => {
                    current.log_bytes += bytes;
                    current.base = Some(encoded);
                    return Ok(staged);
                }
                Err(Submit::Stale) => {}
                Err(refusal) => return Err(refused(refusal)),
            }
        }
    }
    // Compaction: the full snapshot, then an empty log naming it. A crash
    // between the two leaves the old log, whose header names the old
    // snapshot, so it is ignored: the new snapshot contains it.
    let nonce = snapshot::nonce();
    let schema = serde_json::to_value(schema()).expect("schemas serialize");
    let header = snapshot::header(&nonce, &schema);
    let log_bytes = header.len() + 1;
    let record = Record::Compact {
        snapshot: snapshot::snapshot(&encoded.bytes(), &nonce, &schema),
        header,
    };
    let current = match all.entry(directory.to_owned()) {
        Entry::Occupied(current) => current.into_mut(),
        Entry::Vacant(vacant) => vacant.insert(Published {
            writer: Writer::new(Files::new(directory, None), writer::retry_then_degrade),
            log_bytes: 0,
            base: None,
        }),
    };
    let staged = submit(&current.writer, Some(record)).map_err(refused)?;
    current.log_bytes = log_bytes;
    current.base = Some(encoded);
    Ok(staged)
}

/// The files a writer owns: `state.json`, replaced whole by a compaction, and
/// `state.log`, appended to in between.
struct Files {
    directory: PathBuf,
    log: Option<FileJournal>,
    /// The header the log must restart with before its next line, when a
    /// compaction could not restart it.
    header: Option<Vec<u8>>,
}
impl Files {
    fn new(directory: &Path, log: Option<FileJournal>) -> Self {
        Self {
            directory: directory.to_owned(),
            log,
            header: None,
        }
    }
    /// The log, opened and restarted first where an earlier step failed.
    fn log(&mut self) -> io::Result<&mut FileJournal> {
        // Behind a new snapshot nothing in the log matters: a fresh handle
        // repairs a tail a failed restart left unknown. The old handle goes
        // first, or the reopen would wait for its lock.
        if self.header.is_some() && self.log.as_ref().is_some_and(FileJournal::poisoned) {
            self.log = None;
        }
        let log = match self.log.take() {
            Some(log) => log,
            None => FileJournal::open(&self.directory.join(LOG))?,
        };
        let log = self.log.insert(log);
        if let Some(header) = &self.header {
            log.truncate(0).and_then(|()| log.append(header))?;
            self.header = None;
        }
        Ok(log)
    }
}
impl Sink for Files {
    fn append(&mut self, lines: &[&[u8]]) -> Result<(), CheckpointError> {
        let log = self.log().map_err(CheckpointError::NotReplaced)?;
        log.append_batch(lines).map(drop).map_err(|error| {
            if log.poisoned() {
                CheckpointError::Unconfirmed(error)
            } else {
                CheckpointError::NotReplaced(error)
            }
        })
    }
    fn compact(&mut self, snapshot: &[u8], header: &[u8]) -> Result<Log, CheckpointError> {
        write_checkpoint(&self.directory, snapshot)?;
        // The snapshot is durable; a log that cannot restart only costs the
        // next save another compaction (the writer says so on stderr).
        self.header = Some(header.to_vec());
        Ok(self.log().map_or_else(Log::Broken, |_| Log::Restarted))
    }
    /// A small synced file beside the state (a full disk refuses it), then the
    /// log, restarted if a compaction left that undone.
    fn probe(&mut self) -> io::Result<()> {
        let path = self.directory.join("state.probe");
        let written = fs::File::create(&path).and_then(|mut file| {
            file.write_all(&[0; 4096])?;
            file.sync_all()
        });
        let _ = fs::remove_file(&path);
        written.and_then(|()| self.log().map(drop))
    }
}
fn write_checkpoint(directory: &std::path::Path, data: &[u8]) -> Result<(), CheckpointError> {
    commit_checkpoint(directory, data, sync_directory)
}

fn sync_directory(directory: &std::path::Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

fn open_checkpoint(directory: &std::path::Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("state.json"))?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Checkpoint is not a regular file"));
    }
    Ok(file)
}

fn read_checkpoint(directory: &std::path::Path) -> io::Result<Option<Vec<u8>>> {
    let mut file = match open_checkpoint(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let size = usize::try_from(file.metadata()?.len()).map_err(io::Error::other)?;
    let mut data = Vec::with_capacity(size);
    file.read_to_end(&mut data)?;
    file.sync_all()?;
    sync_directory(directory)?;
    Ok(Some(data))
}

/// Reconcile only the exact intended bytes, under the existing exclusive writer
/// lock. A bounded buffer avoids cloning a full-state checkpoint on the rare
/// error path. A mismatch is not evidence that rollback is safe.
fn confirm_checkpoint(
    directory: &std::path::Path,
    data: &[u8],
    sync: impl FnOnce(&std::path::Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut file = open_checkpoint(directory)?;
    let mut buffer = [0; 8192];
    for expected in data.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..expected.len()])?;
        if &buffer[..expected.len()] != expected {
            return Err(io::Error::other("Published checkpoint differs from the candidate"));
        }
    }
    if file.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::other("Published checkpoint differs from the candidate"));
    }
    file.sync_all()?;
    sync(directory)
}

fn commit_checkpoint(
    directory: &std::path::Path,
    data: &[u8],
    mut sync: impl FnMut(&std::path::Path) -> io::Result<()>,
) -> Result<(), CheckpointError> {
    match write_checkpoint_with_sync(directory, data, &mut sync) {
        Err(CheckpointError::Unconfirmed(_)) => {
            confirm_checkpoint(directory, data, sync).map_err(CheckpointError::Unconfirmed)
        }
        result => result,
    }
}

fn write_checkpoint_with_sync(
    directory: &std::path::Path,
    data: &[u8],
    sync_directory: impl FnOnce(&std::path::Path) -> io::Result<()>,
) -> Result<(), CheckpointError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let temporary = directory.join("state.json.tmp");
    let mut file = options.open(&temporary).map_err(CheckpointError::NotReplaced)?;
    file.write_all(data).map_err(CheckpointError::NotReplaced)?;
    file.sync_all().map_err(CheckpointError::NotReplaced)?;
    fs::rename(temporary, directory.join("state.json")).map_err(CheckpointError::Unconfirmed)?;
    sync_directory(directory).map_err(CheckpointError::Unconfirmed)
}

#[cfg(test)]
mod tests;
