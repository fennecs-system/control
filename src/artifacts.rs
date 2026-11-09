//! Bounded, immutable file outputs. Publication is deterministic; content is untrusted.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const FILE_BYTES: u64 = 2 * 1024 * 1024;
const TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const FILE_COUNT: usize = 512;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub files: Vec<Entry>,
    /// Omissions recorded by older versions. New checkpoints reject unpublishable files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<Omitted>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Omitted {
    pub path: String,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub media_type: String,
}

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn root() -> io::Result<PathBuf> {
    crate::config::state("artifacts").map_err(io::Error::other)
}
fn valid_id(id: &str) -> bool {
    crate::chat::valid_id(id)
}
fn excluded(name: &str) -> bool {
    name.starts_with(".control-")
        || matches!(
            name,
            ".git"
                | ".agents"
                | ".codex"
                | ".claude"
                | ".claude.json"
                | ".mcp.json"
                | ".env"
                | "discord.env"
                | "auth.json"
                | "target"
                | "node_modules"
        )
        || name.starts_with(".env.")
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path.split('/').count() <= 32
        && Path::new(path).components().all(
            |c| matches!(c, Component::Normal(name) if name.to_str().is_some_and(|s| !excluded(s))),
        )
}
fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn safe_read(root: &Path, relative: &str, limit: u64) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_path(relative) || !fs::symlink_metadata(root)?.is_dir() {
        return Err(fail("Invalid artifact path"));
    }
    let mut path = root.to_owned();
    let components: Vec<_> = Path::new(relative).components().collect();
    for component in &components[..components.len() - 1] {
        path.push(component.as_os_str());
        if !fs::symlink_metadata(&path)?.is_dir() {
            return Err(fail("Artifact directories must not be symlinks"));
        }
    }
    path.push(components.last().unwrap().as_os_str());
    let flags = libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
        return Err(fail(
            "Artifact exceeds the file limit or is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(fail("Artifact exceeds the file limit"));
    }
    Ok(bytes)
}

/// Best-effort bounded text inspection for chat previews. Use the artifact path
/// rules and no-follow reads; omitted files must never look like deletions.
pub(crate) fn diff_snapshot(root: &Path) -> io::Result<control::diff::Snapshot> {
    use std::os::unix::fs::OpenOptionsExt;
    fn collect(directory: &fs::File, prefix: &str, snapshot: &mut control::diff::Snapshot, total: &mut usize, visited: &mut usize, depth: usize) -> io::Result<()> {
        if depth>32 || *visited>=4096 {snapshot.incomplete=true;return Ok(());}
        let mut entries=diff_directory_names(directory)?;
        if entries.len()>2048 {snapshot.incomplete=true;return Ok(());}
        entries.sort();
        for name in entries {
            *visited+=1;
            if *visited>4096 || *total>=4*1024*1024 || snapshot.files.len()>=512 {snapshot.incomplete=true;break;}
            let Some(label)=name.to_str() else {snapshot.incomplete=true;continue;};
            if label=="." || label==".." || excluded(label)
                || [".ssh",".aws",".config",".secrets",".direnv","dist","credentials.json",".credentials.json",".git-credentials",".netrc","id_rsa","id_ed25519"].contains(&label)
                || [".pem",".key",".p12",".pfx"].iter().any(|suffix|label.ends_with(suffix)) {continue;}
            let relative=if prefix.is_empty(){label.into()}else{format!("{prefix}/{label}")};
            if !valid_path(&relative) {snapshot.incomplete=true;continue;}
            let Ok(file)=diff_open_child(directory,&name) else {snapshot.skipped.insert(relative);continue;};
            let metadata=file.metadata()?;
            if metadata.is_dir() {
                if collect(&file,&relative,snapshot,total,visited,depth+1).is_err() {snapshot.incomplete=true;}
            } else if metadata.is_file() && metadata.len()<=128*1024 {
                let mut bytes=Vec::new();
                let read=file.take(128*1024+1).read_to_end(&mut bytes);
                let text=if read.is_ok() && bytes.len()<=128*1024 && !bytes.contains(&0) {String::from_utf8(bytes).ok()}else{None};
                match text {
                    Some(text) if *total+text.len()<=4*1024*1024 => {*total+=text.len();snapshot.files.insert(relative,text);}
                    _ => {snapshot.skipped.insert(relative);}
                }
            } else {snapshot.skipped.insert(relative);}
        }
        Ok(())
    }
    let directory=fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_DIRECTORY|libc::O_CLOEXEC).open(root)?;
    let mut snapshot=control::diff::Snapshot::default();
    collect(&directory,"",&mut snapshot,&mut 0,&mut 0,0)?;
    Ok(snapshot)
}

pub(crate) fn diff_open_child(directory: &fs::File, name: &std::ffi::OsStr) -> io::Result<fs::File> {
    use std::os::{fd::{AsRawFd,FromRawFd},unix::ffi::OsStrExt};
    let name=std::ffi::CString::new(name.as_bytes()).map_err(|_|fail("Invalid diff filename"))?;
    // One relative component and a pinned parent descriptor: no intermediate
    // symlink can redirect a live workspace read into the host filesystem.
    let fd=unsafe{libc::openat(directory.as_raw_fd(),name.as_ptr(),libc::O_RDONLY|libc::O_NOFOLLOW|libc::O_NONBLOCK|libc::O_CLOEXEC)};
    if fd<0 {return Err(io::Error::last_os_error());}
    Ok(unsafe{fs::File::from_raw_fd(fd)})
}
pub(crate) fn diff_directory_names(directory: &fs::File) -> io::Result<Vec<std::ffi::OsString>> {
    directory_names(directory, Some(2050))
}
pub(crate) fn directory_names(directory: &fs::File, limit: Option<usize>) -> io::Result<Vec<std::ffi::OsString>> {
    use std::os::{fd::IntoRawFd,unix::ffi::OsStrExt};
    let fd=directory.try_clone()?.into_raw_fd();
    // fdopendir owns the duplicated descriptor only on success.
    let dir=unsafe{libc::fdopendir(fd)};
    if dir.is_null() {let error=io::Error::last_os_error();unsafe{libc::close(fd)};return Err(error);}
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {fn drop(&mut self){unsafe{libc::closedir(self.0)};}}
    let dir=Directory(dir);
    let mut names=Vec::new();
    loop {
        #[cfg(target_os="macos")]
        let errno=unsafe{libc::__error()};
        #[cfg(target_os="linux")]
        let errno=unsafe{libc::__errno_location()};
        unsafe{*errno=0};
        let entry=unsafe{libc::readdir(dir.0)};
        if entry.is_null() {
            if unsafe{*errno}!=0 {return Err(io::Error::last_os_error());}
            break;
        }
        let name=unsafe{std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())};
        names.push(std::ffi::OsStr::from_bytes(name.to_bytes()).to_owned());
        if limit.is_some_and(|limit|names.len()>limit) {break;}
    }
    Ok(names)
}
/// Write without syncing; callers sync the whole tree once with `sync_tree`
/// before any rename or pointer makes it durable-visible.
fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    fs::DirBuilder::new().recursive(true).mode(0o700).create(
        path.parent()
            .ok_or_else(|| fail("Invalid artifact destination"))?,
    )?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}
/// Generated or environment directories that are never published (as in `runtime::snapshot`).
/// Kept out of `excluded` so older manifests that contain them remain valid.
fn skipped_directory(name: &str) -> bool {
    matches!(name, "dist" | "pkg" | ".venv" | "__pycache__")
}
const OMITTED_LIMIT: usize = 128;
fn omit(manifest: &mut Manifest, path: String, reason: &str) {
    if manifest.omitted.len() < OMITTED_LIMIT {
        manifest.omitted.push(Omitted { path, reason: reason.into() });
    } else if manifest.omitted.len() == OMITTED_LIMIT {
        manifest.omitted.push(Omitted { path: "…".into(), reason: "Further omissions not listed".into() });
    }
}
fn walk(
    source: &Path,
    directory: &Path,
    destination: &Path,
    manifest: &mut Manifest,
    total: &mut u64,
    visited: &mut usize,
    depth: usize,
) -> io::Result<()> {
    let label = |path: &Path| path.strip_prefix(source).unwrap_or(path).to_string_lossy().into_owned();
    if depth > 32 {
        return Err(fail(&format!("{}: directory depth limit (32) exceeded", label(directory))));
    }
    let mut entries = fs::read_dir(directory)?
        .take(FILE_COUNT * 4 + 1)
        .collect::<io::Result<Vec<_>>>()?;
    if entries.len() > FILE_COUNT * 4 {
        return Err(fail(&format!("{}: more than {} directory entries", label(directory), FILE_COUNT * 4)));
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        *visited += 1;
        if *visited > FILE_COUNT * 8 {
            return Err(fail(&format!("{}: tree entry limit ({}) exceeded", label(&entry.path()), FILE_COUNT * 8)));
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            omit(manifest, label(&entry.path()), "Filename is not UTF-8");
            continue;
        };
        if excluded(name) {
            continue;
        }
        // DirEntry::file_type does not follow symlinks; neither does safe_read.
        let kind = entry.file_type()?;
        let relative = label(&entry.path());
        if kind.is_dir() {
            if !skipped_directory(name) {
                walk(source, &entry.path(), destination, manifest, total, visited, depth + 1)?;
            }
        } else if !kind.is_file() {
            omit(manifest, relative, "Symlink or special file");
        } else if !valid_path(&relative) {
            omit(manifest, relative, "Path is not publishable");
        } else {
            let length = entry.metadata()?.len();
            if length > FILE_BYTES {
                omit(manifest, relative, "Larger than the 2 MiB file limit");
            } else if manifest.files.len() >= FILE_COUNT {
                omit(manifest, relative, "Over the 512-file limit");
            } else if *total + length > TOTAL_BYTES {
                omit(manifest, relative, "Over the 16 MiB total limit");
            } else {
                let bytes = safe_read(source, &relative, FILE_BYTES)
                    .map_err(|e| fail(&format!("{relative}: {e}")))?;
                if *total + bytes.len() as u64 > TOTAL_BYTES {
                    omit(manifest, relative, "Over the 16 MiB total limit");
                    continue;
                }
                *total += bytes.len() as u64;
                private_write(&destination.join(&relative), &bytes)?;
                manifest.files.push(Entry {
                    path: relative,
                    bytes: bytes.len() as u64,
                    sha256: hash(&bytes),
                    media_type: if std::str::from_utf8(&bytes).is_ok() && !bytes.contains(&0) {
                        "text/plain"
                    } else {
                        "application/octet-stream"
                    }
                    .into(),
                });
            }
        }
    }
    Ok(())
}
pub(crate) fn publish_in(root: &Path, source: &Path, id: &str) -> io::Result<Manifest> {
    if !valid_id(id) {
        return Err(fail("Invalid artifact ID"));
    }
    let staging = root.join(format!("{id}.staging"));
    fs::create_dir_all(root)?;
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let mut manifest = Manifest {
        id: id.into(),
        files: Vec::new(),
        omitted: Vec::new(),
    };
    let mut total = 0;
    let mut visited = 0;
    walk(
        source,
        source,
        &staging.join("files"),
        &mut manifest,
        &mut total,
        &mut visited,
        0,
    )
    .map_err(|e| fail(&format!("{e}. Outputs are retained in {}.", source.display())))?;
    // This manifest is also the next turn's workspace, not just a viewer index.
    // Never promote a partial checkpoint: even an unchanged file could otherwise
    // disappear on restore when another file pushes the workspace over a limit.
    if let Some(omitted) = manifest.omitted.first() {
        return Err(fail(&format!(
            "{}: {}. Cannot publish an incomplete workspace checkpoint. Outputs are retained in {}.",
            omitted.path, omitted.reason, source.display()
        )));
    }
    manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
    let bytes = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    if bytes.len() > 256 * 1024 {
        return Err(fail("Artifact manifest limit exceeded"));
    }
    private_write(&staging.join("manifest.json"), &bytes)?;
    // Publish files before any durable chat pointer refers to them: one batched
    // sync of every file and directory, then the atomic rename.
    sync_tree(&staging)?;
    if root.join(id).exists() {
        return Err(fail("Artifact ID already published"));
    }
    fs::rename(staging, root.join(id))?;
    fs::File::open(root)?.sync_all()?;
    Ok(manifest)
}
pub(crate) fn sync_tree(directory: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // One filesystem-wide flush instead of an fsync per file.
        use std::os::fd::AsRawFd;
        let handle = fs::File::open(directory)?;
        if unsafe { libc::syncfs(handle.as_raw_fd()) } == 0 {
            return Ok(());
        }
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            fs::File::open(entry.path())?.sync_all()?;
        }
    }
    fs::File::open(directory)?.sync_all()
}
pub fn publish(source: &Path, id: &str) -> io::Result<Manifest> {
    publish_in(&root()?, source, id)
}
pub fn manifest(id: &str) -> io::Result<Manifest> {
    manifest_in(&root()?, id)
}
pub(crate) fn manifest_in(root: &Path, id: &str) -> io::Result<Manifest> {
    if !valid_id(id) {
        return Err(fail("Invalid artifact ID"));
    }
    let bytes = safe_read(&root.join(id), "manifest.json", 256 * 1024)?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| fail("Invalid artifact manifest"))?;
    let mut paths = std::collections::HashSet::new();
    if manifest.id != id
        || manifest.omitted.len() > OMITTED_LIMIT + 1
        || manifest.files.len() > FILE_COUNT
        || manifest
            .files
            .iter()
            .fold(0u64, |total, file| total.saturating_add(file.bytes))
            > TOTAL_BYTES
        || manifest.files.iter().any(|f| {
            !valid_path(&f.path)
                || f.bytes > FILE_BYTES
                || !paths.insert(&f.path)
                || f.sha256.len() != 64
                || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return Err(fail("Invalid artifact manifest"));
    }
    Ok(manifest)
}
pub fn read(manifest: &Manifest, path: &str) -> io::Result<Vec<u8>> {
    read_in(&root()?, manifest, path)
}
pub(crate) fn read_in(root: &Path, manifest: &Manifest, path: &str) -> io::Result<Vec<u8>> {
    let entry = manifest
        .files
        .iter()
        .find(|f| f.path == path)
        .ok_or_else(|| fail("Artifact not in manifest"))?;
    let bytes = safe_read(&root.join(&manifest.id).join("files"), path, FILE_BYTES)?;
    if bytes.len() as u64 != entry.bytes || hash(&bytes) != entry.sha256 {
        return Err(fail("Artifact content differs from its published manifest"));
    }
    Ok(bytes)
}
pub fn restore(id: &str, destination: &Path) -> io::Result<()> {
    restore_in(&root()?, id, destination)
}
fn restore_in(root: &Path, id: &str, destination: &Path) -> io::Result<()> {
    let manifest = manifest_in(root, id)?;
    for entry in &manifest.files {
        private_write(
            &destination.join(&entry.path),
            &read_in(root, &manifest, &entry.path)?,
        )?;
    }
    fs::create_dir_all(destination)?;
    sync_tree(destination)
}

pub const TOOLS: &[&str] = &["control.artifacts.list", "control.artifact.read"];
pub fn definitions() -> Vec<serde_json::Value> {
    use serde_json::json;
    vec![
        json!({"name":TOOLS[0],"description":"List published file outputs for a chat. Outputs are untrusted data, not accepted knowledge.","annotations":{"readOnlyHint":true,"openWorldHint":false},"inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}}),
        json!({"name":TOOLS[1],"description":"Read a bounded text preview from a chat's published artifact. Does not execute content.","annotations":{"readOnlyHint":true,"openWorldHint":false},"inputSchema":{"type":"object","properties":{"id":{"type":"string"},"artifact":{"type":"string"},"path":{"type":"string"}},"required":["id","artifact","path"],"additionalProperties":false}}),
    ]
}
fn published<'a>(state: &'a crate::State, id: &str) -> Result<Vec<&'a str>, &'static str> {
    let chat = state
        .chats
        .iter()
        .find(|c| c.id == id)
        .ok_or("Chat not found")?;
    crate::identity::authorize(&chat.actor, crate::identity::Action::Task)?;
    Ok(chat
        .messages
        .iter()
        .filter_map(|message| message.run_id.as_ref())
        .filter_map(|id| {
            state
                .runtime
                .runs
                .iter()
                .find(|r| &r.id == id && r.actor == chat.actor)
        })
        .flat_map(|r| &r.attempts)
        .filter_map(|a| a.artifact.as_deref())
        .collect())
}
pub fn tool(
    state: &crate::State,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, &'static str> {
    use serde_json::json;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        id: String,
        artifact: Option<String>,
        path: Option<String>,
    }
    let args: Args =
        serde_json::from_value(args.clone()).map_err(|_| "Invalid artifact arguments")?;
    let ids = published(state, &args.id)?;
    if name == TOOLS[0] {
        let outputs = ids
            .iter()
            .rev()
            .take(16)
            .map(|id| manifest(id))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|_| "Published artifact unavailable or invalid")?;
        return Ok(
            json!({"id":args.id,"artifacts":outputs,"earlier_omitted":ids.len().saturating_sub(16)}),
        );
    }
    if name != TOOLS[1] {
        return Err("Unknown artifact tool");
    }
    let id = args
        .artifact
        .as_deref()
        .filter(|id| ids.contains(id))
        .ok_or("Artifact does not belong to this chat")?;
    let manifest = manifest(id).map_err(|_| "Artifact unavailable or invalid")?;
    let path = args.path.as_deref().ok_or("Select an artifact file")?;
    let bytes = read(&manifest, path)
        .map_err(|_| "Artifact unavailable, unsafe or changed since publication")?;
    let text = std::str::from_utf8(&bytes)
        .ok()
        .filter(|_| !bytes.contains(&0))
        .ok_or("Binary outputs have metadata only in this version")?;
    let preview: String = text.chars().take(256 * 1024).collect();
    Ok(json!({"artifact":id,"path":path,"text":preview,"truncated":preview.len()<text.len()}))
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// Empty checkpoints still restore workspaces, but are not file outputs. Keep
// them in durable state and in the explicit read API; omit them from the viewer.
fn visible_outputs<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    mut load: impl FnMut(&str) -> io::Result<Manifest>,
) -> io::Result<Vec<Manifest>> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(*id))
        .map(&mut load)
        .filter_map(|result| match result {
            Ok(manifest) if manifest.files.is_empty() => None,
            other => Some(other),
        })
        .collect()
}
pub fn render(
    state: &crate::State,
    chat: &str,
    args: &std::collections::HashMap<String, String>,
) -> Result<String, &'static str> {
    use crate::escape;
    let ids = published(state, chat)?;
    let outputs = visible_outputs(ids.iter().copied(), manifest)
        .map_err(|_| "Artifact unavailable or invalid")?;
    let selected = args
        .get("artifact")
        .map(String::as_str)
        .or_else(|| outputs.last().map(|output| output.id.as_str()));
    let mut html = format!("<section class='artifacts'><div class='section-heading'><h2>Artifacts</h2><a href='/chat/{chat}'>Back to chat ↗</a></div>");
    let Some(id) = selected.filter(|id| ids.contains(id)) else {
        html.push_str("<p class='muted'>No published files yet.</p></section>");
        return Ok(html);
    };
    let manifest = manifest(id).map_err(|_| "Artifact unavailable or invalid")?;
    html.push_str("<nav class='artifact-versions' aria-label='Output versions'>");
    for (index, output) in outputs.iter().enumerate().rev().take(16) {
        let version = output.id.as_str();
        html.push_str(&format!(
            "<a href='/chat/{chat}/artifacts?artifact={version}'{}>Output {}</a>",
            if version == id {
                " aria-current='page'"
            } else {
                ""
            },
            index + 1
        ));
    }
    html.push_str("</nav><div class='artifact-layout'><nav class='artifact-files' aria-label='Artifact files'>");
    let file = args.get("file").map(String::as_str);
    for entry in &manifest.files {
        html.push_str(&format!(
            "<a href='/chat/{chat}/artifacts?artifact={id}&amp;file={}'{}>{}</a>",
            encode(&entry.path),
            if file == Some(entry.path.as_str()) {
                " aria-current='page'"
            } else {
                ""
            },
            escape(&entry.path)
        ));
    }
    html.push_str("</nav><div class='artifact-viewer'>");
    if let Some(path) = file {
        html.push_str(&format!("<h3>{}</h3>", escape(path)));
        match tool(
            state,
            TOOLS[1],
            &serde_json::json!({"id":chat,"artifact":id,"path":path}),
        ) {
            Ok(value) => {
                if value["truncated"] == true {
                    html.push_str("<p class='muted'>Preview truncated.</p>");
                }
                html.push_str(&format!(
                    "<pre>{}</pre>",
                    escape(value["text"].as_str().unwrap_or(""))
                ));
            }
            Err(message) => html.push_str(&format!("<p class='muted'>{}</p>", escape(message))),
        }
    } else {
        html.push_str("<p class='muted'>Choose a file.</p>");
    }
    html.push_str("</div></div></section>");
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_diff_reads_stay_inside_pinned_directories_and_skip_private_outputs() {
        let root=std::env::temp_dir().join(format!("control-diff-safe-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let workspace=root.join("workspace");let outside=root.join("outside");
        fs::create_dir_all(workspace.join("src")).unwrap();fs::create_dir_all(&outside).unwrap();
        fs::write(workspace.join("src/main.rs"),"inside\n").unwrap();
        fs::write(outside.join("main.rs"),"outside-fixture\n").unwrap();
        fs::write(workspace.join(".env"),"private-fixture").unwrap();
        fs::write(workspace.join("private.key"),"private-key-fixture").unwrap();
        fs::write(workspace.join("binary.dat"),[0,1,2]).unwrap();
        fs::write(workspace.join("large.txt"),vec![b'x';128*1024+1]).unwrap();
        std::os::unix::fs::symlink(&outside,workspace.join("linked")).unwrap();
        let snapshot=diff_snapshot(&workspace).unwrap();
        assert_eq!(snapshot.files.len(),1);
        assert_eq!(snapshot.files["src/main.rs"],"inside\n");
        assert!(snapshot.skipped.contains("linked") && snapshot.skipped.contains("binary.dat") && snapshot.skipped.contains("large.txt"));
        let pinned=fs::File::open(workspace.join("src")).unwrap();
        fs::rename(workspace.join("src"),workspace.join("original-src")).unwrap();
        std::os::unix::fs::symlink(&outside,workspace.join("src")).unwrap();
        let mut text=String::new();
        diff_open_child(&pinned,std::ffi::OsStr::new("main.rs")).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text,"inside\n");
        let snapshot=diff_snapshot(&workspace).unwrap();
        assert!(snapshot.files.values().all(|text| !text.contains("outside-fixture")));
        assert!(diff_snapshot(&workspace.join("src")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn output_browser_omits_empty_checkpoints_and_duplicate_versions() {
        let outputs = visible_outputs(["empty", "file", "empty", "file"], |id| {
            Ok(Manifest {
                id: id.into(),
                files: if id == "file" { vec![Entry {
                    path: "empty-but-real.txt".into(), bytes: 0,
                    sha256: "0".repeat(64), media_type: "text/plain".into(),
                }] } else { vec![] },
                omitted: vec![],
            })
        }).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].files[0].bytes, 0);
        assert!(visible_outputs(["broken"], |_| Err(fail("bad manifest"))).is_err());
    }
    #[test]
    fn artifact_tools_keep_chat_authority_and_reject_actor_overrides() {
        let mut state = crate::State::default();
        let first = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &first, "create an output", "test").unwrap();
        state.runtime.runs[0].attempts.push(crate::usage::Attempt {
            artifact: Some("output-first-chat".into()),
            ..Default::default()
        });
        let second = crate::chat::create(&mut state);
        let error = tool(
            &state,
            TOOLS[1],
            &serde_json::json!({"id":second,"artifact":"output-first-chat","path":"file.txt"}),
        )
        .unwrap_err();
        assert_eq!(error, "Artifact does not belong to this chat");
        state.chats[1].actor = crate::identity::Actor::Unknown;
        assert!(tool(&state, TOOLS[0], &serde_json::json!({"id":second})).is_err());
        assert!(tool(
            &state,
            TOOLS[0],
            &serde_json::json!({"id":second,"actor":{"type":"local_operator"}})
        )
        .is_err());
        for definition in definitions() {
            assert_eq!(definition["annotations"]["readOnlyHint"], true);
            assert_eq!(definition["annotations"]["openWorldHint"], false);
        }
    }
    #[test]
    fn followup_restores_published_files_and_detects_corruption() {
        let directory =
            std::env::temp_dir().join(format!("control-artifact-lineage-{}", std::process::id()));
        fs::create_dir_all(directory.join("first/src")).unwrap();
        fs::write(directory.join("first/src/main.rs"), "first turn").unwrap();
        let outputs = directory.join("outputs");
        publish_in(&outputs, &directory.join("first"), "attempt-1").unwrap();
        restore_in(&outputs, "attempt-1", &directory.join("second")).unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("second/src/main.rs")).unwrap(),
            "first turn"
        );
        fs::write(directory.join("second/src/main.rs"), "second turn").unwrap();
        publish_in(&outputs, &directory.join("second"), "attempt-2").unwrap();
        restore_in(&outputs, "attempt-2", &directory.join("after-restart")).unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("after-restart/src/main.rs")).unwrap(),
            "second turn"
        );
        let first = manifest_in(&outputs, "attempt-1").unwrap();
        assert_eq!(
            read_in(&outputs, &first, "src/main.rs").unwrap(),
            b"first turn"
        );
        fs::write(outputs.join("attempt-1/files/src/main.rs"), "changed").unwrap();
        assert!(read_in(&outputs, &first, "src/main.rs").is_err());
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn publication_is_bounded_sorted_and_never_promotes_symlinks_or_private_files() {
        let root = std::env::temp_dir().join(format!("control-artifacts-{}", std::process::id()));
        fs::create_dir_all(root.join("source/src")).unwrap();
        fs::write(root.join("source/src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("source/.env"), "not-a-real-secret").unwrap();
        fs::write(root.join("source/.control-result"), "harness-only").unwrap();
        let manifest = publish_in(&root.join("output"), &root.join("source"), "test-1").unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "src/main.rs");
        assert_eq!(manifest.files[0].sha256, hash(b"fn main() {}\n"));
        assert!(!root.join("output/test-1/files/.env").exists());
        assert!(publish_in(&root.join("output"), &root.join("source"), "test-1").is_err());
        std::os::unix::fs::symlink("/etc/passwd", root.join("source/link")).unwrap();
        // Generated output is excluded by policy; unsafe project entries reject publication.
        fs::create_dir_all(root.join("source/dist")).unwrap();
        fs::write(root.join("source/dist/bundle.js"), "built").unwrap();
        let error = publish_in(&root.join("output"), &root.join("source"), "test-2").err().unwrap().to_string();
        assert!(error.contains("link: Symlink or special file"));
        assert!(!root.join("output/test-2").exists());
        assert!(safe_read(&root.join("source"), "../source/src/main.rs", FILE_BYTES).is_err());
        assert!(safe_read(&root.join("source"), "link", FILE_BYTES).is_err());
        fs::remove_file(root.join("source/link")).unwrap();
        fs::File::create(root.join("source/large"))
            .unwrap()
            .set_len(FILE_BYTES + 1)
            .unwrap();
        let error = publish_in(&root.join("output"), &root.join("source"), "test-3").err().unwrap().to_string();
        assert!(error.contains("large: Larger than the 2 MiB file limit"));
        assert!(!root.join("output/test-3").exists());
        fs::remove_file(root.join("source/large")).unwrap();
        for n in 0..9 {
            fs::File::create(root.join(format!("source/big-{n}"))).unwrap().set_len(FILE_BYTES).unwrap();
        }
        let error = publish_in(&root.join("output"), &root.join("source"), "test-4").err().unwrap().to_string();
        assert!(error.contains("16 MiB total limit"));
        assert!(!root.join("output/test-4").exists());
        restore_in(&root.join("output"), "test-1", &root.join("restored")).unwrap();
        assert_eq!(fs::read(root.join("restored/src/main.rs")).unwrap(), b"fn main() {}\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incomplete_turn_never_replaces_the_restorable_project_checkpoint() {
        let root = std::env::temp_dir().join(format!("control-checkpoint-limits-{}", std::process::id()));
        let source = root.join("source");
        let outputs = root.join("outputs");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("project.txt"), "original project data").unwrap();
        publish_in(&outputs, &source, "before").unwrap();
        let workspace = root.join("turn-two");
        restore_in(&outputs, "before", &workspace).unwrap();
        fs::OpenOptions::new().write(true).open(workspace.join("project.txt")).unwrap().set_len(FILE_BYTES + 1).unwrap();
        let error = publish_in(&outputs, &workspace, "oversized").err().unwrap().to_string();
        assert!(error.contains("project.txt") && error.contains("incomplete workspace checkpoint"));
        assert!(!outputs.join("oversized").exists());
        assert_eq!(fs::metadata(workspace.join("project.txt")).unwrap().len(), FILE_BYTES + 1);
        fs::write(workspace.join("project.txt"), "updated project data").unwrap();
        for n in 0..FILE_COUNT {
            fs::write(workspace.join(format!("file-{n:04}")), "x").unwrap();
        }
        let error = publish_in(&outputs, &workspace, "too-many").err().unwrap().to_string();
        assert!(error.contains("512-file limit"));
        assert!(!outputs.join("too-many").exists());
        let next = root.join("next-turn");
        restore_in(&outputs, "before", &next).unwrap();
        assert_eq!(fs::read(next.join("project.txt")).unwrap(), b"original project data");
        fs::remove_dir_all(root).unwrap();
    }
}
