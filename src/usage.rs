use crate::runtime::RunStatus;
use crate::{escape, identity::Actor};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

pub use control_ai::{Accounting, Activity, StatusPart, Tokens, attempt_status};

/// An attempt's lifecycle status, saved as its lowercase name. Unlike a run,
/// an attempt is never queued, cancelling or continued.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AttemptStatus {
    /// No status recorded: saved as the empty string, as `String::default()` was.
    #[default]
    #[serde(rename = "")]
    Unset,
    Running,
    Completed,
    Waiting,
    Blocked,
    Failed,
    Interrupted,
    Cancelled,
    /// A saved status no current code writes, kept verbatim so old state and
    /// history still load and save unchanged.
    #[serde(untagged)]
    Unrecognised(String),
}
impl AttemptStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unset => "",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Unrecognised(status) => status,
        }
    }
}
impl std::fmt::Display for AttemptStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
/// A failed or paused turn's outcome becomes its attempt's status.
impl From<control_agent::Status> for AttemptStatus {
    fn from(status: control_agent::Status) -> Self {
        use control_agent::Status;
        match status {
            Status::Blocked => Self::Blocked,
            Status::Failed => Self::Failed,
            Status::Cancelled => Self::Cancelled,
            Status::Waiting => Self::Waiting,
            Status::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Attempt {
    /// None means the default policy: bounded ordinary attempts, unlimited
    /// approved persistent agents. Explicit operator limits always take priority.
    pub explicit_timeout_seconds: Option<u64>,
    /// The bot turn limit captured at admission, applied while the chat is an
    /// approved persistent bot and no explicit limit is set. None: unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_timeout_seconds: Option<u64>,
    /// The local-model turn limit captured at admission, applied to every
    /// attempt on the local backend (chats and bots) when no explicit limit is
    /// set. None: unlimited (or captured before the setting existed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<crate::attempt_control::Termination>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<crate::recovery::Candidate>,
    #[serde(skip_serializing_if = "crate::history::Spilled::is_unset")]
    pub turn_mailbox: crate::history::Spilled<Option<Box<control_context::turn_mailbox::Mailbox>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_trace: Option<Box<crate::turn_context::TraceRef>>,
    pub github_source: Option<crate::github::Source>,
    pub command_jobs: Vec<crate::command_jobs::Job>,
    pub review_only: bool,
    pub project: Option<String>,
    pub project_parent: Option<String>,
    pub project_base: Option<String>,
    pub project_checkpoint: Option<String>,
    pub backend: crate::backend::Backend,
    /// The account its run captured at admission, kept with its usage so it
    /// can be accounted per account. Absent: the implicit `default` account.
    #[serde(skip_serializing_if = "crate::accounts::AccountName::is_implicit")]
    pub account: crate::accounts::AccountName,
    pub image: Option<String>,
    pub node_id: Option<String>,
    pub parent_artifact: Option<String>,
    pub artifact: Option<String>,
    pub workspace_id: Option<String>,
    pub activity: Vec<Activity>,
    /// Line of the chat's history journal holding this finished attempt's
    /// activity, once it has left state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<u64>,
    pub omitted_history: usize,
    pub started_unix: u64,
    pub elapsed_ms: u64,
    pub provider: String,
    pub model: String,
    pub status: AttemptStatus,
    pub tokens: Option<Tokens>,
    /// How far `tokens` can be trusted. Absent on records from before it was kept.
    #[serde(skip_serializing_if = "Accounting::is_unrecorded")]
    pub accounting: Accounting,
    /// The model the provider says served the attempt, when it says
    /// (`model` is the requested one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_model: Option<String>,
    /// Advisory notes already appended to this attempt's tool results.
    #[serde(skip_serializing_if = "crate::worker_tools::nudges::Nudges::is_clear")]
    pub nudges: crate::worker_tools::nudges::Nudges,
    /// A local turn's latest reasoning when it was cut off, bounded, for the
    /// host checkpoint the worker builds next. Never saved with the attempt.
    #[serde(skip)]
    pub interrupted_reasoning: Option<String>,
    /// The provider failure the backend classified, for the host to act on
    /// (a usage limit cools the account down). Never saved with the attempt.
    #[serde(skip)]
    pub cause: Option<control_agent::Cause>,
}
impl Attempt {
    /// Records what the provider reported, whatever the attempt's outcome.
    pub fn record_usage(&mut self, usage: control_ai::Usage) {
        self.accounting = match (usage.tokens, usage.complete) {
            (None, _) => Accounting::Unknown,
            (Some(_), true) => Accounting::Reported,
            (Some(_), false) => Accounting::Partial,
        };
        self.tokens = usage.tokens;
        if usage.model.is_some() {
            self.reported_model = usage.model;
        }
    }
    /// Still in progress: its tokens, if any, are the totals reported so far.
    pub fn running(&self) -> bool {
        self.status == AttemptStatus::Running
    }
    /// The attempt's compact status line; see [`attempt_status`].
    pub fn status(&self, index: usize) -> Vec<StatusPart> {
        attempt_status(
            index,
            self.tokens,
            self.accounting,
            self.running(),
            self.omitted_history,
        )
    }
    /// Callers expose this only for successfully completed runs. The accepted
    /// handoff is the single retained source, not a duplicated summary field.
    pub(crate) fn task_progress(&self) -> Option<&str> {
        self.turn_mailbox.as_ref()?.accepted()?.progress.as_deref()
    }
    pub fn new() -> Self {
        let model = default_model();
        Self {
            started_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            provider: "OpenAI / Codex".into(),
            model,
            status: AttemptStatus::Running,
            ..Self::default()
        }
    }
}
pub fn valid_model(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('-')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
}
pub fn default_model() -> String {
    std::env::var("CONTROL_CODEX_MODEL")
        .ok()
        .filter(|s| valid_model(s))
        .unwrap_or_else(|| "gpt-6.1-sol".into())
}

/// Keep numeric accounting and a bounded selection of command/activity fields.
#[cfg(test)]
pub fn capture(reader: impl std::io::Read) -> Option<Tokens> {
    control_ai::json_lines_usage(reader, control_ai::codex::Codex::default(), |_| {})
        .1
        .tokens
}

#[derive(Default)]
struct Summary {
    tokens: Tokens,
    attempts: u64,
    measured: u64,
    completed: u64,
    failed: u64,
    interrupted: u64,
    partial: u64,
    no_call: u64,
    /// Attempts still running: their tokens are the totals reported so far.
    running: u64,
    /// Running attempts with nothing reported yet: pending, not unknown.
    pending: u64,
}
impl Summary {
    fn add(&mut self, attempt: &Attempt) {
        self.attempts += 1;
        if attempt.running() {
            self.running += 1;
            self.pending += u64::from(attempt.tokens.is_none());
        }
        match attempt.accounting {
            // A running total is a lower bound until the attempt ends: counted as running.
            Accounting::Partial if !attempt.running() => self.partial += 1,
            // No model call: counted as measured with nothing to add.
            Accounting::NotLaunched if attempt.tokens.is_none() => self.measured += 1,
            _ => {}
        }
        if attempt.accounting == Accounting::NotLaunched {
            self.no_call += 1;
        }
        match attempt.status {
            AttemptStatus::Completed => self.completed += 1,
            AttemptStatus::Failed => self.failed += 1,
            AttemptStatus::Interrupted => self.interrupted += 1,
            AttemptStatus::Unset
            | AttemptStatus::Running
            | AttemptStatus::Waiting
            | AttemptStatus::Blocked
            | AttemptStatus::Cancelled
            | AttemptStatus::Unrecognised(_) => {}
        }
        if let Some(tokens) = attempt.tokens {
            self.measured += 1;
            self.tokens.add(tokens);
        }
    }
}

fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::LocalOperator => "Local operator".into(),
        Actor::DiscordDm { user_id, .. } | Actor::Discord { user_id, .. } => {
            format!("Discord {user_id}")
        }
        Actor::Unknown => "Unknown".into(),
    }
}

fn table(title: &str, groups: &BTreeMap<String, Summary>) -> String {
    let mut html = format!(
        "<details class='record'><summary>{title}</summary><div class='usage-scroll'><table class='usage-table'><thead><tr><th>Group</th><th>Input</th><th>Cached</th><th>Output</th><th>Measured</th></tr></thead><tbody>"
    );
    for (name, sum) in groups {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td></tr>",
            escape(name),
            sum.tokens.input_tokens,
            sum.tokens.cached_input_tokens,
            sum.tokens.output_tokens,
            sum.measured,
            sum.attempts
        ));
    }
    html.push_str("</tbody></table></div></details>");
    html
}

/// Reuse model summaries rather than scanning runs or cloning activity histories.
fn outcomes(models: &BTreeMap<String, Summary>) -> String {
    use std::fmt::Write;

    let mut rows: Vec<_> = models.iter().collect();
    rows.sort_unstable_by(|(a, x), (b, y)| y.attempts.cmp(&x.attempts).then_with(|| a.cmp(b)));
    let mut html = String::from("<figure class='usage-outcomes'><figcaption>Attempt outcomes</figcaption>");
    if rows.is_empty() {
        html.push_str("<p class='muted'>No recorded attempts yet.</p></figure>");
        return html;
    }
    for (name, sum) in rows.iter().take(6) {
        let other = sum.attempts - sum.completed - sum.failed - sum.interrupted;
        let label = format!(
            "{}: {} completed, {} failed, {} interrupted, {} other attempts",
            name, sum.completed, sum.failed, sum.interrupted, other
        );
        let _ = write!(
            html,
            "<div class='usage-outcome-row'><div><span>{}</span><span>{} attempts</span></div><svg viewBox='0 0 100 3' preserveAspectRatio='none' role='img' aria-label='{}'><title>{}</title>",
            escape(name),
            sum.attempts,
            escape(&label),
            escape(&label)
        );
        let mut offset = 0.0;
        for (class, count) in [
            ("completed", sum.completed),
            ("failed", sum.failed),
            ("interrupted", sum.interrupted),
            ("other", other),
        ] {
            let width = count as f64 / sum.attempts as f64 * 100.0;
            if count > 0 {
                let _ = write!(
                    html,
                    "<rect x='{offset:.4}' y='0' width='{width:.4}' height='3' class='usage-outcome-{class}'/>"
                );
            }
            offset += width;
        }
        html.push_str("</svg></div>");
    }
    html.push_str("<div class='usage-outcome-legend'><span class='usage-outcome-completed'>Completed</span><span class='usage-outcome-failed'>Failed</span><span class='usage-outcome-interrupted'>Interrupted</span><span class='usage-outcome-other'>Other / active</span></div><p class='muted'>Attempt status, not verified task success.</p>");
    if rows.len() > 6 {
        html.push_str("<p class='muted'>Six most-used models; all models below.</p>");
    }
    html.push_str("</figure>");
    html
}

/// An attempt in progress, as the Usage page lists it.
struct Running {
    run: String,
    provider: String,
    model: String,
    /// The model the provider reported serving, when it differs.
    served: Option<String>,
    tokens: Option<Tokens>,
    /// Why no tokens are shown yet: no model call, or nothing reported.
    unrecorded: bool,
}
impl Running {
    fn of(run: &crate::runtime::Run) -> Option<Self> {
        let attempt = run.attempts.last().filter(|a| a.running())?;
        Some(Self {
            run: run.id().as_str().to_owned(),
            provider: attempt.provider.clone(),
            model: attempt.model.clone(),
            served: attempt.reported_model.clone().filter(|m| *m != attempt.model),
            tokens: attempt.tokens,
            unrecorded: attempt.accounting.is_unrecorded(),
        })
    }
}
/// Attempts in progress with the usage their providers reported so far
/// (per model call for local endpoints and Claude; Codex only at turn end).
fn running(attempts: &[Running]) -> String {
    use std::fmt::Write;
    let mut rows = String::new();
    for attempt in attempts {
        let cell = |n: Option<u64>| n.map_or_else(|| "—".to_owned(), |n| n.to_string());
        let served = attempt
            .served
            .as_deref()
            .map(|m| format!(" (served: {})", escape(m)))
            .unwrap_or_default();
        let _ = write!(
            rows,
            "<tr><td>{}</td><td>{} · {}{served}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(&attempt.run),
            escape(&attempt.provider),
            escape(&attempt.model),
            cell(attempt.tokens.map(|t| t.input_tokens)),
            cell(attempt.tokens.map(|t| t.cached_input_tokens)),
            cell(attempt.tokens.map(|t| t.output_tokens)),
            if attempt.tokens.is_some() {
                "so far"
            } else if attempt.unrecorded {
                "no model call yet"
            } else {
                "not reported yet"
            }
        );
    }
    if rows.is_empty() {
        return rows;
    }
    format!(
        "<details class='record' open><summary>Running now</summary><div class='usage-scroll'><table class='usage-table'><thead><tr><th>Run</th><th>Provider / model</th><th>Input</th><th>Cached</th><th>Output</th><th>Usage</th></tr></thead><tbody>{rows}</tbody></table></div><p class='muted'>Live totals reported so far: after every model call on local endpoints and Claude; Codex reports at the end of its turn.</p></details>"
    )
}

/// The Usage page: node totals (DMs, runs, genes) above model usage.
pub fn render(state: &crate::State) -> String {
    Page::of(state).render(today())
}
/// The current UTC day, which ends the Usage page's activity grid.
pub(crate) fn today() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86400)
}
/// What the Usage page shows: attempt totals summed while state is read, so
/// the page renders after the state lock is released.
pub(crate) struct Page {
    chats: usize,
    runs: usize,
    genes: usize,
    overall: Summary,
    people: BTreeMap<String, Summary>,
    models: BTreeMap<String, Summary>,
    days: BTreeMap<String, Summary>,
    /// Tokens per UTC day.
    daily: BTreeMap<u64, u64>,
    /// Finished runs from before attempts were tracked.
    legacy: u64,
    running: Vec<Running>,
}
impl Page {
    pub(crate) fn of(state: &crate::State) -> Self {
        let runtime = &state.runtime;
        let mut page = Self {
            chats: state.chats.len(),
            runs: runtime.runs.len(),
            genes: state.lens_library.current().len(),
            overall: Summary::default(),
            people: BTreeMap::new(),
            models: BTreeMap::new(),
            days: BTreeMap::new(),
            daily: BTreeMap::new(),
            legacy: 0,
            running: runtime.runs.iter().rev().filter_map(Running::of).collect(),
        };
        for run in runtime.runs.iter() {
            if run.attempts.is_empty() && !matches!(run.status, RunStatus::Queued | RunStatus::Running) {
                page.legacy += 1;
            }
            for attempt in &run.attempts {
                page.overall.add(attempt);
                page.people.entry(actor_label(&run.actor)).or_default().add(attempt);
                page.models
                    .entry(format!("{} · {}", attempt.provider, attempt.model))
                    .or_default()
                    .add(attempt);
                page.days
                    .entry(day_label(attempt.started_unix))
                    .or_default()
                    .add(attempt);
                if let Some(tokens) = attempt.tokens {
                    *page.daily.entry(attempt.started_unix / 86400).or_default() +=
                        tokens.input_tokens + tokens.output_tokens;
                }
            }
        }
        page
    }
    /// The page, its activity grid ending on UTC day `today`.
    pub(crate) fn render(&self, today: u64) -> String {
        let (overall, legacy) = (&self.overall, self.legacy);
        let mut html = format!(
            "<h2>Usage</h2><div class='metrics usage-summary' aria-label='Node summary'><div><b>{}</b><span>DMS</span></div><div><b>{}</b><span>RUNS</span></div><div><b>{}</b><span>GENES</span></div></div>",
            self.chats, self.runs, self.genes
        );
        html.push_str(&heatmap(&self.daily, today));
        html.push_str(&running(&self.running));
        html.push_str(&outcomes(&self.models));
        html.push_str(&format!("<details class='record'><summary>Token accounting</summary><div class='metrics'><div><b>{}</b><span>INPUT</span></div><div><b>{}</b><span>OUTPUT</span></div><div><b>{}/{}</b><span>MEASURED ATTEMPTS</span></div></div><p class='muted'>Cached input: {} · running (totals so far): {} · unknown attempts: {} · partial (stopped mid-turn, lower bound): {} · no model call: {} · older runs without tracking: {legacy}</p><p class='muted'>Cached tokens are part of input. Counts are provider-reported; subscription billing is not estimated.</p></details>", overall.tokens.input_tokens, overall.tokens.output_tokens, overall.measured, overall.attempts, overall.tokens.cached_input_tokens, overall.running, overall.attempts - overall.measured - overall.pending, overall.partial, overall.no_call));
        html.push_str(&table("By person", &self.people));
        html.push_str(&table("By provider / requested model", &self.models));
        html.push_str(&table("By day (UTC)", &self.days));
        html.push_str(
            "<p class='muted'>Per-turn attempts and tokens are in <a href='/bots/history?expand=1'>History ↗</a>.</p>",
        );
        html
    }
}

const WEEKS: u64 = 26;
/// Tokens per UTC day over recent weeks as a contribution grid: one column per
/// week starting Sunday, shaded in five purple levels relative to the busiest day.
fn heatmap(daily: &BTreeMap<u64, u64>, today: u64) -> String {
    // Unix day 0 was a Thursday; columns start on Sunday.
    let start = today.saturating_sub((today + 4) % 7 + (WEEKS - 1) * 7);
    let max = daily.range(start..=today).map(|(_, tokens)| *tokens).max().unwrap_or(0);
    let cells = (start..=today)
        .map(|day| {
            let tokens = daily.get(&day).copied().unwrap_or(0);
            let level = if tokens == 0 {
                0
            } else {
                1 + (3 * tokens / max.max(1)).min(3)
            };
            format!(
                "<span class='l{level}' title='{}: {tokens} tokens'></span>",
                day_label(day * 86400)
            )
        })
        .collect::<String>();
    let legend = (0..5)
        .map(|level| format!("<span class='l{level}'></span>"))
        .collect::<String>();
    format!(
        "<section class='usage-graph'><div class='usage-heatmap' role='img' aria-label='Tokens per day over the last {WEEKS} weeks'>{cells}</div><p class='usage-legend muted'>Less {legend} More · tokens per day (UTC)</p></section>"
    )
}

// Gregorian civil date from Unix days; display grouping is explicitly UTC.
pub(crate) fn day_label(timestamp: u64) -> String {
    let z = (timestamp / 86400).min(i64::MAX as u64 / 2) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests;
