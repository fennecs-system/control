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
pub struct Project {
    pub request: String,
    pub path: String,
    pub grant: Option<String>,
    pub head: Option<String>,
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
    if chat.project.as_ref().is_some_and(|p| p.grant.is_some()) {
        return Err(
            "Start a new chat to open another project; this chat keeps its existing workspace.",
        );
    }
    let project = Project {
        request: nonce(),
        path: path.into(),
        ..Project::default()
    };
    let result = json!({"approval_required":true,"request":project.request,"path":project.path,"url":format!("/chat/{id}/project")});
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
    json!({"name":if worker {"control_project_request"} else {"control.project.request"},"description":"Request an operator-approved project directory on this Control node. Does not read, mount or authorize the path; the operator must confirm in Control.","annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false},"inputSchema":{"type":"object","properties":{"id":{"type":"string"},"path":{"type":"string","maxLength":4096}},"required":if worker {json!(["path"])}else{json!(["id","path"])},"additionalProperties":false}})
}
pub fn panel(state: &State, id: &str) -> String {
    let Some(chat) = state.chats.iter().find(|c| c.id == id) else {
        return String::new();
    };
    let path = chat.project.as_ref().map(|p| p.path.as_str()).unwrap_or("");
    let mut html = format!(
        "<details class='project-picker' data-transcript-key='project'{}><summary>{}</summary>",
        if chat.project.as_ref().is_some_and(|p| p.grant.is_none()) {
            " open"
        } else {
            ""
        },
        if chat.project.as_ref().is_some_and(|p| p.grant.is_some()) {
            "Project · isolated worktree"
        } else {
            "Open folder"
        }
    );
    if let Some(project) = &chat.project {
        html.push_str(&format!(
            "<p><code>{}</code></p>",
            crate::escape(&project.path)
        ));
        if project.grant.is_some() {
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
            html.push_str(&format!("<form method='post' action='/chat/{id}/project' target='_top'><input type='hidden' name='request' value='{}'><p>Trust this folder’s code for this chat and allow local previews? Control will copy its files into an isolated worktree. Credentials and generated directories are excluded. Preview containers have read-only project access, loopback ingress and no external network. The folder is on the machine running Control.</p><button name='approve' value='yes'>Trust folder and open</button></form>",crate::escape(&project.request)));
        }
    } else {
        html.push_str(&format!("<form method='post' action='/chat/{id}/project' target='_top'><label>Folder on this node<input name='path' value='{}' placeholder='/absolute/path' required></label><button>Continue</button></form>",crate::escape(path)));
    }
    html.push_str("</details>");
    html
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
/// Descriptor-relative no-follow reads prevent a source path from redirecting a
/// copy into another host directory. Preserve executable bits, never hardlinks.
fn copy_tree(source: &Path, destination: &Path, metadata: bool) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fn walk(
        source: &fs::File,
        destination: &Path,
        metadata: bool,
        depth: usize,
        git_metadata: bool,
        counts: &mut (usize, u64),
    ) -> io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if depth > 64 {
            return Err(fail("Project directory depth exceeds 64"));
        }
        fs::DirBuilder::new().mode(0o700).create(destination)?;
        let mut names = crate::artifacts::diff_directory_names(source)?;
        if names.len() > 2048 {
            return Err(fail("A project directory exceeds 2048 entries"));
        }
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
            counts.0 += 1;
            if counts.0 > 100_000 {
                return Err(fail("Project exceeds 100000 entries"));
            }
            let mut input=crate::artifacts::diff_open_child(source,&name)
                .map_err(|_|fail(format!("Project entry {label} is unreadable or a symlink. Symlinks require a separate scoped mount and are not imported.")))?;
            let info = input.metadata()?;
            let target = destination.join(&name);
            if info.is_dir() {
                walk(
                    &input,
                    &target,
                    metadata,
                    depth + 1,
                    git_metadata || matches!(label, ".git" | ".control-git"),
                    counts,
                )?;
            } else if info.is_file() {
                counts.1 = counts
                    .1
                    .checked_add(info.len())
                    .ok_or_else(|| fail("Project size overflow"))?;
                if counts.1 > 1024 * 1024 * 1024 {
                    return Err(fail("Project exceeds the 1 GiB checkpoint limit"));
                }
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
                        fs::File::open(&target)?.sync_all()?;
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
                output.sync_all()?;
            } else {
                return Err(fail(
                    "Project contains a socket, device or other special file",
                ));
            }
        }
        fs::File::open(destination)?.sync_all()
    }
    let source = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(source)?;
    walk(&source, destination, metadata, 0, false, &mut (0, 0))
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
        if let Ok(private) = private.canonicalize() {
            if source.starts_with(&private) || private.starts_with(&source) {
                return Err(fail(
                    "Control configuration and state cannot be opened as a project",
                ));
            }
        }
    }
    // Start with the current files, including uncommitted edits. Reading is an
    // explicit operator action, and no source Git metadata is ever mounted.
    copy_tree(&source, destination, false)?;
    let metadata = destination.parent().unwrap().join("repository.git");
    let git_source = source.join(".git");
    if git_source.exists() {
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
    copy_tree(&source, baseline, true)?;
    copy_tree(&source, workspace, true)
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
            .filter(|p| p.request == request && p.grant.is_none())
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
    seed(Path::new(&project.path), &directory.join("seed"))?;
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
        .is_some_and(|p| p.request == request && p.grant.is_none())
    {
        return Err(fail("Project request changed during approval"));
    }
    let previous = chat.clone();
    crate::identity::authorize(&chat.actor, Action::Task).map_err(fail)?;
    chat.project.as_mut().unwrap().grant = Some(grant);
    chat.workspace = None;
    chat.improve_mode = false;
    chat.revision = chat.revision.saturating_add(1);
    if crate::storage::save(&state).is_err() {
        *state.chats.iter_mut().find(|c| c.id == id).unwrap() = previous;
        return Err(fail("Could not save project approval"));
    }
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
            let _=stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
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
        let _=write!(stream,"HTTP/1.1 {status}\r\nLocation: /chat/{id}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{text}",text.len());
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
            return Ok(format!("<!doctype html><html lang='en'><head><title>Open project</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/workspace.css'></head><body><main>{}<a href='/chat/{id}'>Back to chat</a></main></body></html>",panel(&state,id).replace("data-transcript-key='project'>","data-transcript-key='project' open>")));
        }
        if method != "POST" {
            return Err(fail("Use GET or POST"));
        }
        let fields = crate::fields(&String::from_utf8_lossy(body));
        if fields.get("approve").is_some_and(|v| v == "yes") {
            approve(
                shared,
                id,
                fields.get("request").map(String::as_str).unwrap_or(""),
            )?;
        } else {
            let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
            let previous = state.chats.clone();
            request(
                &mut state,
                id,
                fields.get("path").map(String::as_str).unwrap_or(""),
            )
            .map_err(fail)?;
            if crate::storage::save(&state).is_err() {
                state.chats = previous;
                return Err(fail("Could not save project request"));
            }
        }
        Ok(String::new())
    })();
    match result {
        Ok(body) if method == "GET" => {
            let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'self'\r\nConnection: close\r\n\r\n{body}",body.len());
        }
        Ok(_) => {
            let location = if shared.lock().is_ok_and(|s| {
                s.chats
                    .iter()
                    .find(|c| c.id == id)
                    .and_then(|c| c.project.as_ref())
                    .is_some_and(|p| p.grant.is_some())
            }) {
                format!("/chat/{id}")
            } else {
                format!("/chat/{id}/project")
            };
            let _=write!(stream,"HTTP/1.1 303 See Other\r\nLocation: {location}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
        }
        Err(error) => {
            let body = crate::error_page(&error.to_string(), &format!("/chat/{id}/project"));
            let _=write!(stream,"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'self'; base-uri 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n{body}",body.len());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "control-project-test-{}-{}",
                std::process::id(),
                nonce()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn source(&self) -> PathBuf {
            let path = self.0.join("source");
            fs::create_dir(&path).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn command(directory: &Path, args: &[&str]) -> std::process::Output {
        let output = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .arg("-C")
            .arg(directory)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    #[test]
    fn project_request_is_only_intent_and_cannot_replace_an_approved_grant() {
        let mut state = State::default();
        let id = crate::chat::create(&mut state);
        let value = request(&mut state, &id, "/a/nonexistent/project").unwrap();
        assert_eq!(value["approval_required"], true);
        assert!(state.chats[0].project.as_ref().unwrap().grant.is_none());
        assert!(request(&mut state, &id, "../private").is_err());
        let before = serde_json::to_vec(&state).unwrap();
        assert!(
            crate::session_http::rpc(
                &mut state,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.project.approve","arguments":{"id":id,"path":"/tmp"}}})
            )["result"]["isError"]
                == true
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        state.chats[0].project.as_mut().unwrap().grant = Some("approved".into());
        assert!(request(&mut state, &id, "/another").is_err());
    }
    #[test]
    fn git_worktree_preserves_dirty_files_deletions_history_and_executable_modes() {
        let fixture = Fixture::new();
        let source = fixture.source();
        command(&source, &["init"]);
        fs::write(source.join("main.rs"), "before").unwrap();
        fs::write(source.join("removed.txt"), "remove me").unwrap();
        fs::write(source.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        command(&source, &["add", "--all"]);
        command(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-m",
                "original history",
            ],
        );
        fs::write(source.join("main.rs"), "uncommitted").unwrap();
        fs::remove_file(source.join("removed.txt")).unwrap();
        fs::write(source.join("new.txt"), "new file").unwrap();
        fs::write(source.join(".env"), "synthetic excluded value").unwrap();
        fs::create_dir(source.join(".ssh")).unwrap();
        fs::write(source.join(".ssh/id_ed25519"), "synthetic excluded value").unwrap();
        fs::write(source.join(".netrc"), "synthetic excluded value").unwrap();
        fs::create_dir(source.join("target")).unwrap();
        fs::write(source.join("target/build"), "generated").unwrap();
        let imported = fixture.0.join("seed");
        seed(&source, &imported).unwrap();
        assert_eq!(fs::read(imported.join("main.rs")).unwrap(), b"uncommitted");
        assert!(!imported.join("removed.txt").exists());
        assert!(!imported.join(".env").exists());
        assert!(!imported.join(".ssh").exists());
        assert!(!imported.join(".netrc").exists());
        assert!(!imported.join("target").exists());
        assert_eq!(
            fs::metadata(imported.join("run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0o111
        );
        assert!(
            String::from_utf8(command(&imported, &["log", "-1", "--format=%s"]).stdout)
                .unwrap()
                .contains("original history")
        );
        let status =
            String::from_utf8(command(&imported, &["status", "--porcelain"]).stdout).unwrap();
        assert!(
            status.contains("main.rs")
                && status.contains("removed.txt")
                && status.contains("new.txt")
        );
        assert!(command(&imported, &["remote"]).stdout.is_empty());
        assert!(!status.contains(".control-git"));
        let worktrees =
            String::from_utf8(command(&imported, &["worktree", "list", "--porcelain"]).stdout)
                .unwrap();
        assert!(
            worktrees.contains(&format!(
                "worktree {}",
                imported.canonicalize().unwrap().display()
            )),
            "Relative worktree metadata must work on the host: {worktrees}"
        );
        let second = fixture.0.join("independent");
        copy_tree(&imported, &second, true).unwrap();
        fs::write(second.join("main.rs"), "independent edit").unwrap();
        command(&second, &["add", "main.rs"]);
        command(
            &second,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-m",
                "private change",
            ],
        );
        assert_eq!(fs::read(imported.join("main.rs")).unwrap(), b"uncommitted");
        assert_eq!(fs::read(source.join("main.rs")).unwrap(), b"uncommitted");
        assert!(
            String::from_utf8(command(&source, &["log", "-1", "--format=%s"]).stdout)
                .unwrap()
                .contains("original history")
        );
    }
    #[test]
    fn checkpoints_keep_large_files_and_git_separate_from_bounded_previews() {
        let fixture = Fixture::new();
        let source = fixture.source();
        fs::write(source.join("large.bin"), vec![42; 3 * 1024 * 1024]).unwrap();
        seed(&source, &fixture.0.join("seed")).unwrap();
        let root = fixture.0.join("projects");
        let mut attempt = crate::usage::Attempt {
            project: Some("grant".into()),
            ..Default::default()
        };
        publish_with(
            &root,
            &mut attempt,
            &fixture.0.join("seed"),
            "generation-1",
            || Err(fail("viewer limit")),
        )
        .unwrap();
        assert_eq!(attempt.project_checkpoint.as_deref(), Some("generation-1"));
        assert!(attempt.artifact.is_none());
        attempt.project_parent = attempt.project_checkpoint.clone();
        prepare_in(
            &root,
            &attempt,
            &fixture.0.join("baseline"),
            &fixture.0.join("restored"),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(fixture.0.join("restored/large.bin"))
                .unwrap()
                .len(),
            3 * 1024 * 1024
        );
        assert!(fixture.0.join("restored/.git/HEAD").is_file());
        assert!(publish_with(
            &root,
            &mut attempt,
            &fixture.0.join("seed"),
            "generation-1",
            || Ok("unused".into())
        )
        .is_err());
    }
    #[test]
    fn import_rejects_symlinks_without_following_them_or_publishing_a_grant() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("private.txt"), "synthetic private file").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
        assert!(copy_tree(&source, &fixture.0.join("copy"), false).is_err());
        assert!(!fixture.0.join("copy/escape/private.txt").exists());
    }
    #[test]
    #[ignore = "requires CONTROL_QA_IMAGE pinned Nix image and local engine; no credentials or model calls"]
    fn live_container_git_commits_and_restores_an_independent_project_worktree() {
        let fixture = Fixture::new();
        let source = fixture.source();
        command(&source, &["init"]);
        fs::write(source.join("code.txt"), "original").unwrap();
        command(&source, &["add", "--all"]);
        command(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@localhost",
                "commit",
                "-m",
                "original",
            ],
        );
        let workspace = fixture.0.join("workspace");
        seed(&source, &workspace).unwrap();
        let image = std::env::var("CONTROL_QA_IMAGE").expect("Pinned local Nix image");
        assert!(image.starts_with("sha256:"));
        let run = |root: &Path, script: &str| {
            let mount = format!(
                "type=bind,src={},dst=/workspace",
                root.canonicalize().unwrap().display()
            );
            let output = crate::runtime::docker_query(&[
                "run",
                "--rm",
                "--pull=never",
                "--network=none",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--user",
                &format!("{}:{}", unsafe { libc::getuid() }, unsafe {
                    libc::getgid()
                }),
                "--tmpfs",
                "/tmp:rw,nosuid,size=64m",
                "--env",
                "HOME=/tmp",
                "--mount",
                &mount,
                "--workdir",
                "/workspace",
                &image,
                "bash",
                "-c",
                script,
            ])
            .unwrap();
            output
        };
        assert!(run(&workspace,"set -eu; test ! -e /home/agent/.codex/auth.json; test ! -S /var/run/docker.sock; git status --porcelain; printf edited > code.txt; git add --all; git -c user.name=Fixture -c user.email=fixture@localhost commit -m edited; git worktree list --porcelain").contains("worktree /workspace\n"));
        let copy = fixture.0.join("restored");
        copy_tree(&workspace, &copy, true).unwrap();
        assert_eq!(run(&copy, "git log -1 --format=%s").trim(), "edited");
        assert_eq!(fs::read(source.join("code.txt")).unwrap(), b"original");
        assert!(!run(&copy, "git status --porcelain").contains(".control-git"));
    }
}
