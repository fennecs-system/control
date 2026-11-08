//! Messages from an agent to a chat it spawned, queued as that chat's next turn,
//! so a blocked parent continues an existing child instead of spawning another.
use crate::runtime::RunKind;
use crate::{State, identity, tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use control_core::resources::{Phase, ResourceId};
use std::{io, sync::{Arc, Mutex}};

const MAX_SENT: usize = 256;
const MAX_SERVICES: usize = 8;

pub struct MessageCall {
    pub chat_id: String,
    pub text: String,
    pub services: Vec<ResourceId>,
}
pub trait MessageOps {
    fn message(&self, attempt: &AttemptRef<'_>, call: MessageCall) -> io::Result<Value>;
}
/// One delivered message, keyed by its request so retries queue nothing new.
#[derive(Clone, Serialize, Deserialize)]
pub struct Sent {
    key: String,
    origin: String,
    chat: String,
    run: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    services: Vec<ResourceId>,
}

pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({"name":"control_agent_message","description":"Send a follow-up to a chat you spawned, directly or through your children. Queues one turn behind any active turn. Optional services explicitly shares selected service IDs you can access, including resources created after the child was spawned. Exact owner/generation grants take effect in the queued turn and persist through its continuations; the active turn is unchanged. Shared services permit service tools, not just network access; port approvals remain separate. Omit services to share no new authority. Prefer continuing the child to spawning a replacement. Its reply stays in its chat, without an automatic completion notification.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"},"text":{"type":"string","minLength":1,"maxLength":2000},"services":{"type":"array","items":{"type":"string"},"maxItems":MAX_SERVICES,"uniqueItems":true}},"required":["chat_id","text"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})]
    }
    fn known(&self, name: &str) -> bool {
        name == "control_agent_message"
    }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        run(cx.host, &cx.attempt, args)
    }
}
impl MessageOps for LocalHost<'_> {
    fn message(&self, attempt: &AttemptRef<'_>, call: MessageCall) -> io::Result<Value> {
        execute(self.0, attempt, call)
    }
}
/// Decodes a call and runs it on whatever provides `MessageOps`.
fn run(ops: &dyn MessageOps, attempt: &AttemptRef<'_>, args: &Value) -> io::Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        chat_id: String,
        text: String,
        #[serde(default)]
        services: Vec<ResourceId>,
    }
    let input = serde_json::from_value::<Input>(args.clone()).map_err(|_| io::Error::other("Expected chat_id and text"))?;
    ops.message(attempt, MessageCall { chat_id: input.chat_id, text: input.text, services: input.services })
}

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
pub(crate) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: MessageCall) -> io::Result<Value> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    handle(&mut state, attempt, call, crate::storage::save)
}
fn handle(state: &mut State, attempt: &AttemptRef<'_>, call: MessageCall, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<Value> {
    let run = state.runtime.runs.iter()
        .find(|run| run.id == attempt.run && run.status == "running"
            && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation))
        .ok_or_else(|| fail("Attempt is not active"))?;
    identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::is_judge(run) || crate::delegation::pausing(run) {
        return Err(fail("Read-only or pausing attempts cannot message agents"));
    }
    let sender = run.chat_id.clone().ok_or_else(|| fail("A chat is required"))?;
    let actor = run.actor.clone();
    if !crate::chat::valid_id(attempt.nonce) || attempt.nonce.len() > 128 {
        return Err(fail("Invalid request identity"));
    }
    let key = format!("{}/{}/{}", attempt.run, attempt.generation, attempt.nonce);
    if let Some(sent) = state.agent_messages.iter().find(|sent| sent.key == key) {
        return if sent.chat == call.chat_id && sent.services == call.services { Ok(json!({"chat_id":sent.chat,"run_id":sent.run,"queued":true,"services":sent.services})) } else { Err(fail("Request identity reused for another chat or service grant")) };
    }
    let text = call.text.trim();
    if text.is_empty() || text.chars().count() > 2000 {
        return Err(fail("text must be 1–2000 characters"));
    }
    if call.services.len() > MAX_SERVICES || call.services.iter().collect::<std::collections::HashSet<_>>().len() != call.services.len() {
        return Err(fail("Select at most 8 distinct services to share"));
    }
    let grants = call.services.iter().map(|id| {
        let service = state.services.iter().find(|service| &service.resource.id == id)
            .filter(|service| run.attempts.last().and_then(|attempt| attempt.node_id.as_deref()) == Some(service.resource.owner.node.as_str()))
            .filter(|service| crate::services::accessible(service, &service.resource.owner.node, run))
            .ok_or_else(|| fail("Share only services this attempt can access"))?;
        if matches!(service.resource.phase, Phase::Stopping { .. } | Phase::Stopped(_) | Phase::Fenced { .. }) {
            return Err(fail("Cannot share a stopping, stopped or fenced service"));
        }
        Ok(service.grant())
    }).collect::<io::Result<Vec<_>>>()?;
    if call.chat_id == sender || !crate::chat::within(state, &call.chat_id, &sender) {
        return Err(fail("Message only chats you spawned, directly or through your children"));
    }
    let target = state.chats.iter().find(|session| session.id == call.chat_id).ok_or_else(|| fail("Chat not found"))?;
    if target.actor != actor {
        return Err(fail("Message only chats you spawned, directly or through your children"));
    }
    let from = state.runtime.runs.iter().rposition(|run| run.chat_id.as_deref() == Some(call.chat_id.as_str()) && run.actor == actor)
        .ok_or_else(|| fail("That chat has no task to continue"))?;
    if crate::delegation::is_judge(&state.runtime.runs[from]) {
        return Err(fail("Read-only judges cannot be continued"));
    }
    let title = state.chats.iter().find(|session| session.id == sender).map_or("", |session| session.title.as_str()).chars().take(80).collect::<String>();
    let objective = format!("Message from the agent that spawned you ({title}, {sender}):\n{text}").chars().take(2000).collect::<String>();
    let sent = state.agent_messages.clone();
    if state.agent_messages.len() >= MAX_SENT {
        let runs = &state.runtime.runs;
        state.agent_messages.retain(|sent| runs.iter().any(|run| run.id == sent.origin && run.status == "running"));
        if state.agent_messages.len() >= MAX_SENT {
            state.agent_messages = sent;
            return Err(fail("Too many unanswered agent messages; wait for running turns to finish"));
        }
    }
    let runs = state.runtime.runs.len();
    let notice = state.runtime.notice.clone();
    let id = match crate::chat::enqueue_continuation(state, &call.chat_id, from, objective, RunKind::AgentMessage, "agent-message") {
        Ok(id) => id,
        Err(error) => {
            state.agent_messages = sent;
            return Err(fail(error));
        }
    };
    let session = state.chats.iter_mut().find(|session| session.id == call.chat_id).unwrap();
    let (messages, revision) = (session.messages.len(), session.revision);
    session.messages.push(crate::chat::Message { text: format!("Message from {sender}\n{text}"), run_id: Some(id.clone()), reply: String::new() });
    session.revision = revision.saturating_add(1);
    let queued = state.runtime.runs.last_mut().unwrap();
    for grant in grants {
        if !queued.resource_grants.contains(&grant) { queued.resource_grants.push(grant); }
    }
    state.agent_messages.push(Sent { key, origin: attempt.run.into(), chat: call.chat_id.clone(), run: id.clone(), services: call.services.clone() });
    if let Err(error) = save(state) {
        state.runtime.runs.truncate(runs);
        state.runtime.notice = notice;
        let session = state.chats.iter_mut().find(|session| session.id == call.chat_id).unwrap();
        session.messages.truncate(messages);
        session.revision = revision;
        state.agent_messages = sent;
        return Err(error);
    }
    Ok(json!({"chat_id":call.chat_id,"run_id":id,"queued":true,"services":call.services}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family() -> (State, String, String, String) {
        let mut state = State::default();
        let lead = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &lead, "Lead the playtest", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        run.attempts.push(crate::usage::Attempt { workspace_id: Some("attempt-a".into()), ..Default::default() });
        let child = spawned(&mut state, &lead);
        let grandchild = spawned(&mut state, &child);
        (state, lead, child, grandchild)
    }
    fn spawned(state: &mut State, parent: &str) -> String {
        let chat = crate::chat::create(state);
        state.chats.iter_mut().find(|session| session.id == chat).unwrap().parent_chat = Some(parent.into());
        crate::chat::send(state, &chat, "Set up the bot", "test").unwrap();
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        chat
    }
    fn send(state: &mut State, nonce: &str, chat: &str) -> io::Result<Value> {
        let run = state.runtime.runs[0].id.clone();
        handle(state, &AttemptRef { run: &run, generation: "attempt-a", nonce }, MessageCall { chat_id: chat.into(), text: "Ports are free; retry".into(), services: vec![] }, |_| Ok(()))
    }

    #[test]
    fn a_parent_continues_its_descendants_once_per_request() {
        let (mut state, lead, child, grandchild) = family();
        let first = send(&mut state, "n1", &child).unwrap();
        let queued = state.runtime.runs.last().unwrap();
        assert_eq!((queued.kind.as_str(), queued.chat_id.as_deref(), queued.status.as_str()), ("agent-message", Some(child.as_str()), "queued"));
        assert!(queued.objective.contains("Ports are free; retry") && queued.objective.contains(&lead));
        assert_eq!(queued.kind.speaker(), control_context::agent_policy::MessageSpeaker::Control, "not a human turn that supersedes approvals");
        let runs = state.runtime.runs.len();
        assert_eq!(send(&mut state, "n1", &child).unwrap()["run_id"], first["run_id"], "retries are idempotent");
        assert_eq!(state.runtime.runs.len(), runs);
        assert!(send(&mut state, "n1", &grandchild).is_err(), "a request identity names one chat");
        send(&mut state, "n2", &grandchild).unwrap();
        assert_eq!(state.runtime.runs.last().unwrap().chat_id.as_deref(), Some(grandchild.as_str()));
        assert!(state.chats.iter().find(|session| session.id == grandchild).unwrap().messages.last().unwrap().text.starts_with(&format!("Message from {lead}")));
    }

    #[test]
    fn messages_reach_only_same_actor_descendants_of_active_attempts() {
        let (mut state, lead, child, _) = family();
        let stranger = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &stranger, "Unrelated", "test").unwrap();
        assert!(send(&mut state, "n1", &stranger).is_err(), "not spawned by the sender");
        assert!(send(&mut state, "n2", &lead).is_err(), "not the sender itself");
        state.chats.iter_mut().find(|session| session.id == child).unwrap().actor = crate::chat::discord_actor("d-123-456").unwrap();
        assert!(send(&mut state, "n3", &child).is_err(), "another actor's chat");
        let other = spawned(&mut state, &lead);
        assert!(send(&mut state, "bad nonce", &other).is_err());
        state.runtime.runs[0].attempts.last_mut().unwrap().workspace_id = Some("attempt-b".into());
        assert!(send(&mut state, "n4", &other).is_err(), "stale attempts");
    }

    #[test]
    fn failed_saves_leave_nothing_queued() {
        let (mut state, _, child, _) = family();
        let before = serde_json::to_vec(&state).unwrap();
        let run = state.runtime.runs[0].id.clone();
        let result = handle(&mut state, &AttemptRef { run: &run, generation: "attempt-a", nonce: "n1" }, MessageCall { chat_id: child.clone(), text: "Retry".into(), services: vec![] }, |_| Err(io::Error::other("disk full")));
        assert!(result.is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }

    #[test]
    fn receipts_of_running_attempts_are_never_evicted() {
        let (mut state, _, child, _) = family();
        let active = state.runtime.runs[0].id.clone();
        let first = send(&mut state, "n0", &child).unwrap();
        state.agent_messages.extend((1..MAX_SENT).map(|index| Sent { key: format!("other/{index}"), origin: active.clone(), chat: child.clone(), run: format!("r{index}"), services: vec![] }));
        assert!(send(&mut state, "n1", &child).is_err(), "a full cache of live receipts refuses instead of evicting");
        assert_eq!(send(&mut state, "n0", &child).unwrap()["run_id"], first["run_id"]);
        for sent in state.agent_messages.iter_mut().skip(1) {
            sent.origin = "finished-run".into();
        }
        send(&mut state, "n1", &child).unwrap();
        assert_eq!(send(&mut state, "n0", &child).unwrap()["run_id"], first["run_id"], "stale receipts of finished attempts go first");
    }

    fn late_service(state: &mut State, owner: &str) -> ResourceId {
        let id = ResourceId::try_from("svc-late".to_owned()).unwrap();
        state.runtime.runs[0].attempts[0].node_id = Some("node-a".into());
        state.services.push(serde_json::from_value(json!({
            "resource":{"id":id,"owner":{"node":"node-a","chat":owner},"generation":1,
                "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},
                "lease":[1,u64::MAX],"phase":"Requested"},
            "origin":{"run":state.runtime.runs[0].id,"attempt":"attempt-a"},
            "actor":{"type":"local_operator"},"command":"sleep 60","uid":502,"gid":20
        })).unwrap());
        id
    }

    #[test]
    fn explicit_late_service_grants_reach_only_the_queued_turn_and_survive_continuation() {
        let (mut state, lead, child, _) = family();
        let service = late_service(&mut state, &lead);
        let node = state.services[0].resource.owner.node.clone();
        let before_child = state.runtime.runs.iter().find(|run| run.chat_id.as_deref() == Some(&child)).unwrap().clone();
        assert!(!crate::services::accessible(&state.services[0], &node, &before_child));
        send(&mut state, "plain", &child).unwrap();
        assert!(!crate::services::accessible(&state.services[0], &node, state.runtime.runs.last().unwrap()), "ordinary messages do not grant access");
        let run = state.runtime.runs[0].id.clone();
        let call = || MessageCall { chat_id: child.clone(), text: "The new service is ready".into(), services: vec![service.clone()] };
        let attempt = AttemptRef { run: &run, generation: "attempt-a", nonce: "share" };
        let response = handle(&mut state, &attempt, call(), |_| Ok(())).unwrap();
        assert!(crate::services::accessible(&state.services[0], &node, state.runtime.runs.last().unwrap()));
        assert!(!crate::services::accessible(&state.services[0], &node, &before_child), "running or finished attempts are immutable");
        let count = state.runtime.runs.len();
        assert_eq!(handle(&mut state, &attempt, call(), |_| Ok(())).unwrap(), response);
        assert_eq!(state.runtime.runs.len(), count);
        let mut changed = call(); changed.services.clear();
        assert!(handle(&mut state, &attempt, changed, |_| Ok(())).is_err(), "a nonce cannot change the grant selection");
        let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let from = restored.runtime.runs.len() - 1;
        crate::chat::enqueue_continuation(&mut restored, &child, from, "Continue".into(), RunKind::AgentMessage, "test").unwrap();
        assert!(crate::services::accessible(&restored.services[0], &node, restored.runtime.runs.last().unwrap()));
        restored.services[0].resource.generation = serde_json::from_value(json!(2)).unwrap();
        assert!(!crate::services::accessible(&restored.services[0], &node, restored.runtime.runs.last().unwrap()), "a replacement generation is not granted");
    }

    #[test]
    fn late_grants_reject_foreign_services_and_roll_back_atomically() {
        let (mut state, lead, child, grandchild) = family();
        let service = late_service(&mut state, &grandchild);
        let run = state.runtime.runs[0].id.clone();
        let call = || MessageCall { chat_id: child.clone(), text: "Continue".into(), services: vec![service.clone()] };
        let attempt = AttemptRef { run: &run, generation: "attempt-a", nonce: "share" };
        assert!(handle(&mut state, &attempt, call(), |_| Ok(())).is_err(), "being an ancestor does not itself grant service access");
        state.services[0].resource.owner.chat = lead.try_into().unwrap();
        let before = serde_json::to_vec(&state).unwrap();
        assert!(handle(&mut state, &attempt, call(), |_| Err(io::Error::other("disk full"))).is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(), before, "grant, message and queued turn commit together");
        state.services[0].resource.owner.node = "other-node".to_owned().try_into().unwrap();
        assert!(handle(&mut state, &attempt, call(), |_| Ok(())).is_err());
        state.services[0].resource.owner.node = "node-a".to_owned().try_into().unwrap();
        state.services[0].resource.phase = Phase::Stopped(control_core::resources::StopReason::Requested);
        assert!(handle(&mut state, &attempt, call(), |_| Ok(())).is_err());
    }
}
