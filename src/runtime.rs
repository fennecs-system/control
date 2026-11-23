use crate::escape;
use crate::identity::{self, Action, Actor};
pub use control_context::agent_policy::RunKind;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn hot_runs<S: serde::Serializer>(runs: &[Run], serializer: S) -> Result<S::Ok, S::Error> {
    if crate::history::is_persisting() {
        serializer.collect_seq(runs.iter().filter(|run| !run.archived()))
    } else {
        serializer.collect_seq(runs)
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Runtime {
    #[serde(skip)]
    pub execution: crate::scheduler::Limits,
    #[serde(skip)]
    pub models: Vec<crate::providers::Model>,
    #[serde(skip)]
    pub compaction: crate::compaction::Memory,
    #[serde(skip)]
    pub backend: crate::backend::Backend,
    // Read old checkpoints for migration; new configuration belongs in the home directory.
    #[serde(skip_serializing)]
    pub model: Option<String>,
    /// Every run, in creation order. Saves leave out runs kept in archive
    /// blocks (`storage::archive`); they are loaded back at startup.
    #[serde(serialize_with = "hot_runs")]
    pub runs: Vec<Run>,
    pub notice: String,
    // Why dispatch is paused, shown on queued chat messages; rebuilt by the next reconcile.
    #[serde(skip)]
    pub paused: Option<String>,
}
pub use crate::application::runs::Run;
use crate::application::runs::Submission;

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
                .map(|run| (run.id.clone(), run.status.clone(), run.result.to_string(), run.question.clone())).collect(),
            notice: runtime.notice.clone(),
        }
    }
    pub(crate) fn restore(self, runtime: &mut Runtime) {
        for (id, status, result, question) in self.runs {
            if let Some(run) = runtime.runs.iter_mut().find(|run| run.id == id) {
                run.status = status;
                *run.result = result;
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
        if objective.is_empty() || control_context::limits::message_too_long(&objective) {
            return Err(control_context::limits::CHAT_MESSAGE_ERROR);
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let request = Submission { objective, kind, source: source.into(), actor, action };
        self.runs.push(request.into_run(format!("{stamp:x}-{}", self.runs.len()),
            self.runs.iter().map(|r| r.queue_order).max().unwrap_or(0).saturating_add(1),
            self.backend, Some(self.configured_model())));
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
                return crate::history_view::AFTER_ACTION;
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
                    && crate::application::runs::status::resumable(&r.status)
                    && !crate::design::asked(r)
            }) {
                if let Err(message) = identity::authorize(&run.actor, run.action) {
                    self.notice = message.into();
                    return crate::history_view::AFTER_ACTION;
                }
                run.status = "queued".into();
                run.queue_order = order;
                run.result.clear();
            }
        }
        crate::history_view::AFTER_ACTION
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
                // The Bots page process tree shows active turns; only queued work
                // that has not started is listed here.
                let mut h = String::new();
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
            "integrations" => Some(integrations()),
            _ => None,
        }
    }
}

/// Attempts and provider-reported tokens for one run, when any were measured.
pub(crate) fn usage(run: &Run) -> String {
    let (input, cached, output, measured) = run.attempts.iter().filter_map(|attempt| attempt.tokens).fold((0, 0, 0, 0), |(i, c, o, n), t| (i + t.input_tokens, c + t.cached_input_tokens, o + t.output_tokens, n + 1));
    let running = run.attempts.last().is_some_and(crate::usage::Attempt::running);
    let partial = run.attempts.iter().any(|attempt| attempt.accounting == crate::usage::Accounting::Partial);
    let bound = if running { " so far" } else if partial { " at least" } else { "" };
    match (run.attempts.len(), measured) {
        (0, _) => String::new(),
        (attempts, 0) => format!(" · {attempts} attempt(s) · tokens unknown"),
        (attempts, _) => format!(" · {attempts} attempt(s) · {input} input ({cached} cached) · {output} output tokens{bound} ({measured}/{attempts} measured)"),
    }
}
fn run_record(run: &Run) -> String {
    let mut html = format!("<article class='record'><span class='tag'>{}</span><span class='eyebrow'>{} / {}</span><h3>{}</h3><p class='muted tiny'>run {} · {}{}{}</p>", escape(&run.status), escape(run.kind.as_str()), escape(&run.source), escape(&run.objective), escape(&run.id), escape(run.backend.label()), run.model.as_ref().map(|m| format!(" · {}", escape(m))).unwrap_or_default(), usage(run));
    if let Some(persona)=&run.persona {
        html.push_str(&format!("<span class='tag'>{} · depth {}</span>",if persona.role==crate::delegation::Role::Judge{"judge · read only"}else{"worker"},persona.depth));
        if let Some(bundle)=&persona.bundle {
            html.push_str(&format!("<a class='tag' href='/personas?id={}'>persona {}@{}</a>",escape(&bundle.id),escape(&bundle.id),bundle.revision));
        }
    }
    if !run.result.is_empty() {
        html.push_str(&format!(
            "<pre class='run-result'>{}</pre>",
            escape(&run.result)
        ));
    }
    if let Some(chat) = run.chat_id.as_ref().filter(|id| crate::chat::valid_id(id)) {
        html.push_str(&format!("<a href='/dm/{}'>Open DM ↗</a>", escape(chat)));
    }
    if ["queued", "running", "waiting"].contains(&run.status.as_str()) {
        html.push_str(&format!("<form class='decision-form' method='post' action='/cancel-run'><input type='hidden' name='id' value='{}'><button>Cancel</button></form>",escape(&run.id)));
    }
    if crate::application::runs::status::resumable(&run.status) {
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
    let backends: String = crate::backend::Backend::hosted()
    .map(|backend| {
        // The product name: the label without its vendor.
        let name = backend.label().rsplit(" / ").next().unwrap_or_default();
        let login = backend.agent().credentials_ready().is_ok();
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
    format!("<h2>Connections</h2><div class='record'><h3>Local service</h3><p>Running on this laptop</p></div><div class='section-heading'><h3>Agent backends</h3><a href='/?view=providers'>Models &amp; providers ↗</a></div>{backends}<div class='record'><h3>Control MCP</h3><p>Local stdio · <code>control mcp</code></p></div><div class='record'><span class='tag'>{label}</span><h3>Discord · direct messages</h3>{profile}<p class='muted'>Only approved users can chat. DMs are not shared with rooms.</p>{}<a href='/?view=discord-access'>Manage access ↗</a></div>", crate::channel_settings::panel())
}

#[cfg(test)]
mod tests;
