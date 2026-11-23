//! One bounded, throwaway tool container (the Rust playground, the solvers):
//! input on stdin, the head of its standard output, optionally the tail of
//! its diagnostics, and a wall-clock deadline after which the named container
//! is force-removed. The caller owns the container's specification and the
//! meaning of the output.
use control_isolation::{Isolation, RunSpec, Stream, Streams};
use std::{
    io::{self, Read, Write},
    process::{Child, ExitStatus},
    time::{Duration, Instant},
};

/// What to run and how much of it to keep.
pub(crate) struct Spec {
    /// The container; its name is the one removed at the deadline.
    pub(crate) run: RunSpec,
    /// Written to stdin, which is then closed.
    pub(crate) input: Vec<u8>,
    pub(crate) deadline: Duration,
    /// First bytes of standard output kept.
    pub(crate) stdout_bytes: usize,
    /// Last bytes of standard error kept; `None` discards it.
    pub(crate) stderr_bytes: Option<usize>,
    /// The error when the engine CLI cannot be started, naming the tool.
    pub(crate) unavailable: &'static str,
}
/// A finished or stopped container.
#[derive(Debug)]
pub(crate) struct Finished {
    /// `None` when the deadline passed (or waiting failed) and it was removed.
    pub(crate) status: Option<ExitStatus>,
    /// Whether all of `input` was written before stdin closed.
    pub(crate) delivered: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr: Vec<u8>,
    pub(crate) elapsed: Duration,
}

/// Runs `spec` through `isolation`.
pub(crate) fn run(isolation: &dyn Isolation, spec: Spec) -> io::Result<Finished> {
    let run = spec.run.clone();
    run_with(|streams| isolation.spawn(&run, streams), spec, || {
        let _ = isolation.remove(&run.name);
    })
}
/// The runner behind [`run`], with the start and the deadline's removal
/// supplied so tests can drive it without an engine.
fn run_with(spawn: impl FnOnce(Streams) -> io::Result<Child>, spec: Spec, remove: impl FnOnce()) -> io::Result<Finished> {
    let start = Instant::now();
    let stderr = if spec.stderr_bytes.is_some() { Stream::Piped } else { Stream::Null };
    let mut child = spawn(Streams::new(Stream::Piped, Stream::Piped, stderr))
        .map_err(|_| io::Error::other(spec.unavailable))?;
    let stdout = child.stdout.take().unwrap();
    let out = std::thread::spawn(move || head(stdout, spec.stdout_bytes));
    let err = match (child.stderr.take(), spec.stderr_bytes) {
        (Some(stderr), Some(limit)) => Some(std::thread::spawn(move || tail(stderr, limit))),
        _ => None,
    };
    let mut stdin = child.stdin.take().unwrap();
    let input = spec.input;
    // A program that exits early closes the pipe; that is reported by its status.
    let feed = std::thread::spawn(move || stdin.write_all(&input).is_ok());
    let status = wait(&mut child, start, spec.deadline);
    let elapsed = start.elapsed();
    if status.is_none() {
        remove();
        let _ = child.kill();
        let _ = child.wait();
    }
    let delivered = feed.join().unwrap_or(false);
    let (stdout, stdout_truncated) = out.join().map_err(|_| io::Error::other("Tool container output reader failed"))??;
    let stderr = match err {
        Some(err) => err.join().map_err(|_| io::Error::other("Tool container diagnostics reader failed"))??,
        None => Vec::new(),
    };
    Ok(Finished { status, delivered, stdout, stdout_truncated, stderr, elapsed })
}
fn wait(child: &mut Child, start: Instant, deadline: Duration) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if start.elapsed() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => return None,
        }
    }
}
/// Keep the first `limit` bytes, reading to EOF so the child never blocks on
/// a full pipe.
pub(crate) fn head(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let (mut kept, mut chunk, mut truncated) = (Vec::new(), [0u8; 8192], false);
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok((kept, truncated));
        }
        let room = limit.saturating_sub(kept.len()).min(count);
        kept.extend_from_slice(&chunk[..room]);
        truncated |= room < count;
    }
}
/// Keep at least the last `limit` bytes (at most twice that): errors and
/// summaries come at the end.
pub(crate) fn tail(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let (mut kept, mut chunk) = (Vec::new(), [0u8; 8192]);
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(kept);
        }
        kept.extend_from_slice(&chunk[..count]);
        if kept.len() > 2 * limit {
            kept.drain(..kept.len() - limit);
        }
    }
}

#[cfg(test)]
mod tests;
