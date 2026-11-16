#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Tokens {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}
impl Tokens {
    pub fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
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
/// One attempt's token line for transcripts and terminals. A running attempt
/// shows the totals reported so far; nothing is shown before a model call.
pub fn tokens_label(tokens: Option<Tokens>, accounting: Accounting, running: bool) -> Option<String> {
    match (tokens, accounting) {
        (Some(t), _) => Some(format!("{} input ({} cached) · {} output tokens{}", t.input_tokens, t.cached_input_tokens, t.output_tokens,
            if running { " so far" } else if accounting == Accounting::Partial { ", at least" } else { "" })),
        (None, Accounting::Unknown | Accounting::Partial | Accounting::Reported) => Some(if running { "tokens not reported yet" } else { "tokens not reported" }.into()),
        (None, Accounting::Unrecorded | Accounting::NotLaunched) => None,
    }
}
