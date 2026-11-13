//! Bounded source pages; history length is independent of inference capacity.
use std::{
    io,
    path::{Path, PathBuf},
};
pub(super) const PAGE_BYTES: usize = 1_048_576;
pub(super) enum Records {
    Pending(usize),
    Pages {
        directory: PathBuf,
        pages: Vec<Page>,
        count: usize,
    },
    #[cfg(test)]
    Memory(Vec<String>),
}
pub(super) struct Page {
    start: usize,
    end: usize,
    key: String,
}
impl Records {
    pub fn len(&self) -> usize {
        match self {
            Self::Pending(count) | Self::Pages { count, .. } => *count,
            #[cfg(test)]
            Self::Memory(entries) => entries.len(),
        }
    }
    pub fn capture(
        count: usize,
        directory: &Path,
        mut read: impl FnMut(usize, usize) -> io::Result<Vec<String>>,
    ) -> io::Result<Self> {
        let mut pages = Vec::new();
        let mut start = 0;
        while start < count {
            let entries = read(start, (count - start).min(64))?;
            if entries.is_empty() || entries.len() > 64 || entries.len() > count - start {
                return Err(io::Error::other("Invalid source page"));
            }
            let bytes = serde_json::to_vec(&entries).map_err(io::Error::other)?;
            if bytes.len() > PAGE_BYTES {
                return Err(io::Error::other("Source page exceeds byte budget"));
            }
            let key = super::hash(&bytes);
            let filename = format!("source-{key}.json");
            match crate::config::read_regular(&directory.join(&filename), PAGE_BYTES, true) {
                Ok(existing) if existing == bytes => {}
                Ok(_) => return Err(io::Error::other("Source cache checksum mismatch")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    crate::config::write_in(directory, &filename, &bytes)?
                }
                Err(e) => return Err(e),
            }
            let end = start + entries.len();
            pages.push(Page { start, end, key });
            start = end;
        }
        Ok(Self::Pages {
            directory: directory.to_owned(),
            pages,
            count,
        })
    }
    fn visit(
        &self,
        start: usize,
        end: usize,
        mut f: impl FnMut(&str) -> io::Result<()>,
    ) -> io::Result<()> {
        if start > end || end > self.len() {
            return Err(io::Error::other("Source range unavailable"));
        }
        match self {
            Self::Pending(_) => Err(io::Error::other("Source pages have not been captured")),
            #[cfg(test)]
            Self::Memory(entries) => {
                for entry in &entries[start..end] {
                    f(entry)?;
                }
                Ok(())
            }
            Self::Pages {
                directory, pages, ..
            } => {
                let first = pages.partition_point(|p| p.end <= start);
                for page in pages[first..].iter().take_while(|p| p.start < end) {
                    let bytes = crate::config::read_regular(
                        &directory.join(format!("source-{}.json", page.key)),
                        PAGE_BYTES,
                        true,
                    )?;
                    if super::hash(&bytes) != page.key {
                        return Err(io::Error::other("Source page checksum mismatch"));
                    }
                    let entries: Vec<String> =
                        serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                    if entries.len() != page.end - page.start {
                        return Err(io::Error::other("Source page range mismatch"));
                    }
                    for entry in
                        &entries[start.saturating_sub(page.start)..end.min(page.end) - page.start]
                    {
                        f(entry)?;
                    }
                }
                Ok(())
            }
        }
    }
    pub fn read(&self, start: usize, end: usize, budget: usize) -> io::Result<Vec<String>> {
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        self.visit(start, end, |text| {
            bytes = bytes.saturating_add(text.len() + 1);
            if bytes > budget {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Source range exceeds input budget",
                ));
            }
            entries.push(text.to_owned());
            Ok(())
        })?;
        Ok(entries)
    }
    pub fn digest(&self, start: usize, end: usize) -> io::Result<String> {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        self.visit(start, end, |text| {
            update(&mut hash, text);
            Ok(())
        })?;
        Ok(hex(hash.finish().as_ref()))
    }
}
pub(super) fn update(hash: &mut ring::digest::Context, text: &str) {
    hash.update(&(text.len() as u64).to_be_bytes());
    hash.update(text.as_bytes());
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
