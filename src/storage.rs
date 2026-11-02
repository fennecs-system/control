use crate::State;
use control_effect::sync::Changes;
use std::{
    fs,
    io::{self, Write},
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
    let path = crate::config::state("state.json").map_err(io::Error::other)?;
    let mut state: State = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?
    } else {
        State::initial()
    };
    crate::providers::load_settings(&mut state.runtime)?;
    state.runtime.execution = crate::scheduler::load_limits()?;
    let mut changed = crate::knowledge::migrate(&mut state);
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
    write_checkpoint(&crate::config::state_directory().map_err(io::Error::other)?, &data)?;
    changes().bump();
    Ok(())
}
fn write_checkpoint(directory: &std::path::Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let temporary = directory.join("state.json.tmp");
    let mut file = options.open(&temporary)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(temporary, directory.join("state.json"))?;
    fs::File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use crate::runtime::RunKind;
    use super::*;
    #[test]
    fn finished_diagnostics_survive_compact_and_pretty_serialization() {
        let mut state = State::initial();
        state.runtime.enqueue("t".into(), RunKind::Task, "test");
        let run = &mut state.runtime.runs[0];
        let mut attempt = crate::usage::Attempt::new();
        attempt.activity.push(crate::usage::Activity { id: "workspace-diff".into(), kind: "file_change".into(), text: "1 file".into(), output: "x".repeat(5000), status: "completed".into() });
        for i in 0..20 {
            attempt.activity.push(crate::usage::Activity { id: i.to_string(), kind: "command_execution".into(), text: "c".into(), output: "y".repeat(3000), status: "completed".into() });
        }
        run.attempts.push(attempt);
        run.status = "completed".into();
        let pretty = serde_json::to_vec_pretty(&state).unwrap();
        let compact = serde_json::to_vec(&state).unwrap();
        assert!(compact.len() < pretty.len());
        assert_eq!(serde_json::to_vec(&serde_json::from_slice::<State>(&pretty).unwrap()).unwrap(), compact);
        let restored: State = serde_json::from_slice(&compact).unwrap();
        let activity = &restored.runtime.runs[0].attempts[0].activity;
        assert_eq!(activity.len(), 21);
        assert_eq!(activity[0].output.len(), 5000);
        assert_eq!(activity[1].id, "0");
        assert!(activity[1..].iter().all(|event| event.output.len() == 3000));
    }
    #[test]
    fn restart_fences_running_and_cancelling_without_replaying_or_publishing() {
        let mut state = State::initial();
        for status in ["running", "cancelling", "queued", "completed"] {
            state.runtime.enqueue(status.into(), RunKind::Task, "test");
            let run = state.runtime.runs.last_mut().unwrap();
            run.status = status.into();
            let mut a = crate::usage::Attempt::new();
            a.workspace_id = Some(format!("{}-attempt-1", run.id));
            a.activity.push(crate::usage::Activity {id:"project-prepare".into(),kind:"workspace".into(),text:"Preparing workspace".into(),output:String::new(),status:"in_progress".into()});
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
        assert_eq!(state.runtime.runs[1].attempts[0].activity[0].status, "interrupted");
        assert_eq!(state.runtime.runs[2].attempts[0].activity[0].status, "in_progress");
        assert!(!interrupt_active(&mut state));
        assert!(state
            .runtime
            .runs
            .iter()
            .all(|r| r.attempts[0].artifact.is_none()));
    }
    #[test]
    #[ignore = "manual benchmark: state lock held by a save at 1/3/10 MB, idle and under fsync load; no containers or models"]
    fn profile_checkpoint_by_state_size() {
        use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
        use std::time::{Duration, Instant};
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
        struct Background(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);
        impl Drop for Background {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
                let failed = self.1.drain(..).map(std::thread::JoinHandle::join).collect::<Vec<_>>().iter().any(Result::is_err);
                assert!(!failed || std::thread::panicking(), "benchmark background thread failed");
            }
        }
        let root = Cleanup(std::env::temp_dir().join(format!("control-checkpoint-{}", std::process::id())));
        let load_root = root.0.join("load");
        fs::create_dir_all(&load_root).unwrap();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let median = |mut samples: Vec<Duration>| { samples.sort(); ms(samples[samples.len() / 2]) };
        for megabytes in [1usize, 3, 10] {
            let mut state = State::initial();
            for n in 0..megabytes * 4 {
                state.runtime.enqueue(format!("run {n}"), RunKind::Task, "test");
                let mut attempt = crate::usage::Attempt::new();
                for i in 0..64 {
                    let mut output = format!("{n}-{i} ");
                    output.extend(std::iter::repeat_n('o', 4000 - output.len()));
                    attempt.activity.push(crate::usage::Activity { id: i.to_string(), kind: "command_execution".into(), text: "cargo test".into(), output, status: "completed".into() });
                }
                let run = state.runtime.runs.last_mut().unwrap();
                run.attempts.push(attempt);
                run.status = "completed".into();
            }
            let size = serde_json::to_vec(&state).unwrap().len();
            assert!((megabytes << 20) * 9 / 10 <= size && size <= (megabytes << 20) * 3 / 2, "fixture is {size} bytes");
            let state = Arc::new(Mutex::new(state));
            for loaded in [false, true] {
                let mut background = Background(Arc::new(AtomicBool::new(false)), Vec::new());
                let syncs = Arc::new(AtomicU64::new(0));
                if loaded {
                    let (stop, counter, load_root) = (background.0.clone(), syncs.clone(), load_root.clone());
                    let (ready, started) = std::sync::mpsc::sync_channel(1);
                    background.1.push(std::thread::spawn(move || {
                        let mut n = 0u64;
                        while !stop.load(Ordering::Relaxed) {
                            let mut file = fs::File::create(load_root.join((n % 4096).to_string())).unwrap();
                            file.write_all(&[b'l'; 1024]).unwrap();
                            file.sync_all().unwrap();
                            n += 1;
                            counter.store(n, Ordering::Relaxed);
                            if n == 1 { let _ = ready.send(()); }
                        }
                    }));
                    started.recv_timeout(Duration::from_secs(10)).expect("fsync load did not start");
                }
                let waits = Arc::new(Mutex::new(Vec::new()));
                {
                    let (stop, state, waits) = (background.0.clone(), state.clone(), waits.clone());
                    background.1.push(std::thread::spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            let started = Instant::now();
                            drop(state.lock().unwrap());
                            waits.lock().unwrap().push(started.elapsed());
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }));
                }
                let before = syncs.load(Ordering::Relaxed);
                let (mut serialize, mut held) = (Vec::new(), Vec::new());
                for _ in 0..7 {
                    let guard = state.lock().unwrap();
                    let started = Instant::now();
                    let data = serde_json::to_vec(&*guard).unwrap();
                    serialize.push(started.elapsed());
                    write_checkpoint(&root.0, &data).unwrap();
                    held.push(started.elapsed());
                    drop(guard);
                    std::thread::sleep(Duration::from_millis(5));
                }
                let progressed = syncs.load(Ordering::Relaxed) - before;
                assert!(!loaded || progressed > 0, "fsync load made no progress while sampling");
                drop(background);
                let worst = waits.lock().unwrap().iter().max().copied().unwrap_or_default();
                println!("state_bytes={size} fsync_load={loaded} load_syncs={progressed} serialize_ms={:.1} lock_held_ms={:.1} reader_max_wait_ms={:.1}", median(serialize), median(held), ms(worst));
            }
        }
    }
}
