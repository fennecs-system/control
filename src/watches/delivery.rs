//! Bridge off-lock spool reads to the existing attempt mailbox. A receipt is
//! evidence delivered, not processed; only workspace publication retires it.
use crate::{State, tool_registry::AttemptRef};
use control_context::turn_mailbox::{Delivery, Range};
use std::io;

pub(crate) struct Read {
    pub after: u64,
    pub maximum: Option<u64>,
    pub gap: bool,
    key: String,
    request: String,
    managed: bool,
}
fn index(state: &State, attempt: &AttemptRef<'_>) -> io::Result<usize> {
    let index = state.runtime.runs.iter().position(|r| r.id == attempt.run && r.status == "running"
        && r.attempts.last().is_some_and(|a| a.workspace_id.as_deref() == Some(attempt.generation)))
        .ok_or_else(|| io::Error::other("Attempt is not active"))?;
    crate::identity::authorize(&state.runtime.runs[index].actor, state.runtime.runs[index].action).map_err(io::Error::other)?;
    Ok(index)
}
fn position(range: &Range, watch: &str) -> Option<u64> {
    match range { Range::Watch { watch: id, through, .. } if id == watch => Some(*through), _ => None }
}
fn digest(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn prepare(state: &State, attempt: &AttemptRef<'_>, watch: &str, after: Option<u64>) -> io::Result<Read> {
    let run = &state.runtime.runs[index(state, attempt)?];
    let owned = state.watches.iter().any(|w| w.id == watch && Some(&w.chat) == run.chat_id.as_ref());
    let mailbox = run.attempts.last().and_then(|a| a.turn_mailbox.as_ref()).filter(|_| owned);
    let working = state.chats.iter().find(|c| Some(&c.id) == run.chat_id.as_ref())
        .and_then(|c| c.continuity.as_ref()).and_then(|c| c.working.as_ref());
    let processed = working.into_iter().flat_map(|w| &w.processed)
        .chain(mailbox.into_iter().flat_map(|m| &m.baseline)).filter_map(|r| position(r, watch)).max().unwrap_or(0);
    let delivered = mailbox.into_iter().flat_map(|m| m.deliveries.values()).flat_map(|d| &d.ranges)
        .filter_map(|r| position(r, watch)).max().unwrap_or(processed).max(processed);
    let key = digest(&format!("{}/{}/watch/{}", attempt.run, attempt.generation, attempt.nonce));
    let request = format!("{watch}/{after:?}");
    let previous = mailbox.and_then(|m| m.deliveries.get(&key));
    if previous.is_some_and(|d| d.request != request) { return Err(io::Error::other("Watch request identity reused with different arguments")); }
    let (after, maximum) = match previous.and_then(|d| d.ranges.first()) {
        Some(Range::Watch { after, through, .. }) => (*after, Some(*through)),
        _ => (after.unwrap_or(delivered), None),
    };
    let gap = previous.map_or(after > delivered, |d| d.gap);
    Ok(Read { after, maximum, gap, key, request, managed: mailbox.is_some_and(|m| m.accepted().is_none()) })
}
pub(crate) fn record(state: &mut State, attempt: &AttemptRef<'_>, watch: &str, read: &Read,
    through: u64, gap: bool, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<Option<String>> {
    let index = index(state, attempt)?;
    if !read.managed { return Ok(None); }
    let run = &state.runtime.runs[index];
    if !state.watches.iter().any(|w| w.id == watch && Some(&w.chat) == run.chat_id.as_ref()) {
        return Err(io::Error::other("Watch owner changed"));
    }
    let original = run.attempts.last().and_then(|a| a.turn_mailbox.as_ref()).ok_or_else(|| io::Error::other("Attempt mailbox unavailable"))?;
    if original.accepted().is_some() { return Ok(None); }
    let mut mailbox = original.clone();
    let empty = through == read.after;
    // Explicit skips and truncated/missing source data require resynchronisation.
    let gap = gap || read.gap;
    let key = if empty { digest(&format!("{}/{}/watch/{watch}/empty/{through}/{gap}", attempt.run, attempt.generation)) } else { read.key.clone() };
    mailbox.deliver(key.clone(), Delivery { ranges: vec![Range::Watch { watch:watch.into(), after:read.after, through }], gap,
        request: if empty { String::new() } else { read.request.clone() } }).map_err(io::Error::other)?;
    if &mailbox == original { return Ok(Some(key)); }
    let previous = state.runtime.runs[index].attempts.last_mut().unwrap().turn_mailbox.replace(mailbox);
    if let Err(error) = save(state) {
        state.runtime.runs[index].attempts.last_mut().unwrap().turn_mailbox = previous.into();
        return Err(error);
    }
    Ok(Some(key))
}
