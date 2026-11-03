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
    with_path(path(chat)?, f)
}
fn with_path<T>(path: PathBuf, f: impl FnOnce(&mut FileJournal) -> io::Result<T>) -> io::Result<T> {
    static JOURNALS: OnceLock<Mutex<HashMap<PathBuf, FileJournal>>> = OnceLock::new();
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
        if run.objective.at.is_none() || run.result.at.is_none() {
            moved |= archive_text(run).is_ok();
        }
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

    /// A chat whose journal no parallel test shares (clock-based ids can collide).
    fn own(state: &mut crate::State, chat: &str, name: &str) -> String {
        let id = format!("c-test-{name}");
        for session in state.chats.iter_mut().filter(|s| s.id == chat) {
            session.id = id.clone();
        }
        for run in state.runtime.runs.iter_mut().filter(|r| r.chat_id.as_deref() == Some(chat)) {
            run.chat_id = Some(id.clone());
        }
        let _ = std::fs::remove_file(path(Some(&id)).unwrap());
        id
    }

    fn event(id: &str) -> Activity {
        Activity { id: id.into(), kind: "command_execution".into(), text: "cargo test".into(), output: "ok".repeat(50), status: "completed".into() }
    }

    #[test]
    fn finished_attempts_move_to_journals_and_read_back_unchanged() {
        let mut state = crate::State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Build it", "test").unwrap();
        let chat = own(&mut state, &chat, "attempts");
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

    #[test]
    fn finished_run_text_saves_as_a_position_and_edits_save_in_full() {
        let mut state = crate::State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Write the long report", "test").unwrap();
        own(&mut state, &chat, "text");
        *state.runtime.runs[0].result = "A long finished answer. ".repeat(200);
        state.runtime.runs[0].status = "completed".into();
        assert!(archive_finished(&mut state));
        let saved = persisting(|| serde_json::to_string(&state).unwrap());
        let runs = persisting(|| serde_json::to_string(&state.runtime.runs).unwrap());
        assert!(!runs.contains("long finished answer") && !runs.contains("Write the long report"), "a save carries positions, not run text");
        assert!(serde_json::to_string(&state).unwrap().contains("long finished answer"), "views still see the full value");
        let mut loaded: crate::State = serde_json::from_str(&saved).unwrap();
        assert!(loaded.runtime.runs[0].result.is_empty(), "markers load empty");
        hydrate_text(&mut loaded).unwrap();
        assert_eq!(*loaded.runtime.runs[0].result, *state.runtime.runs[0].result);
        assert_eq!(*loaded.runtime.runs[0].objective, *state.runtime.runs[0].objective);
        let copy = loaded.runtime.runs[0].clone();
        assert!(persisting(|| serde_json::to_string(&copy).unwrap()).contains("long finished answer"), "a copy never borrows another run's journal position");
        loaded.runtime.runs[0].result.push_str(" Edited.");
        let resaved = persisting(|| serde_json::to_string(&loaded.runtime.runs).unwrap());
        assert!(resaved.contains("Edited.") && !resaved.contains("Write the long report"), "only the edited field is saved in full");
    }

    #[test]
    fn deliveries_are_emitted_once_per_status_and_retried_after_a_failed_save() {
        let mut state = crate::State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Tell me when done", "test").unwrap();
        let chat = own(&mut state, &chat, "outbox");
        let outbox = outbox_path().unwrap();
        let events = || control::outbox::read(&outbox, 0).unwrap().0.into_iter().filter(|e| e.chat.as_deref() == Some(chat.as_str())).collect::<Vec<_>>();
        emit(&mut state, |_| Ok(())).unwrap();
        assert!(events().is_empty(), "queued runs are not deliverable");
        state.runtime.runs[0].status = "waiting".into();
        emit(&mut state, |_| Err(io::Error::other("disk"))).unwrap_err();
        assert_eq!(state.runtime.runs[0].emitted, None, "a failed save re-emits next pass");
        emit(&mut state, |_| Ok(())).unwrap();
        emit(&mut state, |_| Ok(())).unwrap();
        state.runtime.runs[0].status = "completed".into();
        *state.runtime.runs[0].result = "Finished.\n\nReview workspace: x".into();
        emit(&mut state, |_| Ok(())).unwrap();
        let statuses: Vec<_> = events().iter().map(|e| (e.status.clone(), e.report.clone())).collect();
        assert_eq!(statuses.iter().filter(|(s, _)| s == "waiting").count(), 2, "at least once: the failed save's event and its retry");
        assert_eq!(statuses.last(), Some(&("completed".to_owned(), "Finished.".to_owned())), "one event per new status, without the review footer");
    }

    #[test]
    fn finishing_a_run_is_one_transaction_with_its_save() {
        let mut state = crate::State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Finish atomically", "test").unwrap();
        let chat = own(&mut state, &chat, "transaction");
        let mut attempt = Attempt::new();
        attempt.activity = vec![event("x")];
        state.runtime.runs[0].attempts.push(attempt);
        state.runtime.runs[0].status = "completed".into();
        let before = serde_json::to_value(&state.runtime.runs[0]).unwrap();
        let len = || with(Some(&chat), |j| Ok(j.len())).unwrap();
        let start = path(Some(&chat)).unwrap().exists().then(len).unwrap_or(0);
        // A failed save: the run is as it was in memory; the unreferenced line is
        // dropped by recovery from the state that was actually saved.
        let saved = persisting(|| serde_json::to_string(&state).unwrap());
        let archived = archive_run(&mut state.runtime.runs[0]).unwrap();
        assert_eq!(len(), start + 1);
        undo(&mut state.runtime.runs[0], archived);
        assert_eq!(serde_json::to_value(&state.runtime.runs[0]).unwrap(), before);
        recover(&serde_json::from_str(&saved).unwrap()).unwrap();
        assert_eq!(len(), start);
        // A crash after the append but before the save: recovery drops the line.
        archive_run(&mut state.runtime.runs[0]).unwrap();
        assert_eq!(len(), start + 1);
        let restarted: crate::State = serde_json::from_str(&saved).unwrap();
        recover(&restarted).unwrap();
        assert_eq!(len(), start, "an append whose save never happened is not history");
    }
}

/// A run field whose value can live in its chat's journal. In memory it is
/// always the full value; only a save while `persisting` writes the journal
/// position in its place. Any mutable access forgets the position, so an
/// edited value is saved in full again.
#[derive(Debug, Default)]
pub(crate) struct Spilled<T> {
    value: T,
    at: Option<u64>,
}
/// A copy is a new value: positions belong to one run in one chat's journal.
impl<T: Clone> Clone for Spilled<T> {
    fn clone(&self) -> Self {
        self.value.clone().into()
    }
}
impl<T> From<T> for Spilled<T> {
    fn from(value: T) -> Self {
        Self { value, at: None }
    }
}
impl From<&str> for Spilled<String> {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl<T> std::ops::Deref for Spilled<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> std::ops::DerefMut for Spilled<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.at = None;
        &mut self.value
    }
}
impl<T: PartialEq> PartialEq for Spilled<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl PartialEq<str> for Spilled<String> {
    fn eq(&self, other: &str) -> bool {
        self.value == other
    }
}
impl PartialEq<&str> for Spilled<String> {
    fn eq(&self, other: &&str) -> bool {
        self.value == *other
    }
}
impl std::fmt::Display for Spilled<String> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value)
    }
}

thread_local! {
    static PERSISTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// Runs `f` (a state save) so journal-backed fields write their position.
pub(crate) fn persisting<R>(f: impl FnOnce() -> R) -> R {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            PERSISTING.with(|p| p.set(self.0));
        }
    }
    let _reset = Reset(PERSISTING.with(|p| p.replace(true)));
    f()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    journal: u64,
}
impl<T: Serialize> Serialize for Spilled<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.at {
            Some(journal) if PERSISTING.with(|p| p.get()) => Marker { journal }.serialize(serializer),
            _ => self.value.serialize(serializer),
        }
    }
}
impl<'de, T: Deserialize<'de> + Default> Deserialize<'de> for Spilled<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored<T> {
            Journal(Marker),
            Value(T),
        }
        Ok(match Stored::<T>::deserialize(deserializer)? {
            Stored::Journal(Marker { journal }) => Self { value: T::default(), at: Some(journal) },
            Stored::Value(value) => Self { value, at: None },
        })
    }
}

/// The text of a finished run, kept as one journal line.
#[derive(Serialize, Deserialize)]
struct Text<'a> {
    run: Cow<'a, str>,
    objective: Cow<'a, str>,
    result: Cow<'a, str>,
    context: Cow<'a, Option<control_context::Resolved>>,
    continuation: Cow<'a, Option<crate::design::Continuation>>,
}
/// Moves a finished run's text into its chat's journal; the run keeps it in
/// memory and saves only the position until the text changes.
pub(crate) fn archive_text(run: &mut crate::runtime::Run) -> io::Result<()> {
    if !finished(&run.status) || (run.objective.at.is_some() && run.result.at.is_some()) {
        return Ok(());
    }
    let text = Text { run: Cow::Borrowed(&run.id), objective: Cow::Borrowed(&run.objective), result: Cow::Borrowed(&run.result), context: Cow::Borrowed(&run.context), continuation: Cow::Borrowed(&run.continuation) };
    let line = serde_json::to_vec(&text).map_err(io::Error::other)?;
    let at = Some(with(run.chat_id.as_deref(), |journal| journal.append(&line))?);
    (run.objective.at, run.result.at, run.context.at, run.continuation.at) = (at, at, at, at);
    Ok(())
}
/// Fills journal-backed run text back in after loading.
pub(crate) fn hydrate_text(state: &mut crate::State) -> io::Result<()> {
    for run in &mut state.runtime.runs {
        let Some(at) = run.objective.at.or(run.result.at).or(run.context.at).or(run.continuation.at) else { continue };
        let line = with(run.chat_id.as_deref(), |journal| journal.read(at))?.ok_or_else(|| io::Error::other("History entry missing"))?;
        let text: Text<'static> = serde_json::from_slice(&line).map_err(io::Error::other)?;
        if run.objective.at.is_some() { run.objective.value = text.objective.into_owned(); }
        if run.result.at.is_some() { run.result.value = text.result.into_owned(); }
        if run.context.at.is_some() { run.context.value = text.context.into_owned(); }
        if run.continuation.at.is_some() { run.continuation.value = text.continuation.into_owned(); }
    }
    Ok(())
}

/// What `archive_run` wrote, so a failed save can undo it.
pub(crate) struct Archived {
    at: u64,
    activity: Vec<Activity>,
}
/// A run that just finished: its last attempt's activity and its text as one
/// journal line, so finishing costs one sync. Half of a transaction with the
/// state save that follows: commit it with that save or `undo` it.
pub(crate) fn archive_run(run: &mut crate::runtime::Run) -> io::Result<Archived> {
    #[derive(Serialize)]
    struct Finished<'a> {
        run: &'a str,
        attempt: usize,
        activity: &'a [Activity],
        objective: &'a str,
        result: &'a str,
        context: &'a Option<control_context::Resolved>,
        continuation: &'a Option<crate::design::Continuation>,
    }
    let index = run.attempts.len().saturating_sub(1);
    let empty = Attempt::default();
    let last = run.attempts.last().filter(|a| a.journal.is_none()).unwrap_or(&empty);
    let line = serde_json::to_vec(&Finished { run: &run.id, attempt: index, activity: &last.activity, objective: &run.objective, result: &run.result, context: &run.context, continuation: &run.continuation }).map_err(io::Error::other)?;
    let at = with(run.chat_id.as_deref(), |journal| journal.append(&line))?;
    (run.objective.at, run.result.at, run.context.at, run.continuation.at) = (Some(at), Some(at), Some(at), Some(at));
    let mut activity = Vec::new();
    if let Some(last) = run.attempts.last_mut().filter(|a| a.journal.is_none() && !a.activity.is_empty()) {
        activity = std::mem::take(&mut last.activity);
        last.journal = Some(at);
    }
    Ok(Archived { at, activity })
}
/// Undoes `archive_run` in memory after its save failed, so the next save writes
/// the run in full. The journal line stays: a failed save may still have
/// published state that references it, and `recover` decides at load from what
/// was actually saved.
pub(crate) fn undo(run: &mut crate::runtime::Run, archived: Archived) {
    (run.objective.at, run.result.at, run.context.at, run.continuation.at) = (None, None, None, None);
    if let Some(last) = run.attempts.last_mut().filter(|a| a.journal == Some(archived.at)) {
        last.activity = archived.activity;
        last.journal = None;
    }
}
/// After a crash between a journal append and its state save, drops journal
/// lines the saved state never referenced: appends only grow the tail, and every
/// committed line is referenced by the save that followed it.
pub(crate) fn recover(state: &crate::State) -> io::Result<()> {
    // Keyed by file: chats without a valid id share the unscoped journal.
    let mut referenced: HashMap<PathBuf, (Option<&str>, u64)> = HashMap::new();
    for chat in &state.chats {
        referenced.entry(path(Some(&chat.id))?).or_insert((Some(&chat.id), 0));
    }
    for run in &state.runtime.runs {
        let positions = run.attempts.iter().filter_map(|a| a.journal).chain([run.objective.at, run.result.at, run.context.at, run.continuation.at].into_iter().flatten());
        let (_, end) = referenced.entry(path(run.chat_id.as_deref())?).or_insert((run.chat_id.as_deref(), 0));
        *end = positions.fold(*end, |end, at| end.max(at + 1));
    }
    for (file, (chat, end)) in referenced {
        if file.exists() {
            with(chat, |journal| if journal.len() > end { journal.truncate(end) } else { Ok(()) })?;
        }
    }
    Ok(())
}

fn outbox_path() -> io::Result<PathBuf> {
    #[cfg(test)]
    return Ok(std::env::temp_dir().join(format!("control-history-test-{}", std::process::id())).join("_outbox.jsonl"));
    #[cfg(not(test))]
    control::outbox::path().map_err(io::Error::other)
}
/// Appends a delivery event for every run that reached a new deliverable
/// status, for frontends following the outbox. Events are at least once; the
/// caller's save records what was emitted, and followers deduplicate by run
/// and status.
pub(crate) fn emit(state: &mut crate::State, save: impl FnOnce(&crate::State) -> io::Result<()>) -> io::Result<()> {
    use control::outbox::{Change, DELIVERABLE, Delivery};
    let mut emitted = Vec::new();
    for (index, run) in state.runtime.runs.iter().enumerate() {
        if !DELIVERABLE.contains(&run.status.as_str()) || run.emitted.as_deref() == Some(run.status.as_str()) {
            continue;
        }
        let change = run.attempts.last().and_then(|attempt| {
            activity(run.chat_id.as_deref(), attempt).iter().rev().find(|e| e.kind == "file_change" && !e.output.is_empty()).map(|e| Change { text: e.text.clone(), output: e.output.clone() })
        });
        let delivery = Delivery {
            run: run.id.clone(),
            source: run.source.clone(),
            chat: run.chat_id.clone(),
            actor: run.actor.clone(),
            action: run.action,
            status: run.status.clone(),
            report: run.result.split_once("\n\nReview workspace: ").map_or(run.result.as_str(), |(reply, _)| reply).to_owned(),
            question: serde_json::to_value(&run.question).unwrap_or_default(),
            change,
        };
        let line = serde_json::to_vec(&delivery).map_err(io::Error::other)?;
        with_path(outbox_path()?, |journal| journal.append(&line))?;
        emitted.push((index, run.emitted.clone()));
    }
    if emitted.is_empty() {
        return Ok(());
    }
    for (index, _) in &emitted {
        let run = &mut state.runtime.runs[*index];
        run.emitted = Some(run.status.clone());
    }
    save(state).inspect_err(|_| {
        // Re-emitted next pass; followers deduplicate by run and status.
        for (index, previous) in emitted {
            state.runtime.runs[index].emitted = previous;
        }
    })
}
