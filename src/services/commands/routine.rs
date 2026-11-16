//! A one-line nudge from step-by-step driving toward a routine. When one
//! attempt has issued [`HINT_AFTER`] commands to the same service and no
//! routine (active watch) runs there, the exec result carries a `hint`. It is
//! advice only: nothing is refused, delayed or recorded.
use super::*;

/// Commands from one attempt to one service before the hint appears.
pub(super) const HINT_AFTER: usize = 5;
pub(super) const HINT: &str = "Repeated commands on this service: for anything repeated or polled, consider a routine (control_watch_start).";

/// Adds the hint to an exec result when this request is exactly the attempt's
/// [`HINT_AFTER`]th command on the service, so it appears once per attempt and
/// service without extra state; a retry of that request returns it again. The
/// count reads only this service's retained records (live attempts keep a
/// receipt for every evicted output); copies are not counted.
pub(super) fn hint(state: &State, run: &str, attempt: &str, service_id: &str, result: &mut Value) {
    let Some(service) = state.services.iter().find(|s| s.resource.id.as_str() == service_id) else { return };
    let mine = |r: &str, a: &str| r == run && a == attempt;
    let commands = service.executions.iter().filter(|e| e.copy.is_none() && mine(&e.run, &e.attempt)).count()
        + service.command_receipts.iter().filter(|r| mine(&r.run, &r.attempt)).count();
    if commands == HINT_AFTER && !state.watches.iter().any(|w| w.service == service_id && w.active()) {
        result["hint"] = json!(HINT);
    }
}

#[cfg(test)]
mod tests;
