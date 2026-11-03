//! Append-only history as JSON lines: one file per conversation, one JSON
//! value per line, readable with `cat` and `jq`.
//!
//! An append writes and syncs only the new line, so recording a turn costs the
//! size of the turn, not of the history before it. Positions are line numbers
//! and never move. A journal is a [`control_comonad::Space`], so stores and
//! lenses read it directly. [`Journal`] is the pluggable seam: files today,
//! sealed or compressed segments later, memory in tests.
use control_comonad::Space;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
};

/// Ordered, append-only entries. An entry is one JSON value without newlines.
pub trait Journal {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Durably appends `entry` and returns its position.
    fn append(&mut self, entry: &[u8]) -> io::Result<u64>;
    fn read(&self, pos: u64) -> io::Result<Option<Vec<u8>>>;
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
/// Entries stay one inspectable JSON value per line.
fn check(entry: &[u8]) -> io::Result<()> {
    if entry.is_empty() || entry.contains(&b'\n') {
        return Err(invalid("A journal entry is one non-empty line"));
    }
    serde_json::from_slice::<serde::de::IgnoredAny>(entry).map(|_| ()).map_err(|_| invalid("A journal entry is a JSON value"))
}

/// Entries held in memory; for tests and transient histories.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    entries: Vec<Vec<u8>>,
}
impl Journal for Memory {
    fn len(&self) -> u64 {
        self.entries.len() as u64
    }
    fn append(&mut self, entry: &[u8]) -> io::Result<u64> {
        check(entry)?;
        self.entries.push(entry.to_vec());
        Ok(self.entries.len() as u64 - 1)
    }
    fn read(&self, pos: u64) -> io::Result<Option<Vec<u8>>> {
        Ok(usize::try_from(pos).ok().and_then(|pos| self.entries.get(pos)).cloned())
    }
}

/// A JSON-lines file with an in-memory index of line offsets, built once when
/// the file is opened. A crash can leave at most a torn final line, which
/// opening removes.
pub struct FileJournal {
    file: File,
    starts: Vec<u64>,
    end: u64,
}
impl FileJournal {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).append(true).create(true).mode(0o600).open(path)?;
        let mut starts = Vec::new();
        let (mut offset, mut line) = (0u64, 0u64);
        let mut buffer = vec![0; 1 << 16];
        file.seek(SeekFrom::Start(0))?;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            for (i, byte) in buffer[..n].iter().enumerate() {
                if *byte == b'\n' {
                    starts.push(line);
                    line = offset + i as u64 + 1;
                }
            }
            offset += n as u64;
        }
        if line != offset {
            // A torn final line from an interrupted append: drop it.
            file.set_len(line)?;
            file.sync_data()?;
        }
        Ok(Self { file, starts, end: line })
    }
}
impl Journal for FileJournal {
    fn len(&self) -> u64 {
        self.starts.len() as u64
    }
    fn append(&mut self, entry: &[u8]) -> io::Result<u64> {
        check(entry)?;
        let mut line = Vec::with_capacity(entry.len() + 1);
        line.extend_from_slice(entry);
        line.push(b'\n');
        if let Err(error) = self.file.write_all(&line).and_then(|()| self.file.sync_data()) {
            // Keep the file a whole number of lines for the next append.
            let _ = self.file.set_len(self.end);
            return Err(error);
        }
        self.starts.push(self.end);
        self.end += line.len() as u64;
        Ok(self.starts.len() as u64 - 1)
    }
    fn read(&self, pos: u64) -> io::Result<Option<Vec<u8>>> {
        let Some(&start) = usize::try_from(pos).ok().and_then(|pos| self.starts.get(pos)) else { return Ok(None) };
        let end = self.starts.get(pos as usize + 1).copied().unwrap_or(self.end) - 1;
        let mut entry = vec![0; (end - start) as usize];
        self.file.read_exact_at(&mut entry, start)?;
        Ok(Some(entry))
    }
}

/// A journal of `T`, serialized as JSON.
pub struct Typed<J, T> {
    journal: J,
    item: PhantomData<fn() -> T>,
}
impl<J: Journal, T: Serialize + DeserializeOwned> Typed<J, T> {
    pub fn new(journal: J) -> Self {
        Self { journal, item: PhantomData }
    }
    pub fn len(&self) -> u64 {
        self.journal.len()
    }
    pub fn is_empty(&self) -> bool {
        self.journal.is_empty()
    }
    pub fn append(&mut self, item: &T) -> io::Result<u64> {
        self.journal.append(&serde_json::to_vec(item).map_err(io::Error::other)?)
    }
    pub fn get(&self, pos: u64) -> io::Result<Option<T>> {
        self.journal.read(pos)?.map(|entry| serde_json::from_slice(&entry).map_err(io::Error::other)).transpose()
    }
    pub fn into_inner(self) -> J {
        self.journal
    }
}

macro_rules! raw_space {
    ($($journal:ty),*) => {$(
        impl Space for $journal {
            type Pos = u64;
            type Item<'a> = Vec<u8>;
            fn at(&self, pos: u64) -> Option<Vec<u8>> {
                self.read(pos).ok().flatten()
            }
            fn positions(&self) -> impl Iterator<Item = u64> + '_ {
                0..self.len()
            }
        }
    )*};
}
raw_space!(Memory, FileJournal);

impl<J: Journal, T: Serialize + DeserializeOwned> Space for Typed<J, T> {
    type Pos = u64;
    type Item<'a>
        = T
    where
        Self: 'a;
    fn at(&self, pos: u64) -> Option<T> {
        self.get(pos).ok().flatten()
    }
    fn positions(&self) -> impl Iterator<Item = u64> + '_ {
        0..self.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_comonad::{Store, cut};
    use serde::Deserialize;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("control-journal-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl")
    }

    #[test]
    fn appends_are_durable_lines_that_reopen_at_the_same_positions() {
        let path = scratch("reopen");
        let mut journal = FileJournal::open(&path).unwrap();
        assert_eq!(journal.append(br#"{"turn":0}"#).unwrap(), 0);
        assert_eq!(journal.append(br#"{"turn":1,"text":"hi"}"#).unwrap(), 1);
        assert!(journal.append(b"{\"a\":\n1}").is_err() && journal.append(b"not json").is_err() && journal.append(b"").is_err());
        drop(journal);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"turn\":0}\n{\"turn\":1,\"text\":\"hi\"}\n", "plain JSON lines on disk");
        let mut journal = FileJournal::open(&path).unwrap();
        assert_eq!((journal.len(), journal.read(1).unwrap().as_deref()), (2, Some(&br#"{"turn":1,"text":"hi"}"#[..])));
        assert_eq!(journal.append(br#"{"turn":2}"#).unwrap(), 2);
        assert_eq!(journal.read(9).unwrap(), None);
    }

    #[test]
    fn a_torn_final_line_is_dropped_on_open() {
        let path = scratch("torn");
        std::fs::write(&path, "{\"turn\":0}\n{\"turn\":1}\n{\"tur").unwrap();
        let mut journal = FileJournal::open(&path).unwrap();
        assert_eq!(journal.len(), 2);
        journal.append(br#"{"turn":2}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"turn\":0}\n{\"turn\":1}\n{\"turn\":2}\n");
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Turn {
        n: u64,
        text: String,
    }

    #[test]
    fn typed_journals_are_comonad_spaces() {
        let mut typed = Typed::new(Memory::default());
        for n in 0..100 {
            typed.append(&Turn { n, text: format!("turn {n}") }).unwrap();
        }
        let focus = Store::new(&typed, 99).unwrap();
        assert_eq!(focus.extract(), Turn { n: 99, text: "turn 99".into() });
        assert_eq!(focus.peek(3).map(|t| t.n), Some(3));
        let view: Vec<u64> = cut(typed.len(), 4).map(|b| b.start).collect();
        assert_eq!(&view[..4], &[99, 98, 97, 96], "verbatim at the focus, coarser behind");
        let file = FileJournal::open(&scratch("space")).unwrap();
        let mut typed = Typed::<_, Turn>::new(file);
        typed.append(&Turn { n: 7, text: "on disk".into() }).unwrap();
        assert_eq!(Store::new(&typed, 0).unwrap().extract().text, "on disk");
    }
}
