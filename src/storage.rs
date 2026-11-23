pub(crate) mod archive;

use crate::State;
use crate::config::{STATE_LOG as LOG, state_schema as schema};
use crate::runtime::RunStatus;
use control_effect::sync::Changes;
use control_journal::{
    FileJournal, Journal,
    lens::{self, Schema},
    snapshot::{self, Encoded},
};
use std::{
    collections::HashMap,
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
        save(&state)?;
    }
    Ok(state)
}

/// A log may grow to the snapshot's size (at least this) before compaction:
/// replay stays bounded by twice the snapshot, and compaction at most doubles
/// the bytes the log itself wrote.
const LOG_MINIMUM: usize = 4 << 20;

/// The published state this process extends: the snapshot's name, the log
/// handle (exclusively locked) and the encoding the next diff starts from.
struct Published {
    log: FileJournal,
    log_bytes: usize,
    base: Option<Encoded>,
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
        .insert(directory.to_owned(), archived);
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
    published().lock().unwrap_or_else(PoisonError::into_inner).insert(
        directory.to_owned(),
        Published {
            log,
            log_bytes,
            base: Some(encoded),
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
                    if ["preparing", "running", "stopping"].contains(&job.status.as_str()) {
                        job.status = "interrupted".into();
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
pub fn save(state: &State) -> io::Result<()> {
    legacy_result(checkpoint(state))
}

fn legacy_result(result: Result<(), CheckpointError>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
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

/// Whether publication was attempted matters to an effect interpreter deciding
/// whether rollback is safe. `Unconfirmed` requires reconciliation before retry;
/// it does not mean the new state is absent. Legacy `save` callers must stop
/// rather than erase an unconfirmed outcome into a rollback-safe error.
#[derive(Debug)]
pub enum CheckpointError {
    /// Serialization, configuration or temporary-file preparation failed before
    /// issuing rename. The published checkpoint was not touched by this call.
    NotReplaced(io::Error),
    /// Rename was attempted but publication was not durably acknowledged.
    /// Even a rename error is conservative here (for example filesystem I/O
    /// errors); a directory-sync error can leave the replacement visible.
    Unconfirmed(io::Error),
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReplaced(error) => write!(formatter, "checkpoint not replaced: {error}"),
            Self::Unconfirmed(error) => write!(formatter, "checkpoint unconfirmed: {error}"),
        }
    }
}

impl std::error::Error for CheckpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotReplaced(error) | Self::Unconfirmed(error) => Some(error),
        }
    }
}

/// The state's saved encoding. Finished runs reuse their saved bytes
/// (`history::Cached`), so this costs what changed plus a copy of the rest.
/// Debug builds and tests check every encoding against a fresh, uncached
/// serialization: the cache can only change the cost, never the bytes.
/// Archived runs are left out; `archive` lists their blocks.
fn encode(state: &State, archive: &archive::Archive) -> serde_json::Result<Encoded> {
    let saved = archive::Saved { archive, state };
    let encoded = crate::history::persisting(|| snapshot::encode(&saved))?;
    #[cfg(debug_assertions)]
    {
        let fresh = crate::history::uncached(|| crate::history::persisting(|| serde_json::to_vec(&saved)))?;
        assert!(
            fresh == encoded.bytes(),
            "the cached state encoding differs from a fresh one"
        );
    }
    Ok(encoded)
}

/// The archive blocks the state in `directory` references, by directory like
/// `published` but never dropped on a failed save: the runs marked archived in
/// memory are only in those blocks.
fn archives() -> &'static Mutex<HashMap<PathBuf, archive::Archive>> {
    static ARCHIVES: OnceLock<Mutex<HashMap<PathBuf, archive::Archive>>> = OnceLock::new();
    ARCHIVES.get_or_init(Mutex::default)
}
fn archived(directory: &Path) -> archive::Archive {
    archives()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(directory)
        .cloned()
        .unwrap_or_default()
}

/// Synchronous, phase-aware checkpoint boundary. Success includes file and
/// directory sync; only success wakes state-change subscribers. This API does
/// not permit releasing the application's write lock during persistence.
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
pub fn checkpoint(state: &State) -> Result<(), CheckpointError> {
    let directory =
        crate::config::state_directory().map_err(|error| CheckpointError::NotReplaced(io::Error::other(error)))?;
    checkpoint_in(&directory, state)?;
    changes().bump();
    crate::live_updates::notify();
    Ok(())
}
fn checkpoint_in(directory: &Path, state: &State) -> Result<(), CheckpointError> {
    let directory = directory.to_owned();
    let mut archive = archived(&directory);
    let pending = archive::step(&directory, state, &mut archive).map_err(CheckpointError::NotReplaced)?;
    let saved = encode(state, &archive)
        .map_err(|error| CheckpointError::NotReplaced(io::Error::other(error)))
        .and_then(|encoded| persist(&directory, encoded, false));
    match saved {
        Ok(()) => {
            archives()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(directory, archive);
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

/// Persists `encoded` in `directory`: nothing when unchanged, else one patch
/// line, else (when `compact`, with no base yet, or once the log outgrows the
/// snapshot) a compaction.
fn persist(directory: &Path, encoded: Encoded, compact: bool) -> Result<(), CheckpointError> {
    let mut all = published().lock().unwrap_or_else(PoisonError::into_inner);
    // `None`: compact; `Some(None)`: nothing changed; `Some(Some(patch))`: append.
    let step = all.get(directory).filter(|_| !compact).and_then(|current| {
        let base = current.base.as_ref()?;
        match snapshot::diff(base, &encoded) {
            Some(patch) if current.log_bytes + patch.len() >= base.bytes().len().max(LOG_MINIMUM) => None,
            patch => Some(patch),
        }
    });
    match step {
        Some(None) => Ok(()),
        Some(Some(patch)) => {
            let current = all.get_mut(directory).expect("a base was found");
            let line = snapshot::commit(&patch);
            if let Err(error) = current.log.append(&line) {
                let poisoned = current.log.poisoned();
                // The next save starts over from a compaction.
                all.remove(directory);
                return Err(if poisoned {
                    CheckpointError::Unconfirmed(error)
                } else {
                    CheckpointError::NotReplaced(error)
                });
            }
            current.log_bytes += line.len() + 1;
            current.base = Some(encoded);
            Ok(())
        }
        None => {
            // Compaction: the full snapshot, then an empty log naming it. A
            // crash between the two leaves the old log, whose header names the
            // old snapshot, so it is ignored: the new snapshot contains it.
            let previous = all.remove(directory);
            let nonce = snapshot::nonce();
            let schema = serde_json::to_value(schema()).expect("schemas serialize");
            write_checkpoint(directory, &snapshot::snapshot(&encoded, &nonce, &schema))?;
            // The snapshot is durable; failing to start its log only costs the
            // next save another compaction.
            let log = previous.map_or_else(|| FileJournal::open(&directory.join(LOG)), |p| Ok(p.log));
            let header = snapshot::header(&nonce, &schema);
            if let Ok(mut log) = log
                && log.truncate(0).and_then(|()| log.append(&header)).is_ok()
            {
                all.insert(
                    directory.to_owned(),
                    Published {
                        log,
                        log_bytes: header.len() + 1,
                        base: Some(encoded),
                    },
                );
            }
            Ok(())
        }
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
