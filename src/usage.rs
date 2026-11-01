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
    pub omitted_history: usize,
    pub started_unix: u64,
    pub elapsed_ms: u64,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub tokens: Option<Tokens>,
}
impl Attempt {
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
}
impl Summary {
    fn add(&mut self, attempt: &Attempt) {
        self.attempts += 1;
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

pub fn render(runtime: &Runtime) -> String {
    let mut overall = Summary::default();
    let mut people = BTreeMap::<String, Summary>::new();
    let mut models = BTreeMap::<String, Summary>::new();
    let mut days = BTreeMap::<String, Summary>::new();
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
        }
    }
    let mut html = format!("<h2>Usage</h2><div class='metrics'><div><b>{}</b><span>INPUT</span></div><div><b>{}</b><span>OUTPUT</span></div><div><b>{}/{}</b><span>MEASURED ATTEMPTS</span></div></div><p class='muted'>Cached input: {} · unknown attempts: {} · older runs without tracking: {legacy}</p><p class='muted'>Cached tokens are part of input. Counts are provider-reported; subscription billing is not estimated.</p>", overall.tokens.input_tokens, overall.tokens.output_tokens, overall.measured, overall.attempts, overall.tokens.cached_input_tokens, overall.attempts - overall.measured);
    html.push_str(&table("By person", &people));
    html.push_str(&table("By provider / requested model", &models));
    html.push_str(&table("By day (UTC)", &days));
    html.push_str("<details class='record'><summary>Runs and attempts</summary>");
    for run in runtime.runs.iter().rev().filter(|r| !r.attempts.is_empty()) {
        html.push_str(&format!("<h3>{}</h3>", escape(&run.objective)));
        for (index, attempt) in run.attempts.iter().enumerate() {
            let counts = attempt
                .tokens
                .map(|t| {
                    format!(
                        "{} input · {} cached · {} output",
                        t.input_tokens, t.cached_input_tokens, t.output_tokens
                    )
                })
                .unwrap_or_else(|| "tokens unknown".into());
            html.push_str(&format!(
                "<p>#{} · {} · {} · {} ms · {counts}</p>",
                index + 1,
                escape(&attempt.status),
                escape(&attempt.model),
                attempt.elapsed_ms
            ));
        }
    }
    html.push_str("</details>");
    html
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
mod tests {
    use super::*;
    #[test]
    fn structured_activity_keeps_tools_bounded_and_excludes_reasoning() {
        let events = format!(
            "{}\n{}\n{}\n{}",
            serde_json::json!({"type":"item.started","item":{"id":"cmd1","type":"command_execution","command":"cargo check","status":"in_progress"}}),
            serde_json::json!({"type":"item.completed","item":{"id":"cmd1","type":"command_execution","command":"cargo check","status":"completed","aggregated_output":format!("done\u{1b}[31m{}", "x".repeat(5000))}}),
            serde_json::json!({"type":"item.completed","item":{"id":"thought","type":"reasoning","text":"private reasoning"}}),
            serde_json::json!({"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":4,"output_tokens":8}})
        );
        let mut activity = Vec::new();
        let tokens = capture_with(events.as_bytes(), |event| activity.push(event)).unwrap();
        assert_eq!(tokens.output_tokens, 8);
        assert_eq!(activity.len(), 2);
        assert_eq!(activity[0].text, "cargo check");
        assert_eq!(activity[1].output.chars().count(), 4000);
        assert!(!activity[1].output.contains('\u{1b}'));
        let message=control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"visible-message","type":"agent_message","text":"Checking the bot connection."}})).unwrap();
        assert_eq!(message.kind,"message");
        assert_eq!(message.text,"Checking the bot connection.");
        let failed = control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"mcp1","type":"mcp_tool_call","server":"control","tool":"control_port_request","arguments":{},"error":{"message":"Egress requires exactly one of target or peer_port_id"},"status":"failed"}})).unwrap();
        assert_eq!((failed.text.as_str(), failed.status.as_str()), ("control_port_request", "failed"));
        assert!(failed.output.contains("peer_port_id"));
    }
    #[test]
    fn aggregates_retries_preserves_legacy_runs_and_escapes_content() {
        let mut state = crate::State::default();
        let mut run: crate::runtime::Run = serde_json::from_value(serde_json::json!({
            "id":"sample", "objective":"Review <script>bad</script>", "kind":"task", "status":"completed", "source":"website", "result":""
        })).unwrap();
        assert!(run.attempts.is_empty());
        run.actor = Actor::LocalOperator;
        run.attempts = vec![
            Attempt {
                model: "gpt-6.1-sol".into(),
                provider: "OpenAI / Codex".into(),
                tokens: Some(Tokens {
                    input_tokens: 100,
                    cached_input_tokens: 80,
                    output_tokens: 20,
                }),
                status: "failed".into(),
                ..Attempt::default()
            },
            Attempt {
                model: "gpt-6.1-sol".into(),
                provider: "OpenAI / Codex".into(),
                tokens: Some(Tokens {
                    input_tokens: 50,
                    cached_input_tokens: 10,
                    output_tokens: 5,
                }),
                status: "completed".into(),
                ..Attempt::default()
            },
        ];
        state.runtime.runs.push(run);
        let html = render(&state.runtime);
        assert!(html.contains("<b>150</b><span>INPUT"));
        assert!(html.contains("<b>25</b><span>OUTPUT"));
        assert!(html.contains("2/2"));
        assert!(html.contains("Review &lt;script&gt;bad&lt;/script&gt;"));
        assert!(!html.contains("<script>bad"));
        if let Ok(directory) = std::env::var("CONTROL_PREVIEW_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            for (view, file) in [
                ("monitor", "usage.html"),
                ("hub", "overview.html"),
                ("agents", "agents.html"),
                ("lenses", "lenses.html"),
                ("rooms", "rooms.html"),
                ("session-list", "chat.html"),
            ] {
                let channel = if view == "session-list" {
                    crate::chat::landing(&state, "research", Some("knowledge"))
                } else {
                    "general".into()
                };
                let page = crate::render(&state, view, "research", &channel)
                    .replace(
                        "<link rel=\"stylesheet\" href=\"/style.css\">",
                        &format!("<style>{}</style>", include_str!("../assets/style.css")),
                    )
                    .replace(
                        "<link rel='stylesheet' href='/collective.css'>",
                        &format!(
                            "<style>{}</style>",
                            include_str!("../assets/collective.css")
                        ),
                    )
                    .replace(
                        "<link rel='stylesheet' href='/runtime.css'>",
                        &format!("<style>{}</style>", include_str!("../assets/runtime.css")),
                    );
                std::fs::write(std::path::Path::new(&directory).join(file), page).unwrap();
            }
        }
    }
    #[test]
    fn accounting_ignores_chat_malformed_events_and_counts_cache_once() {
        let events = b"{\"type\":\"item.completed\",\"usage\":{\"input_tokens\":999}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":20}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":50,\"cached_input_tokens\":10,\"output_tokens\":5}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":-5}}";
        let tokens = capture(&events[..]).unwrap();
        assert_eq!(
            tokens,
            Tokens {
                input_tokens: 150,
                cached_input_tokens: 90,
                output_tokens: 25
            }
        );
        assert!(capture(&b"broken\n{\"type\":\"turn.failed\"}"[..]).is_none());
    }
    #[test]
    fn days_and_legacy_attempts_keep_unknown_distinct_from_zero() {
        assert_eq!(day_label(0), "1970-01-01");
        assert_eq!(day_label(1793145600), "2026-10-28");
        let mut sum = Summary::default();
        sum.add(&Attempt::default());
        sum.add(&Attempt {
            tokens: Some(Tokens::default()),
            ..Attempt::default()
        });
        assert_eq!((sum.attempts, sum.measured), (2, 1));
    }
}
