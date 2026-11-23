//! Finished runs far from the focus leave the hot state for immutable blocks.
//!
//! The state is a comonad focused on its newest work. Runs that are finished,
//! not resumable, not referenced by a hot run, older than the newest
//! [`RECENT`] and unchanged since their last save are positions far from that
//! focus: their saved bytes move, 64 or more at a time, into JSON-lines blocks
//! under `history/runs/`, aligned like `control_comonad::lens::cut` blocks
//! (`start` a multiple of `2^level`, never changed once written). Two adjacent
//! blocks of one level merge into their parent, so an archive of n runs has
//! O(log n) blocks. The state saves only the block list (`_archive`).
//!
//! In memory every run stays in `Runtime::runs`, so lookups by id, views and
//! recovery are unchanged; an archived run is only left out of the state's
//! encoding. Any mutable access to an archived run un-archives it
//! (`history::Cached`): it is saved in the state again and shadows its block
//! copy, which the next merge drops.
//!
//! Crash safety: a block is written to a temporary file, synced, renamed and
//! the directory synced before the save that references it; that save is the
//! commit. Blocks no committed save references are deleted at load, and blocks
//! a merge replaced are deleted only after the merge's save commits.
use crate::State;
use crate::application::runs::{Record, Run, status};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// The smallest block: 64 runs.
const BASE: u32 = 6;
/// The newest runs stay in the state whatever their status.
pub(crate) const RECENT: usize = 512;

/// Positions `start .. start + 2^level` of the archive sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Span {
    pub start: u64,
    pub level: u32,
}
impl Span {
    fn file(&self, directory: &Path) -> PathBuf {
        directory.join(format!("history/runs/{:012}-{:02}.jsonl", self.start, self.level))
    }
}
/// The block list saved in the state as `_archive`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Archive {
    /// The archive position the next block starts at.
    pub next: u64,
    pub blocks: Vec<Span>,
}
impl Archive {
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// What the state saves: the state and its archive block list.
#[derive(Serialize)]
pub(crate) struct Saved<'a> {
    #[serde(rename = "_archive", skip_serializing_if = "Archive::is_empty")]
    pub archive: &'a Archive,
    #[serde(flatten)]
    pub state: &'a State,
}

/// Runs that may leave the hot state now, in creation order.
pub(crate) fn candidates(state: &State) -> Vec<usize> {
    let runs = &state.runtime.runs;
    let recent = runs.len().saturating_sub(RECENT);
    let movable = |index: usize, run: &Run| {
        index < recent
            && !run.archived()
            && run.saved().is_some()
            && status::finished(&run.status)
            && !status::resumable(&run.status)
    };
    // Runs that stay hot keep the runs they point at hot too.
    let mut referenced = HashSet::new();
    for (_, run) in runs
        .iter()
        .enumerate()
        .filter(|(index, run)| !run.archived() && !movable(*index, run))
    {
        referenced.extend(
            run.persona
                .iter()
                .flat_map(|p| std::iter::once(&p.root).chain(&p.parent)),
        );
        referenced.extend(
            run.delegation
                .iter()
                .flat_map(|d| d.child.iter().chain(&d.continuation)),
        );
        referenced.extend(run.spawns.iter().flat_map(|batch| &batch.children));
    }
    runs.iter()
        .enumerate()
        .filter(|(index, run)| movable(*index, run) && !referenced.contains(&run.id))
        .map(|(index, _)| index)
        .collect()
}

/// An archive change waiting for the save that commits it.
#[derive(Default)]
pub(crate) struct Pending {
    marked: Vec<usize>,
    written: Vec<PathBuf>,
    replaced: Vec<PathBuf>,
}
impl Pending {
    /// The save committed: blocks a merge replaced can go.
    pub(crate) fn commit(self) {
        for file in self.replaced {
            let _ = fs::remove_file(file);
        }
    }
    /// The save failed: the runs are hot again and new blocks are unreferenced.
    pub(crate) fn undo(self, state: &State) {
        for index in self.marked {
            state.runtime.runs[index].set_archived(false);
        }
        for file in self.written {
            let _ = fs::remove_file(file);
        }
    }
}

fn write_block(directory: &Path, span: Span, lines: &[&str]) -> io::Result<PathBuf> {
    let file = span.file(directory);
    let parent = file.parent().expect("blocks live in a directory");
    fs::create_dir_all(parent)?;
    let temporary = file.with_extension("jsonl.tmp");
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    for line in lines {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.sync_all()?;
    fs::rename(&temporary, &file)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(file)
}

/// Moves whole blocks of candidates into the archive and merges equal
/// neighbours. Marks the moved runs archived; the caller saves the state and
/// then commits or undoes the returned change.
pub(crate) fn step(directory: &Path, state: &State, archive: &mut Archive) -> io::Result<Pending> {
    let mut pending = Pending::default();
    let candidates = candidates(state);
    let mut remaining = &candidates[..candidates.len() >> BASE << BASE];
    let result = (|| {
        while !remaining.is_empty() {
            let start = archive.next;
            let level = (BASE..63)
                .rev()
                .find(|&level| start.trailing_zeros() >= level && (1usize << level) <= remaining.len())
                .expect("whole base blocks remain");
            let (block, rest) = remaining.split_at(1 << level);
            let lines: Vec<&str> = block
                .iter()
                .map(|&index| state.runtime.runs[index].saved().expect("candidates are saved").get())
                .collect();
            pending
                .written
                .push(write_block(directory, Span { start, level }, &lines)?);
            for &index in block {
                state.runtime.runs[index].set_archived(true);
                pending.marked.push(index);
            }
            archive.blocks.push(Span { start, level });
            archive.next = start + (1 << level);
            remaining = rest;
            merge(directory, state, archive, &mut pending)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(pending),
        Err(error) => {
            pending.undo(state);
            Err(error)
        }
    }
}

fn merge(directory: &Path, state: &State, archive: &mut Archive, pending: &mut Pending) -> io::Result<()> {
    while let [.., left, right] = archive.blocks[..]
        && left.level == right.level
        && left.start % (2 << left.level) == 0
    {
        // Runs un-archived since (now hot again) leave the merged block.
        let archived: HashSet<&str> = state
            .runtime
            .runs
            .iter()
            .filter(|run| run.archived())
            .map(|run| run.id.as_str())
            .collect();
        // A run archived twice keeps only its newer (right-most) copy.
        let mut lines: Vec<Option<String>> = Vec::new();
        let mut at: HashMap<String, usize> = HashMap::new();
        for span in [left, right] {
            for line in fs::read_to_string(span.file(directory))?.lines() {
                #[derive(Deserialize)]
                struct Id {
                    id: String,
                }
                let id: Id = serde_json::from_str(line).map_err(io::Error::other)?;
                if archived.contains(id.id.as_str()) {
                    if let Some(older) = at.insert(id.id, lines.len()) {
                        lines[older] = None;
                    }
                    lines.push(Some(line.to_owned()));
                }
            }
        }
        let parent = Span {
            start: left.start,
            level: left.level + 1,
        };
        let lines: Vec<&str> = lines.iter().flatten().map(String::as_str).collect();
        pending.written.push(write_block(directory, parent, &lines)?);
        pending.replaced.extend([left.file(directory), right.file(directory)]);
        archive.blocks.truncate(archive.blocks.len() - 2);
        archive.blocks.push(parent);
    }
    Ok(())
}

/// Loads archived runs back beside the hot ones, in creation order, each with
/// its saved bytes and marked archived. A run also present in the state was
/// un-archived after its block was written: the state's copy wins. Unreferenced
/// block files (a crash before their save committed) are deleted.
pub(crate) fn load(directory: &Path, state: &mut State, archive: &Archive) -> io::Result<()> {
    let referenced: HashSet<PathBuf> = archive.blocks.iter().map(|span| span.file(directory)).collect();
    if let Ok(entries) = fs::read_dir(directory.join("history/runs")) {
        for entry in entries.flatten() {
            if !referenced.contains(&entry.path()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    if archive.is_empty() {
        return Ok(());
    }
    let hot: HashSet<String> = state.runtime.runs.iter().map(|run| run.id.clone()).collect();
    // A run un-archived and later archived again is in an older block too,
    // until a merge drops that copy: the newest block's copy wins.
    let mut archived: Vec<Run> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for span in &archive.blocks {
        for line in fs::read_to_string(span.file(directory))?.lines() {
            let record: Record = serde_json::from_str(line).map_err(io::Error::other)?;
            if !hot.contains(&record.id) {
                let saved = serde_json::value::RawValue::from_string(line.to_owned()).map_err(io::Error::other)?;
                let run = Run::restored(record, saved, true);
                match at.get(&run.id) {
                    Some(&index) => archived[index] = run,
                    None => {
                        at.insert(run.id.clone(), archived.len());
                        archived.push(run);
                    }
                }
            }
        }
    }
    // Re-archived runs sit at later archive positions than their creation.
    archived.sort_by(|a, b| creation(&a.id).cmp(&creation(&b.id)));
    let hot = std::mem::take(&mut state.runtime.runs);
    state.runtime.runs = merge_by_creation(archived, hot);
    Ok(())
}

/// Run ids start with their creation time in fixed-width hex, then a counter.
fn creation(id: &str) -> (&str, u64) {
    let (time, counter) = id.split_once('-').unwrap_or((id, ""));
    (time, counter.parse().unwrap_or(0))
}
fn merge_by_creation(archived: Vec<Run>, hot: Vec<Run>) -> Vec<Run> {
    let mut merged = Vec::with_capacity(archived.len() + hot.len());
    let (mut archived, mut hot) = (archived.into_iter().peekable(), hot.into_iter().peekable());
    loop {
        let take_archived = match (archived.peek(), hot.peek()) {
            (Some(a), Some(h)) => creation(&a.id) <= creation(&h.id),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return merged,
        };
        merged.extend(if take_archived { archived.next() } else { hot.next() });
    }
}
