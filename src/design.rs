//! Durable design decisions. Asking yields execution; answering queues a fresh
//! attempt from an immutable checkpoint, never grants additional authority.
use crate::runtime::RunKind;
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
/// Decode only the advertised fields; report fixed field errors without echoing
/// potentially private model arguments from a serde error.
pub(crate) fn parse_input(arguments: Value) -> Result<Input, &'static str> {
    #[derive(Default, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Fields {
        question: Value,
        options: Value,
        recommended: Value,
    }
    let fields: Fields = serde_json::from_value(arguments).map_err(|_| {
        "Design question arguments must be an object containing only question, options, and optional recommended."
    })?;
    Ok(Input {
        question: serde_json::from_value(fields.question)
            .map_err(|_| "question must be a string.")?,
        options: serde_json::from_value(fields.options)
            .map_err(|_| "options must be an array of strings.")?,
        recommended: if fields.recommended.is_null() { None } else {
            Some(serde_json::from_value(fields.recommended)
                .map_err(|_| "recommended must be a nonnegative zero-based integer, or omitted.")?)
        },
    })
}
fn validate_input(input: &Input) -> Result<(), &'static str> {
    if input.question.trim().is_empty() || input.question.chars().count() > 1000 {
        return Err("question must contain 1–1000 characters and must not be blank.");
    }
    if !(2..=4).contains(&input.options.len()) {
        return Err("options must contain 2–4 choices.");
    }
    if input.options.iter().any(|option| option.trim().is_empty() || option.chars().count() > 200) {
        return Err("Each option must contain 1–200 characters and must not be blank.");
    }
    if input.options.iter().enumerate().any(|(i, option)| {
        input.options[..i].iter().any(|other| other.trim() == option.trim())
    }) {
        return Err("options must be distinct after trimming whitespace.");
    }
    if input.recommended.is_some_and(|index| index >= input.options.len()) {
        return Err("recommended must be a zero-based index smaller than the number of options.");
    }
    Ok(())
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
    json!({"name":"control_design_ask","description":"Ask the user about a consequential architecture, design, scope or behavior choice: changing scope or the goal, giving up, new access, or anything irreversible. Do not ask which routine next step or diagnostic to run; choose it yourself and report. If you would take your own recommended option, take it instead of asking. Include 2–4 distinct, nonblank options of at most 200 characters each; optionally recommend a zero-based index into those options. This durably pauses this task and releases its worker; Control resumes from a safe checkpoint when the user answers. Correct a field validation error and retry this tool rather than replacing it with an untracked chat question. Do not use for routine sandbox edits, builds or tests. This is not permission escalation. After asking, stop work and await the user's answer.","inputSchema":{"type":"object","properties":{"question":{"type":"string","minLength":1,"maxLength":1000},"options":{"type":"array","minItems":2,"maxItems":4,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":200}},"recommended":{"type":"integer","minimum":0,"maximum":3,"description":"Zero-based index smaller than the number of options."}},"required":["question","options"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
pub fn requested(bridge: Option<(&Arc<Mutex<State>>, &str)>) -> bool {
    bridge.is_some_and(|(state, id)| {
        state.lock().is_ok_and(|s| {
            s.runtime.runs.iter().any(|r| {
                r.id == id
                    && r.status == "running"
                    && (crate::delegation::pausing(r) || r.question.as_ref().is_some_and(|q| {
                        q.answer.is_none()
                            && r.attempts
                                .last()
                                .is_some_and(|a| a.workspace_id.as_deref() == Some(&q.generation))
                    }))
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
    validate_input(&input)?;
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Invalid design request identity; retry the tool call.");
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
    if let Some(q) = &run.question
        && q.generation == generation {
            return if q.nonce == nonce && q.input == input {
                Ok(
                    json!({"question_id":q.id,"status":"pausing","instruction":"Stop work. Control will resume this task when the user answers."}),
                )
            } else {
                Err("This attempt already has a design question.")
            };
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

/// Another chat's decisions surface through recorded delegation edges or a
/// Control-recorded spawn lineage, always for the same actor; a persona root
/// alone grants no access.
pub fn pending<'a>(state: &'a State, chat_id: &str) -> Vec<&'a runtime::Run> {
    let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id) else { return vec![]; };
    let mut visible = std::collections::HashSet::new();
    for run in state.runtime.runs.iter().filter(|run|
        run.chat_id.as_deref() == Some(chat_id) && run.actor == chat.actor
    ) {
        visible.insert(run.id.as_str());
        if let Ok((_, children)) = crate::delegated_results::scope(state, run) {
            visible.extend(children);
        }
    }
    state.runtime.runs.iter().filter(|run|
        (visible.contains(run.id.as_str()) || run.chat_id.as_deref().is_some_and(|chat| crate::chat::within(state, chat, chat_id)))
        && run.actor == chat.actor
        && run.status == "waiting" && run.question.as_ref().is_some_and(|q| q.answer.is_none())
    ).collect()
}

pub fn pending_json(state: &State, chat_id: &str) -> Vec<Value> {
    pending(state, chat_id).into_iter().map(|run| {
        let question = run.question.as_ref().unwrap();
        json!({"run_id":run.id,"chat_id":run.chat_id,"question":question,"summary":summary(question)})
    }).collect()
}

pub fn decision_revision(state: &State) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for run in state.runtime.runs.iter().filter(|run|run.question.is_some()) {
        (&run.id, &run.status, &run.question.as_ref().unwrap().answer).hash(&mut hash);
    }
    hash.finish()
}

pub fn answer_target<'a>(state: &'a State, chat_id: &'a str, question: &str) -> Result<&'a str, &'static str> {
    pending(state, chat_id).into_iter()
        .find(|run|run.question.as_ref().is_some_and(|q|q.id == question))
        .and_then(|run|run.chat_id.as_deref())
        .ok_or("That design question is no longer pending or accessible from this chat.")
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
    let target = question_id.map(|question|answer_target(state, chat_id, question).map(str::to_owned)).transpose()?;
    let chat_id = target.as_deref().unwrap_or(chat_id);
    let run = waiting(state, chat_id)
        .ok_or("No design question is ready for an answer in this chat.")?;
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
    // Capture continuation inputs, not diagnostic histories or previous attempts.
    let actor = run.actor.clone();
    let action = run.action;
    let backend = run.backend;
    let persona = crate::delegation::lineage(run);
    let model = run.model.clone();
    let context = run.context.clone();
    let lenses = run.lenses.clone();
    let queue_order = run.queue_order;
    let resources = run.resource_grants.clone();
    let objective = run.continuation.as_ref().map(|c|c.objective.clone()).unwrap_or_else(|| run.objective.to_string());
    let continuation = Continuation {
        project_base: attempt.project_base.clone(), checkpoint,
        published_parent: baseline.clone(), objective, decision: question.input.clone(),
    };
    // Replacing the waiting run with its continuation keeps admission bounded.
    state.runtime.runs[index].status = "continued".into();
    let notice = state.runtime.notice.clone();
    if let Err(error) = state.runtime.enqueue_for(
        answer.clone(),
        RunKind::DesignAnswer,
        source,
        actor,
        action,
    ) {
        state.runtime.runs[index].status = "waiting".into();
        state.runtime.notice = notice;
        return Err(error);
    }
    let next = state.runtime.runs.last_mut().unwrap();
    next.backend = backend;
    next.resource_grants = resources;
    next.persona = Some(persona);
    next.model = model;
    next.context = context;
    next.lenses = lenses;
    next.chat_id = Some(chat_id.into());
    next.queue_order = queue_order;
    *next.continuation = Some(continuation);
    let next_id = next.id.clone();
    let question = state.runtime.runs[index].question.as_mut().unwrap();
    question.answer = Some(answer.clone());
    question.continuation = Some(next_id.clone());
    *state.runtime.runs[index].result =
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
pub(crate) mod tests;
