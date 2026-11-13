//! Exactly-once approval notices. Persisting a receipt and its queued run is one
//! checkpoint; restarting Control never replays an acknowledged decision.
use crate::runtime::RunKind;
use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    allowed: bool,
    acknowledged: bool,
}
impl Receipt {
    pub(super) fn pending(allowed: bool) -> Self { Self { allowed, acknowledged:false } }
}

fn target<'a>(state: &'a State, port: &Port) -> Option<&'a crate::runtime::Run> {
    let origin = state.runtime.runs.iter().find(|run|run.id == port.origin_run && run.actor == port.actor)?;
    if matches!(origin.status.as_str(),"cancelled"|"cancelling"|"failed"|"interrupted"|"blocked")
        || !origin.attempts.iter().any(|attempt|attempt.workspace_id.as_deref()==Some(&port.origin_attempt)) {return None;}
    let root = origin.persona.as_ref().map_or(origin.id.as_str(), |persona|persona.root.as_str());
    let root = state.runtime.runs.iter().find(|run|run.id == root && run.actor == origin.actor && run.action == origin.action)?;
    // Any agent in the root's conversation tree acts for it, however deep and
    // whichever of the root chat's turns spawned it.
    let related = origin.id == root.id || origin.chat_id.as_deref().zip(root.chat_id.as_deref()).is_some_and(|(chat, top)| crate::chat::within(state, chat, top));
    if !related { return None; }
    Some(root)
}

pub(super) fn advance(state: &mut State, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<()> {
    let Some(root) = state.ports.iter()
        .filter(|port|port.decision.as_ref().is_some_and(|decision|!decision.acknowledged))
        .filter_map(|port|target(state,port))
        .find(|root| {
            let Some(chat)=root.chat_id.as_deref() else{return false;};
            if state.runtime.runs.iter().any(|run|run.chat_id.as_deref()==Some(chat)
                && matches!(run.status.as_str(),"queued"|"running"|"waiting"|"cancelling")){return false;}
            let mut pending=false;let mut denied=false;
            for port in state.ports.iter().filter(|port|target(state,port).is_some_and(|candidate|candidate.id==root.id)) {
                pending |= port.status==Status::Pending;
                denied |= port.decision.as_ref().is_some_and(|decision|!decision.acknowledged&&!decision.allowed);
            }
            !pending || denied
        }) else {return Ok(());};
    let Some(chat_id) = root.chat_id.clone() else { return Ok(()); };
    let root_id = root.id.clone();
    let actor = root.actor.clone();
    let indices = state.ports.iter().enumerate().filter(|(_,port)| {
        port.decision.as_ref().is_some_and(|decision|!decision.acknowledged)
            && target(state,port).is_some_and(|run|run.id == root_id)
    }).map(|(index,_)|index).collect::<Vec<_>>();
    let pending = state.ports.iter().any(|port|port.status == Status::Pending && target(state,port).is_some_and(|run|run.id == root_id));
    if pending && indices.iter().all(|index|state.ports[*index].decision.as_ref().unwrap().allowed) { return Ok(()); }
    let Some(chat_index) = state.chats.iter().position(|chat|chat.id == chat_id && chat.actor == actor) else { return Ok(()); };
    let task = root.objective.clone();
    let origins = indices.iter().map(|index|state.ports[*index].origin_run.clone()).collect::<Vec<_>>();
    let mut grants = Vec::new();
    for origin in state.runtime.runs.iter().filter(|run|origins.contains(&run.id)) {
        for grant in crate::services::grants(state,origin) {
            if !grants.contains(&grant) {grants.push(grant);}
        }
    }
    let decisions = indices.iter().map(|index| {
        let port = &state.ports[*index];
        format!("{} ({}): {}. Recheck control_port_status for current readiness and endpoints.",port.name.0,port.id.as_str(),if port.decision.as_ref().unwrap().allowed {"approved"} else {"denied"})
    }).collect::<Vec<_>>().join("\n");
    let run_count = state.runtime.runs.len();
    let message_count = state.chats[chat_index].messages.len();
    let revision = state.chats[chat_index].revision;
    let notice = state.runtime.notice.clone();
    let origin_ids = origins.iter().map(String::as_str).collect::<Vec<_>>();
    let outcome = crate::approvals::resume(state, crate::approvals::Decisions {
        chat: &chat_id, origins: &origin_ids, heading: "Port decisions", lines: &decisions,
        instruction: "Continue the original task within these grants. Do not assume traffic is ready; inspect status first.",
        task: &task, kind: RunKind::PortDecision, source: "port-decision", grants,
    });
    if outcome == crate::approvals::Outcome::Wait { return Ok(()); }
    for index in &indices {state.ports[*index].decision.as_mut().unwrap().acknowledged = true;}
    if let Err(error) = save(state) {
        state.runtime.runs.truncate(run_count);
        state.runtime.notice = notice;
        state.chats[chat_index].messages.truncate(message_count);
        state.chats[chat_index].revision = revision;
        for index in indices {state.ports[index].decision.as_mut().unwrap().acknowledged = false;}
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
