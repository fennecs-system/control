use crate::escape;
use crate::identity::{self, Action, Actor};
pub use control_context::agent_policy::RunKind;
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
    pub execution: crate::scheduler::Limits,
    #[serde(skip)]
    pub models: Vec<crate::providers::Model>,
    #[serde(skip)]
    pub backend: crate::backend::Backend,
    // Read old checkpoints for migration; new configuration belongs in the home directory.
    #[serde(skip_serializing)]
    pub model: Option<String>,
    pub runs: Vec<Run>,
    pub notice: String,
    // Why dispatch is paused, shown on queued chat messages; rebuilt by the next reconcile.
    #[serde(skip)]
    pub paused: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_turn: Option<crate::bots::Turn>,
    #[serde(default, skip_serializing_if="Vec::is_empty")]
    pub resource_grants: Vec<crate::services::Grant>,
    #[serde(default)]
    pub spawns: Vec<crate::spawning::Batch>,
    #[serde(default)]
    pub room_wake_used: bool,
    #[serde(default)]
    pub persona: Option<crate::delegation::Persona>,
    #[serde(default)]
    pub delegation: Option<crate::delegation::Delegation>,
    #[serde(default)]
    pub question: Option<crate::design::Question>,
    #[serde(default)]
    pub continuation: Option<crate::design::Continuation>,
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
    pub kind: RunKind,
    pub status: String,
    pub source: String,
    pub result: String,
}
impl Run {
    /// Milliseconds since the epoch when the run was queued, from its identifier.
    pub(crate) fn created_ms(&self) -> Option<u64> {
        let nanos = u128::from_str_radix(self.id.split('-').next()?, 16).ok()?;
        u64::try_from(nanos / 1_000_000).ok()
    }
}

// Pending transitions change status/result and possibly a design answer. Keep
// rollback bounded, without cloning attempts or completed conversation history.
pub(crate) struct RunStatusCheckpoint {
    runs: Vec<(String, String, String, Option<crate::design::Question>)>,
    notice: String,
}
impl RunStatusCheckpoint {
    pub(crate) fn capture(runtime: &Runtime, matches: impl Fn(&Run) -> bool) -> Self {
        let mut selected=runtime.runs.iter().filter(|r|matches(r)).map(|r|r.id.as_str()).collect::<std::collections::HashSet<_>>();
        loop {
            let before=selected.len();
            for run in &runtime.runs {
                if run.persona.as_ref().and_then(|p|p.parent.as_deref()).is_some_and(|parent|selected.contains(parent)) {
                    selected.insert(run.id.as_str());
                }
            }
            if before==selected.len(){break;}
        }
        Self {
            runs: runtime.runs.iter()
                .filter(|run| ["queued", "running", "cancelling", "waiting"].contains(&run.status.as_str()) && selected.contains(run.id.as_str()))
                .map(|run| (run.id.clone(), run.status.clone(), run.result.clone(), run.question.clone())).collect(),
            notice: runtime.notice.clone(),
        }
    }
    pub(crate) fn restore(self, runtime: &mut Runtime) {
        for (id, status, result, question) in self.runs {
            if let Some(run) = runtime.runs.iter_mut().find(|run| run.id == id) {
                run.status = status;
                run.result = result;
                run.question = question;
            }
        }
        runtime.notice = self.notice;
    }
    pub(crate) fn save(self, state: &mut crate::State, save: impl FnOnce(&crate::State) -> std::io::Result<()>) -> std::io::Result<()> {
        if let Err(error) = save(state) {
            self.restore(&mut state.runtime);
            return Err(error);
        }
        Ok(())
    }
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

// Drain while the process runs: waiting first can deadlock on a full stdout pipe.
// Ownership inspection must fail closed on oversized output, never use a prefix.
pub(crate) fn engine_output(command: &mut Command, timeout: std::time::Duration) -> std::io::Result<String> {
    engine_output_diagnostic(command, timeout, false)
}
pub(crate) fn engine_output_diagnostic(command: &mut Command, timeout: std::time::Duration, diagnostic: bool) -> std::io::Result<String> {
    use std::{
        io::Read,
        process::Stdio,
        sync::mpsc,
        time::{Duration, Instant},
    };
    const OUTPUT_LIMIT: u64 = 1024 * 1024;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if diagnostic { Stdio::piped() } else { Stdio::null() })
        .spawn()?;
    let errors = child.stderr.take().map(|stderr| {
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr.take(4097).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
        receiver
    });
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("No engine response"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                if let Some(errors) = errors {
                    let bytes = errors.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|_|std::io::Error::other("Container engine diagnostic timed out"))??;
                    let message = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
                    return Err(std::io::Error::other(format!("Container engine request failed: {}", message.trim())));
                }
                return Err(std::io::Error::other("Container engine request failed"));
            }
            let output = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| std::io::Error::other("Container engine response timed out"))??;
            if output.len() as u64 > OUTPUT_LIMIT {
                return Err(std::io::Error::other(
                    "Container engine response exceeded its limit",
                ));
            }
            return String::from_utf8(output).map_err(std::io::Error::other);
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
    pub fn enqueue(&mut self, objective: String, kind: RunKind, source: &str) {
        let action = if kind == RunKind::SelfImprovement {
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
        kind: RunKind,
        source: &str,
        actor: Actor,
        action: Action,
    ) -> Result<(), &'static str> {
        identity::authorize(&actor, action)?;
        crate::scheduler::admit(self)?;
        if objective.is_empty() || objective.chars().count() > 2000 {
            return Err("An objective must contain 1–2000 characters.");
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.runs.push(Run {
            bot_turn: None,
            resource_grants: Vec::new(),
            spawns: Vec::new(),
            room_wake_used: false,
            persona: None,
            delegation: None,
            question: None,
            continuation: None,
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
            kind,
            source: source.into(),
            status: "queued".into(),
            result: String::new(),
        });
        self.notice = "Run queued.".into();
        crate::worker::wake();
        Ok(())
    }
    pub fn action(&mut self, path: &str, f: &HashMap<String, String>) -> &'static str {
        self.notice.clear();
        if path == "/cancel-run" {
            let id = field(f, "id");
            self.notice = crate::lifecycle::request_cancel(self, &id, &Actor::LocalOperator)
                .map(|()| "Cancellation requested.".to_owned())
                .unwrap_or_else(str::to_owned);
        }
        if path == "/task" {
            self.enqueue(field(f, "objective"), RunKind::Task, "website");
        }
        if path == "/retry-run" {
            if let Err(message) = crate::scheduler::admit(self) {
                self.notice = message.into();
                return "/?view=runs";
            }
            let id = field(f, "id");
            let order = self
                .runs
                .iter()
                .map(|r| r.queue_order)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            if let Some(run) = self.runs.iter_mut().find(|r| {
                r.id == id
                    && ["blocked", "failed", "interrupted", "cancelled"]
                        .contains(&r.status.as_str())
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
            "agents" => {
                let mut h = "<div class='section-heading'><h2>Bots</h2><a href='/?view=runs'>Run history ↗</a></div>".to_owned();
                let mut running = self
                    .runs
                    .iter()
                    .filter(|r| ["running", "cancelling", "waiting"].contains(&r.status.as_str()))
                    .peekable();
                if running.peek().is_none() {
                    h.push_str("<div class='empty'>No active agents.</div>");
                }
                for run in running {
                    h.push_str(&run_record(run));
                }
                let mut queued = self
                    .runs
                    .iter()
                    .filter(|r| r.status == "queued")
                    .collect::<Vec<_>>();
                queued.sort_by_key(|r| r.queue_order);
                if !queued.is_empty() {
                    h.push_str("<h3 class='form-title'>Queued</h3>");
                    for run in queued {
                        h.push_str(&run_record(run));
                    }
                }
                Some(h)
            }
            "runs" => {
                Some(self.runs_page(None,None))
            }
            "integrations" => Some(integrations()),
            _ => None,
        }
    }
    pub fn runs_page(&self, older:Option<&str>, newer:Option<&str>)->String {
        let find=|id:Option<&str>|id.and_then(|id|self.runs.iter().rev().position(|run|run.id==id));
        let range=crate::pagination::window(self.runs.len(),find(older),find(newer));
        let mut html=crate::navigation::history_tabs(true);
        if !self.notice.is_empty(){html.push_str(&format!("<p role='status'>{}</p>",escape(&self.notice)));}
        if self.runs.is_empty(){html.push_str("<div class='empty'>No runs yet.</div>");}
        for run in self.runs.iter().rev().skip(range.start).take(range.len()){html.push_str(&run_record(run));}
        let id=|index:usize|self.runs.get(self.runs.len().checked_sub(index+1)?).map(|run|run.id.as_str());
        html.push_str(&crate::pagination::controls("/?view=runs",(range.start>0).then(||id(range.start)).flatten(),(range.end<self.runs.len()).then(||id(range.end-1)).flatten()));
        html
    }
}

fn run_record(run: &Run) -> String {
    let mut html = format!("<article class='record'><span class='tag'>{}</span><span class='eyebrow'>{} / {}</span><h3>{}</h3><p class='muted tiny'>run {} · {}{}</p>", escape(&run.status), escape(run.kind.as_str()), escape(&run.source), escape(&run.objective), escape(&run.id), escape(run.backend.label()), run.model.as_ref().map(|m| format!(" · {}", escape(m))).unwrap_or_default());
    if let Some(persona)=&run.persona {
        html.push_str(&format!("<span class='tag'>{} · depth {}</span>",if persona.role==crate::delegation::Role::Judge{"judge · read only"}else{"worker"},persona.depth));
    }
    if !run.result.is_empty() {
        html.push_str(&format!(
            "<pre class='run-result'>{}</pre>",
            escape(&run.result)
        ));
    }
    if let Some(chat) = run.chat_id.as_ref().filter(|id| crate::chat::valid_id(id)) {
        html.push_str(&format!("<a href='/chat/{}'>Open chat ↗</a>", escape(chat)));
    }
    if ["queued", "running", "waiting"].contains(&run.status.as_str()) {
        html.push_str(&format!("<form class='decision-form' method='post' action='/cancel-run'><input type='hidden' name='id' value='{}'><button>Cancel</button></form>",escape(&run.id)));
    }
    if ["blocked", "failed", "interrupted", "cancelled"].contains(&run.status.as_str()) {
        html.push_str(&format!("<form class='decision-form' method='post' action='/retry-run'><input type='hidden' name='id' value='{}'><button class='primary'>Retry</button></form>",escape(&run.id)));
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
        "tests",
        ".github",
        "assets",
        "crates",
        "examples",
        "web",
        "tools",
        "docs",
        "hosting",
        "roadmap.md",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "AGENTS.md",
        "CONTRIBUTING.md",
        "DESIGN.md",
        "README.md",
        "flake.nix",
        "flake.lock",
    ] {
        let from = source.join(name);
        match fs::symlink_metadata(&from) {
            Ok(_) => copy_source(&from, &destination.join(name))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
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
    let backends: String = [
        (
            crate::backend::Backend::Codex,
            "Codex",
            crate::worker::credentials_ready().is_ok(),
        ),
        (
            crate::backend::Backend::Claude,
            "Claude Code",
            crate::claude::credentials_ready().is_ok(),
        ),
    ]
    .into_iter()
    .map(|(backend, name, login)| {
        format!(
            "<div class='record'><h3>{name}</h3><p>{} · {}</p></div>",
            if backend.image().is_some() {
                "Image pinned"
            } else {
                "Image missing"
            },
            if login {
                "Login present"
            } else {
                "Login missing"
            }
        )
    })
    .collect();
    format!("<h2>Connections</h2><div class='record'><h3>Local service</h3><p>Running on this laptop</p></div><div class='section-heading'><h3>Agent backends</h3><a href='/?view=providers'>Models &amp; providers ↗</a></div>{backends}<div class='record'><h3>Control MCP</h3><p>Local stdio · <code>control mcp</code></p></div><div class='record'><span class='tag'>{label}</span><h3>Discord · direct messages</h3>{profile}<p class='muted'>Only approved users can chat. DMs are not shared with rooms.</p><a href='/?view=discord-access'>Manage access ↗</a></div>")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_cancel_form_checkpoint_restores_target_run_and_notice() {
        for status in ["queued", "running"] {
            let mut state = crate::State::default();
            state.runtime.enqueue("Keep my work".into(), RunKind::Task, "test");
            state.runtime.runs[0].status = status.into();
            state.runtime.runs[0].result = "Existing progress".into();
            state.runtime.notice = "Before cancellation".into();
            let id = state.runtime.runs[0].id.clone();
            let checkpoint = RunStatusCheckpoint::capture(&state.runtime, |run| run.id == id);
            let before = serde_json::to_vec(&state).unwrap();
            state.runtime.action("/cancel-run", &HashMap::from([("id".into(), id)]));
            assert!(checkpoint.save(&mut state, |_| Err(std::io::Error::other("disk full"))).is_err());
            assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        }
    }
    #[test]
    fn engine_inspection_drains_pipes_and_rejects_partial_results() {
        let output = engine_output(
            Command::new("head").args(["-c", "131072", "/dev/zero"]),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.len(), 131072);
        assert!(engine_output(
            Command::new("head").args(["-c", "1048577", "/dev/zero"]),
            std::time::Duration::from_secs(5),
        )
        .is_err());
    }
    #[test]
    fn engine_inspection_deadline_does_not_wait_for_process_exit() {
        let started = std::time::Instant::now();
        assert!(engine_output(
            Command::new("sleep").arg("5"),
            std::time::Duration::from_millis(30),
        )
        .is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
    #[test]
    fn engine_creation_diagnostics_are_opt_in_and_bounded() {
        let missing = "/control-qa-missing-storage-probe";
        let error = engine_output_diagnostic(Command::new("ls").arg(missing), std::time::Duration::from_secs(2), true).unwrap_err();
        assert!(error.to_string().contains(missing));
        let redacted = engine_output(Command::new("ls").arg(missing), std::time::Duration::from_secs(2)).unwrap_err();
        assert_eq!(redacted.to_string(), "Container engine request failed");
        let error = engine_output_diagnostic(Command::new("sh").args(["-c", "head -c 65536 /dev/zero >&2; exit 1"]), std::time::Duration::from_secs(2), true).unwrap_err();
        assert!(error.to_string().len() <= 4096 + "Container engine request failed: ".len());
    }
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
        rt.enqueue("Improve review".into(), RunKind::SelfImprovement, "website");
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
        fs::create_dir_all(source.join("assets")).unwrap();
        fs::write(source.join("assets/discord-access.example.json"), include_bytes!("../assets/discord-access.example.json")).unwrap();
        fs::write(source.join("discord-access.json"), "private-node-config").unwrap();
        fs::write(source.join(".env"), "fake-secret").unwrap();
        fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(source.join("tests")).unwrap();
        fs::create_dir_all(source.join(".github/workflows")).unwrap();
        fs::write(source.join("tests/integration.rs"), "#[test] fn fixture() {}").unwrap();
        fs::write(source.join(".github/workflows/rust.yml"), "name: fixture").unwrap();
        fs::create_dir_all(source.join("tools/playground")).unwrap();
        fs::write(source.join("tools/playground/main.rs"), "fn main() {}").unwrap();
        fs::write(source.join("tools/.env"), "nested-secret").unwrap();
        fs::write(source.join("src/.env"), "nested-secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.join(".env"), source.join("src/linked.rs")).unwrap();
        snapshot(&source, &dest).unwrap();
        assert!(dest.join("src/main.rs").exists());
        assert_eq!(fs::read(dest.join("assets/discord-access.example.json")).unwrap(), include_bytes!("../assets/discord-access.example.json"));
        assert!(!dest.join("discord-access.json").exists());
        assert!(dest.join("tests/integration.rs").exists());
        assert!(dest.join(".github/workflows/rust.yml").exists());
        assert!(dest.join("tools/playground/main.rs").exists());
        assert!(!dest.join("tools/.env").exists());
        assert!(!dest.join(".env").exists());
        assert!(!dest.join("src/.env").exists());
        assert!(!dest.join("src/linked.rs").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
