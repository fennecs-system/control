use crate::State;
use control_effect::sync::Changes;
use std::{
    fs,
    io::{self, Read, Write},
    sync::{Arc, OnceLock},
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
        return Err(io::Error::other(
            "State directory must be a directory, not a symlink",
        ));
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
    let mut state: State = match read_checkpoint(&directory)? {
        Some(data) => serde_json::from_slice(&data).map_err(io::Error::other)?,
        None => State::initial(),
    };
    crate::providers::load_settings(&mut state.runtime)?;
    state.runtime.execution = crate::scheduler::load_limits()?;
    // Journal lines from a save that never happened are dropped, then
    // journal-backed run text is read back before anything uses it.
    crate::history::recover(&state)?;
    crate::history::hydrate_text(&mut state)?;
    crate::turn_context::recover(&state)?;
    let mut changed = crate::knowledge::migrate(&mut state);
    changed |= state.lens_library.add_missing_defaults();
    changed |= interrupt_active(&mut state);
    if changed {
        save(&state)?;
    }
    Ok(state)
}
fn interrupt_active(state: &mut State) -> bool {
    let mut changed = false;
    for run in &mut state.runtime.runs {
        if ["running", "cancelling"].contains(&run.status.as_str()) {
            changed = true;
            run.status = "interrupted".into();
            run.result =
                "The control plane stopped during this run. Inspect the container before retrying."
                    .into();
            if let Some(attempt) = run.attempts.last_mut() {
                attempt.status = "interrupted".into();
                attempt.termination.get_or_insert(crate::attempt_control::Termination::Restarted);
                for activity in &mut attempt.activity {
                    if activity.kind == "workspace" && activity.status == "in_progress" { activity.status = "interrupted".into(); }
                }
                for job in &mut attempt.command_jobs {
                    if ["preparing","running","stopping"].contains(&job.status.as_str()) {
                        job.status="interrupted".into();
                        if let Some(activity)=attempt.activity.iter_mut().find(|a|a.id==format!("job-{}",job.id)) {activity.status="interrupted".into();}
                    }
                }
            }
            // A run a crash interrupted is finished: its text and activity leave
            // state for its journal like any other (on failure they stay).
            let _ = crate::history::archive_run(run);
            if let Some(session) = state
                .chats
                .iter_mut()
                .find(|c| Some(&c.id) == run.chat_id.as_ref())
            {
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

/// Synchronous, phase-aware checkpoint boundary. Success includes file and
/// directory sync; only success wakes state-change subscribers. This API does
/// not permit releasing the application's write lock during persistence.
pub fn checkpoint(state: &State) -> Result<(), CheckpointError> {
    let data = crate::history::persisting(|| serde_json::to_vec(state))
        .map_err(|error| CheckpointError::NotReplaced(io::Error::other(error)))?;
    let directory = crate::config::state_directory()
        .map_err(|error| CheckpointError::NotReplaced(io::Error::other(error)))?;
    write_checkpoint(&directory, &data)?;
    changes().bump();
    crate::live_updates::notify();
    Ok(())
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
            return Err(io::Error::other(
                "Published checkpoint differs from the candidate",
            ));
        }
    }
    if file.read(&mut buffer[..1])? != 0 {
        return Err(io::Error::other(
            "Published checkpoint differs from the candidate",
        ));
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
    let mut file = options
        .open(&temporary)
        .map_err(CheckpointError::NotReplaced)?;
    file.write_all(data).map_err(CheckpointError::NotReplaced)?;
    file.sync_all().map_err(CheckpointError::NotReplaced)?;
    fs::rename(temporary, directory.join("state.json")).map_err(CheckpointError::Unconfirmed)?;
    sync_directory(directory).map_err(CheckpointError::Unconfirmed)
}

#[cfg(test)]
mod tests;
