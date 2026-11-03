//! Finished history leaves `state.json` for per-chat journals (docs/HISTORY.md).
//!
//! When an attempt finishes, its tool activity becomes one line of the chat's
//! journal, `history/<chat>.jsonl`, and the attempt keeps only that line's
//! position. Saves then cost live work, not every attempt ever made.
use crate::usage::{Activity, Attempt};
use control_journal::{FileJournal, Journal};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

/// Open journals; each file is scanned once per process and stays locked to it.
const OPEN: usize = 64;
/// Journal for runs outside any chat.
const UNSCOPED: &str = "_runs";

#[derive(Serialize, Deserialize)]
struct Entry<'a> {
    run: Cow<'a, str>,
    attempt: usize,
    activity: Cow<'a, [Activity]>,
}

pub(crate) fn path(chat: Option<&str>) -> io::Result<PathBuf> {
    let name = chat.filter(|id| crate::chat::valid_id(id)).unwrap_or(UNSCOPED);
    // Tests never touch the node's real history.
    #[cfg(test)]
    return Ok(std::env::temp_dir().join(format!("control-history-test-{}", std::process::id())).join(format!("{name}.jsonl")));
    #[cfg(not(test))]
    crate::config::state(&format!("history/{name}.jsonl")).map_err(io::Error::other)
}

fn with<T>(chat: Option<&str>, f: impl FnOnce(&mut FileJournal) -> io::Result<T>) -> io::Result<T> {
    static JOURNALS: OnceLock<Mutex<HashMap<PathBuf, FileJournal>>> = OnceLock::new();
    let path = path(chat)?;
    let mut open = JOURNALS.get_or_init(Mutex::default).lock().map_err(|_| io::Error::other("History unavailable"))?;
    if !open.contains_key(&path) {
        if open.len() >= OPEN {
            open.clear();
        }
        std::fs::create_dir_all(path.parent().expect("history files live in a directory"))?;
        let journal = FileJournal::open(&path)?;
        open.insert(path.clone(), journal);
    }
    f(open.get_mut(&path).expect("just opened"))
}

/// Moves a finished attempt's activity into its chat's journal. On failure the
/// activity stays in state, so nothing is lost.
pub(crate) fn archive(chat: Option<&str>, run: &str, index: usize, attempt: &mut Attempt) -> io::Result<()> {
    if attempt.activity.is_empty() || attempt.journal.is_some() {
        return Ok(());
    }
    let entry = Entry { run: run.into(), attempt: index, activity: Cow::Borrowed(&attempt.activity) };
    let line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
    attempt.journal = Some(with(chat, |journal| journal.append(&line))?);
    attempt.activity.clear();
    Ok(())
}

/// An attempt's activity, from state while it runs or from its journal after.
pub(crate) fn activity<'a>(chat: Option<&str>, attempt: &'a Attempt) -> Cow<'a, [Activity]> {
    match attempt.journal {
        None => Cow::Borrowed(&attempt.activity),
        Some(pos) => Cow::Owned(read(chat, pos).unwrap_or_default()),
    }
}
fn read(chat: Option<&str>, pos: u64) -> io::Result<Vec<Activity>> {
    let line = with(chat, |journal| journal.read(pos))?.ok_or_else(|| io::Error::other("History entry missing"))?;
    Ok(serde_json::from_slice::<Entry<'static>>(&line).map_err(io::Error::other)?.activity.into_owned())
}

/// Attempts with their activity filled in, for views that show everything.
pub(crate) fn hydrated(chat: Option<&str>, attempts: &[Attempt]) -> Vec<Attempt> {
    attempts.iter().map(|attempt| {
        let mut full = attempt.clone();
        if attempt.journal.is_some() {
            full.activity = activity(chat, attempt).into_owned();
            full.journal = None;
        }
        full
    }).collect()
}

/// Archives every finished run's attempts, e.g. after loading older state or
/// recovering runs a crash interrupted. Returns whether anything moved.
pub(crate) fn archive_finished(state: &mut crate::State) -> bool {
    let mut moved = false;
    for run in state.runtime.runs.iter_mut().filter(|run| finished(&run.status)) {
        let (chat, id) = (run.chat_id.clone(), run.id.clone());
        for (index, attempt) in run.attempts.iter_mut().enumerate() {
            if !attempt.activity.is_empty() && attempt.journal.is_none() {
                moved |= archive(chat.as_deref(), &id, index, attempt).is_ok();
            }
        }
    }
    moved
}
/// Statuses after which an attempt's activity no longer changes.
pub(crate) fn finished(status: &str) -> bool {
    !matches!(status, "queued" | "running" | "cancelling" | "waiting")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str) -> Activity {
        Activity { id: id.into(), kind: "command_execution".into(), text: "cargo test".into(), output: "ok".repeat(50), status: "completed".into() }
    }

    #[test]
    fn finished_attempts_move_to_journals_and_read_back_unchanged() {
        let mut state = crate::State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Build it", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        let mut attempt = Attempt::new();
        attempt.activity = vec![event("a"), event("b")];
        run.attempts.push(attempt);
        run.status = "running".into();
        assert!(!archive_finished(&mut state), "live attempts stay in state");
        state.runtime.runs[0].status = "completed".into();
        assert!(archive_finished(&mut state));
        let attempt = &state.runtime.runs[0].attempts[0];
        assert!(attempt.activity.is_empty() && attempt.journal.is_some());
        let saved = serde_json::to_string(&state).unwrap();
        assert!(!saved.contains("cargo test") && saved.contains("\"journal\""), "state carries only the position");
        let read = activity(Some(&chat), attempt);
        assert_eq!(read.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(hydrated(Some(&chat), &state.runtime.runs[0].attempts)[0].activity.len(), 2);
        assert!(!archive_finished(&mut state), "archiving is idempotent");
        let line = std::fs::read_to_string(path(Some(&chat)).unwrap()).unwrap();
        let entry: serde_json::Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
        assert_eq!((entry["run"].as_str(), entry["attempt"].as_u64()), (Some(state.runtime.runs[0].id.as_str()), Some(0)), "inspectable JSON lines");
    }
}
