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
