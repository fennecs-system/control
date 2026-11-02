//! Messages from an agent to a chat it spawned, queued as that chat's next turn,
//! so a blocked parent continues an existing child instead of spawning another.
use crate::{State, identity, tool_registry::{AttemptCall, AttemptRef, AttemptTools}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, sync::{Arc, Mutex}};

const MAX_SENT: usize = 256;

pub struct MessageCall {
    pub chat_id: String,
    pub text: String,
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
}

pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({"name":"control_agent_message","description":"Send a follow-up to a chat you spawned (directly or through your children), for example to continue a finished or blocked child after a blocker clears. It queues one turn in that chat with your text, behind any turn already running there. Prefer this to spawning a replacement agent. The child's reply appears in its own chat and its completion is not reported back automatically.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"},"text":{"type":"string","minLength":1,"maxLength":2000}},"required":["chat_id","text"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})]
    }
    fn known(&self, name: &str) -> bool {
        name == "control_agent_message"
    }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            chat_id: String,
            text: String,
        }
        let input = serde_json::from_value::<Input>(args.clone()).map_err(|_| io::Error::other("Expected chat_id and text"))?;
        cx.host.messages().message(&cx.attempt, MessageCall { chat_id: input.chat_id, text: input.text })
    }
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
        return if sent.chat == call.chat_id { Ok(json!({"chat_id":sent.chat,"run_id":sent.run,"queued":true})) } else { Err(fail("Request identity reused for another chat")) };
    }
    let text = call.text.trim();
    if text.is_empty() || text.chars().count() > 2000 {
        return Err(fail("text must be 1–2000 characters"));
    }
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
    let id = match crate::chat::enqueue_continuation(state, &call.chat_id, from, objective, "agent-message", "agent-message") {
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
    state.agent_messages.push(Sent { key, origin: attempt.run.into(), chat: call.chat_id.clone(), run: id.clone() });
    if let Err(error) = save(state) {
        state.runtime.runs.truncate(runs);
        state.runtime.notice = notice;
        let session = state.chats.iter_mut().find(|session| session.id == call.chat_id).unwrap();
        session.messages.truncate(messages);
        session.revision = revision;
        state.agent_messages = sent;
        return Err(error);
    }
    Ok(json!({"chat_id":call.chat_id,"run_id":id,"queued":true}))
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
        handle(state, &AttemptRef { run: &run, generation: "attempt-a", nonce }, MessageCall { chat_id: chat.into(), text: "Ports are free; retry".into() }, |_| Ok(()))
    }

    #[test]
    fn a_parent_continues_its_descendants_once_per_request() {
        let (mut state, lead, child, grandchild) = family();
        let first = send(&mut state, "n1", &child).unwrap();
        let queued = state.runtime.runs.last().unwrap();
        assert_eq!((queued.kind.as_str(), queued.chat_id.as_deref(), queued.status.as_str()), ("agent-message", Some(child.as_str()), "queued"));
        assert!(queued.objective.contains("Ports are free; retry") && queued.objective.contains(&lead));
        assert_eq!(control_context::agent_policy::MessageSpeaker::for_run_kind(Some(&queued.kind)), control_context::agent_policy::MessageSpeaker::Control, "not a human turn that supersedes approvals");
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
        let result = handle(&mut state, &AttemptRef { run: &run, generation: "attempt-a", nonce: "n1" }, MessageCall { chat_id: child.clone(), text: "Retry".into() }, |_| Err(io::Error::other("disk full")));
        assert!(result.is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }

    #[test]
    fn receipts_of_running_attempts_are_never_evicted() {
        let (mut state, _, child, _) = family();
        let active = state.runtime.runs[0].id.clone();
        let first = send(&mut state, "n0", &child).unwrap();
        state.agent_messages.extend((1..MAX_SENT).map(|index| Sent { key: format!("other/{index}"), origin: active.clone(), chat: child.clone(), run: format!("r{index}") }));
        assert!(send(&mut state, "n1", &child).is_err(), "a full cache of live receipts refuses instead of evicting");
        assert_eq!(send(&mut state, "n0", &child).unwrap()["run_id"], first["run_id"]);
        for sent in state.agent_messages.iter_mut().skip(1) {
            sent.origin = "finished-run".into();
        }
        send(&mut state, "n1", &child).unwrap();
        assert_eq!(send(&mut state, "n0", &child).unwrap()["run_id"], first["run_id"], "stale receipts of finished attempts go first");
    }
}
