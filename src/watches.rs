//! Watches: supervised commands in a service whose output lines wake bots. The
//! service runner owns each process and its framed log; Control follows that
//! log from its cursor into a local spool, and the bot adapter offers new lines
//! and exits to the owning chat's bot. Control never speaks the watched
//! program's protocol; anything that prints lines is a source.
pub(crate) mod delivery;
mod ops;
mod policy;
pub(crate) mod salience;
mod tools;
use crate::store::Store;
use crate::watch_frame::{Event, Frame};
pub use policy::WakePolicy;
pub use salience::Salience;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    process::Child,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub use tools::{Tools, WatchCall};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub after: u64,
    pub through: u64,
}

/// Creation provenance, not proof that this attempt caused every stream line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Creator {
    pub run: String,
    pub attempt: String,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Notifications {
    pub pending: u64,
    pub through: u64,
    pub recent: Vec<String>,
}
impl Notifications {
    fn wake(&mut self, seq: u64, preview: String) {
        self.pending = self.pending.saturating_add(1);
        self.through = self.through.max(seq);
        if self.recent.len() == 5 {
            self.recent.remove(0);
        }
        self.recent.push(preview);
    }
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
    #[serde(default)]
    pub wake_policy: WakePolicy,
    #[serde(default)]
    pub notification_channel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<Box<Notifications>>,
    /// Declared notification filter and its bounded state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salience: Option<Box<Salience>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Creator>,
    /// Last sequence in the local spool: where the follower resumes.
    #[serde(default)]
    pub seen: u64,
    /// Selected lines and all exits spooled but not yet offered to the bot.
    #[serde(default)]
    pub lines: u64,
    #[serde(default)]
    pub line_seq: u64,
    /// Last selected line; old any-line records use line_seq.
    #[serde(default)]
    pub wake_seq: Option<u64>,
    #[serde(default)]
    pub gap: Option<Gap>,
    #[serde(default)]
    pub exits: Vec<Exit>,
    #[serde(default)]
    pub last_exit: Option<Exit>,
    /// The latest few lines, for wake prompts without reading the spool.
    #[serde(default)]
    pub recent: Vec<String>,
}
impl Watch {
    pub(crate) fn pending_notifications(&self) -> u64 {
        self.notifications
            .as_ref()
            .map_or(0, |notifications| notifications.pending)
    }
    /// Notifications a salience debounce holds; they may still wake later.
    pub(crate) fn held_notifications(&self) -> usize {
        self.salience.as_ref().map_or(0, |salience| salience.holding())
    }
    /// Wakes with held notifications whose quiet period ended by `now`.
    pub(crate) fn release(&mut self, now: u64) -> bool {
        let Some(salience) = self.salience.as_deref_mut().filter(|s| s.holding() > 0) else {
            return false;
        };
        let mut woke = false;
        let notifications = self.notifications.get_or_insert_with(Default::default);
        salience.release(now, |seq, preview| {
            woke = true;
            notifications.wake(seq, preview.into());
        });
        woke
    }
    pub(crate) fn active(&self) -> bool {
        !self.stopped
            && !self
                .last_exit
                .as_ref()
                .is_some_and(|exit| exit.gave_up || (!self.permanent && exit.seq >= self.line_seq))
    }
    /// What a start or stop returns: the summary without the provenance and
    /// service generation the caller already holds (status still shows them).
    pub(crate) fn brief(&self) -> serde_json::Value {
        let mut value = self.summary();
        if let Some(fields) = value.as_object_mut() {
            fields.remove("created_by");
            fields.remove("service_generation");
        }
        value
    }
    pub(crate) fn summary(&self) -> serde_json::Value {
        serde_json::json!({"watch_id":self.id,"name":self.name,"service_id":self.service,"service_generation":self.generation,"created_by":self.created_by,"wake_policy":self.wake_policy,"salience":self.salience.as_ref().map(|s| s.summary()),"notification_channel":self.notification_channel.then(|| serde_json::json!({"fd":3,"max_bytes":crate::watch_frame::MAX_LINE})),"restart":if self.permanent {"permanent"} else {"transient"},"active":self.active(),"stopped":self.stopped,"cursor":self.seen,"last_exit":self.last_exit,"pending_source_gap":self.gap})
    }
    #[cfg(test)]
    pub(crate) fn observe_for_test(&mut self, seq: u64, exit: Option<i32>) {
        let event = match exit {
            None => Event::Line(b"line".to_vec()),
            Some(code) => Event::Exit {
                code: Some(code),
                signal: None,
                restarts: 0,
                gave_up: false,
            },
        };
        self.observe(&Frame { seq, event });
    }
    /// Records a spooled frame; returns false for a replay.
    pub(crate) fn observe(&mut self, frame: &Frame) -> bool {
        // Only filters read the clock; unfiltered delivery stays clock-free.
        let now = if self.salience.is_some() {
            crate::bots::now_ms()
        } else {
            0
        };
        self.observe_at(frame, now)
    }
    pub(crate) fn observe_at(&mut self, frame: &Frame, now: u64) -> bool {
        if frame.seq <= self.seen {
            return false;
        }
        if frame.seq > self.seen.saturating_add(1) {
            let gap = self.gap.get_or_insert(Gap {
                after: self.seen,
                through: frame.seq - 1,
            });
            gap.through = frame.seq - 1;
        }
        self.seen = frame.seq;
        match &frame.event {
            Event::Line(line) => {
                self.line_seq = frame.seq;
                if !self.notification_channel && self.wake_policy.admits(line) {
                    self.lines = self.lines.saturating_add(1);
                    self.wake_seq = Some(frame.seq);
                    if self.recent.len() == 5 {
                        self.recent.remove(0);
                    }
                    self.recent
                        .push(String::from_utf8_lossy(line).chars().take(200).collect());
                }
            }
            Event::Notification(payload) => {
                // A hold that fell due before this event wakes first, in order.
                self.release(now);
                if self
                    .salience
                    .as_deref_mut()
                    .is_none_or(|salience| salience.admits(frame.seq, payload, now))
                {
                    self.notifications
                        .get_or_insert_with(Default::default)
                        .wake(frame.seq, String::from_utf8_lossy(payload).chars().take(200).collect());
                }
            }
            Event::Exit {
                code,
                signal,
                restarts,
                gave_up,
            } => {
                let exit = Exit {
                    seq: frame.seq,
                    code: *code,
                    signal: *signal,
                    restarts: *restarts,
                    gave_up: *gave_up,
                };
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
    let length = file.metadata()?.len();
    let record = [line, b"\n"].concat();
    // A partial write is undone so a retry never appends to a broken record.
    file.write_all(&record).inspect_err(|_| {
        let _ = file.set_len(length);
    })
}
/// Spooled frames after `after`, oldest first, at most `limit`.
pub(crate) fn frames(id: &str, after: u64, limit: usize) -> Vec<(Frame, bool)> {
    let Ok(path) = spool(id) else { return Vec::new() };
    read_frames(&path, after, limit)
}
pub(super) struct Page {
    pub events: Vec<(Frame, bool)>,
    pub cursor: u64,
    pub gap: bool,
}
/// Diagnostics do not consume a notification page's event budget. The cursor
/// still covers the scanned source range so missing control events remain visible.
pub(super) fn page(id: &str, after: u64, maximum: u64, logs: bool) -> Page {
    let all = frames(id, after, usize::MAX);
    select_page(all, after, maximum, logs, 200)
}
fn select_page(all: Vec<(Frame, bool)>, after: u64, maximum: u64, logs: bool, limit: usize) -> Page {
    let mut events = Vec::new();
    let mut cursor = after;
    let mut gap = false;
    for (frame, truncated) in all.into_iter().filter(|(frame, _)| frame.seq <= maximum) {
        gap |= frame.seq > cursor.saturating_add(1);
        cursor = frame.seq;
        if logs || !matches!(frame.event, Event::Line(_)) {
            gap |= truncated;
            events.push((frame, truncated));
            if events.len() == limit {
                return Page { events, cursor, gap };
            }
        }
    }
    gap |= cursor < maximum;
    Page { events, cursor, gap }
}
fn read_frames(path: &std::path::Path, after: u64, limit: usize) -> Vec<(Frame, bool)> {
    let mut frames = Vec::new();
    for file in [path.with_extension("log.1"), path.to_owned()] {
        let Ok(mut file) = fs::File::open(file) else { continue };
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        frames.extend(
            bytes
                .split(|&b| b == b'\n')
                .filter_map(Frame::decode)
                .filter(|(frame, _)| frame.seq > after),
        );
    }
    frames.sort_by_key(|(frame, _)| frame.seq);
    frames.dedup_by_key(|(frame, _)| frame.seq);
    frames.truncate(limit);
    frames
}
#[cfg(test)]
fn source_gap(frames: &[(Frame, bool)], after: u64, seen: u64, limit: usize) -> bool {
    if frames.is_empty() {
        return seen > after;
    }
    let mut previous = after;
    let missing = frames.iter().any(|(frame, _)| {
        let gap = frame.seq > previous.saturating_add(1);
        previous = frame.seq;
        gap
    });
    missing || (frames.len() < limit && previous < seen)
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
pub(crate) fn reconcile(shared: &Store) {
    let Ok(wanted) = shared.read(|state| {
        state
            .watches
            .iter()
            .filter(|watch| watch.active())
            .filter(|watch| {
                state.services.iter().any(|service| {
                    service.resource.id.as_str() == watch.service
                        && service.resource.generation.get() == watch.generation
                        && service.resource.phase.container().is_some()
                })
            })
            .map(|watch| watch.id.clone())
            .collect::<Vec<_>>()
    }) else {
        return;
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
        let follower = Follower {
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        };
        let (cancel, child, state, watch) = (
            follower.cancel.clone(),
            follower.child.clone(),
            shared.clone(),
            id.clone(),
        );
        if std::thread::Builder::new()
            .name(format!("watch-{id}"))
            .spawn(move || follow(&state, &watch, &cancel, &child))
            .is_ok()
        {
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
fn follow(shared: &Store, id: &str, cancel: &AtomicBool, slot: &Mutex<Option<Child>>) {
    let mut backoff = 1;
    while !cancel.load(Ordering::Relaxed) {
        let target = shared
            .read(|state| {
                let watch = state.watches.iter().find(|watch| watch.id == id && watch.active())?;
                let service = state
                    .services
                    .iter()
                    .find(|service| service.resource.id.as_str() == watch.service)?;
                Some((service.binding_snapshot(), watch.seen))
            })
            .ok()
            .flatten();
        let Some((service, seen)) = target else { break };
        let isolation = crate::engine::isolation();
        let streamed = crate::services::exec_target(isolation, &service).and_then(|(container, user)| {
            let tail = control_isolation::ExecSpec {
                container,
                user: Some(user),
                argv: ["control-service-runner", "tail", id, &seen.to_string()]
                    .map(str::to_owned)
                    .to_vec(),
                ..Default::default()
            };
            let mut child = isolation.exec(
                &tail,
                control_isolation::Streams::new(
                    control_isolation::Stream::Null,
                    control_isolation::Stream::Piped,
                    control_isolation::Stream::Null,
                ),
            )?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("Missing watch output"))?;
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
                let read = (&mut reader)
                    .take(crate::watch_frame::MAX_FRAME as u64 + 2)
                    .read_until(b'\n', &mut line)?;
                if read == 0 || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let frame_bytes = line.strip_suffix(b"\n").unwrap_or(&line);
                let Some((frame, _)) = Frame::decode(frame_bytes) else {
                    continue;
                };
                let fresh = shared
                    .read(|state| {
                        state
                            .watches
                            .iter()
                            .find(|watch| watch.id == id)
                            .map(|watch| frame.seq > watch.seen)
                    })
                    .ok()
                    .flatten();
                match fresh {
                    None => break,
                    Some(false) => continue,
                    Some(true) => {}
                }
                // Spool before advancing the cursor, so a failed write is retried from it.
                append(id, frame_bytes)?;
                let observed = shared.write(|state| {
                    state
                        .watches
                        .iter_mut()
                        .find(|watch| watch.id == id)
                        .map(|watch| watch.observe(&frame))
                });
                match observed {
                    Ok(Some(true)) => {
                        crate::storage::changes().bump();
                        any = true;
                    }
                    Ok(Some(false)) => {}
                    Ok(None) | Err(_) => break,
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
mod tests;
