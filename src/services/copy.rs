//! Copy a file or tree from the calling attempt's task workspace into an
//! accessible service's `/workspace`, without a host mount.
//!
//! The host side only reads: the source is opened beneath the attempt's
//! workspace through pinned directory descriptors, never following a symlink,
//! with the artifact publication path policy. Entries stream as a tar archive
//! into `tar -x` running as the service's own user via `docker exec`, so a copy
//! can write nothing that user could not already write with a service command.
//! A copy takes the service exclusively (no command runs beside it) and shares
//! durable receipts and recovery with `control_service_exec`.
use super::*;
use crate::artifacts::{diff_open_child, directory_names, excluded, valid_path};
use crate::store::Store;
use std::{
    ffi::{CString, OsStr},
    fs,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

const MIB: u64 = 1024 * 1024;
/// Matches artifact publication; deeper trees are refused, never truncated.
const MAX_DEPTH: usize = 32;
const SKIPPED_EXAMPLES: usize = 8;
/// Runs as the service user; members are relative and contain no links.
/// Existing files at the same paths are replaced; other files are kept.
pub(super) const EXTRACT: &str = "exec 2>&1; exec tar -x -f - -C /workspace --no-same-owner --no-overwrite-dir";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    service_id: String,
    from: String,
    to: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    /// How long to wait for the result inline; not part of request identity.
    #[serde(default = "commands::default_wait")]
    wait_seconds: u64,
}
fn default_timeout() -> u64 {
    300
}
/// The durable request: workspace-relative paths only, never host paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Transfer {
    from: String,
    to: String,
}

pub(super) fn definition() -> Value {
    json!({
        "name": "control_service_copy",
        "description": concat!(
            "Copy a file or directory tree from your task workspace (/workspace in your run container, including ",
            "approved project folders) into an accessible running service's /workspace, e.g. to bring a reusable tool ",
            "into a service. from and to are paths relative to /workspace (a leading /workspace/ is accepted); no ",
            "host paths, mounts or engine options. Files are written as the service user; existing files at the same ",
            "paths are replaced and other files are kept. Symlinks, special files, non-UTF-8 names and credential, ",
            "Control-internal or generated names (.git, .env*, auth.json, target, node_modules, .control-*) are ",
            "skipped and reported, never followed. Admitted tool packages at .control-tools/<name> are the one ",
            "Control-internal source you may copy; destinations cannot use .control-* names. Total bytes and entries ",
            "are bounded by node settings (default 256 MiB and 20000 entries) and by memory-backed or capped legacy ",
            "workspaces; the tree is checked before anything is written. The copy takes the service exclusively: it ",
            "waits for running commands to finish, and new commands and copies are refused until it completes. The ",
            "outcome returns inline, marked final=true, when the copy finishes within wait_seconds (default 15, max ",
            "30). Only when the result says running and gives an execution_id, call control_service_exec_status until ",
            "it completes before finishing your turn, because the copy reads your live workspace. A failed or ",
            "interrupted copy may leave some files written; retries with the same request identity never replay it."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "service_id": {"type": "string"},
                "from": {"type": "string", "minLength": 1, "maxLength": 1024},
                "to": {"type": "string", "minLength": 1, "maxLength": 1024},
                "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 1800, "default": 300},
                "wait_seconds": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": commands::MAX_WAIT_SECONDS,
                    "default": commands::default_wait()
                }
            },
            "required": ["service_id", "from", "to"],
            "additionalProperties": false
        },
        "annotations": {"readOnlyHint": false, "destructiveHint": true, "openWorldHint": false}
    })
}

/// Normalizes a `/workspace`-relative path. Each segment must be a plain name:
/// no empty, `.` or `..` segment, absolute path, NUL or control character.
fn relative(path: &str) -> Option<&str> {
    let path = path.strip_prefix("/workspace/").unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);
    (!path.is_empty()
        && path.len() <= 1024
        && path.split('/').count() <= MAX_DEPTH
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | "..") && !segment.chars().any(char::is_control)))
    .then_some(path)
}
/// Admitted tool packages are materialized read-only at
/// `/workspace/.control-tools/<name>` (tool packages, #216). They are the one
/// Control-internal source a copy may read; the rest of each path, and every
/// other `.control-*` path such as mailbox or room files, keeps the publication
/// policy, and tree walks apply the same link, special-file and bound rules.
const TOOL_PACKAGES: &str = ".control-tools/";
fn copyable_source(path: &str) -> bool {
    valid_path(path) || path.strip_prefix(TOOL_PACKAGES).is_some_and(valid_path)
}
impl Transfer {
    pub(super) fn new(from: &str, to: &str) -> io::Result<Self> {
        let from = relative(from)
            .filter(|path| copyable_source(path))
            .ok_or_else(|| fail("from must be a path under /workspace without '..', links or excluded names such as .git, .env or target; .control-tools/<name> is the only Control-internal source"))?;
        let to = relative(to)
            .filter(|path| !path.split('/').any(|segment| segment.starts_with(".control-")))
            .ok_or_else(|| fail("to must be a path under the service's /workspace without '..' or .control-* names"))?;
        Ok(Self {
            from: from.into(),
            to: to.into(),
        })
    }
    fn display(&self) -> String {
        format!("copy /workspace/{} -> service /workspace/{}", self.from, self.to)
    }
}

/// Bounds for one copy. Node settings `service-copy-max-mib` and
/// `service-copy-max-entries`; invalid or absent values keep defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Bounds {
    pub bytes: u64,
    pub entries: u64,
}
impl Bounds {
    pub(super) const DEFAULT: Self = Self {
        bytes: 256 * MIB,
        entries: 20_000,
    };
    pub(super) fn from_settings(setting: impl Fn(&str) -> Option<String>) -> Self {
        let read = |name: &str, range: std::ops::RangeInclusive<u64>| {
            setting(name)
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|value| range.contains(value))
        };
        Self {
            // Below 8 GiB, so every size fits a ustar header field.
            bytes: read("service-copy-max-mib", 1..=8191).map_or(Self::DEFAULT.bytes, |mib| mib * MIB),
            entries: read("service-copy-max-entries", 1..=1_000_000).unwrap_or(Self::DEFAULT.entries),
        }
    }
    /// A memory-backed or capped legacy `/workspace` cannot hold more than its
    /// size. A retained volume has no Control cap; engine disk limits apply.
    pub(super) fn for_service(self, service: &Service) -> Self {
        let cap = match service.storage {
            Storage::Memory => Some(service.profile.service_workspace_mb()),
            Storage::Disk { limit_mb: Some(limit) } => Some(u64::from(limit.get())),
            Storage::Disk { limit_mb: None } | Storage::Workspace { .. } => None,
        };
        Self {
            bytes: cap.map_or(self.bytes, |mb| self.bytes.min(mb * MIB)),
            ..self
        }
    }
}
#[cfg(not(test))]
fn bounds() -> Bounds {
    let directory = crate::config::directory().ok();
    Bounds::from_settings(|name| fs::read_to_string(directory.as_ref()?.join(name)).ok())
}
/// Unit tests never read the operator's node configuration.
#[cfg(test)]
fn bounds() -> Bounds {
    Bounds::DEFAULT
}

/// What one pass admitted and skipped. Paths are relative to the copy source.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Tally {
    pub files: u64,
    pub directories: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub examples: Vec<String>,
}
impl Tally {
    fn skip(&mut self, path: &str, reason: &str) {
        self.skipped += 1;
        if self.examples.len() < SKIPPED_EXAMPLES {
            self.examples.push(format!("{path} ({reason})"));
        }
    }
    fn admit(&mut self, bounds: Bounds, path: &str, bytes: u64, directory: bool) -> io::Result<()> {
        if self.files + self.directories >= bounds.entries {
            return Err(fail(format!(
                "{}: copy exceeds the {}-entry limit",
                shown(path),
                bounds.entries
            )));
        }
        if self.bytes.saturating_add(bytes) > bounds.bytes {
            return Err(fail(format!(
                "{}: copy exceeds the {}-byte limit",
                shown(path),
                bounds.bytes
            )));
        }
        self.bytes += bytes;
        if directory {
            self.directories += 1
        } else {
            self.files += 1
        }
        Ok(())
    }
    pub(super) fn summary(&self, bounds: Bounds) -> Value {
        json!({"files":self.files,"directories":self.directories,"bytes":self.bytes,"skipped":self.skipped,"skipped_examples":self.examples,"limits":{"bytes":bounds.bytes,"entries":bounds.entries}})
    }
    fn report(&self, transfer: &Transfer) -> String {
        let mut text = format!(
            "Copied {} files and {} directories ({} bytes) from /workspace/{} to the service's /workspace/{}.\n",
            self.files, self.directories, self.bytes, transfer.from, transfer.to
        );
        if self.skipped > 0 {
            text.push_str(&format!("Skipped {}: {}\n", self.skipped, self.examples.join(", ")));
        }
        text
    }
}
fn shown(path: &str) -> &str {
    if path.is_empty() { "source" } else { path }
}

enum Kind {
    Directory,
    File,
    Other,
}
fn name(part: &OsStr) -> io::Result<CString> {
    CString::new(part.as_bytes()).map_err(|_| fail("Invalid file name"))
}
/// Classifies one entry without following it.
fn kind_at(directory: &fs::File, part: &OsStr) -> io::Result<Kind> {
    let part = name(part)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            part.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(match unsafe { stat.assume_init() }.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Directory,
        libc::S_IFREG => Kind::File,
        _ => Kind::Other,
    })
}
/// One component under a pinned parent; a symlink fails instead of resolving.
fn open_directory(directory: &fs::File, part: &OsStr) -> io::Result<fs::File> {
    let part = name(part)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            part.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}
fn open_file(directory: &fs::File, part: &OsStr) -> io::Result<(fs::File, fs::Metadata)> {
    let file = diff_open_child(directory, part)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(fail("Entry changed while copying"));
    }
    Ok((file, metadata))
}
fn open_root(root: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(root)
}

pub(super) enum Node<'a> {
    Directory {
        mtime: u64,
    },
    File {
        file: &'a mut fs::File,
        len: u64,
        executable: bool,
        mtime: u64,
    },
}
fn mtime(metadata: &fs::Metadata) -> u64 {
    u64::try_from(metadata.mtime()).unwrap_or(0)
}
/// Visits the source in sorted order with paths relative to it ("" is the
/// source itself), enforcing `bounds` before each visit.
fn traverse(
    root: &Path,
    from: &str,
    bounds: Bounds,
    visit: &mut dyn FnMut(&str, Node<'_>) -> io::Result<()>,
) -> io::Result<Tally> {
    let mut tally = Tally::default();
    let mut directory = open_root(root).map_err(|_| fail("Task workspace is unavailable"))?;
    let parts: Vec<&str> = from.split('/').collect();
    let (last, parents) = parts.split_last().ok_or_else(|| fail("Missing copy source"))?;
    for part in parents {
        directory = open_directory(&directory, OsStr::new(part)).map_err(|_| {
            fail(format!(
                "{from}: every parent must be a directory in the task workspace, not a link"
            ))
        })?;
    }
    let last = OsStr::new(last);
    match kind_at(&directory, last).map_err(|_| fail(format!("{from}: not found in the task workspace")))? {
        Kind::Directory => {
            let child = open_directory(&directory, last)?;
            tally.admit(bounds, "", 0, true)?;
            visit(
                "",
                Node::Directory {
                    mtime: mtime(&child.metadata()?),
                },
            )?;
            descend(&child, "", 1, bounds, &mut tally, visit)?;
        }
        Kind::File => {
            let (mut file, metadata) = open_file(&directory, last)?;
            tally.admit(bounds, "", metadata.len(), false)?;
            visit(
                "",
                Node::File {
                    file: &mut file,
                    len: metadata.len(),
                    executable: metadata.mode() & 0o111 != 0,
                    mtime: mtime(&metadata),
                },
            )?;
        }
        Kind::Other => {
            return Err(fail(format!(
                "{from}: must be a regular file or directory; symlinks are not followed"
            )));
        }
    }
    Ok(tally)
}
fn descend(
    directory: &fs::File,
    prefix: &str,
    depth: usize,
    bounds: Bounds,
    tally: &mut Tally,
    visit: &mut dyn FnMut(&str, Node<'_>) -> io::Result<()>,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(fail(format!(
            "{}: directory depth limit ({MAX_DEPTH}) exceeded",
            shown(prefix)
        )));
    }
    // "." and ".." plus one more than the entry bound: never an unbounded listing.
    let limit = usize::try_from(bounds.entries).unwrap_or(usize::MAX).saturating_add(3);
    let mut names = directory_names(directory, Some(limit))?;
    if names.len() > limit {
        return Err(fail(format!(
            "{}: copy exceeds the {}-entry limit",
            shown(prefix),
            bounds.entries
        )));
    }
    names.sort();
    for part in names {
        if part == "." || part == ".." {
            continue;
        }
        let Some(label) = part.to_str() else {
            tally.skip(&format!("{}/{}", prefix, part.to_string_lossy()), "name is not UTF-8");
            continue;
        };
        let path = if prefix.is_empty() {
            label.to_owned()
        } else {
            format!("{prefix}/{label}")
        };
        if excluded(label) {
            tally.skip(&path, "excluded name");
            continue;
        }
        if label.chars().any(char::is_control) {
            tally.skip(&path, "control character in name");
            continue;
        }
        match kind_at(directory, &part)? {
            Kind::Directory => {
                let child = open_directory(directory, &part)?;
                tally.admit(bounds, &path, 0, true)?;
                visit(
                    &path,
                    Node::Directory {
                        mtime: mtime(&child.metadata()?),
                    },
                )?;
                descend(&child, &path, depth + 1, bounds, tally, visit)?;
            }
            Kind::File => {
                let (mut file, metadata) = open_file(directory, &part)?;
                tally.admit(bounds, &path, metadata.len(), false)?;
                visit(
                    &path,
                    Node::File {
                        file: &mut file,
                        len: metadata.len(),
                        executable: metadata.mode() & 0o111 != 0,
                        mtime: mtime(&metadata),
                    },
                )?;
            }
            Kind::Other => tally.skip(&path, "symlink or special file"),
        }
    }
    Ok(())
}
/// Metadata-only check of the whole source against `bounds`.
pub(super) fn scan(root: &Path, from: &str, bounds: Bounds) -> io::Result<Tally> {
    traverse(root, from, bounds, &mut |_, _| Ok(()))
}
/// Streams the source as a tar archive owned by `owner`. Each file is read
/// from its open descriptor with the size checked when it was opened; a file
/// that shrinks fails the copy, and growth after opening is not copied. On
/// error the archive is left unterminated so the extractor fails too.
pub(super) fn stream(
    root: &Path,
    transfer: &Transfer,
    bounds: Bounds,
    owner: (u32, u32),
    out: impl Write,
) -> io::Result<Tally> {
    let mut tar = Tar {
        out: io::BufWriter::with_capacity(64 * 1024, out),
        owner,
    };
    let tally = traverse(root, &transfer.from, bounds, &mut |path, node| {
        let member = if path.is_empty() {
            transfer.to.clone()
        } else {
            format!("{}/{path}", transfer.to)
        };
        match node {
            Node::Directory { mtime } => tar.header(&format!("{member}/"), b'5', 0o755, 0, mtime),
            Node::File {
                file,
                len,
                executable,
                mtime,
            } => {
                tar.header(&member, b'0', if executable { 0o755 } else { 0o644 }, len, mtime)?;
                let copied = io::copy(&mut io::Read::take(file, len), &mut tar.out)?;
                if copied != len {
                    return Err(fail(format!("{path}: file changed while copying")));
                }
                tar.pad(len)
            }
        }
    })?;
    tar.out.write_all(&[0; 1024])?;
    tar.out.flush()?;
    Ok(tally)
}

/// A minimal POSIX (ustar + pax path) archive writer: directories and regular
/// files only, so the extractor never sees links, devices or absolute names.
struct Tar<W: Write> {
    out: W,
    owner: (u32, u32),
}
impl<W: Write> Tar<W> {
    fn header(&mut self, name: &str, kind: u8, mode: u32, size: u64, mtime: u64) -> io::Result<()> {
        if name.len() > 100 {
            let record = pax_record("path", name);
            self.raw_header("././@PaxHeader", b'x', 0o644, record.len() as u64, 0)?;
            self.out.write_all(record.as_bytes())?;
            self.pad(record.len() as u64)?;
        }
        let short = &name.as_bytes()[..name.len().min(100)];
        self.raw_header(std::str::from_utf8(short).unwrap_or("member"), kind, mode, size, mtime)
    }
    fn raw_header(&mut self, name: &str, kind: u8, mode: u32, size: u64, mtime: u64) -> io::Result<()> {
        let mut block = [0u8; 512];
        block[..name.len()].copy_from_slice(name.as_bytes());
        octal(&mut block[100..108], u64::from(mode));
        octal(&mut block[108..116], u64::from(self.owner.0));
        octal(&mut block[116..124], u64::from(self.owner.1));
        octal(&mut block[124..136], size);
        octal(&mut block[136..148], mtime.min(0o77777777777));
        block[148..156].fill(b' ');
        block[156] = kind;
        block[257..263].copy_from_slice(b"ustar\0");
        block[263..265].copy_from_slice(b"00");
        let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
        block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        self.out.write_all(&block)
    }
    fn pad(&mut self, len: u64) -> io::Result<()> {
        let rest = (512 - len % 512) % 512;
        self.out.write_all(&[0u8; 512][..rest as usize])
    }
}
/// Zero-padded octal digits followed by NUL, filling the field. Values are
/// clamped to the field; sizes are bounded below that by `Bounds`, and owner
/// IDs are informational because the extractor never restores ownership.
fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let value = value.min((1u64 << (3 * digits)) - 1);
    field[..digits].copy_from_slice(format!("{value:0digits$o}").as_bytes());
    field[digits] = 0;
}
/// `"<length> key=value\n"`, where the length counts the whole record.
fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut length = body.len();
    loop {
        let total = body.len() + length.to_string().len();
        if total == length {
            return format!("{length}{body}");
        }
        length = total;
    }
}

/// Admits a copy after checking the source against its bounds outside the
/// state lock. A retried identity returns its recorded outcome without
/// reading the workspace again.
pub(super) fn request(shared: &Store, attempt: &AttemptRef<'_>, node: &NodeId, input: Input) -> io::Result<Value> {
    let wait = commands::wait_bound(input.wait_seconds)?;
    let service_id = input.service_id.clone();
    let admitted = request_with(
        shared,
        attempt,
        node,
        input,
        |generation| crate::runtime::workspace(generation).map_err(fail),
        crate::storage::save,
    )?;
    let execution_id = admitted["execution_id"].as_str().unwrap_or_default().to_owned();
    let waiter = commands::Waiter {
        run: attempt.run,
        generation: attempt.generation,
        node,
        service_id: &service_id,
    };
    Ok(commands::settle(
        shared,
        waiter,
        &execution_id,
        admitted,
        wait,
        &crate::shutdown::token(),
    ))
}
pub(super) fn request_with(
    shared: &Store,
    attempt: &AttemptRef<'_>,
    node: &NodeId,
    input: Input,
    workspace: impl FnOnce(&str) -> io::Result<PathBuf>,
    save: impl FnOnce(&mut State) -> io::Result<()>,
) -> io::Result<Value> {
    let (run, generation, nonce) = (attempt.run, attempt.generation, attempt.nonce);
    let transfer = Transfer::new(&input.from, &input.to)?;
    if !(1..=1800).contains(&input.timeout_seconds) {
        return Err(fail("timeout_seconds must be 1–1800"));
    }
    let digest = commands::hash(&serde_json::to_vec(&("copy", &transfer, input.timeout_seconds)).map_err(fail)?);
    let bounds = {
        let state = shared.guard()?;
        let index = commands::owned(ServiceAuthority::of(&state), run, generation, node, &input.service_id)?;
        if let Some(value) = commands::previous(&state.services[index], run, generation, nonce, &digest)? {
            return Ok(value);
        }
        bounds().for_service(&state.services[index])
    };
    // `generation` is the caller's active attempt, checked by `owned` above.
    let tally = scan(&workspace(generation)?, &transfer.from, bounds)?;
    shared.write(|state| -> io::Result<Value> {
        let work = commands::Work {
            service_id: input.service_id,
            digest,
            command: String::new(),
            display: transfer.display(),
            timeout_seconds: input.timeout_seconds,
            copy: Some(transfer),
        };
        let mut value = commands::admit_work(state, run, generation, nonce, node, work, save)?;
        value["copy"] = tally.summary(bounds);
        Ok(value)
    })?
}

/// A copy ready to launch: its source was rechecked against current bounds
/// while the requesting attempt is still active.
pub(super) struct Job {
    root: PathBuf,
    transfer: Transfer,
    bounds: Bounds,
    owner: (u32, u32),
    outcome: Arc<Mutex<Option<io::Result<Tally>>>>,
}
impl Job {
    pub(super) fn prepare(
        shared: &Store,
        service: &Service,
        run: &str,
        attempt: &str,
        transfer: &Transfer,
    ) -> io::Result<Self> {
        if !shared
            .read(|state| commands::live_attempt(&state.runtime.runs, run, attempt))
            .unwrap_or(false)
        {
            return Err(fail("the requesting attempt ended before the copy started"));
        }
        let root = crate::runtime::workspace(attempt).map_err(fail)?;
        let bounds = bounds().for_service(service);
        scan(&root, &transfer.from, bounds)?;
        Ok(Self {
            root,
            transfer: transfer.clone(),
            bounds,
            owner: (service.uid.get(), service.gid),
            outcome: Default::default(),
        })
    }
    pub(super) fn feed(&self) -> commands::Feed {
        let (root, transfer, bounds, owner, outcome) = (
            self.root.clone(),
            self.transfer.clone(),
            self.bounds,
            self.owner,
            self.outcome.clone(),
        );
        Box::new(move |stdin| {
            let result = stream(&root, &transfer, bounds, owner, stdin);
            if let Ok(mut slot) = outcome.lock() {
                *slot = Some(result);
            }
        })
    }
    /// The summary of a fully streamed archive, or why streaming stopped.
    /// `None` when nothing was streamed.
    pub(super) fn outcome(&self) -> Option<Result<String, String>> {
        let result = self.outcome.lock().ok()?.take()?;
        Some(
            result
                .map(|tally| tally.report(&self.transfer))
                .map_err(|error| error.to_string()),
        )
    }
}

#[cfg(test)]
mod tests;
