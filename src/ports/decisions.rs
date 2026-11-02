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
mod tests {
    use super::*;
    fn approved() -> (State,String) {
        let (mut state,run,chat) = super::super::tests::fixture();
        admit(&mut state,&run,"attempt-a","notice",Input {
            service_id:"svc-a".to_owned().try_into().unwrap(),name:"server".to_owned().try_into().unwrap(),
            direction:Direction::Ingress,service_port:std::num::NonZeroU16::new(25565).unwrap(),target:None,peer_port_id:None,
        },"node-a".to_owned().try_into().unwrap(),|_|Ok(())).unwrap();
        let id=state.ports[0].id.as_str().to_owned();
        approve(&mut state,&id,60,false,None,now(),|_|Ok(())).unwrap();
        (state,chat)
    }
    #[test]
    fn approval_waits_for_turn_and_wakes_once_with_captured_provider_after_reload() {
        let (mut state,chat)=approved();
        advance(&mut state,|_|panic!("active turn must not queue another run")).unwrap();
        state.runtime.runs[0].status="completed".into();
        state.runtime.runs[0].model=Some("captured-model".into());
        let before=serde_json::to_vec(&state).unwrap();
        assert!(advance(&mut state,|_|Err(fail("disk full"))).is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(),before);
        let mut state:State=serde_json::from_slice(&before).unwrap();
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),2);
        assert_eq!(state.runtime.runs[1].chat_id.as_deref(),Some(chat.as_str()));
        assert_eq!(state.runtime.runs[1].model.as_deref(),Some("captured-model"));
        assert_eq!(state.runtime.runs[1].kind,RunKind::PortDecision);
        let mut restored:State=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        advance(&mut restored,|_|panic!("acknowledged decisions must not replay")).unwrap();
        assert_eq!(restored.runtime.runs.len(),2);
    }
    #[test]
    fn bot_wake_turns_do_not_supersede_approvals() {
        let (mut state,chat)=approved();
        state.runtime.runs[0].status="completed".into();
        let mut wake=state.runtime.runs[0].clone();
        wake.id="bot-wake".into();
        wake.kind=crate::runtime::RunKind::BotWake;
        state.runtime.runs.push(wake);
        let count=state.runtime.runs.len();
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),count+1,"a bot waking is not a human steering the task");
        assert_eq!(state.runtime.runs.last().unwrap().chat_id.as_deref(),Some(chat.as_str()));
    }
    #[test]
    fn newer_human_instruction_suppresses_delayed_approval_wake() {
        let (mut state,chat)=approved();
        state.runtime.runs[0].status="completed".into();
        crate::chat::send(&mut state,&chat,"Stop the playtest and leave resources alone","test").unwrap();
        state.runtime.runs.last_mut().unwrap().status="completed".into();
        let count=state.runtime.runs.len();
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),count);
        assert!(state.ports[0].decision.as_ref().unwrap().acknowledged);
    }
    #[test]
    fn request_from_answered_design_continuation_wakes_the_original_conversation() {
        let (mut state,chat)=approved();
        let mut continuation=state.runtime.runs[0].clone();
        continuation.id="after-design".into();
        continuation.status="completed".into();
        continuation.kind=RunKind::DesignAnswer;
        continuation.persona=Some(crate::delegation::lineage(&state.runtime.runs[0]));
        state.runtime.runs[0].status="continued".into();
        state.runtime.runs[0].question=Some(crate::design::Question {
            id:"q-route".into(),generation:"attempt-a".into(),nonce:"route".into(),
            input:crate::design::Input {question:"Route?".into(),options:vec!["Relay".into(),"Stop".into()],recommended:None},
            answer:Some("Relay".into()),continuation:Some(continuation.id.clone()),
        });
        state.ports[0].origin_run=continuation.id.clone();
        state.runtime.runs.push(continuation);
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),3);
        assert_eq!(state.runtime.runs[2].chat_id.as_deref(),Some(chat.as_str()));
        assert_eq!(state.runtime.runs[2].kind,RunKind::PortDecision);
    }
    #[test]
    fn requests_from_a_resumed_turn_wake_the_conversation_again() {
        let (mut state,chat)=approved();
        state.runtime.runs[0].status="completed".into();
        advance(&mut state,|_|Ok(())).unwrap();
        let resumed=state.runtime.runs.len()-1;
        state.runtime.runs[resumed].status="completed".into();
        state.runtime.runs[resumed].attempts.push(crate::usage::Attempt{workspace_id:Some("attempt-b".into()),..Default::default()});
        let mut second=state.ports[0].clone();
        second.id="port-second".to_owned().try_into().unwrap();
        second.origin_run=state.runtime.runs[resumed].id.clone();
        second.origin_attempt="attempt-b".into();
        second.decision=Some(Receipt::pending(true));
        state.ports.push(second);
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),resumed+2,"a second round of approvals resumes the chat");
        assert_eq!(state.runtime.runs.last().unwrap().chat_id.as_deref(),Some(chat.as_str()));
        assert!(state.ports[1].decision.as_ref().unwrap().acknowledged);
    }
    #[test]
    fn requests_from_a_grandchild_spawned_by_a_later_turn_resume_the_lead() {
        let (mut state,lead)=approved();
        state.runtime.runs[0].status="completed".into();
        state.ports[0].decision=None;
        let root=state.runtime.runs[0].id.clone();
        let mut later=state.runtime.runs[0].clone();
        later.id="later".into();
        later.kind=crate::runtime::RunKind::AgentCompletion;
        state.runtime.runs.push(later);
        let mut parent="later".to_owned();
        let mut chat=lead.clone();
        for (depth,id) in [(1,"helper"),(2,"coordinator")] {
            let child=crate::chat::create(&mut state);
            state.chats.iter_mut().find(|s|s.id==child).unwrap().parent_chat=Some(chat.clone());
            let mut run=state.runtime.runs[0].clone();
            run.id=id.into();
            run.kind=crate::runtime::RunKind::Spawned;
            run.chat_id=Some(child.clone());
            run.persona=Some(crate::delegation::Persona{role:crate::delegation::Role::Worker,root:root.clone(),depth,parent:Some(parent.clone())});
            run.attempts=vec![crate::usage::Attempt{workspace_id:Some(format!("{id}-attempt")),..Default::default()}];
            state.runtime.runs.push(run);
            (parent,chat)=(id.to_owned(),child);
        }
        let mut port=state.ports[0].clone();
        port.id="port-grandchild".to_owned().try_into().unwrap();
        port.origin_run="coordinator".into();
        port.origin_attempt="coordinator-attempt".into();
        port.decision=Some(Receipt::pending(true));
        state.ports.push(port);
        let count=state.runtime.runs.len();
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),count+1,"the lead resumes with the grandchild's approval");
        assert_eq!(state.runtime.runs.last().unwrap().chat_id.as_deref(),Some(lead.as_str()));
        assert!(state.ports[1].decision.as_ref().unwrap().acknowledged);
    }
    #[test]
    fn waiting_chat_does_not_starve_an_independent_ready_callback() {
        let (mut state,_)=approved(); // first task is still running
        let (mut other,chat)=approved();
        other.runtime.runs[0].status="completed".into();
        state.chats.extend(other.chats);
        state.runtime.runs.extend(other.runtime.runs);
        state.ports.extend(other.ports);
        advance(&mut state,|_|Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(),3);
        assert_eq!(state.runtime.runs[2].chat_id.as_deref(),Some(chat.as_str()));
        assert!(!state.ports[0].decision.as_ref().unwrap().acknowledged);
    }
    #[test]
    fn denial_wakes_without_granting_access_and_cancelled_requests_never_wake() {
        let (mut state,_)=approved();
        state.ports[0].status=Status::Stopping;
        state.ports[0].approval=None;
        state.ports[0].decision=Some(Receipt::pending(false));
        state.runtime.runs[0].status="completed".into();
        advance(&mut state,|_|Ok(())).unwrap();
        assert!(state.runtime.runs[1].objective.contains("denied"));
        assert!(state.ports[0].approval.is_none());
        let (mut cancelled,_)=approved();
        cancelled.runtime.runs[0].status="cancelled".into();
        advance(&mut cancelled,|_|panic!("cancelled origin must not wake")).unwrap();
        assert_eq!(cancelled.runtime.runs.len(),1);
    }
}
