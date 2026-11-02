//! Trusted PID 1 for offline persistent tools. Exiting the PID namespace kills
//! descendants, including detached children, without a control-plane heartbeat.
//! It also supervises watches: commands whose output lines become framed events
//! in a log Control follows. `watch-put`, `watch-rm` and `tail` run via exec.
#[path = "frame.rs"]
#[allow(dead_code)]
mod frame;
use std::{
    process::{Command, ExitCode},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn inotify_init1(flags: i32) -> i32;
    fn inotify_add_watch(fd: i32, path: *const std::ffi::c_char, mask: u32) -> i32;
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
}
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn main() -> ExitCode {
    #[cfg(not(target_os = "linux"))]
    {
        return ExitCode::from(125);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::ExitStatusExt;
        let args: Vec<_> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("watch-put") => return watches::put(&args[1..]),
            Some("watch-rm") => return watches::remove(&args[1..]),
            Some("tail") => return watches::tail(&args[1..]),
            _ => {}
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let Some(remaining) = args
            .first()
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|deadline| deadline.checked_sub(now))
            .filter(|s| (1..=3600).contains(s) || args.first().is_some_and(|s|s == &u64::MAX.to_string()))
        else {
            return ExitCode::from(124);
        };
        let Some(maximum) = args
            .get(1)
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|s| (1..=3600).contains(s) || (*s == u64::MAX && args.first().is_some_and(|s|s == &u64::MAX.to_string())))
        else {
            return ExitCode::from(125);
        };
        let seconds = remaining.min(maximum);
        if args.len() != 3 || args[2].is_empty() || args[2].len() > 16000 || std::process::id() != 1
        {
            return ExitCode::from(125);
        }
        // A child must not modify its supervisor through /proc or ptrace. PID 1
        // cannot receive unhandled stop/kill signals from its own namespace.
        if unsafe { prctl(4, 0usize, 0usize, 0usize, 0usize) } != 0 {
            return ExitCode::from(125);
        }
        if unsafe { signal(15, stop) } == usize::MAX || unsafe { signal(2, stop) } == usize::MAX {
            return ExitCode::from(125);
        }
        let start = Instant::now();
        let Ok(mut child) = Command::new("bash")
            .args(["-c", &format!("exec 2>&1\n{}", args[2])])
            .spawn()
        else {
            return ExitCode::from(125);
        };
        let mut supervisor = watches::Supervisor::default();
        loop {
            if STOP.load(Ordering::Relaxed) {
                supervisor.shutdown();
                return ExitCode::from(143);
            }
            if start.elapsed() >= Duration::from_secs(seconds) {
                supervisor.shutdown();
                return ExitCode::from(124);
            }
            match child.try_wait() {
                Ok(Some(status)) => return ExitCode::from(status.code().unwrap_or(1) as u8),
                Err(_) => return ExitCode::from(125),
                _ => {}
            }
            loop {
                let mut raw = 0;
                let reaped = unsafe { waitpid(-1, &mut raw, 1) };
                if reaped <= 0 {
                    break;
                }
                if reaped as u32 == child.id() {
                    return ExitCode::from(
                        std::process::ExitStatus::from_raw(raw).code().unwrap_or(1) as u8,
                    );
                }
                supervisor.reaped(reaped, std::process::ExitStatus::from_raw(raw));
            }
            supervisor.tick();
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(target_os = "linux")]
mod watches {
    use super::frame::{self, Event, Frame, Restart, Spec};
    use std::{
        collections::{BTreeMap, VecDeque},
        fs,
        io::{Read, Seek, Write},
        os::unix::{fs::MetadataExt, process::{CommandExt, ExitStatusExt}},
        path::{Path, PathBuf},
        process::{Command, ExitCode, ExitStatus, Stdio},
        sync::{Arc, Mutex},
        thread::JoinHandle,
        time::{Duration, Instant, SystemTime},
    };

    fn directory() -> PathBuf {
        Path::new(&std::env::var_os("HOME").unwrap_or_else(|| "/home/agent".into())).join(".control/watches")
    }
    fn spec_path(id: &str) -> PathBuf {
        directory().join(format!("{id}.spec"))
    }
    fn log_path(id: &str) -> PathBuf {
        directory().join(format!("{id}.log"))
    }
    fn previous_log(id: &str) -> PathBuf {
        directory().join(format!("{id}.log.1"))
    }

    pub fn put(args: &[String]) -> ExitCode {
        let [id, restart] = args else { return ExitCode::from(2) };
        let restart = match restart.as_str() {
            "transient" => Restart::Transient,
            "permanent" => Restart::Permanent,
            _ => return ExitCode::from(2),
        };
        let mut command = String::new();
        if !frame::valid_id(id) || std::io::stdin().take(frame::MAX_COMMAND as u64 + 1).read_to_string(&mut command).is_err() {
            return ExitCode::from(2);
        }
        let Some(spec) = Spec::decode(&Spec { restart, command }.encode()) else { return ExitCode::from(2) };
        let dir = directory();
        if fs::create_dir_all(&dir).is_err() {
            return ExitCode::from(1);
        }
        let existing = fs::read_dir(&dir).map(|entries| entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "spec")).count()).unwrap_or(0);
        if !spec_path(id).exists() && existing >= frame::MAX_WATCHES {
            return ExitCode::from(3);
        }
        let temporary = dir.join(format!(".{id}.tmp"));
        if fs::write(&temporary, spec.encode()).and_then(|()| fs::rename(&temporary, spec_path(id))).is_err() {
            return ExitCode::from(1);
        }
        ExitCode::SUCCESS
    }
    pub fn remove(args: &[String]) -> ExitCode {
        let [id] = args else { return ExitCode::from(2) };
        if !frame::valid_id(id) {
            return ExitCode::from(2);
        }
        match fs::remove_file(spec_path(id)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(1),
        }
    }

    /// Appends frames with increasing sequence numbers, rotating to one previous
    /// file past `LOG_BYTES`. The sequence continues across rotation and restart.
    struct Log {
        id: String,
        seq: u64,
    }
    impl Log {
        fn open(id: &str) -> Self {
            let last = |path: PathBuf| fs::read(path).ok().and_then(|bytes| {
                bytes.split(|&b| b == b'\n').filter_map(|line| Frame::decode(line).map(|(frame, _)| frame.seq)).max()
            });
            let seq = last(log_path(id)).or_else(|| last(previous_log(id))).unwrap_or(0);
            Self { id: id.into(), seq }
        }
        fn append(&mut self, event: Event) {
            let path = log_path(&self.id);
            if fs::metadata(&path).is_ok_and(|m| m.len() >= frame::LOG_BYTES) {
                let _ = fs::rename(&path, previous_log(&self.id));
            }
            self.seq += 1;
            let bytes = Frame { seq: self.seq, event }.encode();
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = file.write_all(&bytes);
            }
        }
    }

    struct Running {
        pid: i32,
        reader: JoinHandle<()>,
        exited: Option<(ExitStatus, Instant)>,
    }
    struct Watch {
        spec: Spec,
        log: Arc<Mutex<Log>>,
        running: Option<Running>,
        restarts: VecDeque<Instant>,
        restart_at: Option<Instant>,
        removed: bool,
    }

    #[derive(Default)]
    pub struct Supervisor {
        watches: BTreeMap<String, Watch>,
        seen: Option<SystemTime>,
        scanned: Option<Instant>,
    }
    impl Supervisor {
        pub fn tick(&mut self) {
            let changed = fs::metadata(directory()).and_then(|m| m.modified()).ok();
            if changed != self.seen || self.scanned.is_none_or(|at| at.elapsed() >= Duration::from_secs(5)) {
                self.seen = changed;
                self.scanned = Some(Instant::now());
                self.scan();
            }
            let now = Instant::now();
            for (id, watch) in &mut self.watches {
                if let Some(running) = &watch.running
                    && let Some((status, at)) = running.exited
                    && (running.reader.is_finished() || at.elapsed() >= Duration::from_secs(2))
                {
                    watch.running = None;
                    finish(id, watch, status, now);
                }
                if watch.running.is_none() && !watch.removed && watch.restart_at.is_some_and(|at| at <= now) {
                    watch.restart_at = None;
                    start(id, watch);
                }
            }
            self.watches.retain(|_, watch| !(watch.removed && watch.running.is_none()));
        }
        fn scan(&mut self) {
            let mut present = BTreeMap::new();
            for entry in fs::read_dir(directory()).into_iter().flatten().flatten() {
                let path = entry.path();
                let Some(id) = path.file_stem().and_then(|s| s.to_str()).filter(|id| frame::valid_id(id)) else { continue };
                if path.extension().is_some_and(|x| x == "spec")
                    && let Some(spec) = fs::read_to_string(&path).ok().as_deref().and_then(Spec::decode)
                {
                    present.insert(id.to_owned(), spec);
                }
            }
            for (id, watch) in &mut self.watches {
                if present.get(id) != Some(&watch.spec) && !watch.removed {
                    watch.removed = true;
                    watch.restart_at = None;
                    if let Some(running) = &watch.running {
                        unsafe { super::kill(-running.pid, 15) };
                    }
                }
            }
            for (id, spec) in present.into_iter().take(frame::MAX_WATCHES) {
                if self.watches.contains_key(&id) {
                    continue;
                }
                let mut watch = Watch { spec, log: Arc::new(Mutex::new(Log::open(&id))), running: None, restarts: VecDeque::new(), restart_at: None, removed: false };
                start(&id, &mut watch);
                self.watches.insert(id, watch);
            }
        }
        pub fn reaped(&mut self, pid: i32, status: ExitStatus) {
            if let Some(running) = self.watches.values_mut().filter_map(|watch| watch.running.as_mut()).find(|running| running.pid == pid) {
                // The watch is its process group; stragglers would hold the pipe open.
                unsafe { super::kill(-pid, 9) };
                running.exited = Some((status, Instant::now()));
            }
        }
        pub fn shutdown(&mut self) {
            for running in self.watches.values().filter_map(|watch| watch.running.as_ref()) {
                unsafe { super::kill(-running.pid, 15) };
            }
        }
    }

    fn start(id: &str, watch: &mut Watch) {
        let spawned = Command::new("bash")
            .args(["-c", &format!("exec 2>&1\n{}", watch.spec.command)])
            .current_dir("/workspace")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn();
        let Ok(mut child) = spawned else {
            watch.log.lock().unwrap().append(Event::Exit { code: Some(127), signal: None, restarts: watch.restarts.len() as u32, gave_up: false });
            schedule(id, watch, Instant::now());
            return;
        };
        let stdout = child.stdout.take();
        let log = watch.log.clone();
        let reader = std::thread::spawn(move || {
            let Some(mut stdout) = stdout else { return };
            let mut line = Vec::with_capacity(256);
            let mut overflow = false;
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                for &b in &chunk[..n] {
                    if b == b'\n' {
                        if !overflow {
                            log.lock().unwrap().append(Event::Line(std::mem::take(&mut line)));
                        }
                        line.clear();
                        overflow = false;
                    } else if line.len() <= frame::MAX_LINE {
                        line.push(b);
                    } else if !overflow {
                        overflow = true;
                        log.lock().unwrap().append(Event::Line(std::mem::take(&mut line)));
                    }
                }
            }
            if !line.is_empty() && !overflow {
                log.lock().unwrap().append(Event::Line(line));
            }
        });
        watch.running = Some(Running { pid: child.id() as i32, reader, exited: None });
    }
    /// Records an exit after the watch's last lines, then restarts a permanent
    /// watch with backoff or retires it.
    fn finish(id: &str, watch: &mut Watch, status: ExitStatus, now: Instant) {
        while watch.restarts.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(frame::RESTART_WINDOW_SECONDS)) {
            watch.restarts.pop_front();
        }
        let restarts = watch.restarts.len() as u32;
        let permanent = watch.spec.restart == Restart::Permanent && !watch.removed;
        let gave_up = permanent && restarts >= frame::MAX_RESTARTS;
        watch.log.lock().unwrap().append(Event::Exit { code: status.code(), signal: status.signal(), restarts, gave_up });
        if watch.removed {
            return;
        }
        if gave_up {
            retire(id, watch);
        } else {
            schedule(id, watch, now);
        }
    }
    fn schedule(id: &str, watch: &mut Watch, now: Instant) {
        if watch.spec.restart == Restart::Transient {
            retire(id, watch);
            return;
        }
        watch.restart_at = Some(now + Duration::from_secs(frame::backoff(watch.restarts.len() as u32)));
        watch.restarts.push_back(now);
    }
    fn retire(id: &str, watch: &mut Watch) {
        watch.removed = true;
        let _ = fs::remove_file(spec_path(id));
    }

    /// Streams frames after `after` from the previous and current log, then
    /// follows new frames until stdout closes or ten minutes pass; Control
    /// re-runs it from its saved cursor.
    pub fn tail(args: &[String]) -> ExitCode {
        let [id, after] = args else { return ExitCode::from(2) };
        let (true, Ok(mut last)) = (frame::valid_id(id), after.parse::<u64>()) else { return ExitCode::from(2) };
        let _ = fs::create_dir_all(directory());
        let Ok(path) = std::ffi::CString::new(directory().into_os_string().into_encoded_bytes()) else { return ExitCode::from(2) };
        let fd = unsafe { super::inotify_init1(0o4000) };
        // IN_MODIFY | IN_MOVED_TO | IN_CREATE
        if fd < 0 || unsafe { super::inotify_add_watch(fd, path.as_ptr(), 0x2 | 0x80 | 0x100) } < 0 {
            return ExitCode::from(1);
        }
        let mut events = { use std::os::fd::FromRawFd; unsafe { fs::File::from_raw_fd(fd) } };
        let started = Instant::now();
        let mut out = std::io::stdout().lock();
        let mut emit = |bytes: &[u8], last: &mut u64| -> bool {
            for line in bytes.split(|&b| b == b'\n') {
                if let Some((frame, _)) = Frame::decode(line)
                    && frame.seq > *last
                {
                    *last = frame.seq;
                    if out.write_all(line).and_then(|()| out.write_all(b"\n")).is_err() {
                        return false;
                    }
                }
            }
            out.flush().is_ok()
        };
        let (mut inode, mut offset) = (0, 0u64);
        let mut rotated = true;
        while started.elapsed() < Duration::from_secs(600) {
            match fs::metadata(log_path(id)) {
                Ok(meta) if meta.ino() != inode || meta.len() < offset => {
                    rotated = true;
                    (inode, offset) = (meta.ino(), 0);
                }
                Err(_) => (inode, offset) = (0, 0),
                _ => {}
            }
            // Frames written just before a rotation now live in the previous file.
            if std::mem::take(&mut rotated)
                && let Ok(previous) = fs::read(previous_log(id))
                && !emit(&previous, &mut last)
            {
                return ExitCode::SUCCESS;
            }
            if let Ok(mut file) = fs::File::open(log_path(id)) {
                let mut bytes = Vec::new();
                if file.seek(std::io::SeekFrom::Start(offset)).is_ok() && file.read_to_end(&mut bytes).is_ok() {
                    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
                    offset += complete as u64;
                    if !emit(&bytes[..complete], &mut last) {
                        return ExitCode::SUCCESS;
                    }
                }
            }
            let mut ready = super::PollFd { fd, events: 1, revents: 0 };
            if unsafe { super::poll(&mut ready, 1, 30_000) } > 0 {
                let mut drain = [0u8; 4096];
                while events.read(&mut drain).is_ok_and(|n| n > 0) {}
            }
        }
        ExitCode::SUCCESS
    }
}
