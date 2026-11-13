//! Delivery events for frontends: an append-only JSON-lines feed that Control
//! writes when a run finishes or waits, and that any adapter (Discord now,
//! Telegram, Matrix or Stoat later) follows with its own cursor. Adapters read
//! this feed, never Control's state or chat journals.
use crate::identity::{Action, Actor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// A run reached a status a person should hear about.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delivery {
    pub run: String,
    pub source: String,
    pub chat: Option<String>,
    pub actor: Actor,
    pub action: Action,
    pub status: String,
    /// The run's reply, without any workspace review footer.
    pub report: String,
    /// The pending design question of a waiting run.
    #[serde(default)]
    pub question: Value,
    /// The latest attempt's last edit.
    #[serde(default)]
    pub change: Option<Change>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Change {
    pub text: String,
    pub output: String,
}

/// Statuses delivered to frontends.
pub const DELIVERABLE: [&str; 6] = ["completed", "blocked", "failed", "interrupted", "waiting", "cancelled"];

pub fn path() -> Result<PathBuf, &'static str> {
    crate::config::state("history/_outbox.jsonl")
}

/// Where a follower is: just past a line it consumed, and that line's length
/// and hash, so a rewritten tail is noticed rather than misread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub offset: u64,
    len: u64,
    hash: u64,
}
fn hash(line: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

/// Whole events after `cursor`, each with the cursor just past it. If the line
/// before the cursor is no longer the one consumed (a fresh history, or an
/// append the writer rolled back), reading restarts from the beginning and
/// followers rely on their receipts. A torn final line waits for the next read.
pub fn read(path: &Path, cursor: Cursor) -> io::Result<Vec<(Delivery, Cursor)>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let intact = cursor.offset == 0 || (file.metadata()?.len() >= cursor.offset && {
        let mut last = vec![0; cursor.len as usize];
        file.seek(SeekFrom::Start(cursor.offset - cursor.len))?;
        file.read_exact(&mut last).is_ok() && hash(&last) == cursor.hash
    });
    let start = if intact { cursor.offset } else { 0 };
    file.seek(SeekFrom::Start(start))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let mut events = Vec::new();
    let mut offset = start;
    for line in data.split_inclusive(|b| *b == b'\n').filter(|line| line.ends_with(b"\n")) {
        offset += line.len() as u64;
        let next = Cursor { offset, len: line.len() as u64, hash: hash(line) };
        if let Ok(event) = serde_json::from_slice(&line[..line.len() - 1]) {
            events.push((event, next));
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests;
