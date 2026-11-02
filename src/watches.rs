//! Watches: supervised commands in a service whose output lines wake bots. The
//! service runner owns each process and its framed log; Control follows that
//! log from its cursor into a local spool, and the bot adapter offers new lines
//! and exits to the owning chat's bot. Control never speaks the watched
//! program's protocol; anything that prints lines is a source.
mod ops;
mod tools;
pub use tools::{Tools, WatchCall};
use crate::{State, watch_frame::{Event, Frame}};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    process::{Child, Stdio},
    sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}},
    time::Duration,
};

/// Watches per chat; each service runner also caps its own at eight.
pub(crate) const MAX_WATCHES: usize = 8;
const SPOOL_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub seq: u64,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub restarts: u32,
    pub gave_up: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Watch {
    /// Runner ID and bot topic name.
    pub id: String,
    pub name: String,
    pub chat: String,
    pub service: String,
    pub generation: u64,
    pub permanent: bool,
    pub command: String,
    pub stopped: bool,
    /// Last sequence in the local spool: where the follower resumes.
    #[serde(default)]
    pub seen: u64,
    /// Lines and exits spooled but not yet offered to the chat's bot.
    #[serde(default)]
    pub lines: u64,
    #[serde(default)]
    pub line_seq: u64,
    #[serde(default)]
    pub exits: Vec<Exit>,
    #[serde(default)]
    pub last_exit: Option<Exit>,
    /// The latest few lines, for wake prompts without reading the spool.
    #[serde(default)]
    pub recent: Vec<String>,
}
impl Watch {
    pub(crate) fn active(&self) -> bool {
        !self.stopped && !self.last_exit.as_ref().is_some_and(|exit| exit.gave_up || (!self.permanent && exit.seq >= self.line_seq))
    }
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::json!({"watch_id":self.id,"name":self.name,"service_id":self.service,"restart":if self.permanent {"permanent"} else {"transient"},"active":self.active(),"stopped":self.stopped,"cursor":self.seen,"last_exit":self.last_exit})
    }
    #[cfg(test)]
    pub(crate) fn observe_for_test(&mut self, seq: u64, exit: Option<i32>) {
        let event = match exit {
            None => Event::Line(b"line".to_vec()),
            Some(code) => Event::Exit { code: Some(code), signal: None, restarts: 0, gave_up: false },
        };
        self.observe(&Frame { seq, event });
    }
    /// Records a spooled frame; returns false for a replay.
    fn observe(&mut self, frame: &Frame) -> bool {
        if frame.seq <= self.seen {
            return false;
        }
        self.seen = frame.seq;
        match &frame.event {
            Event::Line(line) => {
                self.lines = self.lines.saturating_add(1);
                self.line_seq = frame.seq;
                if self.recent.len() == 5 {
                    self.recent.remove(0);
                }
                self.recent.push(String::from_utf8_lossy(line).chars().take(200).collect());
            }
            Event::Exit { code, signal, restarts, gave_up } => {
                let exit = Exit { seq: frame.seq, code: *code, signal: *signal, restarts: *restarts, gave_up: *gave_up };
                self.last_exit = Some(exit.clone());
                if self.exits.len() < 16 {
                    self.exits.push(exit);
                }
            }
        }
        true
    }
}

fn spool(id: &str) -> io::Result<PathBuf> {
    let directory = crate::config::state("watches").map_err(io::Error::other)?;
    fs::create_dir_all(&directory)?;
    Ok(directory.join(format!("{id}.log")))
}
fn append(id: &str, line: &[u8]) -> io::Result<()> {
    let path = spool(id)?;
    if fs::metadata(&path).is_ok_and(|m| m.len() >= SPOOL_BYTES) {
        fs::rename(&path, path.with_extension("log.1"))?;
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(line)?;
    file.write_all(b"\n")
}
/// Spooled frames after `after`, oldest first, at most `limit`.
pub(crate) fn frames(id: &str, after: u64, limit: usize) -> Vec<(Frame, bool)> {
    let Ok(path) = spool(id) else { return Vec::new() };
    let mut frames = Vec::new();
    for file in [path.with_extension("log.1"), path] {
        let Ok(mut file) = fs::File::open(file) else { continue };
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        frames.extend(bytes.split(|&b| b == b'\n').filter_map(Frame::decode).filter(|(frame, _)| frame.seq > after));
    }
    frames.sort_by_key(|(frame, _)| frame.seq);
    frames.dedup_by_key(|(frame, _)| frame.seq);
    frames.truncate(limit);
    frames
}
struct Follower {
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}
fn followers() -> &'static Mutex<HashMap<String, Follower>> {
    static FOLLOWERS: OnceLock<Mutex<HashMap<String, Follower>>> = OnceLock::new();
    FOLLOWERS.get_or_init(Default::default)
}

/// Keeps one follower per active watch on a running service and stops the
/// rest. Called from the worker loop; spawning and engine calls happen off the
/// state lock.
pub(crate) fn reconcile(shared: &Arc<Mutex<State>>) {
    let wanted = {
        let Ok(state) = shared.lock() else { return };
        state.watches.iter().filter(|watch| watch.active())
            .filter(|watch| state.services.iter().any(|service| service.resource.id.as_str() == watch.service && service.resource.generation.get() == watch.generation && service.resource.phase.container().is_some()))
            .map(|watch| watch.id.clone()).collect::<Vec<_>>()
    };
    let Ok(mut running) = followers().lock() else { return };
    running.retain(|id, follower| {
        let keep = wanted.contains(id) && !follower.cancel.load(Ordering::Relaxed);
        if !keep {
            follower.cancel.store(true, Ordering::Relaxed);
            if let Ok(mut child) = follower.child.lock()
                && let Some(child) = child.as_mut()
            {
                let _ = child.kill();
            }
        }
        keep
    });
    for id in wanted {
        if running.contains_key(&id) {
            continue;
        }
        let follower = Follower { cancel: Arc::new(AtomicBool::new(false)), child: Arc::new(Mutex::new(None)) };
        let (cancel, child, state, watch) = (follower.cancel.clone(), follower.child.clone(), shared.clone(), id.clone());
        if std::thread::Builder::new().name(format!("watch-{id}")).spawn(move || follow(&state, &watch, &cancel, &child)).is_ok() {
            running.insert(id, follower);
        }
    }
}

fn stop(slot: &Mutex<Option<Child>>) {
    if let Ok(mut child) = slot.lock()
        && let Some(mut child) = child.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}
/// Removes the spooled output of watches that no longer exist.
pub(crate) fn discard(ids: &[String]) {
    for id in ids {
        if let Ok(path) = spool(id) {
            let _ = fs::remove_file(path.with_extension("log.1"));
            let _ = fs::remove_file(path);
        }
    }
}
/// Runs the runner's `tail` from the watch's cursor, spooling each new frame
/// and waking the worker loop. Restarts the tail with backoff until cancelled.
fn follow(shared: &Arc<Mutex<State>>, id: &str, cancel: &AtomicBool, slot: &Mutex<Option<Child>>) {
    let mut backoff = 1;
    while !cancel.load(Ordering::Relaxed) {
        let target = shared.lock().ok().and_then(|state| {
            let watch = state.watches.iter().find(|watch| watch.id == id && watch.active())?;
            let service = state.services.iter().find(|service| service.resource.id.as_str() == watch.service)?;
            Some((service.binding_snapshot(), watch.seen))
        });
        let Some((service, seen)) = target else { break };
        let streamed = crate::services::exec_target(&service).and_then(|(container, user)| {
            let mut command = crate::engine::cli();
            command.args(["exec", "--user", &user, &container, "control-service-runner", "tail", id, &seen.to_string()])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            let mut child = command.spawn()?;
            let stdout = child.stdout.take().ok_or_else(|| io::Error::other("Missing watch output"))?;
            *slot.lock().map_err(|_| io::Error::other("Follower unavailable"))? = Some(child);
            // A cancel that raced the spawn found no child to kill.
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let mut reader = io::BufReader::new(stdout);
            let mut line = Vec::with_capacity(512);
            let mut any = false;
            loop {
                line.clear();
                let read = (&mut reader).take(crate::watch_frame::MAX_FRAME as u64 + 2).read_until(b'\n', &mut line)?;
                if read == 0 || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let frame_bytes = line.strip_suffix(b"\n").unwrap_or(&line);
                let Some((frame, _)) = Frame::decode(frame_bytes) else { continue };
                let fresh = shared.lock().ok().and_then(|state| state.watches.iter().find(|watch| watch.id == id).map(|watch| frame.seq > watch.seen));
                match fresh {
                    None => break,
                    Some(false) => continue,
                    Some(true) => {}
                }
                // Spool before advancing the cursor, so a failed write is retried from it.
                append(id, frame_bytes)?;
                let Ok(mut state) = shared.lock() else { break };
                let Some(watch) = state.watches.iter_mut().find(|watch| watch.id == id) else { break };
                if watch.observe(&frame) {
                    drop(state);
                    crate::storage::changes().bump();
                    any = true;
                }
            }
            stop(slot);
            Ok(any)
        });
        stop(slot);
        backoff = match streamed {
            Ok(true) => 1,
            _ => (backoff * 2).min(30),
        };
        for _ in 0..backoff * 10 {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch() -> Watch {
        Watch { id: "w-1".into(), name: "chat".into(), chat: "c".into(), service: "svc".into(), generation: 1, permanent: false, command: "tail -F log".into(), stopped: false, seen: 0, lines: 0, line_seq: 0, exits: Vec::new(), last_exit: None, recent: Vec::new() }
    }

    #[test]
    fn observed_frames_advance_once_and_a_transient_exit_ends_the_watch() {
        let mut watch = watch();
        assert!(watch.observe(&Frame { seq: 1, event: Event::Line(b"hello".to_vec()) }));
        assert!(!watch.observe(&Frame { seq: 1, event: Event::Line(b"hello".to_vec()) }), "replays are ignored");
        assert!(watch.observe(&Frame { seq: 2, event: Event::Line(b"bye".to_vec()) }));
        assert!(watch.active());
        assert!(watch.observe(&Frame { seq: 3, event: Event::Exit { code: Some(0), signal: None, restarts: 0, gave_up: false } }));
        assert_eq!((watch.seen, watch.lines, watch.line_seq, watch.exits.len()), (3, 2, 2, 1));
        assert!(!watch.active(), "a transient watch ends with its exit");
        let mut permanent = self::watch();
        permanent.permanent = true;
        permanent.observe(&Frame { seq: 1, event: Event::Exit { code: Some(1), signal: None, restarts: 0, gave_up: false } });
        assert!(permanent.active(), "a permanent watch restarts");
        permanent.observe(&Frame { seq: 2, event: Event::Exit { code: Some(1), signal: None, restarts: 10, gave_up: true } });
        assert!(!permanent.active(), "until it gives up");
    }
}
