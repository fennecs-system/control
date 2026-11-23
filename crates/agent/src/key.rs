//! A provider API key on its way from the operator's standard input to the
//! one private file that holds it. It cannot be printed, formatted or
//! serialized, and its bytes are cleared when it is dropped; only the writer
//! that saves it and a CLI's own login that reads it on standard input see
//! the bytes.
use std::{fmt, io::Read};

/// A key that passed [`ApiKey::read`]: one line of printable ASCII, no spaces,
/// at most [`ApiKey::MAX`] bytes.
pub struct ApiKey(Vec<u8>);
impl ApiKey {
    /// Longer than any provider key today, short enough that a pasted file is refused.
    pub const MAX: usize = 4096;
    /// Reads one key from `input`: a trailing newline (LF or CRLF) is dropped,
    /// anything else that is not part of a key is refused. The error never
    /// quotes the input.
    pub fn read(input: impl Read) -> Result<Self, KeyError> {
        let mut bytes = Vec::new();
        input
            .take(Self::MAX as u64 + 3)
            .read_to_end(&mut bytes)
            .map_err(|_| KeyError::Unreadable)?;
        let key = Self(bytes);
        let line = key
            .0
            .strip_suffix(b"\n")
            .map_or(&key.0[..], |line| line.strip_suffix(b"\r").unwrap_or(line));
        match line {
            [] => Err(KeyError::Empty),
            line if line.len() > Self::MAX => Err(KeyError::TooLong),
            line if line.contains(&b'\n') => Err(KeyError::Lines),
            line if !line.iter().all(u8::is_ascii_graphic) => Err(KeyError::Character),
            line => Ok(Self(line.to_vec())),
        }
    }
    /// The bytes, for the private file or the CLI's standard input only.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl Drop for ApiKey {
    fn drop(&mut self) {
        self.0.iter_mut().for_each(|byte| *byte = 0);
    }
}
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(hidden)")
    }
}

/// Why standard input is not a key. Never carries the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    Unreadable,
    Empty,
    TooLong,
    /// More than one line.
    Lines,
    /// A space, control character or non-ASCII byte.
    Character,
}
impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unreadable => "the key could not be read from standard input",
            Self::Empty => "no key arrived on standard input",
            Self::TooLong => "the key is longer than 4096 bytes",
            Self::Lines => "the key must be one line",
            Self::Character => "the key may hold only printable ASCII without spaces",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_one_printable_line_and_errors_never_quote_it() {
        let read = |text: &[u8]| ApiKey::read(text).map(|key| key.expose().to_vec());
        assert_eq!(read(b"sk-abc\n"), Ok(b"sk-abc".to_vec()));
        assert_eq!(read(b"sk-abc\r\n"), Ok(b"sk-abc".to_vec()));
        assert_eq!(read(b"sk-abc"), Ok(b"sk-abc".to_vec()));
        assert_eq!(read(b""), Err(KeyError::Empty));
        assert_eq!(read(b"\n"), Err(KeyError::Empty));
        assert_eq!(read(b"sk-a\nsk-b\n"), Err(KeyError::Lines));
        assert_eq!(read(b"sk a"), Err(KeyError::Character));
        assert_eq!(read("sk-\u{e9}".as_bytes()), Err(KeyError::Character));
        assert_eq!(read(b"sk-\x07"), Err(KeyError::Character));
        assert!(read(&[b'k'; ApiKey::MAX]).is_ok());
        assert_eq!(read(&[b'k'; ApiKey::MAX + 1]), Err(KeyError::TooLong));
        assert_eq!(read(&[b'k'; 100_000]), Err(KeyError::TooLong));
        let canary = "sk-CANARY-0123";
        let shown = [
            format!("{:?}", ApiKey::read(canary.as_bytes()).unwrap()),
            format!("{}", KeyError::Character),
        ];
        assert!(shown.iter().all(|text| !text.contains("CANARY")), "{shown:?}");
    }
}
