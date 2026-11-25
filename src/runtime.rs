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

fn hot_runs<S: serde::Serializer>(runs: &Runs, serializer: S) -> Result<S::Ok, S::Error> {
    if crate::history::is_persisting() {
        serializer.collect_seq(runs.iter().filter(|run| !run.archived()))
    } else {
        serializer.collect_seq(runs.iter())
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
    /// `<config>/accounts.json`; empty: each backend has only its implicit account.
    #[serde(skip)]
    pub accounts: crate::accounts::Store<crate::backend::Backend>,
    /// When accounts.json was last read, to reload it once it changes.
    #[serde(skip)]
    pub accounts_read: Option<std::time::SystemTime>,
    /// Accounts cooling down after a usage limit, saved with the state in the
    /// same transaction as the attempt that reported it. Absent when none.
    #[serde(default, skip_serializing_if = "crate::accounts::Status::is_empty")]
    pub account_status: crate::accounts::Status<crate::backend::Backend>,
    // Read old checkpoints for migration; new configuration belongs in the home directory.
    #[serde(skip_serializing)]
    pub model: Option<String>,
    /// Every run, in creation order. Saves leave out runs kept in archive
    /// blocks (`storage::archive`); they are loaded back at startup.
    #[serde(serialize_with = "hot_runs")]
    pub runs: Runs,
    pub notice: String,
    // Why dispatch is paused, shown on queued chat messages; rebuilt by the next reconcile.
    #[serde(skip)]
    pub paused: Option<String>,
}
use crate::application::effects::Effects;
pub(crate) use crate::application::runs::Runs;
use crate::application::runs::{self, Admission, Refusal, Submission, Tx, admit};
pub use crate::application::runs::{Run, RunStatus};

// Pending transitions change status/result and possibly a design answer. Keep
// rollback bounded, without cloning attempts or completed conversation history.
pub(crate) struct RunStatusCheckpoint {
    runs: Vec<(String, RunStatus, String, Option<crate::design::Question>)>,
    notice: String,
}
impl RunStatusCheckpoint {
    /// The active runs that `matches`, or whose persona parent, at any
    /// depth, does. Only active runs are kept, so only their ancestors are
    /// visited: one pass over the runs' statuses, not a closure over every
    /// run of the node.
    pub(crate) fn capture(runtime: &Runtime, matches: impl Fn(&Run) -> bool) -> Self {
        let selected = |run: &Run| {
            std::iter::successors(Some(run), |run| {
                run.persona
                    .as_ref()?
                    .parent
                    .as_deref()
                    .and_then(|parent| runtime.runs.get(parent))
            })
            // A chain longer than the ledger repeats a run: a cycle.
            .take(runtime.runs.len())
            .any(&matches)
        };
        Self {
            runs: runtime
                .runs
                .iter()
                .filter(|run| run.status.is_active() && selected(run))
                .map(|run| {
                    (
                        run.id().as_str().to_owned(),
                        run.status.clone(),
                        run.result.to_string(),
                        run.question.clone(),
                    )
                })
                .collect(),
            notice: runtime.notice.clone(),
        }
    }
    pub(crate) fn restore(self, runtime: &mut Runtime) {
        for (id, status, result, question) in self.runs {
            if let Some(run) = runtime.runs.get_mut(&id) {
                run.status = status;
                *run.result = result;
                run.question = question;
            }
        }
        runtime.notice = self.notice;
    }
    pub(crate) fn save(
        self,
        state: &mut crate::State,
        save: impl FnOnce(&mut crate::State) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
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
        .or_else(|| fs::read_to_string(crate::config::directory().ok()?.join("worker-image-id")).ok())
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
    /// What new work runs on: the node default model on its backend's default account.
    pub(crate) fn default_target(&self) -> crate::providers::Target {
        crate::providers::Target {
            backend: self.backend,
            account: self.accounts.default_name(self.backend),
            model: Some(self.configured_model()),
        }
    }
    /// Authorizes, admits and queues `submission`: the one way new work
    /// enters the queue. The caller saves, then releases the wake.
    pub(crate) fn queue(&mut self, submission: Submission, tx: Tx) -> Result<runs::Queued, Refusal> {
        let authorized = submission.authorize(identity::authorize)?;
        let admission = Admission {
            runs: &self.runs,
            target: self.default_target(),
        };
        let run = admit(admission, authorized, tx)?;
        self.notice = "Run queued.".into();
        Ok(self.runs.queue(run))
    }
    /// An operator's task; a refusal becomes the notice.
    pub(crate) fn task(&mut self, objective: String, kind: RunKind, source: &str, tx: Tx) -> Effects {
        let action = if kind == RunKind::SelfImprovement {
            Action::Improve
        } else {
            Action::Task
        };
        let submission = Submission {
            objective,
            kind,
            source: source.into(),
            actor: Actor::LocalOperator,
            action,
        };
        self.queue(submission, tx).map_or_else(
            |refusal| {
                self.notice = refusal.message().into();
                Effects::none()
            },
            Effects::from,
        )
    }
    /// `task` for tests, which run no dispatcher to wake.
    #[cfg(test)]
    pub fn enqueue(&mut self, objective: String, kind: RunKind, source: &str) {
        let _ = self.task(objective, kind, source, Tx::at(crate::ids::stamp()));
    }
    /// `queue` for tests, which run no dispatcher to wake.
    #[cfg(test)]
    pub fn enqueue_for(
        &mut self,
        objective: String,
        kind: RunKind,
        source: &str,
        actor: Actor,
        action: Action,
    ) -> Result<(), &'static str> {
        let submission = Submission {
            objective,
            kind,
            source: source.into(),
            actor,
            action,
        };
        let _ = self.queue(submission, Tx::at(crate::ids::stamp()))?;
        Ok(())
    }
    /// `POST /task`; a refusal becomes the notice.
    pub(crate) fn post_task(&mut self, f: &HashMap<String, String>, tx: Tx) -> Effects {
        self.notice.clear();
        self.task(field(f, "objective"), RunKind::Task, "website", tx)
    }
    fn retry(&mut self, id: &str) {
        if let Err(message) = crate::scheduler::admit(self) {
            self.notice = message.into();
            return;
        }
        let order = self
            .runs
            .iter()
            .map(|r| r.queue_order)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        if let Some(run) = self
            .runs
            .get_mut(id)
            .filter(|r| r.status.is_resumable() && !crate::design::asked(r))
        {
            if let Err(message) = identity::authorize(&run.actor, run.action) {
                self.notice = message.into();
                return;
            }
            run.status = RunStatus::Queued;
            run.queue_order = order;
            run.result.clear();
        }
    }
    /// `POST /cancel-run` and `POST /retry-run`. A retried run is picked up
    /// by the dispatcher's next scan, as before.
    pub fn action(&mut self, path: &str, f: &HashMap<String, String>) -> &'static str {
        self.notice.clear();
        if path == "/cancel-run" {
            let id = field(f, "id");
            self.notice = crate::lifecycle::request_cancel(self, &id, &Actor::LocalOperator)
                .map(|()| "Cancellation requested.".to_owned())
                .unwrap_or_else(str::to_owned);
        }
        if path == "/retry-run" {
            self.retry(&field(f, "id"));
        }
        crate::history_view::AFTER_ACTION
    }
    pub fn render(&self, view: &str, _lens: &str) -> Option<String> {
        match view {
            "discord-access" => Some(discord_access()),
            "agents" => Some(Queued::of(self).render()),
            "integrations" => Some(integrations()),
            _ => None,
        }
    }
}

/// Attempts and provider-reported tokens for one run, when any were measured.
pub(crate) fn usage(run: &Run) -> String {
    let (input, cached, output, measured) =
        run.attempts
            .iter()
            .filter_map(|attempt| attempt.tokens)
            .fold((0, 0, 0, 0), |(i, c, o, n), t| {
                (
                    i + t.input_tokens,
                    c + t.cached_input_tokens,
                    o + t.output_tokens,
                    n + 1,
                )
            });
    let running = run.attempts.last().is_some_and(crate::usage::Attempt::running);
    let partial = run
        .attempts
        .iter()
        .any(|attempt| attempt.accounting == crate::usage::Accounting::Partial);
    let bound = if running {
        " so far"
    } else if partial {
        " at least"
    } else {
        ""
    };
    match (run.attempts.len(), measured) {
        (0, _) => String::new(),
        (attempts, 0) => format!(" · {attempts} attempt(s) · tokens unknown"),
        (attempts, _) => format!(
            " · {attempts} attempt(s) · {input} input ({cached} cached) · {output} output tokens{bound} ({measured}/{attempts} measured)"
        ),
    }
}
/// The Bots page's queued turns, copied out of state so the panel renders
/// after the state lock is released. The process tree shows active turns;
/// only queued work that has not started is listed here.
pub(crate) struct Queued(Vec<Record>);
impl Queued {
    pub(crate) fn of(runtime: &Runtime) -> Self {
        let mut queued = runtime
            .runs
            .iter()
            .filter(|r| r.status == RunStatus::Queued)
            .collect::<Vec<_>>();
        queued.sort_by_key(|r| r.queue_order);
        Self(queued.into_iter().map(Record::of).collect())
    }
    pub(crate) fn render(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        std::iter::once("<h3 class='form-title'>Queued</h3>".to_owned())
            .chain(self.0.iter().map(Record::render))
            .collect()
    }
}
/// A run as its record card shows it.
struct Record {
    status: String,
    kind: String,
    source: String,
    objective: String,
    id: String,
    backend: &'static str,
    model: Option<String>,
    /// Attempts and tokens (`usage`).
    usage: String,
    persona: Option<Persona>,
    result: String,
    chat: Option<String>,
    proceeding: bool,
    resumable: bool,
}
/// A delegated run's role on its record card.
struct Persona {
    judge: bool,
    depth: u8,
    /// The captured persona bundle: id and revision.
    bundle: Option<(String, u64)>,
}
impl Record {
    fn of(run: &Run) -> Self {
        Self {
            status: run.status.as_str().to_owned(),
            kind: run.kind.as_str().to_owned(),
            source: run.source.clone(),
            objective: run.objective.as_str().to_owned(),
            id: run.id().as_str().to_owned(),
            backend: run.target.backend.label(),
            model: run.target.shown_model(),
            usage: usage(run),
            persona: run.persona.as_ref().map(|persona| Persona {
                judge: persona.role == crate::delegation::Role::Judge,
                depth: persona.depth,
                bundle: persona
                    .bundle
                    .as_ref()
                    .map(|bundle| (bundle.id.clone(), bundle.revision)),
            }),
            result: run.result.as_str().to_owned(),
            chat: run.chat_id.clone().filter(|id| crate::chat::valid_id(id)),
            proceeding: run.status.is_proceeding(),
            resumable: run.status.is_resumable(),
        }
    }
    fn render(&self) -> String {
        let mut html = format!(
            "<article class='record'><span class='tag'>{}</span><span class='eyebrow'>{} / {}</span><h3>{}</h3><p class='muted tiny'>run {} · {}{}{}</p>",
            escape(&self.status),
            escape(&self.kind),
            escape(&self.source),
            escape(&self.objective),
            escape(&self.id),
            escape(self.backend),
            self.model
                .as_ref()
                .map(|m| format!(" · {}", escape(m)))
                .unwrap_or_default(),
            self.usage
        );
        if let Some(persona) = &self.persona {
            html.push_str(&format!(
                "<span class='tag'>{} · depth {}</span>",
                if persona.judge { "judge · read only" } else { "worker" },
                persona.depth
            ));
            if let Some((id, revision)) = &persona.bundle {
                html.push_str(&format!(
                    "<a class='tag' href='/personas?id={}'>persona {}@{}</a>",
                    escape(id),
                    escape(id),
                    revision
                ));
            }
        }
        if !self.result.is_empty() {
            html.push_str(&format!("<pre class='run-result'>{}</pre>", escape(&self.result)));
        }
        if let Some(chat) = &self.chat {
            html.push_str(&format!("<a href='/dm/{}'>Open DM ↗</a>", escape(chat)));
        }
        if self.proceeding {
            html.push_str(&format!("<form class='decision-form' method='post' action='/cancel-run'><input type='hidden' name='id' value='{}'><button>Cancel</button></form>",escape(&self.id)));
        }
        if self.resumable {
            html.push_str(&format!("<form class='decision-form' method='post' action='/retry-run'><input type='hidden' name='id' value='{}'><button class='primary'>Retry</button></form>",escape(&self.id)));
        }
        html.push_str("</article>");
        html
    }
}
#[cfg(test)]
fn run_record(run: &Run) -> String {
    Record::of(run).render()
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
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
            if name_str.starts_with('.') || ["target", "dist", "node_modules", "pkg"].contains(&name_str.as_ref()) {
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
/// The Discord access page. It reads the access policy file and no state.
pub(crate) fn discord_access() -> String {
    let mut h="<span class='eyebrow'>DISCORD</span><h2>Access</h2><p class='muted'>Access uses Discord IDs. Display names cannot grant permissions.</p>".to_owned();
    match identity::Policy::load() {
        Ok(policy) => {
            h.push_str("<p>DMs only. Server messages are ignored.</p>");
            for p in policy.principals {
                h.push_str(&format!("<article class='record'><span class='tag'>{:?}</span><h3>@{}</h3><p>Discord ID: {}</p><p class='muted'>{}</p></article>",p.role,escape(&p.label),p.user_id.map(|id|id.to_string()).unwrap_or_else(||"not bound — no access".into()),if p.role==identity::Role::Architect {"Can run tasks and request source changes for local review."}else{"Can run tasks. Cannot change Control, access settings, or deploy."}));
            }
        }
        Err(message) => h.push_str(&format!("<div class='callout'>{}</div>", escape(message))),
    }
    h.push_str("<div class='callout'>Bind IDs locally with <code>cargo run -- --bind-discord &lt;label&gt; &lt;ID&gt;</code>. Access settings cannot be changed through Discord.</div>");
    h
}
/// The Integrations page. It reads Discord's status and login files, and no state.
pub(crate) fn integrations() -> String {
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
    let profile = status["bot_id"]
        .as_str()
        .and_then(|s| s.parse::<identity::DiscordId>().ok())
        .map(|id| {
            format!("<p><a href='https://discord.com/users/{id}' rel='noreferrer'>Open Control in Discord ↗</a></p>")
        })
        .unwrap_or_default();
    let backends: String = crate::backend::Backend::hosted()
        .map(|backend| {
            // The product name: the label without its vendor.
            let name = backend.label().rsplit(" / ").next().unwrap_or_default();
            let login = crate::logins::ready(backend).is_ok();
            format!(
                "<div class='record'><h3>{name}</h3><p>{} · {}</p></div>",
                if backend.image().is_some() {
                    "Image pinned"
                } else {
                    "Image missing"
                },
                if login { "Login present" } else { "Login missing" }
            )
        })
        .collect();
    format!(
        "<h2>Connections</h2><div class='record'><h3>Local service</h3><p>Running on this laptop</p></div><div class='section-heading'><h3>Agent backends</h3><a href='/?view=providers'>Models &amp; providers ↗</a></div>{backends}<div class='record'><h3>Control MCP</h3><p>Local stdio · <code>control mcp</code></p></div><div class='record'><span class='tag'>{label}</span><h3>Discord · direct messages</h3>{profile}<p class='muted'>Only approved users can chat. DMs are not shared with rooms.</p>{}<a href='/?view=discord-access'>Manage access ↗</a></div>",
        crate::channel_settings::panel()
    )
}

#[cfg(test)]
mod tests;
