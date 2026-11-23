//! Operator-approved project resources. Models can request access, never grant it.
//! Host Git only touches the initial operator-approved source and fresh metadata;
//! subsequent agent-written repositories are copied without executing host Git.
use crate::{State, identity::Action};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
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
    /// A pending request's predecessor: the chat's project before it was made,
    /// restored when the operator dismisses it. `None` when there was none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Box<Project>>,
    /// The turn that asked for a pending request, told if it is dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// More folders a pending operator `/open` list requested with this one,
    /// opened beside it by the same approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Mount>,
}
impl Project {
    fn needs_approval(&self) -> bool {
        self.pending || self.grant.is_none()
    }
    /// Every folder this request opens, as (name, path): its own, then `also`.
    fn folders(&self) -> impl Iterator<Item = (&str, &str)> {
        std::iter::once((self.name.as_str(), self.path.as_str()))
            .chain(self.also.iter().map(|m| (m.name.as_str(), m.path.as_str())))
    }
    pub fn approved(&self) -> Option<Self> {
        self.grant.as_ref()?;
        let mut inherited = self.clone();
        inherited.pending = false;
        inherited.error = None;
        inherited.message_index = None;
        inherited.previous = None;
        inherited.origin = None;
        inherited.also.clear();
        if let Some(mount) = inherited.mounts.last() {
            inherited.path = mount.path.clone();
            inherited.name = mount.name.clone();
        }
        for mount in &mut inherited.mounts {
            mount.message_index = None;
        }
        Some(inherited)
    }
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !skip(name, false)
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
fn root() -> io::Result<PathBuf> {
    crate::config::state("projects").map_err(io::Error::other)
}
/// Time-ordered and unique within this process, even when concurrent imports
/// read the same clock tick (macOS reports microseconds).
fn nonce() -> String {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut last = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(last + 1);
        match LAST.compare_exchange_weak(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return format!("p-{next:x}"),
            Err(current) => last = current,
        }
    }
}
/// Who is asking for a folder. Only the operator may learn whether a host path
/// exists, so only an operator's `/open` probes it.
#[derive(Clone, Copy)]
pub enum Requester<'a> {
    /// `/open`, typed or posted by the authenticated operator: a mistyped or
    /// missing folder fails at once instead of waiting for approval.
    Operator,
    /// `control_project_request` (worker) or `control.project.request` (MCP).
    /// Never probed. `run` is the requesting turn, if any.
    Agent { run: Option<&'a str> },
}
pub fn request(state: &mut State, id: &str, path: &str, requester: Requester<'_>) -> Result<Value, &'static str> {
    request_named(state, id, path, None, requester)
}
pub fn request_named(
    state: &mut State,
    id: &str,
    path: &str,
    name: Option<&str>,
    requester: Requester<'_>,
) -> Result<Value, &'static str> {
    let path = path.trim();
    if path.len() > 4096 || !Path::new(path).is_absolute() || path.contains(['\n', '\r', '\0', ',']) {
        return Err("Use /open followed by an absolute path on the Control node.");
    }
    let chat = state.chats.iter().find(|c| c.id == id).ok_or("Chat not found.")?;
    crate::identity::authorize(&chat.actor, Action::Task)?;
    let folder = folder(path, name, requester)?;
    file(state, id, vec![folder], requester)
}
/// One folder of a request, checked alone: an absolute path (probed only for
/// the operator) and its workspace name.
fn folder(path: &str, name: Option<&str>, requester: Requester<'_>) -> Result<Mount, &'static str> {
    let path = path.trim();
    if path.len() > 4096 || !Path::new(path).is_absolute() || path.contains(['\n', '\r', '\0', ',']) {
        return Err("Use /open followed by an absolute path on the Control node.");
    }
    if matches!(requester, Requester::Operator) {
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err("That path is not a folder. Use /open followed by a folder's absolute path."),
            Err(_) => {
                return Err("No readable folder at that path on this node. Check it and use /open /absolute/path.");
            }
        }
    }
    let name = name
        .filter(|s| !s.is_empty())
        .or_else(|| Path::new(path).file_name().and_then(|s| s.to_str()))
        .ok_or("Choose a named project folder.")?;
    if !valid_name(name) {
        return Err(
            "Folder names must use letters, numbers, dots, dashes or underscores, without a leading dot. Use /open --as NAME /absolute/path.",
        );
    }
    Ok(Mount {
        name: name.into(),
        path: path.into(),
        message_index: None,
    })
}
/// The operator's `/open` with several folders: each path is checked alone and
/// reported on its own line. When all pass they become one request, reviewed and
/// trusted once; each opens at its own `/workspace/NAME`. Returns the reply
/// lines and whether a request was made.
pub fn request_list(state: &mut State, id: &str, paths: &[String]) -> Result<(String, bool), &'static str> {
    let chat = state.chats.iter().find(|c| c.id == id).ok_or("Chat not found.")?;
    crate::identity::authorize(&chat.actor, Action::Task)?;
    let open = chat
        .project
        .as_ref()
        .map(|p| p.mounts.iter().map(|m| m.name.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut folders: Vec<Mount> = Vec::new();
    let mut lines = Vec::new();
    for path in paths {
        let checked = folder(path, None, Requester::Operator).and_then(|folder| {
            if open.contains(&folder.name) || folders.iter().any(|f| f.name == folder.name) {
                Err("That workspace name is already used; open this one alone with /open --as NAME /absolute/path.")
            } else if open.len() + folders.len() >= 16 {
                Err("A chat holds at most 16 open folders.")
            } else {
                Ok(folder)
            }
        });
        lines.push(match checked {
            Ok(folder) => {
                let line = format!("- {} → /workspace/{}", folder.path, folder.name);
                folders.push(folder);
                line
            }
            Err(error) => format!("- {}: {error}", path.trim()),
        });
    }
    // All or nothing: a later `/open` replaces the pending request, so a partial one
    // would be lost when the refused paths are sent again.
    let made = folders.len() == paths.len();
    if made {
        file(state, id, folders, Requester::Operator)?;
    }
    Ok((lines.join("\n"), made))
}
/// Replaces the chat's pending request with `folders` (at least one): the
/// first is the request's own path and name, the rest open with it.
fn file(state: &mut State, id: &str, folders: Vec<Mount>, requester: Requester<'_>) -> Result<Value, &'static str> {
    let chat = state.chats.iter().find(|c| c.id == id).ok_or("Chat not found.")?;
    let message_index = chat.messages.len().checked_sub(1);
    let mut folders = folders.into_iter();
    let first = folders
        .next()
        .ok_or("Use /open followed by an absolute path on the Control node.")?;
    let also = folders
        .map(|folder| Mount {
            message_index,
            ..folder
        })
        .collect::<Vec<_>>();
    let mut mounts = chat.project.as_ref().map(|p| p.mounts.clone()).unwrap_or_default();
    if let Some(previous) = chat
        .project
        .as_ref()
        .filter(|p| p.grant.is_some() && !p.named && p.mounts.is_empty())
    {
        let name = Path::new(&previous.path)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| valid_name(s))
            .unwrap_or("project");
        mounts.push(Mount {
            name: name.into(),
            path: previous.path.clone(),
            message_index: previous.message_index,
        });
    }
    if mounts.len() + also.len() >= 16 {
        return Err("This chat already has 16 open folders.");
    }
    let title = Path::new(&first.path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let project = Project {
        request: nonce(),
        path: first.path,
        message_index,
        pending: true,
        name: first.name,
        grant: chat.project.as_ref().and_then(|p| p.grant.clone()),
        head: chat.project.as_ref().and_then(|p| p.head.clone()),
        mounts,
        named: chat.project.as_ref().is_some_and(|p| p.named),
        // A replaced request restores what preceded it, not itself.
        previous: chat.project.as_ref().and_then(|p| {
            if p.needs_approval() {
                p.previous.clone()
            } else {
                Some(Box::new(p.clone()))
            }
        }),
        origin: match requester {
            Requester::Agent { run } => run.map(str::to_owned),
            Requester::Operator => None,
        },
        also,
        ..Project::default()
    };
    let result = json!({"approval_required":true,"request":project.request,"path":project.path,"mount":format!("/workspace/{}",project.name),"url":format!("/dm/{id}/project")});
    let chat = state.chats.iter_mut().find(|c| c.id == id).unwrap();
    if chat.messages.is_empty() {
        chat.title = title;
    }
    chat.project = Some(project);
    chat.revision = chat.revision.saturating_add(1);
    Ok(result)
}
/// The folders of an operator's `/open`. Commas or new lines separate paths,
/// and each item is one path, spaces included. On one line without commas, a
/// space separates paths only before the next path's `/` or `~`, so a path with
/// spaces still opens alone; quote a path ('…' or "…") whose name has a space
/// before a `/`. `~` and `~/…` name the Control node user's home.
pub fn paths(tail: &str, home: Option<&str>) -> Vec<String> {
    let unquote = |item: &str| {
        let item = item.trim();
        ['"', '\'']
            .into_iter()
            .find_map(|quote| item.strip_prefix(quote)?.strip_suffix(quote))
            .unwrap_or(item)
            .to_owned()
    };
    let items = if tail.contains(['\n', ',']) {
        tail.split(['\n', ',']).map(unquote).collect::<Vec<_>>()
    } else {
        let mut items = Vec::new();
        let mut current = String::new();
        let mut chars = tail.trim().chars().peekable();
        while let Some(c) = chars.next() {
            if matches!(c, '"' | '\'') && current.trim().is_empty() {
                items.push(chars.by_ref().take_while(|&next| next != c).collect::<String>());
                current.clear();
            } else if c.is_whitespace() && chars.peek().is_some_and(|next| matches!(next, '/' | '~' | '"' | '\'')) {
                items.push(std::mem::take(&mut current));
            } else {
                current.push(c);
            }
        }
        items.push(current);
        items.into_iter().map(|item| item.trim().to_owned()).collect()
    };
    items
        .into_iter()
        .filter(|item| !item.is_empty())
        .map(|item| match (item.as_str(), home) {
            ("~", Some(home)) => home.to_owned(),
            (_, Some(home)) if item.starts_with("~/") => format!("{}/{}", home.trim_end_matches('/'), &item[2..]),
            _ => item,
        })
        .collect()
}
pub fn definition(worker: bool) -> Value {
    json!({
        "name": if worker {"control_project_request"} else {"control.project.request"},
        "description": concat!(
            "Request an operator-approved folder at /workspace/name on this node. Existing approved folders remain ",
            "available. Does not read or authorize the path; the operator must confirm in Control. Supply name to ",
            "resolve duplicate folder names."
        ),
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false},
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "path": {"type": "string", "maxLength": 4096},
                "name": {"type": "string", "maxLength": 128}
            },
            "required": if worker {json!(["path"])}else{json!(["id","path"])},
            "additionalProperties": false
        }
    })
}
pub fn panel(chats: &[crate::chat::Session], id: &str) -> String {
    let Some(chat) = chats.iter().find(|c| c.id == id) else {
        return String::new();
    };
    let path = chat.project.as_ref().map(|p| p.path.as_str()).unwrap_or("");
    let approved = chat.project.as_ref().is_some_and(|p| !p.needs_approval());
    let running = chat
        .project
        .as_ref()
        .filter(|p| p.needs_approval())
        .and_then(|p| importing(id).filter(|import| import.request == p.request));
    let mut html = format!(
        "<details class='project-picker' data-transcript-key='project-{}-{}'{}><summary>{}</summary>",
        chat.project.as_ref().map(|p| p.request.as_str()).unwrap_or("new"),
        if approved {
            "approved"
        } else if running.is_some() {
            "importing"
        } else {
            "pending"
        },
        if chat.project.as_ref().is_some_and(|p| p.needs_approval()) {
            " open"
        } else {
            ""
        },
        if approved {
            "Folder opened"
        } else if running.is_some() {
            "Importing folder…"
        } else {
            "Open folder"
        }
    );
    if let Some(project) = &chat.project {
        html.push_str(&format!("<p><code>{}</code></p>", crate::escape(&project.path)));
        if let Some(import) = &running {
            // An earlier attempt's error gives way to this attempt's outcome.
            html.push_str(&format!("<p role='status'>Approved · copying into <code>/workspace/{}</code> · {} {} copied · {} s</p><p class='muted'>The folder opens in this chat when the copy finishes. You can return to the chat; the import continues.</p>",
                crate::escape(&project.name), import.entries, if import.entries == 1 {"entry"} else {"entries"}, import.elapsed.as_secs()));
        } else if let Some(error) = &project.error {
            html.push_str(&format!("<p role='alert'>{}</p>", crate::escape(error)));
        }
        if running.is_some() {
        } else if !project.needs_approval() {
            html.push_str(
                "<p class='muted'>Changes stay in this chat’s worktree. The original folder is unchanged.</p>",
            );
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
            if project.also.is_empty() {
                html.push_str(&format!("<p>Open as <code>/workspace/{}</code></p><form class='decision-form' method='post' action='/dm/{id}/project' target='_top'><input type='hidden' name='request' value='{}'><p>Trust this folder’s code for this chat and allow local previews? Control will copy its files into an isolated worktree. Credentials, generated directories and symlinks are excluded. The original folder stays unchanged.</p><button class='primary' name='approve' value='yes'>Trust folder and open</button><button class='secondary' name='dismiss' value='yes'>Dismiss</button><p role='alert' hidden></p></form><form method='post' action='/dm/{id}/project' target='_top'><label>Folder on this node<input name='path' value='{}' required></label><label>Workspace name<input name='name' value='{}' required></label><button>Change folder</button></form>",crate::escape(&project.name),crate::escape(&project.request),crate::escape(&project.path),crate::escape(&project.name)));
            } else {
                // A list from one `/open`: one decision for all; changing it is a new /open.
                let rows = project
                    .folders()
                    .map(|(name, path)| {
                        format!(
                            "<li><code>{}</code> → <code>/workspace/{}</code></li>",
                            crate::escape(path),
                            crate::escape(name)
                        )
                    })
                    .collect::<String>();
                html.push_str(&format!("<ul class='project-folders'>{rows}</ul><form class='decision-form' method='post' action='/dm/{id}/project' target='_top'><input type='hidden' name='request' value='{}'><p>Trust these {} folders’ code for this chat and allow local previews? Control will copy their files into an isolated worktree. Credentials, generated directories and symlinks are excluded. The original folders stay unchanged.</p><button class='primary' name='approve' value='yes'>Trust folders and open</button><button class='secondary' name='dismiss' value='yes'>Dismiss</button><p role='alert' hidden></p></form><p class='muted'>To change the list, dismiss it and use /open again.</p>",crate::escape(&project.request),project.also.len()+1));
            }
        }
    } else {
        html.push_str(&format!("<form method='post' action='/dm/{id}/project' target='_top'><label>Folder on this node<input name='path' value='{}' placeholder='/absolute/path' required></label><button>Continue</button></form>",crate::escape(path)));
    }
    html.push_str("</details>");
    html
}
/// The reply an `/open` command shows once its request is dismissed.
pub(crate) const DISMISSED: &str = "Folder request dismissed.";
/// The operator rejects the displayed request. The chat's project returns to
/// what it was before the request, with any checkpoint agents have made since,
/// so approved folders keep working. `Ok(None)` when that request is no longer
/// pending (dismissed, replaced or approved): nothing changes, so a stale page
/// cannot dismiss a newer request. An approved request that is still importing
/// cannot be dismissed; the copy is not cancellable and attaches when done.
/// Like port and bot decisions, the dismissal is noted in the chat and resumes
/// the requesting agent's finished turn. Callers checkpoint, then wake on
/// `Resumed`.
pub fn dismiss(state: &mut State, id: &str, request: &str) -> Result<Option<crate::approvals::Outcome>, &'static str> {
    use crate::application::approval_resume::{self, FOLDER_DECISIONS};
    let chat = state.chats.iter().find(|c| c.id == id).ok_or("Chat not found.")?;
    crate::identity::authorize(&chat.actor, Action::Task)?;
    let Some(pending) = chat
        .project
        .clone()
        .filter(|p| p.request == request && p.needs_approval())
    else {
        return Ok(None);
    };
    if importing(id).is_some_and(|import| import.request == request) {
        return Err("This folder is already approved and importing; it opens when the copy finishes.");
    }
    let restored = match pending.previous.clone() {
        Some(mut previous) => {
            previous.head = pending.head.clone();
            Some(*previous)
        }
        // Requests saved before predecessors were recorded keep their folders.
        None => pending.grant.is_some().then(|| {
            let mut kept = Project {
                request: nonce(),
                pending: false,
                error: None,
                previous: None,
                origin: None,
                also: Vec::new(),
                ..pending.clone()
            };
            if let Some(mount) = kept.mounts.last() {
                (kept.path, kept.name, kept.message_index) =
                    (mount.path.clone(), mount.name.clone(), mount.message_index);
            }
            kept
        }),
    };
    let chat = state.chats.iter_mut().find(|c| c.id == id).unwrap();
    chat.project = restored;
    if let Some(message) = pending
        .message_index
        .and_then(|index| chat.messages.get_mut(index))
        .filter(|m| {
            m.run_id.is_none()
                && control_context::commands::tail(m.text.trim(), control_context::commands::Kind::Open).is_some()
        })
    {
        message.reply = DISMISSED.into();
    }
    chat.revision = chat.revision.saturating_add(1);
    let lines = format!(
        "{} ({}): dismissed by the operator; it is not mounted.",
        pending.path, pending.request
    );
    let origins = pending.origin.iter().map(String::as_str).collect::<Vec<_>>();
    let task = state
        .runtime
        .runs
        .iter()
        .find(|run| pending.origin.as_deref() == Some(run.id.as_str()))
        .map(|run| (*run.objective).clone())
        .unwrap_or_default();
    let outcome = crate::approvals::resume(
        state,
        crate::approvals::Decisions {
            chat: id,
            origins: &origins,
            heading: FOLDER_DECISIONS,
            lines: &lines,
            instruction: "Continue the original task without that folder; request it again only if it is still needed.",
            task: &task,
            kind: crate::runtime::RunKind::FolderDecision,
            source: "folder-decision",
            grants: Vec::new(),
        },
    );
    // A busy chat cannot resume now. A request has no receipt to retry later,
    // so the decision is only noted; the agent reads it on its next turn.
    Ok(Some(if outcome == crate::approvals::Outcome::Wait {
        approval_resume::record(
            approval_resume::Edit {
                runs: &mut state.runtime.runs,
                notice: &mut state.runtime.notice,
                chats: &mut state.chats,
            },
            id,
            None,
            format!("{FOLDER_DECISIONS}\n{lines}"),
        )
    } else {
        outcome
    }))
}
pub fn message_index(chat: &crate::chat::Session) -> Option<usize> {
    let project = chat.project.as_ref()?;
    project
        .message_index
        .filter(|index| *index < chat.messages.len())
        .or_else(|| {
            chat.messages.iter().rposition(|m| {
                m.text
                    .trim()
                    .strip_prefix("/open")
                    .is_some_and(|p| p.trim() == project.path)
            })
        })
        .or_else(|| chat.messages.len().checked_sub(1))
}
fn git(args: &[&std::ffi::OsStr]) -> io::Result<()> {
    git_output(args, &[]).map(drop).map_err(|_| io::Error::other("Could not prepare an isolated Git worktree; check that Git is installed and this is a readable checkout with a commit."))
}
/// Host Git with no system/global config, hooks, fsmonitor, prompts,
/// credential helpers or auto maintenance. Returns stdout; stderr is discarded.
/// A commit otherwise detaches `git maintenance run --auto`, which creates and
/// removes `objects/maintenance.lock` after Git returns, while the import
/// still walks, syncs and moves that repository (ENOENT, failed approvals).
pub(crate) fn git_output(args: &[&std::ffi::OsStr], envs: &[(&str, &std::ffi::OsStr)]) -> io::Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .envs(envs.iter().copied())
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let output = command.output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(io::Error::other("Git command failed"))
    }
}
/// Names project import never copies. Tool package admission rejects them.
pub(crate) fn excluded_name(name: &str) -> bool {
    skip(name, false)
}
fn skip(name: &str, metadata: bool) -> bool {
    (name.starts_with(".control-") && !(metadata && name == ".control-git"))
        || (!metadata && name == ".git")
        || crate::backend::private_name(name)
        || matches!(
            name,
            ".env"
                | "discord.env"
                | "auth.json"
                | ".agents"
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
/// A disposable attempt view of an already durable source; no per-file sync.
pub(crate) fn copy_working(source: &Path, destination: &Path) -> io::Result<()> {
    copy_tree_with(source, destination, false, Durability::WorkingCopy)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Durability {
    WorkingCopy,
    Checkpoint,
}
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
            return Err(io::Error::other("Project directory depth exceeds 64"));
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
                .ok_or_else(|| io::Error::other("Project filenames must be UTF-8"))?;
            if !git_metadata && skip(label, metadata) {
                continue;
            }
            copied_entry()?;
            let mut input = match crate::artifacts::diff_open_child(source, &name) {
                Ok(input) => input,
                // openat is relative to the pinned parent and never follows links.
                // Both dangling and directory links return ELOOP on supported hosts.
                Err(error) if error.raw_os_error() == Some(libc::ELOOP) => continue,
                Err(_) => return Err(io::Error::other(format!("Project entry {label} is unreadable"))),
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
                        .map_err(|_| io::Error::other("Invalid project filename"))?;
                    // APFS copy-on-write keeps project turns cheap without sharing
                    // writable inodes or following source paths a second time.
                    if unsafe { libc::fclonefileat(input.as_raw_fd(), destination_fd.as_raw_fd(), name.as_ptr(), 0) }
                        == 0
                    {
                        fs::set_permissions(
                            &target,
                            fs::Permissions::from_mode(0o600 | (info.permissions().mode() & 0o111)),
                        )?;
                        if durability == Durability::Checkpoint {
                            fs::File::open(&target)?.sync_all()?;
                        }
                        continue;
                    }
                    if target.try_exists()? {
                        return Err(io::Error::other(
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
                    return Err(io::Error::other(
                        "Project files changed while being copied; retry after saving the folder",
                    ));
                }
                if durability == Durability::Checkpoint {
                    output.sync_all()?;
                }
            } else {
                return Err(io::Error::other(
                    "Project contains a socket, device or other special file",
                ));
            }
        }
        if durability == Durability::Checkpoint {
            fs::File::open(destination)?.sync_all()?;
        }
        Ok(())
    }
    let source = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(source)?;
    walk(&source, destination, metadata, 0, false, durability)
}
/// Operator-chosen host folders must not be the home directory, the
/// filesystem root, or Control's own configuration or state.
pub(crate) fn check_source(source: &Path) -> io::Result<PathBuf> {
    let source = source.canonicalize()?;
    if source == Path::new("/") || std::env::var_os("HOME").is_some_and(|home| source == Path::new(&home)) {
        return Err(io::Error::other(
            "Choose a project folder, not the home directory or filesystem root",
        ));
    }
    for private in [
        crate::config::directory().map_err(io::Error::other)?,
        crate::config::state_directory().map_err(io::Error::other)?,
    ] {
        if let Ok(private) = private.canonicalize()
            && (source.starts_with(&private) || private.starts_with(&source))
        {
            return Err(io::Error::other(
                "Control configuration and state cannot be opened as a project",
            ));
        }
    }
    Ok(source)
}
fn seed(source: &Path, destination: &Path) -> io::Result<()> {
    let source = check_source(source)?;
    // Start with the current files, including uncommitted edits. Reading is an
    // explicit operator action, and no source Git metadata is ever mounted.
    copy_tree(&source, destination, false)?;
    let metadata = destination.parent().unwrap().join("repository.git");
    let git_source = source.join(".git");
    if fs::symlink_metadata(&git_source).is_ok_and(|meta| !meta.file_type().is_symlink()) {
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
        fs::write(worktree_metadata, "gitdir: .control-git/worktrees/checkout\n")?;
        // Clones have their own config and no copied credential helpers/hooks.
        // Preserve the clone's object-format settings, with no host remote URL.
        fs::write(tree.join(".control-git/worktrees/checkout/gitdir"), "../../../.git\n")?;
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(tree.join(".control-git/info/exclude"))?
            .write_all(b"\n/.control-git/\n")?;
        fs::rename(tree, destination)?;
    } else {
        // Plain folders remain usable; their first baseline is a private Git repo.
        git(&["init".as_ref(), destination.as_os_str()])?;
        git(&["-C".as_ref(), destination.as_os_str(), "add".as_ref(), "--all".as_ref()])?;
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

pub fn prepare(attempt: &crate::usage::Attempt, baseline: &Path, workspace: &Path) -> io::Result<()> {
    prepare_in(&root()?, attempt, baseline, workspace)
}
fn prepare_in(root: &Path, attempt: &crate::usage::Attempt, baseline: &Path, workspace: &Path) -> io::Result<()> {
    let grant = attempt
        .project
        .as_deref()
        .filter(|id| crate::chat::valid_id(id))
        .ok_or_else(|| io::Error::other("Missing project grant"))?;
    let source = match &attempt.project_parent {
        Some(id) if crate::chat::valid_id(id) => root.join("checkpoints").join(id),
        Some(_) => return Err(io::Error::other("Invalid project checkpoint")),
        None => root.join("grants").join(grant).join("seed"),
    };
    for destination in [baseline, workspace] {
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| io::Error::other("Missing project workspace parent"))?,
        )?;
    }
    // The captured source is already durable. These isolated working copies are
    // disposable; only published checkpoints need per-file persistence barriers.
    copy_tree_with(&source, baseline, true, Durability::WorkingCopy)?;
    copy_tree_with(&source, workspace, true, Durability::WorkingCopy)
}
fn compose(root: &Path, project: &Project, directory: &Path) -> io::Result<()> {
    for (name, _) in project.folders() {
        if !valid_name(name) {
            return Err(io::Error::other("Invalid workspace folder name"));
        }
        if project.mounts.iter().any(|m| m.name == name) {
            return Err(io::Error::other(
                "That workspace name is already open. Change the name below and try again.",
            ));
        }
    }
    let combined = directory.join("combined");
    if let Some(grant) = &project.grant {
        if !crate::chat::valid_id(grant) {
            return Err(io::Error::other("Invalid project grant"));
        }
        let source = match &project.head {
            Some(head) if crate::chat::valid_id(head) => root.join("checkpoints").join(head),
            Some(_) => return Err(io::Error::other("Invalid project checkpoint")),
            None => root.join("grants").join(grant).join("seed"),
        };
        if project.named {
            copy_tree(&source, &combined, true)?;
        } else {
            let name = &project
                .mounts
                .first()
                .ok_or_else(|| io::Error::other("Missing existing folder identity"))?
                .name;
            if !valid_name(name) {
                return Err(io::Error::other("Invalid existing folder name"));
            }
            fs::create_dir(&combined)?;
            copy_tree(&source, &combined.join(name), true)?;
        }
    } else {
        fs::create_dir(&combined)?;
    }
    let incoming = directory.join("incoming");
    fs::create_dir(&incoming)?;
    for (index, (name, path)) in project.folders().enumerate() {
        let staged = incoming.join(if index == 0 {
            "project".to_owned()
        } else {
            format!("project-{index}")
        });
        seed(Path::new(path), &staged)?;
        // Refuse collisions with scratch files as well as previously opened folders.
        if combined.join(name).try_exists()? {
            return Err(io::Error::other(
                "That name is already used in the workspace. Choose another name.",
            ));
        }
        fs::rename(staged, combined.join(name))?;
    }
    // Each input was already copied and validated. Publish without another full
    // tree copy; the grant's parent is synced before attachment below.
    fs::rename(combined, directory.join("seed"))?;
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
        return Err(io::Error::other("Invalid project checkpoint ID"));
    }
    let directory = root.join("checkpoints");
    fs::create_dir_all(&directory)?;
    let staging = directory.join(format!(".pending-{id}"));
    if directory.join(id).exists() {
        return Err(io::Error::other("Project checkpoint already exists"));
    }
    copy_tree(workspace, &staging, true)?;
    fs::rename(staging, directory.join(id))?;
    fs::File::open(&directory)?.sync_all()?;
    attempt.project_checkpoint = Some(id.into());
    // Viewer limits cannot truncate the authoritative project checkpoint.
    attempt.artifact = preview().ok();
    Ok(())
}

/// Durable checkpoint of approval outcomes (`storage::save` outside tests).
type Save = fn(&State) -> io::Result<()>;
/// A folder import running in the background after the operator's approval.
/// Held in memory only: a restart interrupts it, grants nothing, and the
/// approval form reappears for the unchanged request.
struct Import {
    chat: String,
    request: String,
    started: std::time::Instant,
    entries: Arc<AtomicUsize>,
}
static IMPORTS: Mutex<Vec<Import>> = Mutex::new(Vec::new());
static IMPORT_THREADS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());
thread_local! {
    static IMPORT_PROGRESS: std::cell::RefCell<Option<Arc<AtomicUsize>>> = const { std::cell::RefCell::new(None) };
    /// When this thread's import last hinted readers about its progress.
    static IMPORT_HINTED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}
/// Counts entries copied by this thread's import; stopping Control abandons it.
/// The count is in memory only, so open pages are hinted (throttled) to re-read
/// the request panel that shows it.
fn copied_entry() -> io::Result<()> {
    IMPORT_PROGRESS.with(|progress| match progress.borrow().as_ref() {
        Some(_) if crate::shutdown::requested() => {
            Err(io::Error::other("Control is stopping; folder access was not granted"))
        }
        Some(entries) => {
            entries.fetch_add(1, Ordering::Relaxed);
            IMPORT_HINTED.with(|hinted| {
                let mut last = hinted.get();
                crate::live_updates::progress(&mut last, std::time::Instant::now());
                hinted.set(last);
            });
            Ok(())
        }
        None => Ok(()),
    })
}
/// The visible state of an import that has been approved but not yet attached.
pub struct Importing {
    pub request: String,
    pub elapsed: std::time::Duration,
    pub entries: usize,
}
pub fn importing(id: &str) -> Option<Importing> {
    IMPORTS
        .lock()
        .ok()?
        .iter()
        .find(|import| import.chat == id)
        .map(|import| Importing {
            request: import.request.clone(),
            elapsed: import.started.elapsed(),
            entries: import.entries.load(Ordering::Relaxed),
        })
}
/// Remove imports a crash or restart cut short. Call at startup while holding
/// the state lock and before serving, so no live import owns these directories.
pub fn discard_interrupted_imports() -> io::Result<()> {
    discard_interrupted_in(&root()?)
}
fn discard_interrupted_in(root: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(root.join("grants")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(|name| name.starts_with(STAGING)) {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}
/// Wait for approved imports during shutdown; each stops at its next entry.
pub fn join_imports() {
    let threads = IMPORT_THREADS
        .lock()
        .map(|mut threads| std::mem::take(&mut *threads))
        .unwrap_or_default();
    for thread in threads {
        let _ = thread.join();
    }
}
/// Removes the registry entry once the outcome is recorded, including on unwind.
struct Registered {
    chat: String,
    request: String,
}
impl Drop for Registered {
    fn drop(&mut self) {
        if let Ok(mut imports) = IMPORTS.lock() {
            imports.retain(|import| !(import.chat == self.chat && import.request == self.request));
        }
        // An import that ended without a checkpoint, or whose error was saved
        // while it still showed as running, must not leave pages on its progress.
        crate::live_updates::notify();
    }
}

// Retain a candidate on unwind: a checkpoint may already reference its files.
// Ordinary rejected/failed approvals still remove their unpublished directory.
struct PendingGrant(Option<PathBuf>);
impl Drop for PendingGrant {
    fn drop(&mut self) {
        if !std::thread::panicking()
            && let Some(path) = &self.0
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}
/// Called only after HTTP operator authentication, never from model tools.
/// Validates the displayed request and returns immediately; the copy runs on
/// its own thread. Failures are recorded beside the request (`Project::error`).
/// `Ok` means the approval was accepted or its outcome recorded; `Err` only
/// when even the error could not be saved.
fn start(shared: &Arc<Mutex<State>>, id: &str, request: &str, save: Save) -> io::Result<()> {
    let (project, entries) = match accept(shared, id, request) {
        Ok(Some(accepted)) => accepted,
        Ok(None) => return Ok(()),
        Err(error) => return record(shared, id, request, error, save),
    };
    let registered = Registered {
        chat: id.into(),
        request: request.into(),
    };
    let (thread_state, chat, approved) = (shared.clone(), id.to_owned(), request.to_owned());
    let spawned = std::thread::Builder::new()
        .name("control-import".into())
        .spawn(move || {
            let _registered = registered;
            IMPORT_PROGRESS.with(|progress| *progress.borrow_mut() = Some(entries));
            let result = import(&thread_state, &chat, &approved, project, save);
            IMPORT_PROGRESS.with(|progress| *progress.borrow_mut() = None);
            if let Err(error) = result {
                let _ = record(&thread_state, &chat, &approved, error, save);
            }
        });
    match spawned {
        Ok(thread) => {
            if let Ok(mut threads) = IMPORT_THREADS.lock() {
                threads.retain(|thread| !thread.is_finished());
                threads.push(thread);
            }
            Ok(())
        }
        // The closure, and with it the registry entry, was dropped unrun.
        Err(_) => record(
            shared,
            id,
            request,
            io::Error::other("Could not start the folder import; try again"),
            save,
        ),
    }
}
/// Show an import failure beside the request it belongs to. A request that has
/// since been replaced or approved keeps its own state.
fn record(shared: &Arc<Mutex<State>>, id: &str, request: &str, error: io::Error, save: Save) -> io::Result<()> {
    let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
    let chat = state
        .chats
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| io::Error::other("Chat not found"))?;
    if !chat
        .project
        .as_ref()
        .is_some_and(|p| p.request == request && p.needs_approval())
    {
        return Err(error);
    }
    let previous = chat.clone();
    chat.project.as_mut().unwrap().error = Some(error.to_string());
    chat.revision = chat.revision.saturating_add(1);
    if save(&state).is_err() {
        *state.chats.iter_mut().find(|c| c.id == id).unwrap() = previous;
        return Err(io::Error::other("Could not save folder error; access was not granted"));
    }
    Ok(())
}
/// Check the displayed request and register its import. `None`: this request
/// is already importing (a repeated click), so nothing new starts. Approving it
/// again once attached is refused: an approval is never replayed.
fn accept(shared: &Arc<Mutex<State>>, id: &str, request: &str) -> io::Result<Option<(Project, Arc<AtomicUsize>)>> {
    if crate::shutdown::requested() {
        return Err(io::Error::other("Control is stopping; folder access was not granted"));
    }
    let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
    let chat = state
        .chats
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| io::Error::other("Chat not found"))?;
    let mut imports = IMPORTS
        .lock()
        .map_err(|_| io::Error::other("Import state unavailable"))?;
    if let Some(running) = imports.iter().find(|import| import.chat == id) {
        return if running.request == request {
            Ok(None)
        } else {
            Err(io::Error::other(
                "Another folder is still importing into this chat; wait for it to finish",
            ))
        };
    }
    let project = chat
        .project
        .clone()
        .filter(|p| p.request == request && p.needs_approval())
        .ok_or_else(|| io::Error::other("Project request changed; review the current request"))?;
    if state
        .runtime
        .runs
        .iter()
        .any(|r| r.chat_id.as_deref() == Some(id) && r.status.is_active())
    {
        return Err(io::Error::other(
            "Cancel or finish pending work before granting project access",
        ));
    }
    crate::identity::authorize(&chat.actor, Action::Task).map_err(io::Error::other)?;
    let entries = Arc::new(AtomicUsize::new(0));
    imports.push(Import {
        chat: id.into(),
        request: request.into(),
        started: std::time::Instant::now(),
        entries: entries.clone(),
    });
    crate::live_updates::notify();
    Ok(Some((project, entries)))
}
/// Slow copies/Git run without holding the shared state lock; the approval is
/// rechecked before its durable attachment to the chat.
fn import(shared: &Arc<Mutex<State>>, id: &str, request: &str, project: Project, save: Save) -> io::Result<()> {
    import_in(&root()?, shared, id, request, project, save)
}
/// Staging prefix of grants still being composed. Nothing references them.
const STAGING: &str = ".pending-";
/// The copy is composed in `grants/.pending-<grant>` and renamed into place
/// under the state lock just before the checkpoint that attaches it. A failed
/// import removes its directory; one cut short by a crash or restart leaves
/// only a staging directory, discarded at the next start, so a partial copy is
/// never published or mounted.
fn import_in(
    root: &Path,
    shared: &Arc<Mutex<State>>,
    id: &str,
    request: &str,
    project: Project,
    save: Save,
) -> io::Result<()> {
    let grant = nonce();
    let grants = root.join("grants");
    fs::create_dir_all(&grants)?;
    let staging = grants.join(format!("{STAGING}{grant}"));
    fs::create_dir(&staging)?;
    let mut pending = PendingGrant(Some(staging.clone()));
    compose(root, &project, &staging)?;
    let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
    if crate::shutdown::requested() {
        return Err(io::Error::other(
            "Control stopped during approval; folder access was not granted",
        ));
    }
    if state
        .runtime
        .runs
        .iter()
        .any(|r| r.chat_id.as_deref() == Some(id) && r.status.is_active())
    {
        return Err(io::Error::other("Chat changed during approval; request access again"));
    }
    let chat = state
        .chats
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| io::Error::other("Chat not found"))?;
    if !chat.project.as_ref().is_some_and(|p| {
        p.request == request && p.needs_approval() && p.grant == project.grant && p.head == project.head
    }) {
        return Err(io::Error::other("Project request changed during approval"));
    }
    let previous = chat.clone();
    crate::identity::authorize(&chat.actor, Action::Task).map_err(io::Error::other)?;
    let directory = grants.join(&grant);
    fs::rename(&staging, &directory)?;
    pending.0 = Some(directory);
    fs::File::open(&grants)?.sync_all()?;
    chat.project.as_mut().unwrap().grant = Some(grant);
    chat.project.as_mut().unwrap().error = None;
    let approved = chat.project.as_mut().unwrap();
    approved.head = None;
    approved.pending = false;
    approved.named = true;
    approved.previous = None;
    approved.origin = None;
    approved.mounts.push(Mount {
        name: approved.name.clone(),
        path: approved.path.clone(),
        message_index: approved.message_index,
    });
    let also = std::mem::take(&mut approved.also);
    approved.mounts.extend(also);
    chat.workspace = None;
    chat.improve_mode = false;
    chat.revision = chat.revision.saturating_add(1);
    if save(&state).is_err() {
        *state.chats.iter_mut().find(|c| c.id == id).unwrap() = previous;
        return Err(io::Error::other("Could not save project approval"));
    }
    pending.0 = None;
    Ok(())
}

pub fn http(
    stream: &mut impl Write,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> bool {
    http_in(stream, shared, method, path, query, body, crate::storage::save)
}
fn http_in(
    stream: &mut impl Write,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
    save: Save,
) -> bool {
    if let Some(id) = path
        .strip_prefix("/dm/")
        .and_then(|p| p.strip_suffix("/preview/stop"))
        .filter(|id| crate::chat::valid_id(id))
    {
        if method != "POST" {
            let _ = control_http::respond(
                stream,
                "405 Method Not Allowed",
                "text/plain",
                b"",
                "",
                control_http::Policy::Restricted,
            );
            return true;
        }
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let result = crate::previews::stop(shared, id, fields.get("id").map(String::as_str).unwrap_or(""));
        let (status, text) = match result {
            Ok(()) => ("303 See Other", String::new()),
            Err(error) => ("400 Bad Request", crate::escape(&error.to_string())),
        };
        let _ = control_http::respond(
            stream,
            status,
            "text/plain",
            text.as_bytes(),
            &format!("Location: /dm/{id}\r\n"),
            control_http::Policy::Restricted,
        );
        return true;
    }
    let Some(id) = path
        .strip_prefix("/dm/")
        .and_then(|p| p.strip_suffix("/project"))
        .filter(|id| crate::chat::valid_id(id))
    else {
        return false;
    };
    // The approval page polls an import by its request nonce (`p-<hex>`).
    let watched = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("importing="))
        .filter(|request| {
            request.len() <= 64
                && request
                    .strip_prefix("p-")
                    .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        });
    enum Reply {
        Page(String),
        Redirect(String),
        Accepted,
        Dismissed(String),
    }
    let page = |state: &State, status: &str| {
        format!(
            "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><title>Open project</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/workspace.css'></head><body><main>{status}{}<a href='/dm/{id}'>Back to DM</a></main></body></html>",
            panel(&state.chats, id)
        )
    };
    // The chat client posts decisions with `response=json`, like port requests.
    let asynchronous = method == "POST"
        && crate::fields(&String::from_utf8_lossy(body))
            .get("response")
            .is_some_and(|value| value == "json");
    let result: io::Result<Reply> = (|| {
        if method == "GET" {
            let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let chat = state
                .chats
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| io::Error::other("Chat not found"))?;
            let running = chat
                .project
                .as_ref()
                .filter(|p| importing(id).is_some_and(|import| import.request == p.request));
            if let Some(project) = running {
                // Meta refresh needs no script; it returns to the chat once attached.
                return Ok(Reply::Page(format!(
                    "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='color-scheme' content='dark'><meta name='darkreader-lock'><meta http-equiv='refresh' content='1;url=/dm/{id}/project?importing={}'><title>Importing folder</title><link rel='stylesheet' href='/style.css'><link rel='stylesheet' href='/workspace.css'></head><body><main>{}<a href='/dm/{id}'>Back to DM</a></main></body></html>",
                    crate::escape(&project.request),
                    panel(&state.chats, id)
                )));
            }
            if let Some(request) = watched
                && chat
                    .project
                    .as_ref()
                    .is_some_and(|p| p.request == request && !p.needs_approval())
            {
                return Ok(Reply::Redirect(format!("/dm/{id}")));
            }
            return Ok(Reply::Page(page(&state, "")));
        }
        if method != "POST" {
            return Err(io::Error::other("Use GET or POST"));
        }
        let fields = crate::fields(&String::from_utf8_lossy(body));
        if fields.get("dismiss").is_some_and(|v| v == "yes") {
            // Names the displayed request, like approval; a stale one is a no-op.
            let request = fields.get("request").map(String::as_str).unwrap_or("");
            let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let index = state
                .chats
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| io::Error::other("Chat not found"))?;
            let (previous, runs, notice) = (
                state.chats[index].clone(),
                state.runtime.runs.len(),
                state.runtime.notice.clone(),
            );
            let message = match dismiss(&mut state, id, request).map_err(io::Error::other)? {
                None => "That folder request was already dismissed, replaced or approved; nothing changed.",
                Some(outcome) => {
                    if save(&state).is_err() {
                        state.chats[index] = previous;
                        state.runtime.runs.truncate(runs);
                        state.runtime.notice = notice;
                        return Err(io::Error::other(
                            "Could not save the dismissal; the request is unchanged",
                        ));
                    }
                    if matches!(outcome, crate::approvals::Outcome::Resumed(_)) {
                        crate::worker::wake();
                    }
                    crate::live_updates::notify();
                    DISMISSED
                }
            };
            return Ok(if asynchronous {
                Reply::Dismissed(message.into())
            } else {
                Reply::Page(page(
                    &state,
                    &format!("<p role='status'>{}</p>", crate::escape(message)),
                ))
            });
        }
        if fields.get("approve").is_some_and(|v| v == "yes") {
            // Respond at once; the approval page shows the import's progress,
            // returns to the chat when it opens, or shows its error inline.
            let request = fields.get("request").map(String::as_str).unwrap_or("");
            start(shared, id, request, save)?;
            let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let current = state.chats.iter().find(|c| c.id == id).and_then(|c| c.project.as_ref());
            if asynchronous {
                // Same contract as port and bot decisions: accepted, or the
                // reason shown in the form. Completion arrives as a live update.
                let running = importing(id).is_some_and(|import| import.request == request);
                return match current.filter(|p| p.request == request) {
                    Some(p) if running || !p.needs_approval() => Ok(Reply::Accepted),
                    Some(p) => Err(io::Error::other(
                        p.error
                            .clone()
                            .unwrap_or_else(|| "Folder was not opened; review the request".into()),
                    )),
                    None => Err(io::Error::other("Project request changed; review the current request")),
                };
            }
            let current = current.map(|p| p.request.clone()).unwrap_or_default();
            return Ok(Reply::Redirect(format!(
                "/dm/{id}/project?importing={}",
                crate::escape(&current)
            )));
        } else {
            let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let index = state
                .chats
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| io::Error::other("Chat not found"))?;
            let previous = state.chats[index].clone();
            let command = match fields.get("name").filter(|name| !name.is_empty()) {
                Some(name) if valid_name(name) => format!(
                    "/open --as {name} {}",
                    fields.get("path").map(String::as_str).unwrap_or("")
                ),
                Some(_) => {
                    return Err(io::Error::other(
                        "Choose a simple workspace name without spaces or path separators",
                    ));
                }
                None => format!("/open {}", fields.get("path").map(String::as_str).unwrap_or("")),
            };
            crate::chat::send(&mut state, id, &command, "website").map_err(io::Error::other)?;
            if save(&state).is_err() {
                state.chats[index] = previous;
                return Err(io::Error::other("Could not save project request"));
            }
        }
        Ok(Reply::Redirect(format!("/dm/{id}")))
    })();
    if asynchronous && method == "POST" {
        let (status, value) = match result {
            Ok(Reply::Dismissed(message)) => ("200 OK", json!({"ok":true,"message":message})),
            Ok(_) => ("200 OK", json!({"ok":true})),
            Err(error) => ("400 Bad Request", json!({"error":error.to_string()})),
        };
        let _ = control_http::respond(
            stream,
            status,
            "application/json",
            value.to_string().as_bytes(),
            "",
            control_http::Policy::Page,
        );
        return true;
    }
    match result {
        Ok(Reply::Accepted | Reply::Dismissed(_)) => {}
        Ok(Reply::Page(body)) => {
            let _ = control_http::respond(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                body.as_bytes(),
                "",
                control_http::Policy::RestrictedEmbedded,
            );
        }
        Ok(Reply::Redirect(location)) => {
            let _ = control_http::respond(
                stream,
                "303 See Other",
                "text/plain",
                b"",
                &format!("Location: {location}\r\n"),
                control_http::Policy::Restricted,
            );
        }
        Err(error) => {
            let body = crate::error_page(&error.to_string(), &format!("/dm/{id}/project"));
            let _ = control_http::respond(
                stream,
                "400 Bad Request",
                "text/html; charset=utf-8",
                body.as_bytes(),
                "",
                control_http::Policy::Restricted,
            );
        }
    }
    true
}

#[cfg(test)]
mod tests;
