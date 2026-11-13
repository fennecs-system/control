//! Operator-approved project resources. Models can request access, never grant it.
//! Host Git only touches the initial operator-approved source and fresh metadata;
//! subsequent agent-written repositories are copied without executing host Git.
use crate::{identity::Action, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Mount {
    pub name: String,
    pub path: String,
    pub message_index: Option<usize>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Project {
    pub request: String,
    pub path: String,
    pub grant: Option<String>,
    pub head: Option<String>,
    #[serde(default)]
    pub message_index: Option<usize>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub pending: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    #[serde(default)]
    pub named: bool,
}
impl Project {
    fn needs_approval(&self)->bool {self.pending||self.grant.is_none()}
    pub fn approved(&self)->Option<Self> {
        self.grant.as_ref()?;
        let mut inherited=self.clone();
        inherited.pending=false;inherited.error=None;inherited.message_index=None;
        if let Some(mount)=inherited.mounts.last() {inherited.path=mount.path.clone();inherited.name=mount.name.clone();}
        for mount in &mut inherited.mounts {mount.message_index=None;}
        Some(inherited)
    }
}
fn valid_name(name:&str)->bool {
    !name.is_empty()&&name.len()<=128&&!name.starts_with('.')&&!skip(name,false)&&name.bytes().all(|c|c.is_ascii_alphanumeric()||b"-_.".contains(&c))
}
fn fail(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
fn root() -> io::Result<PathBuf> {
    crate::config::state("projects").map_err(fail)
}
fn nonce() -> String {
    format!(
        "p-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
pub fn request(state: &mut State, id: &str, path: &str) -> Result<Value, &'static str> {
    request_named(state,id,path,None)
}
pub fn request_named(state: &mut State, id: &str, path: &str, name:Option<&str>) -> Result<Value, &'static str> {
    let path = path.trim();
    if path.len() > 4096 || !Path::new(path).is_absolute() || path.contains(['\n', '\r', '\0', ','])
    {
        return Err("Use /open followed by an absolute path on the Control node.");
    }
    let chat = state
        .chats
        .iter()
        .find(|c| c.id == id)
        .ok_or("Chat not found.")?;
    crate::identity::authorize(&chat.actor, Action::Task)?;
    let name=name.filter(|s|!s.is_empty()).or_else(||Path::new(path).file_name().and_then(|s|s.to_str())).ok_or("Choose a named project folder.")?;
    if !valid_name(name) {return Err("Folder names must use letters, numbers, dots, dashes or underscores, without a leading dot. Use /open --as NAME /absolute/path.");}
    let mut mounts=chat.project.as_ref().map(|p|p.mounts.clone()).unwrap_or_default();
    if let Some(previous)=chat.project.as_ref().filter(|p|p.grant.is_some()&&!p.named&&p.mounts.is_empty()) {
        let name=Path::new(&previous.path).file_name().and_then(|s|s.to_str()).filter(|s|valid_name(s)).unwrap_or("project");
        mounts.push(Mount{name:name.into(),path:previous.path.clone(),message_index:previous.message_index});
    }
    if mounts.len()>=16 {return Err("This chat already has 16 open folders.");}
    let project = Project {
        request: nonce(),
        path: path.into(),
        message_index: chat.messages.len().checked_sub(1),
        pending:true,
        name:name.into(),
        grant:chat.project.as_ref().and_then(|p|p.grant.clone()),
        head:chat.project.as_ref().and_then(|p|p.head.clone()),
        mounts,
        named:chat.project.as_ref().is_some_and(|p|p.named),
        ..Project::default()
    };
    let result = json!({"approval_required":true,"request":project.request,"path":project.path,"mount":format!("/workspace/{}",project.name),"url":format!("/chat/{id}/project")});
    let chat = state.chats.iter_mut().find(|c| c.id == id).unwrap();
    if chat.messages.is_empty() {
        chat.title = Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    chat.project = Some(project);
    chat.revision = chat.revision.saturating_add(1);
    Ok(result)
}
pub fn definition(worker: bool) -> Value {
    json!({"name":if worker {"control_project_request"} else {"control.project.request"},"description":"Request an operator-approved folder at /workspace/name on this node. Existing approved folders remain available. Does not read or authorize the path; the operator must confirm in Control. Supply name to resolve duplicate folder names.","annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false},"inputSchema":{"type":"object","properties":{"id":{"type":"string"},"path":{"type":"string","maxLength":4096},"name":{"type":"string","maxLength":128}},"required":if worker {json!(["path"])}else{json!(["id","path"])},"additionalProperties":false}})
}
pub fn panel(state: &State, id: &str) -> String {
    let Some(chat) = state.chats.iter().find(|c| c.id == id) else {
        return String::new();
    };
    let path = chat.project.as_ref().map(|p| p.path.as_str()).unwrap_or("");
    let mut html = format!(
        "<details class='project-picker' data-transcript-key='project-{}-{}'{}><summary>{}</summary>",
        chat.project.as_ref().map(|p|p.request.as_str()).unwrap_or("new"),
        if chat.project.as_ref().is_some_and(|p|!p.needs_approval()) {"approved"}else{"pending"},
        if chat.project.as_ref().is_some_and(|p| p.needs_approval()) {
            " open"
        } else {
            ""
        },
        if chat.project.as_ref().is_some_and(|p| !p.needs_approval()) {
            "Folder opened"
        } else {
            "Open folder"
        }
    );
    if let Some(project) = &chat.project {
        html.push_str(&format!(
            "<p><code>{}</code></p>",
            crate::escape(&project.path)
        ));
        if let Some(error)=&project.error {html.push_str(&format!("<p role='alert'>{}</p>",crate::escape(error)));}
        if !project.needs_approval() {
            html.push_str("<p class='muted'>Changes stay in this chat’s worktree. The original folder is unchanged.</p>");
            if let Some(workspace) = project
                .head
                .as_deref()
                .and_then(|id| crate::runtime::workspace(id).ok())
            {
                html.push_str(&format!(
                    "<p><code>{}</code></p>",
                    crate::escape(&workspace.display().to_string())
                ));
            }
        } else {
            html.push_str(&format!("<p>Open as <code>/workspace/{}</code></p><form method='post' action='/chat/{id}/project' target='_top'><input type='hidden' name='request' value='{}'><p>Trust this folder’s code for this chat and allow local previews? Control will copy its files into an isolated worktree. Credentials, generated directories and symlinks are excluded. The original folder stays unchanged.</p><button name='approve' value='yes'>Trust folder and open</button></form><form method='post' action='/chat/{id}/project' target='_top'><label>Folder on this node<input name='path' value='{}' required></label><label>Workspace name<input name='name' value='{}' required></label><button>Change folder</button></form>",crate::escape(&project.name),crate::escape(&project.request),crate::escape(&project.path),crate::escape(&project.name)));
        }
    } else {
        html.push_str(&format!("<form method='post' action='/chat/{id}/project' target='_top'><label>Folder on this node<input name='path' value='{}' placeholder='/absolute/path' required></label><button>Continue</button></form>",crate::escape(path)));
    }
    html.push_str("</details>");
    html
}
pub fn message_index(chat: &crate::chat::Session) -> Option<usize> {
    let project=chat.project.as_ref()?;
    project.message_index.filter(|index|*index<chat.messages.len()).or_else(||
        chat.messages.iter().rposition(|m|m.text.trim().strip_prefix("/open").is_some_and(|p|p.trim()==project.path))
    ).or_else(||chat.messages.len().checked_sub(1))
}
fn git(args: &[&std::ffi::OsStr]) -> io::Result<()> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    if command.status()?.success() {
        Ok(())
    } else {
        Err(fail("Could not prepare an isolated Git worktree; check that Git is installed and this is a readable checkout with a commit."))
    }
}
fn skip(name: &str, metadata: bool) -> bool {
    (name.starts_with(".control-") && !(metadata && name == ".control-git"))
        || (!metadata && name == ".git")
        || matches!(
            name,
            ".env"
                | "discord.env"
                | "auth.json"
                | ".agents"
                | ".codex"
                | ".claude"
                | ".claude.json"
                | ".mcp.json"
                | ".ssh"
                | ".aws"
                | ".config"
                | ".secrets"
                | ".direnv"
                | "credentials.json"
                | ".credentials.json"
                | ".git-credentials"
                | ".netrc"
                | "id_rsa"
                | "id_ed25519"
                | "target"
                | "node_modules"
                | "dist"
                | ".venv"
                | "result"
        )
        || name.starts_with(".env.")
}
/// Imports and published checkpoints must be durable before being referenced.
pub(crate) fn copy_tree(source: &Path, destination: &Path, metadata: bool) -> io::Result<()> {
    copy_tree_with(source, destination, metadata, Durability::Checkpoint)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Durability { WorkingCopy, Checkpoint }
/// Descriptor-relative no-follow reads prevent a source path from redirecting a
/// copy into another host directory. Preserve executable bits, never hardlinks.
fn copy_tree_with(source: &Path, destination: &Path, metadata: bool, durability: Durability) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fn walk(
        source: &fs::File,
        destination: &Path,
        metadata: bool,
        depth: usize,
        git_metadata: bool,
        durability: Durability,
    ) -> io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if depth > 64 {
            return Err(fail("Project directory depth exceeds 64"));
        }
        fs::DirBuilder::new().mode(0o700).create(destination)?;
        let mut names = crate::artifacts::directory_names(source, None)?;
        names.sort();
        for name in names {
            if name == "." || name == ".." {
                continue;
            }
            let label = name
                .to_str()
                .ok_or_else(|| fail("Project filenames must be UTF-8"))?;
            if !git_metadata && skip(label, metadata) {
                continue;
            }
            let mut input=match crate::artifacts::diff_open_child(source,&name) {
                Ok(input)=>input,
                // openat is relative to the pinned parent and never follows links.
                // Both dangling and directory links return ELOOP on supported hosts.
                Err(error) if error.raw_os_error()==Some(libc::ELOOP)=>continue,
                Err(_)=>return Err(fail(format!("Project entry {label} is unreadable"))),
            };
            let info = input.metadata()?;
            let target = destination.join(&name);
            if info.is_dir() {
                walk(
                    &input,
                    &target,
                    metadata,
                    depth + 1,
                    git_metadata || matches!(label, ".git" | ".control-git"),
                    durability,
                )?;
            } else if info.is_file() {
                #[cfg(target_os = "macos")]
                {
                    use std::os::fd::AsRawFd;
                    use std::os::unix::ffi::OsStrExt;
                    let destination_fd = fs::File::open(destination)?;
                    let name = std::ffi::CString::new(name.as_bytes())
                        .map_err(|_| fail("Invalid project filename"))?;
                    // APFS copy-on-write keeps project turns cheap without sharing
                    // writable inodes or following source paths a second time.
                    if unsafe {
                        libc::fclonefileat(
                            input.as_raw_fd(),
                            destination_fd.as_raw_fd(),
                            name.as_ptr(),
                            0,
                        )
                    } == 0
                    {
                        fs::set_permissions(
                            &target,
                            fs::Permissions::from_mode(0o600 | (info.permissions().mode() & 0o111)),
                        )?;
                        if durability == Durability::Checkpoint { fs::File::open(&target)?.sync_all()?; }
                        continue;
                    }
                    if target.try_exists()? {
                        return Err(fail(
                            "A project clone failed after creating its destination",
                        ));
                    }
                }
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600 | (info.permissions().mode() & 0o111))
                    .open(target)?;
                let copied = io::copy(&mut (&mut input).take(info.len() + 1), &mut output)?;
                if copied != info.len() {
                    return Err(fail(
                        "Project files changed while being copied; retry after saving the folder",
                    ));
                }
                if durability == Durability::Checkpoint { output.sync_all()?; }
            } else {
                return Err(fail(
                    "Project contains a socket, device or other special file",
                ));
            }
        }
        if durability == Durability::Checkpoint { fs::File::open(destination)?.sync_all()?; }
        Ok(())
    }
    let source = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(source)?;
    walk(&source, destination, metadata, 0, false, durability)
}
fn seed(source: &Path, destination: &Path) -> io::Result<()> {
    let source = source.canonicalize()?;
    if source == Path::new("/")
        || std::env::var_os("HOME").is_some_and(|home| source == Path::new(&home))
    {
        return Err(fail(
            "Choose a project folder, not the home directory or filesystem root",
        ));
    }
    for private in [
        crate::config::directory().map_err(fail)?,
        crate::config::state_directory().map_err(fail)?,
    ] {
        if let Ok(private) = private.canonicalize()
            && (source.starts_with(&private) || private.starts_with(&source)) {
                return Err(fail(
                    "Control configuration and state cannot be opened as a project",
                ));
            }
    }
    // Start with the current files, including uncommitted edits. Reading is an
    // explicit operator action, and no source Git metadata is ever mounted.
    copy_tree(&source, destination, false)?;
    let metadata = destination.parent().unwrap().join("repository.git");
    let git_source = source.join(".git");
    if fs::symlink_metadata(&git_source).is_ok_and(|meta|!meta.file_type().is_symlink()) {
        git(&[
            "clone".as_ref(),
            "--bare".as_ref(),
            "--no-hardlinks".as_ref(),
            "--dissociate".as_ref(),
            source.as_os_str(),
            metadata.as_os_str(),
        ])?;
        // An empty worktree gives portable, private Git metadata. Overlay the
        // current files after checkout, so dirty edits/deletions are preserved.
        let tree = destination.parent().unwrap().join("checkout");
        git(&[
            "--git-dir".as_ref(),
            metadata.as_os_str(),
            "worktree".as_ref(),
            "add".as_ref(),
            "--relative-paths".as_ref(),
            "--detach".as_ref(),
            tree.as_os_str(),
            "HEAD".as_ref(),
        ])?;
        let worktree_metadata = tree.join(".git");
        git(&[
            "--git-dir".as_ref(),
            metadata.as_os_str(),
            "config".as_ref(),
            "--remove-section".as_ref(),
            "remote.origin".as_ref(),
        ])?;
        for entry in fs::read_dir(&tree)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        for entry in fs::read_dir(destination)? {
            let entry = entry?;
            fs::rename(entry.path(), tree.join(entry.file_name()))?;
        }
        fs::remove_dir(destination)?;
        fs::rename(&metadata, tree.join(".control-git"))?;
        fs::write(
            worktree_metadata,
            "gitdir: .control-git/worktrees/checkout\n",
        )?;
        // Clones have their own config and no copied credential helpers/hooks.
        // Preserve the clone's object-format settings, with no host remote URL.
        fs::write(
            tree.join(".control-git/worktrees/checkout/gitdir"),
            "../../../.git\n",
        )?;
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(tree.join(".control-git/info/exclude"))?
            .write_all(b"\n/.control-git/\n")?;
        fs::rename(tree, destination)?;
    } else {
        // Plain folders remain usable; their first baseline is a private Git repo.
        git(&["init".as_ref(), destination.as_os_str()])?;
        git(&[
            "-C".as_ref(),
            destination.as_os_str(),
            "add".as_ref(),
            "--all".as_ref(),
        ])?;
        git(&[
            "-C".as_ref(),
            destination.as_os_str(),
            "-c".as_ref(),
            "user.name=Control".as_ref(),
            "-c".as_ref(),
            "user.email=control@localhost".as_ref(),
            "commit".as_ref(),
            "--allow-empty".as_ref(),
            "-m".as_ref(),
            "Approved project baseline".as_ref(),
        ])?;
    }
    crate::artifacts::sync_tree(destination)
}

pub fn prepare(
    attempt: &crate::usage::Attempt,
    baseline: &Path,
    workspace: &Path,
) -> io::Result<()> {
    prepare_in(&root()?, attempt, baseline, workspace)
}
fn prepare_in(
    root: &Path,
    attempt: &crate::usage::Attempt,
    baseline: &Path,
    workspace: &Path,
) -> io::Result<()> {
    let grant = attempt
        .project
        .as_deref()
        .filter(|id| crate::chat::valid_id(id))
        .ok_or_else(|| fail("Missing project grant"))?;
    let source = match &attempt.project_parent {
        Some(id) if crate::chat::valid_id(id) => root.join("checkpoints").join(id),
        Some(_) => return Err(fail("Invalid project checkpoint")),
        None => root.join("grants").join(grant).join("seed"),
    };
    for destination in [baseline,workspace] {
        fs::create_dir_all(destination.parent().ok_or_else(||fail("Missing project workspace parent"))?)?;
    }
    // The captured source is already durable. These isolated working copies are
    // disposable; only published checkpoints need per-file persistence barriers.
    copy_tree_with(&source, baseline, true, Durability::WorkingCopy)?;
    copy_tree_with(&source, workspace, true, Durability::WorkingCopy)
}
fn compose(root: &Path, project: &Project, directory: &Path) -> io::Result<()> {
    if !valid_name(&project.name) {return Err(fail("Invalid workspace folder name"));}
    if project.mounts.iter().any(|m|m.name==project.name) {return Err(fail("That workspace name is already open. Change the name below and try again."));}
    let combined=directory.join("combined");
    if let Some(grant)=&project.grant {
        if !crate::chat::valid_id(grant) {return Err(fail("Invalid project grant"));}
        let source=match &project.head {
            Some(head) if crate::chat::valid_id(head)=>root.join("checkpoints").join(head),
            Some(_)=>return Err(fail("Invalid project checkpoint")),
            None=>root.join("grants").join(grant).join("seed"),
        };
        if project.named {copy_tree(&source,&combined,true)?;}else{
            let name=&project.mounts.first().ok_or_else(||fail("Missing existing folder identity"))?.name;
            if !valid_name(name) {return Err(fail("Invalid existing folder name"));}
            fs::create_dir(&combined)?;copy_tree(&source,&combined.join(name),true)?;
        }
    }else{fs::create_dir(&combined)?;}
    let incoming=directory.join("incoming");fs::create_dir(&incoming)?;
    seed(Path::new(&project.path),&incoming.join("project"))?;
    // Refuse collisions with scratch files as well as previously opened folders.
    if combined.join(&project.name).try_exists()? {return Err(fail("That name is already used in the workspace. Choose another name."));}
    fs::rename(incoming.join("project"),combined.join(&project.name))?;
    // Each input was already copied and validated. Publish without another full
    // tree copy; the grant's parent is synced before attachment below.
    fs::rename(combined,directory.join("seed"))?;
    fs::remove_dir_all(incoming)?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}
pub fn publish(attempt: &mut crate::usage::Attempt, workspace: &Path, id: &str) -> io::Result<()> {
    publish_with(&root()?, attempt, workspace, id, || {
        crate::artifacts::publish(workspace, id).map(|m| m.id)
    })
}
fn publish_with(
    root: &Path,
    attempt: &mut crate::usage::Attempt,
    workspace: &Path,
    id: &str,
    preview: impl FnOnce() -> io::Result<String>,
) -> io::Result<()> {
    if attempt.project.is_none() {
        attempt.artifact = Some(preview()?);
        return Ok(());
    }
    if !crate::chat::valid_id(id) {
        return Err(fail("Invalid project checkpoint ID"));
    }
    let directory = root.join("checkpoints");
    fs::create_dir_all(&directory)?;
    let staging = directory.join(format!(".pending-{id}"));
    if directory.join(id).exists() {
        return Err(fail("Project checkpoint already exists"));
    }
    copy_tree(workspace, &staging, true)?;
    fs::rename(staging, directory.join(id))?;
    fs::File::open(&directory)?.sync_all()?;
    attempt.project_checkpoint = Some(id.into());
    // Viewer limits cannot truncate the authoritative project checkpoint.
    attempt.artifact = preview().ok();
    Ok(())
}

// Retain a candidate on unwind: a checkpoint may already reference its files.
// Ordinary rejected/failed approvals still remove their unpublished directory.
struct PendingGrant(Option<PathBuf>);
impl Drop for PendingGrant {
    fn drop(&mut self) {
        if !std::thread::panicking() && let Some(path) = &self.0 {
            let _ = fs::remove_dir_all(path);
        }
    }
}
/// Called only after HTTP operator authentication, never from model tools.
/// Slow copies/Git run without holding the shared state lock; the approval is
/// rechecked before its durable attachment to the chat.
pub fn approve(shared: &Arc<Mutex<State>>, id: &str, request: &str) -> io::Result<()> {
    if crate::shutdown::requested() {
        return Err(fail("Control is stopping; folder access was not granted"));
    }
    let project = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let chat = state
            .chats
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| fail("Chat not found"))?;
        let project = chat
            .project
            .clone()
            .filter(|p| p.request == request && p.needs_approval())
            .ok_or_else(|| fail("Project request changed; review the current request"))?;
        if state.runtime.runs.iter().any(|r| {
            r.chat_id.as_deref() == Some(id)
                && ["queued", "running", "waiting", "cancelling"].contains(&r.status.as_str())
        }) {
            return Err(fail(
                "Cancel or finish pending work before granting project access",
            ));
        }
        crate::identity::authorize(&chat.actor, Action::Task).map_err(fail)?;
        project
    };
    let grant = nonce();
    let directory = root()?.join("grants").join(&grant);
    fs::create_dir_all(directory.parent().unwrap())?;
    fs::create_dir(&directory)?;
    let mut pending=PendingGrant(Some(directory.clone()));
    compose(&root()?, &project, &directory)?;
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    if crate::shutdown::requested() {
        return Err(fail(
            "Control stopped during approval; folder access was not granted",
        ));
    }
    if state.runtime.runs.iter().any(|r| {
        r.chat_id.as_deref() == Some(id)
            && ["queued", "running", "waiting", "cancelling"].contains(&r.status.as_str())
    }) {
        return Err(fail("Chat changed during approval; request access again"));
    }
    let chat = state
        .chats
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| fail("Chat not found"))?;
    if !chat
        .project
        .as_ref()
        .is_some_and(|p| p.request == request && p.needs_approval() && p.grant==project.grant && p.head==project.head)
    {
        return Err(fail("Project request changed during approval"));
    }
    let previous = chat.clone();
    crate::identity::authorize(&chat.actor, Action::Task).map_err(fail)?;
    chat.project.as_mut().unwrap().grant = Some(grant);
    chat.project.as_mut().unwrap().error = None;
    let approved=chat.project.as_mut().unwrap();
    approved.head=None;approved.pending=false;approved.named=true;
    approved.mounts.push(Mount{name:approved.name.clone(),path:approved.path.clone(),message_index:approved.message_index});
    chat.workspace = None;
    chat.improve_mode = false;
    chat.revision = chat.revision.saturating_add(1);
    if crate::storage::save(&state).is_err() {
        *state.chats.iter_mut().find(|c| c.id == id).unwrap() = previous;
        return Err(fail("Could not save project approval"));
    }
    pending.0=None;
    Ok(())
}

pub fn http(
    stream: &mut impl Write,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    body: &[u8],
) -> bool {
    if let Some(id) = path
        .strip_prefix("/chat/")
        .and_then(|p| p.strip_suffix("/preview/stop"))
        .filter(|id| crate::chat::valid_id(id))
    {
        if method != "POST" {
            let _ = control_http::respond(stream, "405 Method Not Allowed", "text/plain", b"", "", control_http::Policy::Restricted);
            return true;
        }
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let result = crate::previews::stop(
            shared,
            id,
            fields.get("id").map(String::as_str).unwrap_or(""),
        );
        let (status, text) = match result {
            Ok(()) => ("303 See Other", String::new()),
            Err(error) => ("400 Bad Request", crate::escape(&error.to_string())),
        };
        let _ = control_http::respond(stream, status, "text/plain", text.as_bytes(), &format!("Location: /chat/{id}\r\n"), control_http::Policy::Restricted);
        return true;
    }
    let Some(id) = path
        .strip_prefix("/chat/")
        .and_then(|p| p.strip_suffix("/project"))
        .filter(|id| crate::chat::valid_id(id))
    else {
        return false;
    };
    let result: io::Result<String> = (|| {
        if method == "GET" {
            let state = shared.lock().map_err(|_| fail("State unavailable"))?;
            if !state.chats.iter().any(|c| c.id == id) {
                return Err(fail("Chat not found"));
            }
            return Ok(format!("<!doctype html><html lang='en'><head><title>Open project</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/workspace.css'></head><body><main>{}<a href='/chat/{id}'>Back to chat</a></main></body></html>",panel(&state,id)));
        }
        if method != "POST" {
            return Err(fail("Use GET or POST"));
        }
        let fields = crate::fields(&String::from_utf8_lossy(body));
        if fields.get("approve").is_some_and(|v| v == "yes") {
            let request=fields.get("request").map(String::as_str).unwrap_or("");
            if let Err(error)=approve(
                shared,
                id,
                request,
            ) {
                let mut state=shared.lock().map_err(|_|fail("State unavailable"))?;
                let chat=state.chats.iter_mut().find(|c|c.id==id).ok_or_else(||fail("Chat not found"))?;
                if chat.project.as_ref().is_some_and(|p|p.request==request&&p.needs_approval()) {
                    let previous=chat.clone();
                    chat.project.as_mut().unwrap().error=Some(error.to_string());
                    chat.revision=chat.revision.saturating_add(1);
                    if crate::storage::save(&state).is_err() {
                        *state.chats.iter_mut().find(|c|c.id==id).unwrap()=previous;
                        return Err(fail("Could not save folder error; access was not granted"));
                    }
                } else {return Err(error);}
            }
        } else {
            let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
            let index = state.chats.iter().position(|c| c.id == id).ok_or_else(|| fail("Chat not found"))?;
            let previous = state.chats[index].clone();
            let command=match fields.get("name").filter(|name|!name.is_empty()) {
                Some(name) if valid_name(name)=>format!("/open --as {name} {}",fields.get("path").map(String::as_str).unwrap_or("")),
                Some(_)=>return Err(fail("Choose a simple workspace name without spaces or path separators")),
                None=>format!("/open {}",fields.get("path").map(String::as_str).unwrap_or("")),
            };
            crate::chat::send(
                &mut state,
                id,
                &command,
                "website",
            )
            .map_err(fail)?;
            if crate::storage::save(&state).is_err() {
                state.chats[index] = previous;
                return Err(fail("Could not save project request"));
            }
        }
        Ok(String::new())
    })();
    match result {
        Ok(body) if method == "GET" => {
            let _ = control_http::respond(stream, "200 OK", "text/html; charset=utf-8", body.as_bytes(), "", control_http::Policy::RestrictedEmbedded);
        }
        Ok(_) => {
            let location = format!("/chat/{id}");
            let _ = control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: {location}\r\n"), control_http::Policy::Restricted);
        }
        Err(error) => {
            let body = crate::error_page(&error.to_string(), &format!("/chat/{id}/project"));
            let _ = control_http::respond(stream, "400 Bad Request", "text/html; charset=utf-8", body.as_bytes(), "", control_http::Policy::Restricted);
        }
    }
    true
}

#[cfg(test)]
mod tests;
