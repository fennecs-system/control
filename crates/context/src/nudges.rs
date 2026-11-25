//! Wording of the advisory notes Control adds to a turn: the host's notes on
//! tool results and the local loop's notes in its own transcript share it, so
//! every backend is told the same thing in the same words.

/// What a checkpoint must carry so findings outlive the turn and its compaction:
/// the working notes are `control_turn_handoff`'s payload, and an omitted
/// payload keeps the older notes.
pub const NOTES: &str = "Record your key findings (facts, diagnoses) and current plan in its working notes, payload.findings and payload.next, so they survive compaction; an omitted payload keeps the older notes.";
/// How a turn without a handoff keeps findings through in-turn compaction.
pub const MARKS: &str = "Write key facts on lines starting Finding: and choices on lines starting Decision:; Control keeps those lines when it summarizes older steps.";
/// The once-per-turn note when a local turn first shortens its older tool
/// results: without it a model re-reads the files it lost, which are shortened
/// again, instead of writing down what it needs.
pub fn elision_text(kept: usize) -> String {
    format!(
        "Control now shortens tool results older than your last {kept} steps to save context, and a file read again is shortened again. {MARKS}"
    )
}
/// What follows a turn that stops now. Only a persistent bot has a next turn:
/// a chat that hands off without one simply ends, so the note must not promise
/// a continuation.
#[derive(Clone, Copy)]
pub enum Next {
    /// A persistent bot with the handoff tool: checkpoint and continue.
    Turn,
    /// The handoff tool without a bot yet, and one can be requested.
    Unscheduled,
    /// Nothing follows: finish.
    Report,
}
impl Next {
    pub fn of(handoff: bool, continues: bool, requestable: bool) -> Self {
        match (handoff, continues, requestable) {
            (true, true, _) => Self::Turn,
            (true, false, true) => Self::Unscheduled,
            _ => Self::Report,
        }
    }
}
const UNSCHEDULED: &str = "this chat has no next turn: before you hand off, request your persistent bot (control_bot_request) or delegate what remains; otherwise finish with a brief final report.";
/// The once-per-turn note when a turn's context passes its threshold (local
/// endpoints, where Control owns the transcript and measures its size).
pub fn context_text(percent: u32, next: Next) -> String {
    match next {
        Next::Turn => format!(
            "Context {percent}% full: publish your checkpoint (control_turn_handoff) and continue in the next turn. {NOTES} {MARKS}"
        ),
        Next::Unscheduled => format!("Context {percent}% full and {UNSCHEDULED} {NOTES} {MARKS}"),
        Next::Report => format!("Context {percent}% full: finish with a brief final report soon. {MARKS}"),
    }
}
/// The once-per-turn note when a local-model turn nears its model-step cap.
pub fn steps_text(left: usize, next: Next) -> String {
    match next {
        Next::Turn => format!(
            "{left} model steps left in this turn: publish your checkpoint (control_turn_handoff) and continue next turn. {NOTES}"
        ),
        Next::Unscheduled => format!("{left} model steps left in this turn and {UNSCHEDULED} {NOTES}"),
        Next::Report => format!("{left} model steps left in this turn: finish with a brief final report now."),
    }
}
