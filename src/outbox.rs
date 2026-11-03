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

/// Whole events after byte `offset`, and the offset to continue from. A torn
/// final line is left for the next read.
pub fn read(path: &Path, offset: u64) -> io::Result<(Vec<Delivery>, u64)> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(error) => return Err(error),
    };
    // A shorter file means a fresh history: start again from the beginning.
    let offset = if file.metadata()?.len() < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let complete = data.iter().rposition(|b| *b == b'\n').map_or(0, |end| end + 1);
    let events = data[..complete].split(|b| *b == b'\n').filter(|line| !line.is_empty()).filter_map(|line| serde_json::from_slice(line).ok()).collect();
    Ok((events, offset + complete as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn followers_read_whole_events_from_their_cursor() {
        let path = std::env::temp_dir().join(format!("control-outbox-{}.jsonl", std::process::id()));
        let event = |run: &str| serde_json::to_string(&Delivery { run: run.into(), source: "discord:dm".into(), chat: None, actor: Actor::LocalOperator, action: Action::Task, status: "completed".into(), report: "done".into(), question: Value::Null, change: None }).unwrap();
        std::fs::write(&path, format!("{}\n{}\n{{\"run\":\"tor", event("a"), event("b"))).unwrap();
        let (events, cursor) = read(&path, 0).unwrap();
        assert_eq!(events.iter().map(|e| e.run.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(cursor).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        writeln!(file, "{}", event("c")).unwrap();
        let (events, next) = read(&path, cursor).unwrap();
        assert_eq!((events.len(), events[0].run.as_str()), (1, "c"));
        assert_eq!(read(&path, next).unwrap().0.len(), 0);
        let _ = std::fs::remove_file(&path);
    }
}
