use crate::State;
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub fn initialize() -> io::Result<()> {
    fs::create_dir_all(".control")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(".control", fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn load() -> io::Result<State> {
    let path = Path::new(".control/state.json");
    if !path.exists() {
        return Ok(State::initial());
    }
    let mut state: State = serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?;
    let migrated = crate::knowledge::migrate(&mut state);
    for run in &mut state.runtime.runs {
        if run.status == "running" {
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
    if migrated {
        save(&state)?;
    }
    Ok(state)
}
pub fn save(state: &State) -> io::Result<()> {
    let data = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(".control/state.json.tmp")?;
    file.write_all(&data)?;
    file.sync_all()?;
    fs::rename(".control/state.json.tmp", ".control/state.json")
}
