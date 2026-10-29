use crate::escape;
use crate::identity::{self, Action, Actor};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Runtime {
    #[serde(skip)]
    pub models: Vec<crate::providers::Model>,
    #[serde(skip)]
    pub backend: crate::backend::Backend,
    // Read old checkpoints for migration; new configuration belongs in the home directory.
    #[serde(skip_serializing)]
    pub model: Option<String>,
    pub runs: Vec<Run>,
    pub notice: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub backend: crate::backend::Backend,
    #[serde(default)]
    pub queue_order: u64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub context: Option<control_context::Resolved>,
    #[serde(default)]
    pub lenses: Option<Vec<String>>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub attempts: Vec<crate::usage::Attempt>,
    #[serde(default)]
    pub actor: Actor,
    #[serde(default)]
    pub action: Action,
    pub id: String,
    pub objective: String,
    pub kind: String,
    pub status: String,
    pub source: String,
    pub result: String,
}

pub fn worker_image() -> Option<String> {
    std::env::var("CONTROL_WORKER_IMAGE")
        .ok()
        .or_else(|| {
            fs::read_to_string(crate::config::directory().ok()?.join("worker-image-id")).ok()
        })
        .map(|s| s.trim().to_owned())
        .filter(|s| pinned_image(s))
}
pub(crate) fn pinned_image(s: &str) -> bool {
    if s.starts_with('-') || s.chars().any(char::is_whitespace) {
        return false;
    }
    let digest = s
        .strip_prefix("sha256:")
        .or_else(|| s.split_once("@sha256:").map(|(_, hash)| hash));
    digest.is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}
pub fn engine_ready() -> bool {
    docker_query(&["info", "--format", "{{.OSType}}"]).is_ok_and(|output| output.trim() == "linux")
}
pub fn docker_query(arguments: &[&str]) -> std::io::Result<String> {
    use std::{
        io::Read,
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut child = Command::new("docker")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(std::io::Error::other("Container engine request failed"));
            }
            let mut output = String::new();
            child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("No engine response"))?
                .take(8192)
                .read_to_string(&mut output)?;
            return Ok(output);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("Container engine response timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn field(f: &HashMap<String, String>, name: &str) -> String {
    f.get(name).map(|s| s.trim().to_owned()).unwrap_or_default()
}
impl Runtime {
    pub fn configured_model(&self) -> String {
        self.model
            .as_deref()
            .filter(|s| crate::usage::valid_model(s))
            .map(str::to_owned)
            .unwrap_or_else(|| self.backend.default_model())
    }
    pub fn enqueue(&mut self, objective: String, kind: &str, source: &str) {
        let action = if kind == "self-improvement" {
            Action::Improve
        } else {
            Action::Task
        };
        if let Err(message) =
            self.enqueue_for(objective, kind, source, Actor::LocalOperator, action)
        {
            self.notice = message.into();
        }
    }
    pub fn enqueue_for(
        &mut self,
        objective: String,
        kind: &str,
        source: &str,
        actor: Actor,
        action: Action,
    ) -> Result<(), &'static str> {
        identity::authorize(&actor, action)?;
        if objective.is_empty() || objective.chars().count() > 2000 {
            return Err("An objective must contain 1–2000 characters.");
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.runs.push(Run {
            backend: self.backend,
            queue_order: self
                .runs
                .iter()
                .map(|r| r.queue_order)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
            model: Some(self.configured_model()),
            context: None,
            lenses: None,
            chat_id: None,
            attempts: Vec::new(),
            actor,
            action,
            id: format!("{stamp:x}-{}", self.runs.len()),
            objective,
            kind: kind.into(),
            source: source.into(),
            status: "queued".into(),
            result: String::new(),
        });
        self.notice = "Run queued.".into();
        Ok(())
    }
    pub fn action(&mut self, path: &str, f: &HashMap<String, String>) -> &'static str {
        self.notice.clear();
        if path == "/improve" {
            self.enqueue(field(f, "objective"), "self-improvement", "website");
        }
        if path == "/task" {
            self.enqueue(field(f, "objective"), "task", "website");
        }
        if path == "/retry-run" {
            let id = field(f, "id");
            let order = self
                .runs
                .iter()
                .map(|r| r.queue_order)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            if let Some(run) = self.runs.iter_mut().find(|r| {
                r.id == id && ["blocked", "failed", "interrupted"].contains(&r.status.as_str())
            }) {
                if let Err(message) = identity::authorize(&run.actor, run.action) {
                    self.notice = message.into();
                    return "/?view=runs";
                }
                run.status = "queued".into();
                run.queue_order = order;
                run.result.clear();
            }
        }
        "/?view=runs"
    }
    pub fn render(&self, view: &str, _lens: &str) -> Option<String> {
        match view {
            "discord-access" => {
                let mut h="<span class='eyebrow'>DISCORD</span><h2>Access</h2><p class='muted'>Access uses Discord IDs. Display names cannot grant permissions.</p>".to_owned();
                match identity::Policy::load() {
                    Ok(policy) => {
                        h.push_str("<p>DMs only. Server messages are ignored.</p>");
                        for p in policy.principals {
                            h.push_str(&format!("<article class='record'><span class='tag'>{:?}</span><h3>@{}</h3><p>Discord ID: {}</p><p class='muted'>{}</p></article>",p.role,escape(&p.label),p.user_id.map(|id|id.to_string()).unwrap_or_else(||"not bound — no access".into()),if p.role==identity::Role::Architect {"Can run tasks and request source changes for local review."}else{"Can run tasks. Cannot change Control, access settings, or deploy."}));
                        }
                    }
                    Err(message) => {
                        h.push_str(&format!("<div class='callout'>{}</div>", escape(message)))
                    }
                }
                h.push_str("<div class='callout'>Bind IDs locally with <code>cargo run -- --bind-discord &lt;label&gt; &lt;ID&gt;</code>. Access settings cannot be changed through Discord.</div>");
                Some(h)
            }
            "evolution" => Some("<div class='section-heading'><h2>Improve Control</h2><a href='/?view=runs'>View runs ↗</a></div><form method='post' action='/improve'><label>What should Control improve?<textarea name='objective' required maxlength='2000' rows='4' placeholder='Describe the change…'></textarea></label><button class='primary'>Queue improvement ↗</button></form>".into()),
            "agents" => {
                let mut h = "<div class='section-heading'><h2>Agents</h2><a href='/?view=runs'>Run history ↗</a></div>".to_owned();
                let mut running = self.runs.iter().filter(|r| r.status == "running").peekable();
                if running.peek().is_none() { h.push_str("<div class='empty'>No active agents.</div>"); }
                for run in running { h.push_str(&run_record(run)); }
                let mut queued = self.runs.iter().filter(|r| r.status == "queued").collect::<Vec<_>>();
                queued.sort_by_key(|r| r.queue_order);
                if !queued.is_empty() {
                    h.push_str("<h3 class='form-title'>Queued</h3>");
                    for run in queued { h.push_str(&run_record(run)); }
                }
                Some(h)
            }
            "runs" => {
                let mut h = "<h2>Runs</h2>".to_owned();
                if !self.notice.is_empty() {
                    h.push_str(&format!("<p role='status'>{}</p>", escape(&self.notice)));
                }
                if self.runs.is_empty() {
                    h.push_str("<div class='empty'>No runs yet.</div>");
                }
                for r in self.runs.iter().rev() {
                    h.push_str(&run_record(r));
                }
                Some(h)
            }
            "integrations" => Some(integrations()),
            _ => None,
        }
    }
}

fn run_record(run: &Run) -> String {
    let mut html = format!("<article class='record'><span class='tag'>{}</span><span class='eyebrow'>{} / {}</span><h3>{}</h3><p class='muted tiny'>run {} · {}{}</p>", escape(&run.status), escape(&run.kind), escape(&run.source), escape(&run.objective), escape(&run.id), escape(run.backend.label()), run.model.as_ref().map(|m| format!(" · {}", escape(m))).unwrap_or_default());
    if !run.result.is_empty() {
        html.push_str(&format!(
            "<pre class='run-result'>{}</pre>",
            escape(&run.result)
        ));
    }
    if let Some(chat) = run.chat_id.as_ref().filter(|id| crate::chat::valid_id(id)) {
        html.push_str(&format!("<a href='/chat/{}'>Open chat ↗</a>", escape(chat)));
    }
    if ["blocked", "failed", "interrupted"].contains(&run.status.as_str()) {
        html.push_str(&format!("<form method='post' action='/retry-run'><input type='hidden' name='id' value='{}'><button class='primary'>Retry</button></form>",escape(&run.id)));
    }
    html.push_str("</article>");
    html
}

/// Only allow known source roots. Never recursively copy the repository root,
/// .env, Git metadata, runtime records, or credentials into an agent workspace.
pub fn snapshot(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for name in [
        "src",
        "assets",
        "crates",
        "examples",
        "web",
        "tools",
        "docs",
        "roadmap.md",
        "Cargo.toml",
        "Cargo.lock",
        "DESIGN.md",
        "README.md",
        "flake.nix",
        "flake.lock",
    ] {
        let from = source.join(name);
        if from.exists() {
            copy_source(&from, &destination.join(name))?;
        }
    }
    Ok(())
}
fn copy_source(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.')
                || ["target", "dist", "node_modules", "pkg"].contains(&name_str.as_ref())
            {
                continue;
            }
            copy_source(&entry.path(), &to.join(name))?;
        }
    } else if meta.is_file() {
        fs::copy(from, to)?;
    }
    Ok(())
}
pub fn workspace(id: &str) -> Result<PathBuf, &'static str> {
    if !crate::chat::valid_id(id) {
        return Err("Invalid workspace ID");
    }
    Ok(crate::config::state("runs")?.join(id).join("workspace"))
}
fn integrations() -> String {
    let status: serde_json::Value = crate::config::state("discord-status.json")
        .ok()
        .and_then(|p| fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let timestamp = status["updated_unix"].as_u64().unwrap_or(0);
    let age = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(timestamp);
    let online = age < 30 && status["ready"].as_bool() == Some(true);
    let label = if online {
        "listening for DMs"
    } else {
        "offline / starting"
    };
    let profile=status["bot_id"].as_str().and_then(|s|s.parse::<identity::DiscordId>().ok()).map(|id|format!("<p><a href='https://discord.com/users/{id}' rel='noreferrer'>Open Control in Discord ↗</a></p>")).unwrap_or_default();
    format!("<span class='eyebrow'>INTEGRATIONS</span><h2>Connections</h2><div class='record'><h3>Local service</h3><p>Running on this laptop</p></div><div class='record'><h3>Codex workers</h3><p>{}</p><p class='muted'>Requires Codex login and the container engine.</p></div><div class='record'><span class='tag'>{label}</span><h3>Discord · direct messages</h3><p>Message Control directly. Architects can use /improve to request source changes.</p>{profile}<p class='muted'>Only approved users can chat. DMs are not shared with rooms.</p><a href='/?view=discord-access'>Manage access ↗</a></div>",if worker_image().is_some(){"Nix container configured."}else{"Blocked: worker image is not configured."})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workers_require_content_addressed_images() {
        assert!(!pinned_image("control-worker:latest"));
        assert!(!pinned_image("control-worker@sha256:short"));
        assert!(pinned_image(&format!("sha256:{}", "a".repeat(64))));
        assert!(pinned_image(&format!(
            "control-worker@sha256:{}",
            "a".repeat(64)
        )));
    }
    #[test]
    fn retry_does_not_repeat_completed_work() {
        let mut rt = Runtime::default();
        rt.enqueue("Improve review".into(), "self-improvement", "website");
        let id = rt.runs[0].id.clone();
        rt.runs[0].status = "completed".into();
        rt.action("/retry-run", &HashMap::from([("id".into(), id)]));
        assert_eq!(rt.runs[0].status, "completed");
    }
    #[test]
    fn snapshots_exclude_secrets_and_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "control-snapshot-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let dest = root.join("dest");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::write(source.join(".env"), "fake-secret").unwrap();
        fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(source.join("tools/playground")).unwrap();
        fs::write(source.join("tools/playground/main.rs"), "fn main() {}").unwrap();
        fs::write(source.join("tools/.env"), "nested-secret").unwrap();
        fs::write(source.join("src/.env"), "nested-secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.join(".env"), source.join("src/linked.rs")).unwrap();
        snapshot(&source, &dest).unwrap();
        assert!(dest.join("src/main.rs").exists());
        assert!(dest.join("tools/playground/main.rs").exists());
        assert!(!dest.join("tools/.env").exists());
        assert!(!dest.join(".env").exists());
        assert!(!dest.join("src/.env").exists());
        assert!(!dest.join("src/linked.rs").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
