use crate::{escape, identity::Actor, runtime::Runtime};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};

pub use control_ai::{Activity, Tokens};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Attempt {
    /// None means the default policy: bounded ordinary attempts, unlimited
    /// approved persistent agents. Explicit operator limits always take priority.
    pub explicit_timeout_seconds: Option<u64>,
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
    pub status: String,
    pub tokens: Option<Tokens>,
}
impl Attempt {
    /// Callers expose this only for successfully completed runs. The accepted
    /// handoff is the single retained source, not a duplicated summary field.
    pub(crate) fn task_progress(&self)->Option<&str> {
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
            status: "running".into(),
            ..Self::default()
        }
    }
}
pub fn valid_model(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b))
}
pub fn default_model() -> String {
    std::env::var("CONTROL_CODEX_MODEL")
        .ok()
        .filter(|s| valid_model(s))
        .unwrap_or_else(|| "gpt-6.1-sol".into())
}

/// Keep numeric accounting and a bounded selection of command/activity fields.
#[cfg(test)]
pub fn capture(reader: impl Read) -> Option<Tokens> {
    capture_with(reader, |_| {})
}
pub fn capture_with(reader: impl Read, mut update: impl FnMut(Activity)) -> Option<Tokens> {
    control_ai::json_lines(reader, control_ai::codex::Codex::default(), |event| {
        if let control_ai::Event::Activity(activity) = event { update(activity); }
    }).ok().and_then(|output| output.tokens)
}

#[derive(Default)]
struct Summary {
    tokens: Tokens,
    attempts: u64,
    measured: u64,
    completed: u64,
    failed: u64,
    interrupted: u64,
}
impl Summary {
    fn add(&mut self, attempt: &Attempt) {
        self.attempts += 1;
        match attempt.status.as_str() {
            "completed" => self.completed += 1,
            "failed" => self.failed += 1,
            "interrupted" => self.interrupted += 1,
            _ => {}
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
    let mut html = format!("<details class='record'><summary>{title}</summary><div class='usage-scroll'><table class='usage-table'><thead><tr><th>Group</th><th>Input</th><th>Cached</th><th>Output</th><th>Measured</th></tr></thead><tbody>");
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
        let label = format!("{}: {} completed, {} failed, {} interrupted, {} other attempts", name, sum.completed, sum.failed, sum.interrupted, other);
        let _ = write!(html, "<div class='usage-outcome-row'><div><span>{}</span><span>{} attempts</span></div><svg viewBox='0 0 100 3' preserveAspectRatio='none' role='img' aria-label='{}'><title>{}</title>", escape(name), sum.attempts, escape(&label), escape(&label));
        let mut offset = 0.0;
        for (class, count) in [("completed", sum.completed), ("failed", sum.failed), ("interrupted", sum.interrupted), ("other", other)] {
            let width = count as f64 / sum.attempts as f64 * 100.0;
            if count > 0 {
                let _ = write!(html, "<rect x='{offset:.4}' y='0' width='{width:.4}' height='3' class='usage-outcome-{class}'/>");
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

pub fn render(runtime: &Runtime) -> String {
    let mut overall = Summary::default();
    let mut people = BTreeMap::<String, Summary>::new();
    let mut models = BTreeMap::<String, Summary>::new();
    let mut days = BTreeMap::<String, Summary>::new();
    let mut daily = BTreeMap::<u64, u64>::new();
    let mut legacy = 0;
    for run in &runtime.runs {
        if run.attempts.is_empty() && !["queued", "running"].contains(&run.status.as_str()) {
            legacy += 1;
        }
        for attempt in &run.attempts {
            overall.add(attempt);
            people
                .entry(actor_label(&run.actor))
                .or_default()
                .add(attempt);
            models
                .entry(format!("{} · {}", attempt.provider, attempt.model))
                .or_default()
                .add(attempt);
            days.entry(day_label(attempt.started_unix))
                .or_default()
                .add(attempt);
            if let Some(tokens) = attempt.tokens {
                *daily.entry(attempt.started_unix / 86400).or_default() += tokens.input_tokens + tokens.output_tokens;
            }
        }
    }
    let mut html = String::from("<h2>Usage</h2>");
    let today = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86400);
    html.push_str(&heatmap(&daily, today));
    html.push_str(&outcomes(&models));
    html.push_str(&format!("<details class='record'><summary>Token accounting</summary><div class='metrics'><div><b>{}</b><span>INPUT</span></div><div><b>{}</b><span>OUTPUT</span></div><div><b>{}/{}</b><span>MEASURED ATTEMPTS</span></div></div><p class='muted'>Cached input: {} · unknown attempts: {} · older runs without tracking: {legacy}</p><p class='muted'>Cached tokens are part of input. Counts are provider-reported; subscription billing is not estimated.</p></details>", overall.tokens.input_tokens, overall.tokens.output_tokens, overall.measured, overall.attempts, overall.tokens.cached_input_tokens, overall.attempts - overall.measured));
    html.push_str(&table("By person", &people));
    html.push_str(&table("By provider / requested model", &models));
    html.push_str(&table("By day (UTC)", &days));
    html.push_str("<p class='muted'>Per-run attempts and tokens are in <a href='/?view=runs'>Run history ↗</a>.</p>");
    html
}

const WEEKS: u64 = 26;
/// Tokens per UTC day over recent weeks as a contribution grid: one column per
/// week starting Sunday, shaded in five purple levels relative to the busiest day.
fn heatmap(daily: &BTreeMap<u64, u64>, today: u64) -> String {
    // Unix day 0 was a Thursday; columns start on Sunday.
    let start = today.saturating_sub((today + 4) % 7 + (WEEKS - 1) * 7);
    let max = daily.range(start..=today).map(|(_, tokens)| *tokens).max().unwrap_or(0);
    let cells = (start..=today).map(|day| {
        let tokens = daily.get(&day).copied().unwrap_or(0);
        let level = if tokens == 0 { 0 } else { 1 + (3 * tokens / max.max(1)).min(3) };
        format!("<span class='l{level}' title='{}: {tokens} tokens'></span>", day_label(day * 86400))
    }).collect::<String>();
    let legend = (0..5).map(|level| format!("<span class='l{level}'></span>")).collect::<String>();
    format!("<section class='usage-graph'><div class='usage-heatmap' role='img' aria-label='Tokens per day over the last {WEEKS} weeks'>{cells}</div><p class='usage-legend muted'>Less {legend} More · tokens per day (UTC)</p></section>")
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
