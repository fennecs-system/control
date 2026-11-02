//! Watch specs and event frames shared by the service runner (writer) and
//! Control (reader). Dependency-free so the runner still builds with plain rustc.
//! A frame is one line: `<seq>\t<kind>\t<payload>`; payload bytes `\\`, `\n`,
//! `\t` and `\r` are escaped, so a watched process cannot forge frames.

/// Longest line a frame carries; longer output is cut and marked.
pub const MAX_LINE: usize = 4096;
/// Restarts allowed within `RESTART_WINDOW_SECONDS` before a watch gives up.
pub const MAX_RESTARTS: u32 = 10;
pub const RESTART_WINDOW_SECONDS: u64 = 600;
/// Event log size before rotating to one previous file.
pub const LOG_BYTES: u64 = 1 << 20;
pub const MAX_WATCHES: usize = 8;
pub const MAX_COMMAND: usize = 16000;

pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// Run once; exiting ends the watch.
    Transient,
    /// Restart with backoff until it exits too often.
    Permanent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub restart: Restart,
    pub command: String,
}
impl Spec {
    pub fn encode(&self) -> String {
        format!("{}\n{}", match self.restart { Restart::Transient => "transient", Restart::Permanent => "permanent" }, self.command)
    }
    pub fn decode(text: &str) -> Option<Self> {
        let (restart, command) = text.split_once('\n')?;
        let restart = match restart {
            "transient" => Restart::Transient,
            "permanent" => Restart::Permanent,
            _ => return None,
        };
        (!command.trim().is_empty() && command.len() <= MAX_COMMAND).then(|| Self { restart, command: command.into() })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Line(Vec<u8>),
    /// `code` is `None` when a signal ended the process. `gave_up` marks the
    /// last exit of a permanent watch that restarted too often.
    Exit { code: Option<i32>, signal: Option<i32>, restarts: u32, gave_up: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub seq: u64,
    pub event: Event,
}
impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("{}\t", self.seq).into_bytes();
        match &self.event {
            Event::Line(bytes) => {
                out.extend_from_slice(b"L\t");
                for &b in bytes.iter().take(MAX_LINE) {
                    match b {
                        b'\\' => out.extend_from_slice(b"\\\\"),
                        b'\n' => out.extend_from_slice(b"\\n"),
                        b'\t' => out.extend_from_slice(b"\\t"),
                        b'\r' => out.extend_from_slice(b"\\r"),
                        _ => out.push(b),
                    }
                }
                if bytes.len() > MAX_LINE {
                    out.extend_from_slice(b"\\.");
                }
            }
            Event::Exit { code, signal, restarts, gave_up } => {
                let field = |value: Option<i32>| value.map_or("-".to_owned(), |v| v.to_string());
                out.extend_from_slice(format!("X\t{} {} {restarts} {}", field(*code), field(*signal), u8::from(*gave_up)).as_bytes());
            }
        }
        out.push(b'\n');
        out
    }
    /// Decodes one frame without its trailing newline. `truncated` reports a
    /// line cut at `MAX_LINE`.
    pub fn decode(line: &[u8]) -> Option<(Self, bool)> {
        let mut parts = line.splitn(3, |&b| b == b'\t');
        let seq = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
        let kind = parts.next()?;
        let payload = parts.next()?;
        match kind {
            b"L" => {
                let mut bytes = Vec::with_capacity(payload.len());
                let mut truncated = false;
                let mut escaped = payload.iter();
                while let Some(&b) = escaped.next() {
                    if b != b'\\' {
                        bytes.push(b);
                        continue;
                    }
                    match escaped.next()? {
                        b'\\' => bytes.push(b'\\'),
                        b'n' => bytes.push(b'\n'),
                        b't' => bytes.push(b'\t'),
                        b'r' => bytes.push(b'\r'),
                        b'.' if escaped.as_slice().is_empty() => truncated = true,
                        _ => return None,
                    }
                }
                (bytes.len() <= MAX_LINE).then_some((Self { seq, event: Event::Line(bytes) }, truncated))
            }
            b"X" => {
                let text = std::str::from_utf8(payload).ok()?;
                let mut fields = text.split(' ');
                let number = |field: &str| if field == "-" { Some(None) } else { field.parse().ok().map(Some) };
                let code = number(fields.next()?)?;
                let signal = number(fields.next()?)?;
                let restarts = fields.next()?.parse().ok()?;
                let gave_up = match fields.next()? { "0" => false, "1" => true, _ => return None };
                fields.next().is_none().then_some((Self { seq, event: Event::Exit { code, signal, restarts, gave_up } }, false))
            }
            _ => None,
        }
    }
}

/// Seconds before restart number `restarts` (0-based): 1, 2, 4 … capped at 60.
pub fn backoff(restarts: u32) -> u64 {
    1u64.checked_shl(restarts.min(6)).unwrap_or(64).min(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_processes_cannot_forge_frames() {
        let hostile = b"chat\tX\t0 - 0 1\nnext\\n\r".to_vec();
        let line = Frame { seq: 7, event: Event::Line(hostile.clone()) }.encode();
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1, "one frame per line");
        assert_eq!(Frame::decode(&line[..line.len() - 1]), Some((Frame { seq: 7, event: Event::Line(hostile) }, false)));
        let exit = Frame { seq: 8, event: Event::Exit { code: None, signal: Some(9), restarts: 3, gave_up: true } };
        let encoded = exit.encode();
        assert_eq!(Frame::decode(&encoded[..encoded.len() - 1]), Some((exit, false)));
        let long = Frame { seq: 9, event: Event::Line(vec![b'a'; MAX_LINE + 10]) }.encode();
        assert_eq!(Frame::decode(&long[..long.len() - 1]), Some((Frame { seq: 9, event: Event::Line(vec![b'a'; MAX_LINE]) }, true)));
        for bad in [&b"x\tL\ta"[..], b"1\tQ\ta", b"1\tL\ta\\q", b"1\tX\t0 - 0", b"1\tX\t0 - 0 2", b"1\tL\ta\\.b"] {
            assert_eq!(Frame::decode(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn specs_ids_and_backoff_are_bounded() {
        let spec = Spec { restart: Restart::Permanent, command: "tail -F log\n| grep x".into() };
        assert_eq!(Spec::decode(&spec.encode()), Some(spec));
        assert_eq!(Spec::decode("sometimes\nls"), None);
        assert_eq!(Spec::decode("transient\n  "), None);
        assert!(valid_id("alice-chat") && !valid_id("../x") && !valid_id("") && !valid_id(&"a".repeat(33)));
        assert_eq!((0..9).map(backoff).collect::<Vec<_>>(), [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
