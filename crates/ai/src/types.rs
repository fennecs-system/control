#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Tokens {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}
impl Tokens {
    pub fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self.cached_input_tokens.saturating_add(other.cached_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Activity {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub output: String,
    pub status: String,
}
use serde::{Deserialize, Serialize};

/// What a provider reported about one attempt's usage, whatever its outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// `None`: the provider reported nothing. Unknown, never zero.
    pub tokens: Option<Tokens>,
    /// The provider's own end-of-turn total. Otherwise `tokens` sums the
    /// per-call reports seen before the stream ended: a lower bound.
    pub complete: bool,
    /// The model the provider says served the attempt, when it says.
    pub model: Option<String>,
}
/// The provenance of an attempt's token counts. Missing usage is never zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Accounting {
    /// Recorded before accounting provenance was kept: unknown.
    #[default]
    Unrecorded,
    /// The provider never started, so no model call was made.
    NotLaunched,
    /// The provider's own end-of-turn totals.
    Reported,
    /// The per-call usage reported before the turn stopped, or so far while it
    /// runs: a lower bound.
    Partial,
    /// The provider ran but reported no usage.
    Unknown,
}
impl Accounting {
    pub fn is_unrecorded(&self) -> bool {
        *self == Self::Unrecorded
    }
}
/// One part of an attempt's status line: compact text and its full wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusPart {
    pub text: String,
    pub title: String,
}
fn part(text: String, title: String) -> StatusPart {
    StatusPart { text, title }
}
/// `12345` as `12,345`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
/// One attempt's compact status line for transcripts and terminals, e.g.
/// `attempt 1 · ↑ 5,074 in (3,235 cached) · ↓ 77 out… · 1 earlier exchange excluded`.
/// A running attempt's totals are those reported so far (`…`); a stopped
/// attempt's per-call sums are a lower bound (`≥`). Nothing is said about
/// tokens before a model call. A lone attempt label means nothing to show.
pub fn attempt_status(
    index: usize,
    tokens: Option<Tokens>,
    accounting: Accounting,
    running: bool,
    omitted: usize,
) -> Vec<StatusPart> {
    let n = index + 1;
    let mut parts = vec![part(format!("attempt {n}"), format!("Attempt {n}"))];
    match (tokens, accounting) {
        (Some(t), _) => {
            let (at_least, more, wording) = match (running, accounting) {
                (true, _) => ("", "…", " so far; the attempt is still running"),
                (false, Accounting::Partial) => {
                    ("≥", "", ", at least; the provider stopped before reporting its totals")
                }
                _ => ("", "", ""),
            };
            let (input, cached, output) = (
                grouped(t.input_tokens),
                grouped(t.cached_input_tokens),
                grouped(t.output_tokens),
            );
            parts.push(part(
                format!("↑ {at_least}{input} in ({cached} cached)"),
                format!("{input} input tokens ({cached} cached){wording}"),
            ));
            parts.push(part(
                format!("↓ {at_least}{output} out{more}"),
                format!("{output} output tokens{wording}"),
            ));
        }
        (None, Accounting::Unknown | Accounting::Partial | Accounting::Reported) => parts.push(if running {
            part(
                "tokens pending…".into(),
                "The provider has not reported token usage yet".into(),
            )
        } else {
            part(
                "tokens unreported".into(),
                "The provider reported no token usage".into(),
            )
        }),
        (None, Accounting::Unrecorded | Accounting::NotLaunched) => {}
    }
    if omitted > 0 {
        let plural = if omitted == 1 { "exchange" } else { "exchanges" };
        parts.push(part(
            format!("{omitted} earlier {plural} excluded"),
            format!("Context window · {omitted} earlier {plural} excluded from attempt {n}. Chat history retained."),
        ));
    }
    parts
}
/// The compact parts joined as plain text, for terminals.
pub fn status_text(parts: &[StatusPart]) -> String {
    parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" · ")
}
#[cfg(test)]
mod tests;
