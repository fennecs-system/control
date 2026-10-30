//! Provider protocol boundary: no credentials, runtime state, networking or execution.
//! Transports frame provider records; decoders normalize them into owned, bounded events.
pub mod claude;
pub mod codex;
mod types;
use serde_json::Value;
use std::io::{self, Read};
pub use types::{Activity, Tokens};

/// Provider-reported results. Missing usage is unknown, never an estimate of zero.
#[derive(Debug, Default)]
pub struct Output {
    pub text: Option<String>,
    pub tokens: Option<Tokens>,
}

#[derive(Debug)]
pub enum Event {
    Activity(Activity),
    /// Abort execution immediately; waiting for EOF could allow unauthorized tools.
    RejectedTools,
}

/// Pure record decoder. CLI JSONL and future HTTP/SSE transports share this contract.
/// Provider-specific validation stays here; permissions and effects stay with the caller.
pub trait Decoder {
    fn decode(&mut self, record: &Value, emit: &mut dyn FnMut(Event));
    fn malformed(&mut self);
    fn accepts_partial_record(&self) -> bool {
        false
    }
    fn finish(self) -> io::Result<Output>;
}

pub const MAX_RECORD_BYTES: usize = 1_048_576;

/// Reuses one bounded buffer and parses each record once. No transcript retention,
/// queues, background tasks, cloned JSON trees or allocation per input chunk.
pub fn json_lines(
    reader: impl Read,
    mut decoder: impl Decoder,
    mut emit: impl FnMut(Event),
) -> io::Result<Output> {
    let mut reader = reader;
    let mut line = Vec::new();
    let mut oversized = false;
    let mut chunk = [0; 8192];
    loop {
        let length = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        for &byte in &chunk[..length] {
            if byte == b'\n' {
                record(&line, oversized, &mut decoder, &mut emit);
                line.clear();
                oversized = false;
            } else if !oversized {
                if line.len() < MAX_RECORD_BYTES {
                    line.push(byte);
                } else {
                    oversized = true;
                    line.clear();
                }
            }
        }
    }
    if oversized || !line.is_empty() {
        if decoder.accepts_partial_record() {
            record(&line, oversized, &mut decoder, &mut emit);
        } else {
            decoder.malformed();
        }
    }
    decoder.finish()
}
fn record(line: &[u8], oversized: bool, decoder: &mut impl Decoder, emit: &mut dyn FnMut(Event)) {
    if oversized {
        decoder.malformed();
    } else if !line.is_empty() {
        match serde_json::from_slice(line) {
            Ok(value) => decoder.decode(&value, emit),
            Err(_) => decoder.malformed(),
        }
    }
}
