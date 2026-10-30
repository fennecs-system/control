//! Durable design decisions. Asking yields execution; answering queues a fresh
//! attempt from an immutable checkpoint, never grants additional authority.
use crate::{chat, identity, runtime, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub recommended: Option<usize>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub generation: String,
    pub nonce: String,
    pub input: Input,
    pub answer: Option<String>,
    pub continuation: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Continuation {
    #[serde(default)]
    pub project_base: Option<String>,
    pub checkpoint: String,
    pub published_parent: Option<String>,
    pub objective: String,
    pub decision: Input,
}
pub fn definition() -> Value {
    json!({"name":"control_design_ask","description":"Ask the user about a consequential architecture, design, scope or behavior choice. Include 2–4 concrete options and optionally a recommended zero-based option index. This durably pauses this task and releases its worker; Control resumes from a safe checkpoint when the user answers. Do not use for routine sandbox edits, builds or tests. This is not permission escalation. After asking, stop work and await the user's answer.","inputSchema":{"type":"object","properties":{"question":{"type":"string","minLength":1,"maxLength":1000},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"string","minLength":1,"maxLength":200}},"recommended":{"type":"integer","minimum":0,"maximum":3}},"required":["question","options"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
pub fn requested(bridge: Option<(&Arc<Mutex<State>>, &str)>) -> bool {
    bridge.is_some_and(|(state, id)| {
        state.lock().is_ok_and(|s| {
            s.runtime.runs.iter().any(|r| {
                r.id == id
                    && r.status == "running"
                    && r.question.as_ref().is_some_and(|q| {
                        q.answer.is_none()
                            && r.attempts
                                .last()
                                .is_some_and(|a| a.workspace_id.as_deref() == Some(&q.generation))
                    })
            })
        })
    })
}
pub fn ask(state: &mut State, id: &str, nonce: &str, input: Input) -> Result<Value, &'static str> {
    ask_with_save(state, id, nonce, input, crate::storage::save)
}
fn ask_with_save(
    state: &mut State,
    id: &str,
    nonce: &str,
    input: Input,
    save: impl FnOnce(&State) -> std::io::Result<()>,
) -> Result<Value, &'static str> {
    if input.question.trim().is_empty()
        || input.question.chars().count() > 1000
        || !(2..=4).contains(&input.options.len())
        || input
            .options
            .iter()
            .any(|s| s.trim().is_empty() || s.chars().count() > 200)
        || input.options.iter().enumerate().any(|(i, s)| {
            input.options[..i]
                .iter()
                .any(|other| other.trim() == s.trim())
        })
        || input.recommended.is_some_and(|i| i >= input.options.len())
        || nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Expected a question, 2–4 distinct options, a valid recommendation and request identity.");
    }
    let index = state
        .runtime
        .runs
        .iter()
        .position(|r| r.id == id && r.status == "running")
        .ok_or("Run is not active.")?;
    let run = &state.runtime.runs[index];
    identity::authorize(&run.actor, run.action)?;
    let chat_id = run
        .chat_id
        .as_deref()
        .ok_or("Design questions require a conversation.")?;
    let chat_index = state
        .chats
        .iter()
        .position(|s| s.id == chat_id && s.actor == run.actor)
        .ok_or("Conversation actor mismatch.")?;
    let generation = run
        .attempts
        .last()
        .and_then(|a| a.workspace_id.clone())
        .ok_or("No active attempt identity.")?;
    if let Some(q) = &run.question {
        if q.generation == generation {
            return if q.nonce == nonce && q.input == input {
                Ok(
                    json!({"question_id":q.id,"status":"pausing","instruction":"Stop work. Control will resume this task when the user answers."}),
                )
            } else {
                Err("This attempt already has a design question.")
            };
        }
    }
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{generation}:{nonce}").as_bytes(),
    );
    let id = format!(
        "q-{}",
        digest
            .as_ref()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let previous = state.runtime.runs[index].question.clone();
    let revision = state.chats[chat_index].revision;
    state.runtime.runs[index].question = Some(Question {
        id: id.clone(),
        generation,
        nonce: nonce.into(),
        input,
        answer: None,
        continuation: None,
    });
    state.chats[chat_index].revision = revision.saturating_add(1);
    if save(state).is_err() {
        state.runtime.runs[index].question = previous;
        state.chats[chat_index].revision = revision;
        return Err("Could not persist the design question; no pause was acknowledged.");
    }
    Ok(
        json!({"question_id":id,"status":"pausing","instruction":"Stop work. Control will resume this task when the user answers."}),
    )
}
pub fn waiting<'a>(state: &'a State, chat_id: &str) -> Option<&'a runtime::Run> {
    state.runtime.runs.iter().find(|r| {
        r.chat_id.as_deref() == Some(chat_id)
            && r.status == "waiting"
            && r.question.as_ref().is_some_and(|q| q.answer.is_none())
    })
}
pub fn summary(question: &Question) -> String {
    let mut text = format!("{}\n", question.input.question);
    for (i, option) in question.input.options.iter().enumerate() {
        text.push_str(&format!(
            "\n{}. {}{}",
            i + 1,
            option,
            if question.input.recommended == Some(i) {
                " (recommended)"
            } else {
                ""
            }
        ));
    }
    if let Some(answer) = &question.answer {
        text.push_str(&format!("\n\nDecision: {answer}"));
    } else {
        text.push_str(&format!("\n\nReply with a number or your own direction. For this question: /answer {} YOUR_ANSWER",question.id));
    }
    text
}
pub fn answer(
    state: &mut State,
    chat_id: &str,
    question_id: Option<&str>,
    text: &str,
    source: &str,
) -> Result<(), &'static str> {
    let run = waiting(state, chat_id)
        .ok_or("No design question is ready for an answer in this chat.")?
        .clone();
    let question = run.question.as_ref().unwrap();
    if question_id.is_some_and(|id| id != question.id) {
        return Err("That design question is no longer pending.");
    }
    let session_index = state
        .chats
        .iter()
        .position(|s| s.id == chat_id && s.actor == run.actor)
        .ok_or("Conversation actor mismatch.")?;
    identity::authorize(&run.actor, run.action)?;
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 2000 {
        return Err("Enter an answer of 1–2000 characters.");
    }
    let answer = match text.parse::<usize>() {
        Ok(n) => question
            .input
            .options
            .get(n.wrapping_sub(1))
            .ok_or("Choose a listed option or write your own direction.")?
            .clone(),
        Err(_) => text.into(),
    };
    let attempt = run
        .attempts
        .last()
        .filter(|a| {
            a.workspace_id.as_deref() == Some(&question.generation) && a.status == "waiting"
        })
        .ok_or("Paused attempt is unavailable.")?;
    let checkpoint = attempt
        .project_checkpoint.as_ref().or(attempt.artifact.as_ref())
        .cloned()
        .ok_or("The paused workspace has no verified checkpoint.")?;
    if attempt.project.is_some() && state.chats[session_index].project.as_ref().and_then(|p|p.head.as_ref())!=attempt.project_base.as_ref() {
        return Err("The project changed; cancel this question and start a new turn.");
    }
    let current = if run.action == identity::Action::Improve {
        &state.chats[session_index].improve_workspace
    } else {
        &state.chats[session_index].workspace
    };
    let baseline = run
        .continuation
        .as_ref()
        .map(|c| &c.published_parent)
        .unwrap_or(&attempt.parent_artifact);
    if current != baseline {
        return Err("The conversation workspace changed; cancel this question before continuing.");
    }
    let index = state
        .runtime
        .runs
        .iter()
        .position(|r| r.id == run.id)
        .unwrap();
    // Replacing the waiting run with its continuation keeps admission bounded.
    state.runtime.runs[index].status = "continued".into();
    let notice = state.runtime.notice.clone();
    if let Err(error) = state.runtime.enqueue_for(
        answer.clone(),
        "design-answer",
        source,
        run.actor.clone(),
        run.action,
    ) {
        state.runtime.runs[index].status = "waiting".into();
        state.runtime.notice = notice;
        return Err(error);
    }
    let next = state.runtime.runs.last_mut().unwrap();
    next.backend = run.backend;
    next.model = run.model;
    next.context = run.context;
    next.lenses = run.lenses;
    next.chat_id = Some(chat_id.into());
    next.queue_order = run.queue_order;
    let objective = run
        .continuation
        .as_ref()
        .map(|c| c.objective.clone())
        .unwrap_or(run.objective);
    next.continuation = Some(Continuation {
        project_base: attempt.project_base.clone(),
        checkpoint,
        published_parent: baseline.clone(),
        objective,
        decision: question.input.clone(),
    });
    let next_id = next.id.clone();
    let question = state.runtime.runs[index].question.as_mut().unwrap();
    question.answer = Some(answer.clone());
    question.continuation = Some(next_id.clone());
    state.runtime.runs[index].result =
        summary(state.runtime.runs[index].question.as_ref().unwrap());
    let session = &mut state.chats[session_index];
    session.messages.push(chat::Message {
        text: answer,
        run_id: Some(next_id),
        reply: String::new(),
    });
    session.revision = session.revision.saturating_add(1);
    Ok(())
}

pub fn discord_chat(state: &State, incoming: &crate::discord::Incoming) -> String {
    let actor = incoming.actor();
    let question_id = incoming
        .objective
        .trim()
        .strip_prefix("/answer ")
        .and_then(|s| s.split_whitespace().next());
    question_id
        .and_then(|id| {
            state
                .runtime
                .runs
                .iter()
                .find(|r| {
                    r.actor == actor
                        && r.question
                            .as_ref()
                            .is_some_and(|q| q.id == id && q.answer.is_none())
                })
                .and_then(|r| r.chat_id.clone())
        })
        .unwrap_or_else(|| crate::discord::chat_id(incoming.user_id, incoming.channel_id))
}

pub fn render(question: &Question, chat_id: &str, ready: bool) -> String {
    let mut html = format!(
        "<div class='design-question'><p class='preserve'>{}</p>",
        crate::escape(&question.input.question)
    );
    if ready {
        for (i, option) in question.input.options.iter().enumerate() {
            html.push_str(&format!("<form method='post' action='/chat/{chat_id}/messages' target='_top'><button name='message' value='/answer {} {}'>{}. {}{}</button></form>",crate::escape(&question.id),i+1,i+1,crate::escape(option),if question.input.recommended == Some(i) {" · recommended"} else {""}));
        }
        html.push_str("<small>Or reply with your own direction.</small>");
    } else if let Some(answer) = &question.answer {
        html.push_str(&format!(
            "<p class='muted preserve'>Decision: {}</p>",
            crate::escape(answer)
        ));
    } else {
        html.push_str("<small>Saving the paused workspace…</small>");
    }
    html.push_str("</div>");
    html
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn fixture() -> (State, String) {
        let mut state = State::default();
        let chat = chat::create(&mut state);
        state.chats[0].workspace = Some("last-good".into());
        chat::send(&mut state, &chat, "Build the parser", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        let mut attempt = crate::usage::Attempt::new();
        attempt.workspace_id = Some("generation-1".into());
        attempt.parent_artifact = Some("last-good".into());
        run.attempts.push(attempt);
        (state, chat)
    }
    pub fn input() -> Input {
        Input {
            question: "Which format? <script>".into(),
            options: vec!["Plain text".into(), "JSON".into()],
            recommended: Some(1),
        }
    }
    pub fn paused() -> (State, String) {
        let (mut state, chat) = fixture();
        let id = state.runtime.runs[0].id.clone();
        ask_with_save(&mut state, &id, "nonce-1", input(), |_| Ok(())).unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "waiting".into();
        run.attempts[0].status = "waiting".into();
        run.attempts[0].artifact = Some("paused-workspace".into());
        run.result = summary(run.question.as_ref().unwrap());
        (state, chat)
    }
    #[test]
    fn question_requires_valid_active_identity_and_durable_admission() {
        let (mut state, chat) = fixture();
        let run = state.runtime.runs[0].id.clone();
        let before = serde_json::to_vec(&state).unwrap();
        assert!(ask_with_save(&mut state, &run, "nonce", input(), |_| Err(
            std::io::Error::other("disk full")
        ))
        .is_err());
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        let first = ask_with_save(&mut state, &run, "nonce", input(), |_| Ok(())).unwrap();
        let again = ask_with_save(&mut state, &run, "nonce", input(), |_| {
            panic!("replay must not write")
        })
        .unwrap();
        assert_eq!(first, again);
        assert!(ask_with_save(&mut state, &run, "other", input(), |_| Ok(())).is_err());
        let html = chat::transcript(&state, &chat).unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("Saving the paused workspace"));
        assert!(!html.contains("value='/answer"));
        let mut invalid = input();
        invalid.recommended = Some(4);
        assert!(ask_with_save(&mut state, &run, "nonce", invalid, |_| Ok(())).is_err());
        state.runtime.runs[0].actor = identity::Actor::Unknown;
        assert!(ask_with_save(&mut state, &run, "nonce", input(), |_| Ok(())).is_err());
    }
    #[test]
    fn pause_survives_reload_and_answer_keeps_authority_and_workspace_baseline() {
        let (mut state, chat) = paused();
        chat::send(&mut state, &chat, "/steer Add tests", "test").unwrap();
        assert_eq!(crate::scheduler::next(&state.runtime), None);
        let other = chat::create(&mut state);
        chat::send(&mut state, &other, "Unrelated work", "test").unwrap();
        assert_eq!(crate::scheduler::next(&state.runtime), Some(2));
        let mut state: State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(waiting(&state, &chat).is_some());
        let question = state.runtime.runs[0].question.as_ref().unwrap().id.clone();
        assert!(answer(&mut state, &other, Some(&question), "1", "test").is_err());
        assert!(answer(&mut state, &chat, Some("q-stale"), "1", "test").is_err());
        assert!(answer(&mut state, &chat, Some(&question), "9", "test").is_err());
        let html = chat::transcript(&state, &chat).unwrap();
        assert!(
            html.contains("Which format? &lt;script&gt;") && html.contains("JSON · recommended")
        );
        chat::send(&mut state, &chat, "2", "test").unwrap();
        assert_eq!(state.runtime.runs[0].status, "continued");
        assert_eq!(state.chats[0].workspace.as_deref(), Some("last-good"));
        let next = state.runtime.runs.last().unwrap();
        assert_eq!(next.objective, "JSON");
        assert_eq!(next.actor, state.runtime.runs[0].actor);
        assert_eq!(next.action, state.runtime.runs[0].action);
        assert_eq!(next.backend, state.runtime.runs[0].backend);
        assert_eq!(next.model, state.runtime.runs[0].model);
        assert_eq!(
            next.continuation.as_ref().unwrap().checkpoint,
            "paused-workspace"
        );
        assert_eq!(
            next.continuation
                .as_ref()
                .unwrap()
                .published_parent
                .as_deref(),
            Some("last-good")
        );
        assert_eq!(crate::scheduler::next(&state.runtime), Some(3));
        assert!(chat::prompt(&state, &next.id)
            .unwrap()
            .contains("Original task:\nBuild the parser"));
        assert!(answer(&mut state, &chat, Some(&question), "1", "test").is_err());
    }
    #[test]
    fn own_direction_is_not_permission_escalation_and_cancel_removes_wait() {
        let (mut state, chat) = paused();
        chat::send(
            &mut state,
            &chat,
            "Use a compact binary format instead",
            "test",
        )
        .unwrap();
        assert_eq!(
            state.runtime.runs.last().unwrap().action,
            identity::Action::Task
        );
        assert_eq!(
            state.runtime.runs[0]
                .question
                .as_ref()
                .unwrap()
                .answer
                .as_deref(),
            Some("Use a compact binary format instead")
        );
        let (mut state, chat) = paused();
        chat::send(&mut state, &chat, "/cancel", "test").unwrap();
        assert_eq!(state.runtime.runs[0].status, "cancelled");
        assert!(waiting(&state, &chat).is_none());
        assert!(answer(&mut state, &chat, None, "1", "test").is_err());
        let (mut state, chat) = paused();
        state.chats[0].workspace = Some("changed".into());
        assert!(answer(&mut state, &chat, None, "1", "test").is_err());
    }
    #[test]
    fn discord_question_routing_is_bound_to_exact_user_and_dm_channel() {
        let (mut state, chat) = paused();
        let question = state.runtime.runs[0].question.as_ref().unwrap().id.clone();
        let mut message = crate::discord::Incoming {
            message_id: "303".parse().unwrap(),
            user_id: "101".parse().unwrap(),
            channel_id: "202".parse().unwrap(),
            objective: format!("/answer {question} 2"),
            action: identity::Action::Task,
            direct: true,
        };
        state.runtime.runs[0].actor = message.actor();
        state.chats[0].actor = message.actor();
        assert_eq!(discord_chat(&state, &message), chat);
        message.user_id = "404".parse().unwrap();
        assert_eq!(discord_chat(&state, &message), "d-404-202");
        message.user_id = "101".parse().unwrap();
        message.channel_id = "505".parse().unwrap();
        assert_eq!(discord_chat(&state, &message), "d-101-505");
        message.channel_id = "202".parse().unwrap();
        message.direct = false;
        assert_eq!(discord_chat(&state, &message), "d-101-202");
    }
    #[test]
    fn multiple_decisions_preserve_original_objective_and_publication_baseline() {
        let (mut state, chat) = paused();
        answer(&mut state, &chat, None, "1", "test").unwrap();
        let run = &mut state.runtime.runs[1];
        run.status = "running".into();
        let mut attempt = crate::usage::Attempt::new();
        attempt.workspace_id = Some("generation-2".into());
        attempt.parent_artifact = Some("paused-workspace".into());
        run.attempts.push(attempt);
        let id = run.id.clone();
        ask_with_save(&mut state, &id, "nonce-2", input(), |_| Ok(())).unwrap();
        state.runtime.runs[1].status = "waiting".into();
        state.runtime.runs[1].attempts[0].status = "waiting".into();
        state.runtime.runs[1].attempts[0].artifact = Some("second-pause".into());
        answer(&mut state, &chat, None, "2", "test").unwrap();
        let continuation = state.runtime.runs[2].continuation.as_ref().unwrap();
        assert_eq!(continuation.objective, "Build the parser");
        assert_eq!(continuation.checkpoint, "second-pause");
        assert_eq!(continuation.published_parent.as_deref(), Some("last-good"));
    }
    #[test]
    fn answering_at_capacity_replaces_wait_instead_of_exceeding_admission() {
        let (mut state, chat) = paused();
        for _ in 1..crate::scheduler::MAX_PENDING {
            state.runtime.enqueue("Other task".into(), "task", "test");
        }
        assert!(crate::scheduler::admit(&state.runtime).is_err());
        answer(&mut state, &chat, None, "1", "test").unwrap();
        assert!(crate::scheduler::admit(&state.runtime).is_err());
        assert_eq!(
            state
                .runtime
                .runs
                .iter()
                .filter(|r| ["queued", "running", "waiting"].contains(&r.status.as_str()))
                .count(),
            crate::scheduler::MAX_PENDING
        );
    }
}
