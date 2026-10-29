use crate::State;
use std::{
    fs,
    io::{self, Write},
};

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
    #[cfg(target_os = "macos")]
    let flags = 0x100;
    #[cfg(not(target_os = "macos"))]
    let flags = 0x20000;
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
    let path = crate::config::state("state.json").map_err(io::Error::other)?;
    let mut state: State = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?
    } else {
        State::initial()
    };
    crate::providers::load_settings(&mut state.runtime)?;
    let mut changed = crate::knowledge::migrate(&mut state);
    changed |= interrupt_active(&mut state);
    for run in &mut state.runtime.runs {
        changed |= trim_finished(run);
    }
    if changed {
        save(&state)?;
    }
    Ok(state)
}
/// Finished runs keep their diffs and only a short command tail. Live runs keep
/// everything; this bounds state.json, which is rewritten on every checkpoint.
pub fn trim_finished(run: &mut crate::runtime::Run) -> bool {
    const KEEP: usize = 8;
    const OUTPUT: usize = 1000;
    if ["queued", "running", "cancelling", "waiting"].contains(&run.status.as_str()) {
        return false;
    }
    let mut changed = false;
    for attempt in &mut run.attempts {
        let commands = attempt.activity.iter().filter(|a| a.kind != "file_change").count();
        let mut skip = commands.saturating_sub(KEEP);
        let before = attempt.activity.len();
        attempt.activity.retain(|a| a.kind == "file_change" || if skip > 0 { skip -= 1; false } else { true });
        changed |= attempt.activity.len() != before;
        for event in attempt.activity.iter_mut().filter(|a| a.kind != "file_change" && a.output.chars().count() > OUTPUT + 20) {
            event.output = event.output.chars().take(OUTPUT).chain("\n… output trimmed".chars()).collect();
            changed = true;
        }
    }
    changed
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
            }
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
    let data = serde_json::to_vec(state).map_err(io::Error::other)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let temporary = crate::config::state("state.json.tmp").map_err(io::Error::other)?;
    let mut file = options.open(&temporary)?;
    file.write_all(&data)?;
    file.sync_all()?;
    fs::rename(
        temporary,
        crate::config::state("state.json").map_err(io::Error::other)?,
    )?;
    fs::File::open(crate::config::state_directory().map_err(io::Error::other)?)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finished_runs_keep_diffs_and_a_short_command_tail_and_pretty_files_load() {
        let mut state = State::initial();
        state.runtime.enqueue("t".into(), "task", "test");
        let run = &mut state.runtime.runs[0];
        let mut attempt = crate::usage::Attempt::new();
        attempt.activity.push(crate::usage::Activity { id: "workspace-diff".into(), kind: "file_change".into(), text: "1 file".into(), output: "x".repeat(5000), status: "completed".into() });
        for i in 0..20 {
            attempt.activity.push(crate::usage::Activity { id: i.to_string(), kind: "command_execution".into(), text: "c".into(), output: "y".repeat(3000), status: "completed".into() });
        }
        run.attempts.push(attempt);
        run.status = "running".into();
        assert!(!trim_finished(run));
        run.status = "completed".into();
        assert!(trim_finished(run));
        let activity = &run.attempts[0].activity;
        assert_eq!(activity.len(), 9);
        assert_eq!(activity[0].output.len(), 5000);
        assert_eq!(activity[1].id, "12");
        assert!(activity[1].output.chars().count() < 1100);
        assert!(!trim_finished(run));
        let pretty = serde_json::to_vec_pretty(&state).unwrap();
        let compact = serde_json::to_vec(&state).unwrap();
        assert!(compact.len() < pretty.len());
        assert_eq!(serde_json::to_vec(&serde_json::from_slice::<State>(&pretty).unwrap()).unwrap(), compact);
    }
    #[test]
    fn restart_fences_running_and_cancelling_without_replaying_or_publishing() {
        let mut state = State::initial();
        for status in ["running", "cancelling", "queued", "completed"] {
            state.runtime.enqueue(status.into(), "task", "test");
            let run = state.runtime.runs.last_mut().unwrap();
            run.status = status.into();
            let mut a = crate::usage::Attempt::new();
            a.workspace_id = Some(format!("{}-attempt-1", run.id));
            run.attempts.push(a);
        }
        assert!(interrupt_active(&mut state));
        assert_eq!(
            state
                .runtime
                .runs
                .iter()
                .map(|r| r.status.as_str())
                .collect::<Vec<_>>(),
            vec!["interrupted", "interrupted", "queued", "completed"]
        );
        assert_eq!(state.runtime.runs[1].attempts[0].status, "interrupted");
        assert!(!interrupt_active(&mut state));
        assert!(state
            .runtime
            .runs
            .iter()
            .all(|r| r.attempts[0].artifact.is_none()));
    }
}
