//! Exactly-once approval notices. Persisting a receipt and its queued run is one
//! checkpoint; restarting Control never replays an acknowledged decision.
//!
//! A decision wakes its requester at most once, from the process that made it.
//! One that cannot be delivered (its requester ended without a host checkpoint
//! or was cancelled by the operator, or an approved port has since stopped) is
//! retired rather than kept for later, and one read back after a restart is
//! never delivered, so neither a redeploy nor a later rule change replays it.
use super::*;
use crate::runtime::RunKind;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub(super) allowed: bool,
    acknowledged: bool,
    /// Decided by this process. Never persisted: a receipt loaded from disk was
    /// decided before the restart and only ever gets acknowledged.
    #[serde(skip)]
    live: bool,
}
impl Receipt {
    pub(super) fn pending(allowed: bool) -> Self {
        Self {
            allowed,
            acknowledged: false,
            live: true,
        }
    }
    /// A decision the requester already received in its tool result (an
    /// automatic approval), so no decision turn is queued for it.
    pub(super) fn settled(allowed: bool) -> Self {
        Self {
            allowed,
            acknowledged: true,
            live: true,
        }
    }
}

fn target<'a>(state: &'a State, port: &Port) -> Option<&'a crate::runtime::Run> {
    let origin = state
        .runtime
        .runs
        .iter()
        .find(|run| run.id == port.origin_run && run.actor == port.actor)?;
    // A turn cut off before its handoff (time limit, step cap, provider
    // failure) left a host checkpoint; its request still belongs to the chat.
    let cut_off = crate::attempt_control::cut_off(origin);
    if (!cut_off
        && matches!(
            origin.status.as_str(),
            "cancelled" | "cancelling" | "failed" | "interrupted" | "blocked"
        ))
        || !origin
            .attempts
            .iter()
            .any(|attempt| attempt.workspace_id.as_deref() == Some(&port.origin_attempt))
    {
        return None;
    }
    let root = origin
        .persona
        .as_ref()
        .map_or(origin.id.as_str(), |persona| persona.root.as_str());
    let root = state
        .runtime
        .runs
        .iter()
        .find(|run| run.id == root && run.actor == origin.actor && run.action == origin.action)?;
    // Any agent in the root's conversation tree acts for it, however deep and
    // whichever of the root chat's turns spawned it.
    let related = origin.id == root.id
        || origin
            .chat_id
            .as_deref()
            .zip(root.chat_id.as_deref())
            .is_some_and(|(chat, top)| crate::chat::within(state, chat, top));
    if !related {
        return None;
    }
    Some(root)
}

pub(super) fn advance(state: &mut State, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<()> {
    advance_with_wake(state, save, crate::worker::wake)
}

/// The undelivered decision on `port` can no longer wake its requester: it was
/// read back after a restart, approves a port or service that has since
/// stopped, or its requester ended without a host checkpoint or belongs to work
/// the operator cancelled after the request.
fn obsolete(state: &State, port: &Port, time: u64) -> bool {
    let Some(decision) = port.decision.as_ref().filter(|decision| !decision.acknowledged) else {
        return false;
    };
    if !decision.live
        || (decision.allowed
            && (port.status != Status::Approved || service(PortAuthority::of(state), port, time).is_err()))
    {
        return true;
    }
    let Some(root) = target(state, port) else { return true };
    let origin = state
        .runtime
        .runs
        .iter()
        .find(|run| run.id == port.origin_run && run.actor == port.actor);
    origin.is_some_and(|origin| crate::chat::cancelled_after(state, origin))
        || crate::chat::cancelled_after(state, root)
}

fn advance_with_wake(
    state: &mut State,
    save: impl FnOnce(&State) -> io::Result<()>,
    wake: impl FnOnce(),
) -> io::Result<()> {
    // Retire undeliverable decisions now instead of keeping them for a later
    // pass. This needs no checkpoint of its own: the next save carries it, and
    // a receipt reloaded before then is not live, so it is retired again.
    let time = now();
    let retired = state
        .ports
        .iter()
        .enumerate()
        .filter(|(_, port)| obsolete(state, port, time))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for index in retired {
        state.ports[index].decision.as_mut().unwrap().acknowledged = true;
    }
    let Some(root) = state
        .ports
        .iter()
        .filter(|port| port.decision.as_ref().is_some_and(|decision| !decision.acknowledged))
        .filter_map(|port| target(state, port))
        .find(|root| {
            let Some(chat) = root.chat_id.as_deref() else {
                return false;
            };
            if state.runtime.runs.iter().any(|run| {
                run.chat_id.as_deref() == Some(chat)
                    && matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling")
            }) {
                return false;
            }
            let mut pending = false;
            let mut denied = false;
            for port in state
                .ports
                .iter()
                .filter(|port| target(state, port).is_some_and(|candidate| candidate.id == root.id))
            {
                pending |= port.status == Status::Pending;
                denied |= port
                    .decision
                    .as_ref()
                    .is_some_and(|decision| !decision.acknowledged && !decision.allowed);
            }
            !pending || denied
        })
    else {
        return Ok(());
    };
    let Some(chat_id) = root.chat_id.clone() else {
        return Ok(());
    };
    let root_id = root.id.clone();
    let actor = root.actor.clone();
    let indices = state
        .ports
        .iter()
        .enumerate()
        .filter(|(_, port)| {
            port.decision.as_ref().is_some_and(|decision| !decision.acknowledged)
                && target(state, port).is_some_and(|run| run.id == root_id)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let pending = state
        .ports
        .iter()
        .any(|port| port.status == Status::Pending && target(state, port).is_some_and(|run| run.id == root_id));
    if pending
        && indices
            .iter()
            .all(|index| state.ports[*index].decision.as_ref().unwrap().allowed)
    {
        return Ok(());
    }
    let Some(chat_index) = state
        .chats
        .iter()
        .position(|chat| chat.id == chat_id && chat.actor == actor)
    else {
        return Ok(());
    };
    let task = root.objective.clone();
    let origins = indices
        .iter()
        .map(|index| state.ports[*index].origin_run.clone())
        .collect::<Vec<_>>();
    let mut grants = Vec::new();
    for origin in state.runtime.runs.iter().filter(|run| origins.contains(&run.id)) {
        for grant in crate::services::grants(state, origin) {
            if !grants.contains(&grant) {
                grants.push(grant);
            }
        }
    }
    let decisions = indices
        .iter()
        .map(|index| {
            let port = &state.ports[*index];
            let allowed = port.decision.as_ref().unwrap().allowed;
            let usage = port
                .usage()
                .filter(|_| allowed)
                .map(|usage| format!(" {usage}"))
                .unwrap_or_default();
            format!(
                "{} ({}): {}. Recheck control_port_status for current readiness and endpoints.{usage}",
                port.name.0,
                port.id.as_str(),
                if allowed { "approved" } else { "denied" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let run_count = state.runtime.runs.len();
    let message_count = state.chats[chat_index].messages.len();
    let revision = state.chats[chat_index].revision;
    let notice = state.runtime.notice.clone();
    let origin_ids = origins.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = crate::approvals::resume(
        state,
        crate::approvals::Decisions {
            chat: &chat_id,
            origins: &origin_ids,
            heading: crate::application::approval_resume::PORT_DECISIONS,
            lines: &decisions,
            instruction: "Continue the original task within these grants. Do not assume traffic is ready; inspect status first.",
            task: &task,
            kind: RunKind::PortDecision,
            source: "port-decision",
            grants,
        },
    );
    if outcome == crate::approvals::Outcome::Wait {
        return Ok(());
    }
    for index in &indices {
        state.ports[*index].decision.as_mut().unwrap().acknowledged = true;
    }
    if let Err(error) = save(state) {
        state.runtime.runs.truncate(run_count);
        state.runtime.notice = notice;
        state.chats[chat_index].messages.truncate(message_count);
        state.chats[chat_index].revision = revision;
        for index in indices {
            state.ports[index].decision.as_mut().unwrap().acknowledged = false;
        }
        return Err(error);
    }
    if matches!(outcome, crate::approvals::Outcome::Resumed(_)) {
        wake();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
