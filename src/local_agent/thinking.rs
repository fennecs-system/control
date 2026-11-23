//! Adaptive thinking for a local turn. The model entry declares a base level
//! (`Thinking`, default medium); each step's level moves around it:
//!
//! - one level higher on the turn's first step, after a failed or malformed
//!   tool call (a refused handoff included), and after an in-turn compaction;
//! - one level lower on routine steps that follow a successful action;
//! - minimal after a reply cut at the output limit, so the answer gets the room;
//! - minimal on a resumed turn (a host checkpoint or a retried attempt) until
//!   the turn's first executed tool call, like pi's resume level: the plan is in
//!   the checkpoint, so the model should act on it rather than re-plan;
//! - Qwen's soft switches: a reply of just `/think` asks for a high level on a
//!   re-sent step, and `/think` or `/no_think` in a reply sets the next step.
//!
//! A base of `off` never thinks. Reasoning never enters the transcript.
use control_ai::model::Thinking;

/// What the previous step left for the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    Start,
    Trouble,
    Routine,
    Cut,
}
pub(super) struct Adaptive {
    base: Thinking,
    resuming: bool,
    cue: Cue,
    asked: Option<Thinking>,
}
impl Adaptive {
    pub(super) fn new(base: Thinking, resumed: bool) -> Self {
        Self {
            base,
            resuming: resumed,
            cue: Cue::Start,
            asked: None,
        }
    }
    /// The next request's level; `compacted`: the loop folded steps for it.
    pub(super) fn level(&mut self, compacted: bool) -> Thinking {
        if self.base == Thinking::Off {
            return Thinking::Off;
        }
        if let Some(level) = self.asked.take() {
            return level;
        }
        if self.resuming {
            return Thinking::Minimal;
        }
        match self.cue {
            _ if compacted => self.base.up(),
            Cue::Start | Cue::Trouble => self.base.up(),
            Cue::Routine => self.base.down(),
            Cue::Cut => Thinking::Minimal,
        }
    }
    pub(super) fn after(&mut self, cue: Cue) {
        self.cue = cue;
    }
    /// A tool call ran: a resumed turn is acting again.
    pub(super) fn acted(&mut self) {
        self.resuming = false;
    }
    /// A reply's soft switch sets the next step's level. Returns the level
    /// asked for, if any, when thinking is on for this entry.
    pub(super) fn switch(&mut self, text: &str) -> Option<Thinking> {
        if self.base == Thinking::Off {
            return None;
        }
        let asked = switch(text)?;
        self.asked = Some(asked);
        Some(asked)
    }
}
/// The last Qwen soft switch in `text`: `/no_think` (no thinking) or `/think` (high).
pub(super) fn switch(text: &str) -> Option<Thinking> {
    let words = text.split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | '(' | ')' | '"' | '\'' | '`'));
    words.rev().find_map(|word| match word {
        "/no_think" => Some(Thinking::Off),
        "/think" => Some(Thinking::High),
        _ => None,
    })
}
/// A reply that is only a request to think longer before acting.
pub(super) fn asks_more(text: &str) -> bool {
    text.trim() == "/think"
}
/// Tells the model about the soft switches; only sent while thinking is on.
pub(super) const NOTE: &str = " You may think before each step. Reply with only /think to think longer before acting; put /no_think in a reply to act on the next step without thinking.";

#[cfg(test)]
mod tests;
