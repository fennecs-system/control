//! Durable design decisions. Asking yields execution; answering queues a fresh
//! attempt from an immutable checkpoint, never grants additional authority.
use crate::runtime::RunKind;
use crate::runtime::RunStatus;
use crate::{State, chat, identity, runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
    let fields: Fields = serde_json::from_value(arguments).map_err(
        |_| "Design question arguments must be an object containing only question, options, and optional recommended.",
    )?;
    Ok(Input {
        question: serde_json::from_value(fields.question).map_err(|_| "question must be a string.")?,
        options: serde_json::from_value(fields.options).map_err(|_| "options must be an array of strings.")?,
        recommended: if fields.recommended.is_null() {
            None
        } else {
            Some(
                serde_json::from_value(fields.recommended)
                    .map_err(|_| "recommended must be a nonnegative zero-based integer, or omitted.")?,
            )
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
    if input
        .options
        .iter()
        .any(|option| option.trim().is_empty() || option.chars().count() > 200)
    {
        return Err("Each option must contain 1–200 characters and must not be blank.");
    }
    if input
        .options
        .iter()
        .enumerate()
        .any(|(i, option)| input.options[..i].iter().any(|other| other.trim() == option.trim()))
    {
        return Err("options must be distinct after trimming whitespace.");
    }
    if input.recommended.is_some_and(|index| index >= input.options.len()) {
        return Err("recommended must be a zero-based index smaller than the number of options.");
    }
    Ok(())
}
/// A question's identity, `q-` and 32 hex digits, derived from its attempt and nonce.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuestionId(String);
impl QuestionId {
    fn derive(generation: &str, nonce: &str) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, format!("{generation}:{nonce}").as_bytes());
        Self(format!(
            "q-{}",
            digest
                .as_ref()
                .iter()
                .take(16)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Fixtures name their questions.
#[cfg(test)]
impl<T: Into<String>> From<T> for QuestionId {
    fn from(id: T) -> Self {
        Self(id.into())
    }
}
impl PartialEq<str> for QuestionId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}
impl std::fmt::Display for QuestionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
/// Seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixSeconds(pub u64);
impl UnixSeconds {
    pub fn now() -> Self {
        Self(control_config::unix_seconds())
    }
}
/// Who asked. Derived from the run that holds the question, so records written
/// before withdrawal existed have one too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asker {
    /// An agent's attempt in this chat.
    Agent { chat: String },
    /// The chat MCP caller (`control_design_ask` with no run).
    ChatMcp,
}
impl Asker {
    pub fn of(run: &runtime::Run) -> Self {
        if asked(run) {
            Self::ChatMcp
        } else {
            Self::Agent {
                chat: run.chat_id.clone().unwrap_or_default(),
            }
        }
    }
}
/// Who withdrew a question, as history keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Withdrawer {
    Asker,
    Operator,
}
/// Proof that the operator let this caller decide design questions for them;
/// made only where that permission is read.
pub struct OperatorGrant(());
impl OperatorGrant {
    pub(crate) fn granted() -> Self {
        Self(())
    }
}
/// Permission to withdraw one question. Only [`Authority::check`] makes one.
#[derive(Debug)]
pub struct Authority {
    by: Withdrawer,
    question: QuestionId,
}
impl Authority {
    /// The asker withdraws its own question; anyone else needs the operator's grant.
    pub fn check(
        question: &Question,
        asker: &Asker,
        caller: &Asker,
        operator: Option<OperatorGrant>,
    ) -> Result<Self, Refusal> {
        let by = match (asker == caller, operator) {
            (true, _) => Withdrawer::Asker,
            (false, Some(OperatorGrant(()))) => Withdrawer::Operator,
            (false, None) => return Err(Refusal::NotAsker),
        };
        Ok(Self {
            by,
            question: question.id.clone(),
        })
    }
}
/// A short reason for withdrawing: 1–200 characters after trimming.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Reason(String);
impl TryFrom<&str> for Reason {
    type Error = &'static str;
    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > 200 {
            return Err("reason must contain 1–200 characters, or be omitted.");
        }
        Ok(Self(text.into()))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Withdrawal {
    pub by: Withdrawer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    pub at: UnixSeconds,
}
/// Where a question is in its life: pending until answered or withdrawn.
/// Stored flattened into the question, so earlier records still load: `answer`
/// is a string once answered and null or absent while pending; `withdrawn`
/// and `asked_at` are absent from records written before they existed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Status {
    Answered {
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_at: Option<UnixSeconds>,
    },
    Withdrawn {
        withdrawn: Withdrawal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_at: Option<UnixSeconds>,
    },
    Pending {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_at: Option<UnixSeconds>,
    },
}
/// Why a question transition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotPending,
    NotAsker,
}
impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Refusal::NotPending => "That design question is no longer pending.",
            Refusal::NotAsker => {
                "Only the party that asked a question, or the operator, may withdraw it. The operator can also /cancel the waiting run."
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Question {
    pub id: QuestionId,
    pub generation: String,
    pub nonce: String,
    pub input: Input,
    #[serde(flatten)]
    pub status: Status,
    pub continuation: Option<String>,
}
impl Question {
    fn ask(generation: String, nonce: &str, input: Input, at: UnixSeconds) -> Self {
        Self {
            id: QuestionId::derive(&generation, nonce),
            generation,
            nonce: nonce.into(),
            input,
            status: Status::Pending { asked_at: Some(at) },
            continuation: None,
        }
    }
    pub fn pending(&self) -> bool {
        matches!(self.status, Status::Pending { .. })
    }
    pub fn answer(&self) -> Option<&str> {
        match &self.status {
            Status::Answered { answer, .. } => Some(answer),
            Status::Withdrawn { .. } | Status::Pending { .. } => None,
        }
    }
    pub fn asked_at(&self) -> Option<UnixSeconds> {
        match self.status {
            Status::Answered { asked_at, .. } | Status::Withdrawn { asked_at, .. } | Status::Pending { asked_at } => {
                asked_at
            }
        }
    }
    /// Pending → answered.
    fn answered(self, answer: String) -> Result<Self, Refusal> {
        match self.status {
            Status::Pending { asked_at } => Ok(Self {
                status: Status::Answered { answer, asked_at },
                ..self
            }),
            Status::Answered { .. } | Status::Withdrawn { .. } => Err(Refusal::NotPending),
        }
    }
    /// Pending → withdrawn, by the holder of `authority` for this question.
    pub fn withdraw(self, authority: Authority, reason: Option<Reason>, at: UnixSeconds) -> Result<Self, Refusal> {
        if authority.question != self.id {
            return Err(Refusal::NotAsker);
        }
        match self.status {
            Status::Pending { asked_at } => Ok(Self {
                status: Status::Withdrawn {
                    withdrawn: Withdrawal {
                        by: authority.by,
                        reason,
                        at,
                    },
                    asked_at,
                },
                ..self
            }),
            Status::Answered { .. } | Status::Withdrawn { .. } => Err(Refusal::NotPending),
        }
    }
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
            s.runtime.runs.get(id).is_some_and(|r| {
                r.status == RunStatus::Running
                    && (crate::delegation::pausing(r)
                        || r.question.as_ref().is_some_and(|q| {
                            q.pending()
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
    if nonce.is_empty() || nonce.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("Invalid design request identity; retry the tool call.");
    }
    let index = state
        .runtime
        .runs
        .position(id)
        .filter(|&index| state.runtime.runs[index].status == RunStatus::Running)
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
        && q.generation == generation
    {
        return if q.nonce == nonce && q.input == input {
            Ok(
                json!({"question_id":q.id,"status":"pausing","instruction":"Stop work. Control will resume this task when the user answers."}),
            )
        } else {
            Err("This attempt already has a design question.")
        };
    }
    let question = Question::ask(generation, nonce, input, UnixSeconds::now());
    let id = question.id.clone();
    let previous = state.runtime.runs[index].question.clone();
    let revision = state.chats[chat_index].revision;
    state.runtime.runs[index].question = Some(question);
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
/// Source of a question the operator's chat MCP caller raised (`control_design_ask` with no run).
/// No attempt waits on it: answering only records the answer.
pub const ASKED: &str = "chat-ask";
/// The operator-desk DM that holds operator-side questions raised without a chat.
pub const DESK: &str = "Decisions";
/// Unanswered operator-side questions at once; each also counts toward admission.
const MAX_ASKED: usize = 8;
pub fn asked(run: &runtime::Run) -> bool {
    run.source == ASKED && run.attempts.is_empty()
}
/// The operator's root DM named [`DESK`], if one exists.
pub fn desk(state: &State) -> Option<&str> {
    state
        .chats
        .iter()
        .find(|c| {
            c.parent_chat.is_none()
                && c.actor == identity::Actor::LocalOperator
                && !c.improve_mode
                && c.name.as_deref() == Some(DESK)
        })
        .map(|c| c.id.as_str())
}
/// Records a question for the operator from the chat MCP caller, in `chat` or
/// else the desk DM (created once). It is an ordinary waiting design question,
/// so every request list and notification shows it, but no run resumes from it
/// and it approves nothing. A named chat must be an idle operator chat, so the
/// question never pauses or queues behind a turn.
pub fn ask_operator(
    state: &mut State,
    chat: Option<&str>,
    input: Input,
    at: UnixSeconds,
) -> Result<Value, &'static str> {
    validate_input(&input)?;
    identity::authorize(&identity::Actor::LocalOperator, identity::Action::Task)?;
    if state
        .runtime
        .runs
        .iter()
        .filter(|r| asked(r) && r.status == RunStatus::Waiting)
        .count()
        >= MAX_ASKED
    {
        return Err("8 questions already wait for the operator; wait for their answers before asking more.");
    }
    crate::scheduler::admit(&state.runtime)?;
    let chat_id = match chat {
        Some(id) => {
            let session = state.chats.iter().find(|c| c.id == id).ok_or("Chat not found.")?;
            if session.actor != identity::Actor::LocalOperator || session.improve_mode {
                return Err("Ask only in the operator's own chat-mode DMs.");
            }
            if state
                .runtime
                .runs
                .iter()
                .any(|r| r.chat_id.as_deref() == Some(id) && r.status.is_active())
            {
                return Err("That chat has a turn in progress or waiting; ask without id to use the Decisions DM.");
            }
            id.to_owned()
        }
        None => match desk(state) {
            Some(id) => id.to_owned(),
            None => {
                let id = chat::create(state);
                chat::rename(state, &id, DESK)?;
                id
            }
        },
    };
    let stamp = crate::ids::stamp();
    let run_id = format!("{stamp:x}-{}", state.runtime.runs.len());
    let order = state
        .runtime
        .runs
        .iter()
        .map(|r| r.queue_order)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut run = crate::application::runs::Submission {
        objective: input.question.clone(),
        kind: RunKind::AgentMessage,
        source: ASKED.into(),
        actor: identity::Actor::LocalOperator,
        action: identity::Action::Task,
    }
    .into_run(
        run_id.clone(),
        order,
        crate::providers::Target {
            model: (state.runtime.backend == crate::backend::Backend::Local).then(|| state.runtime.configured_model()),
            ..state.runtime.default_target()
        },
    );
    let question = Question::ask(run_id.clone(), ASKED, input, at);
    let id = question.id.clone();
    run.status = RunStatus::Waiting;
    run.chat_id = Some(chat_id.clone());
    *run.result = summary(&question);
    run.question = Some(question);
    state.runtime.runs.push(run);
    chat::touch(state, &chat_id);
    let session = state.chats.iter_mut().find(|c| c.id == chat_id).unwrap();
    session.messages.push(chat::Message::notice(
        "Decision requested by the chat MCP caller",
        Some(run_id),
    ));
    session.revision = session.revision.saturating_add(1);
    Ok(json!({"question_id":id,"chat_id":chat_id}))
}
/// `control.chat.ask` on the operator endpoint: `control_design_ask` for a caller with no run.
pub fn tool(state: &mut State, name: &str, arguments: &Value) -> Result<Value, &'static str> {
    match name {
        "control.chat.ask" => {
            let mut fields = arguments.as_object().cloned().ok_or("Arguments must be an object.")?;
            let chat = match fields.remove("id") {
                None | Some(Value::Null) => None,
                Some(Value::String(id)) if chat::valid_id(&id) => Some(id),
                _ => return Err("Invalid chat ID."),
            };
            ask_operator(
                state,
                chat.as_deref(),
                parse_input(Value::Object(fields))?,
                UnixSeconds::now(),
            )
        }
        "control.chat.withdraw" => {
            let question = arguments["question_id"]
                .as_str()
                .ok_or("Specify the question_id control_design_ask returned.")?;
            let reason = match &arguments["reason"] {
                Value::Null => None,
                Value::String(reason) => Some(Reason::try_from(reason.as_str())?),
                _ => return Err("reason must be text."),
            };
            // The chat MCP caller asked its own questions; others it may withdraw
            // only for an operator who lets it decide questions.
            let operator = match &arguments["as"] {
                Value::Null => None,
                Value::String(role) if role == "operator" => Some(OperatorGrant::granted()),
                _ => return Err("as must be operator, or omitted."),
            };
            withdraw(state, question, &Asker::ChatMcp, operator, reason, UnixSeconds::now())
        }
        _ => Err("Unknown tool."),
    }
}
/// The chat holding a question, for checkpointing a withdrawal.
pub fn question_chat<'a>(state: &'a State, question: &str) -> Option<&'a str> {
    state
        .runtime
        .runs
        .iter()
        .find(|r| r.question.as_ref().is_some_and(|q| q.id == *question))
        .and_then(|r| r.chat_id.as_deref())
}
/// Withdraws a waiting question for `caller`. The run holding it ends the way
/// `/cancel` ends a waiting run: no continuation is queued, so an agent never
/// acts on a consequential choice nobody made, and the published workspace
/// stays as it was. An operator-side question just closes.
pub fn withdraw(
    state: &mut State,
    question: &str,
    caller: &Asker,
    operator: Option<OperatorGrant>,
    reason: Option<Reason>,
    at: UnixSeconds,
) -> Result<Value, &'static str> {
    let index = state
        .runtime
        .runs
        .iter()
        .position(|r| {
            r.status == RunStatus::Waiting && r.question.as_ref().is_some_and(|q| q.id == *question && q.pending())
        })
        .ok_or(Refusal::NotPending.message())?;
    let run = &state.runtime.runs[index];
    let held = run.question.clone().ok_or(Refusal::NotPending.message())?;
    let authority = Authority::check(&held, &Asker::of(run), caller, operator).map_err(Refusal::message)?;
    let withdrawn = held.withdraw(authority, reason, at).map_err(Refusal::message)?;
    let (run_id, chat_id) = (run.id().to_string(), run.chat_id.clone());
    crate::lifecycle::request_cancel(&mut state.runtime, &run_id, &identity::Actor::LocalOperator)?;
    let note = outcome(&withdrawn).unwrap_or_default();
    let run = &mut state.runtime.runs[index];
    *run.result = summary(&withdrawn);
    run.question = Some(withdrawn);
    if let Some(chat_id) = chat_id {
        chat::touch(state, &chat_id);
        if let Some(session) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            session
                .messages
                .push(chat::Message::notice(format!("{question}: {note}"), Some(run_id)));
            session.revision = session.revision.saturating_add(1);
        }
    }
    Ok(json!({"question_id":question,"status":"withdrawn"}))
}

/// The unanswered question waiting in `chat_id`: `question_id` when given,
/// else the oldest. An agent's question holds its chat alone; the desk DM
/// may hold several.
fn waiting_for<'a>(state: &'a State, chat_id: &str, question_id: Option<&str>) -> Option<&'a runtime::Run> {
    state.runtime.runs.iter().find(|r| {
        r.chat_id.as_deref() == Some(chat_id)
            && r.status == RunStatus::Waiting
            && r.question
                .as_ref()
                .is_some_and(|q| q.pending() && question_id.is_none_or(|id| q.id == *id))
    })
}
pub fn waiting<'a>(state: &'a State, chat_id: &str) -> Option<&'a runtime::Run> {
    waiting_for(state, chat_id, None)
}

/// Another chat's decisions surface through recorded delegation edges or a
/// Control-recorded spawn lineage, always for the same actor; a persona root
/// alone grants no access.
pub fn pending<'a>(state: &'a State, chat_id: &str) -> Vec<&'a runtime::Run> {
    let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id) else {
        return vec![];
    };
    let mut visible = std::collections::HashSet::new();
    for run in state
        .runtime
        .runs
        .iter()
        .filter(|run| run.chat_id.as_deref() == Some(chat_id) && run.actor == chat.actor)
    {
        visible.insert(run.id().as_str());
        if let Ok((_, children)) = crate::delegated_results::scope(state, run) {
            visible.extend(children);
        }
    }
    state
        .runtime
        .runs
        .iter()
        .filter(|run| {
            (visible.contains(run.id().as_str())
                || run
                    .chat_id
                    .as_deref()
                    .is_some_and(|chat| crate::chat::Lineage::of(&state.chats).within(chat, chat_id)))
                && run.actor == chat.actor
                && run.status == RunStatus::Waiting
                && run.question.as_ref().is_some_and(Question::pending)
        })
        .collect()
}

/// Pending questions as request views list them, each with its age in seconds
/// at `now` when its ask time is known.
pub fn pending_json(state: &State, chat_id: &str, now: UnixSeconds) -> Vec<Value> {
    pending(state, chat_id)
        .into_iter()
        .map(|run| {
            let question = run.question.as_ref().unwrap();
            json!({"run_id":run.id(),"chat_id":run.chat_id,"question":question,"summary":summary(question),
                "age":question.asked_at().map(|at| now.0.saturating_sub(at.0))})
        })
        .collect()
}

/// Changes when a question or a knowledge proposal is asked or decided, so
/// request views (TUI, Discord forwarding) re-read.
pub fn decision_revision(state: &State) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for run in state.runtime.runs.iter().filter(|run| run.question.is_some()) {
        (&run.id(), &run.status, &run.question.as_ref().unwrap().status).hash(&mut hash);
    }
    state.collective.proposals.iter().for_each(|p| p.status.hash(&mut hash));
    hash.finish()
}

pub fn answer_target<'a>(state: &'a State, chat_id: &'a str, question: &str) -> Result<&'a str, &'static str> {
    pending(state, chat_id)
        .into_iter()
        .find(|run| run.question.as_ref().is_some_and(|q| q.id == *question))
        .and_then(|run| run.chat_id.as_deref())
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
    text.push_str("\n\n");
    text.push_str(&outcome(question).unwrap_or_else(|| {
        format!(
            "Reply with a number or your own direction. For this question: /answer {} YOUR_ANSWER",
            question.id
        )
    }));
    text
}
/// How a decided question ended, as one line; `None` while it is pending.
fn outcome(question: &Question) -> Option<String> {
    match &question.status {
        Status::Pending { .. } => None,
        Status::Answered { answer, .. } => Some(format!("Decision: {answer}")),
        Status::Withdrawn { withdrawn, .. } => Some(format!(
            "Withdrawn by the {}{}. No decision is needed.",
            match withdrawn.by {
                Withdrawer::Asker => "asker",
                Withdrawer::Operator => "operator",
            },
            withdrawn
                .reason
                .as_ref()
                .map(|reason| format!(": {}", reason.0))
                .unwrap_or_default()
        )),
    }
}
pub fn answer(
    state: &mut State,
    chat_id: &str,
    question_id: Option<&str>,
    text: &str,
    source: &str,
) -> Result<(), &'static str> {
    let target = question_id
        .map(|question| answer_target(state, chat_id, question).map(str::to_owned))
        .transpose()?;
    let chat_id = target.as_deref().unwrap_or(chat_id);
    let run = waiting_for(state, chat_id, question_id).ok_or(if question_id.is_some() {
        "That design question is no longer pending."
    } else {
        "No design question is ready for an answer in this chat."
    })?;
    let question = run.question.as_ref().unwrap();
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
    let answered = question.clone().answered(answer.clone()).map_err(Refusal::message)?;
    if asked(run) {
        // Nothing resumes from an operator-side question; the answer stays in the DM.
        let index = state.runtime.runs.position(run.id()).unwrap();
        let run = &mut state.runtime.runs[index];
        run.question = Some(answered);
        run.status = RunStatus::Completed;
        *run.result = summary(run.question.as_ref().unwrap());
        let session = &mut state.chats[session_index];
        session.messages.push(chat::Message {
            speaker: Default::default(),
            text: answer,
            run_id: None,
            reply: format!("Decision recorded for {}.", run.question.as_ref().unwrap().id),
        });
        session.revision = session.revision.saturating_add(1);
        return Ok(());
    }
    let attempt = run
        .attempts
        .last()
        .filter(|a| {
            a.workspace_id.as_deref() == Some(&question.generation) && a.status == crate::usage::AttemptStatus::Waiting
        })
        .ok_or("Paused attempt is unavailable.")?;
    let checkpoint = attempt
        .project_checkpoint
        .as_ref()
        .or(attempt.artifact.as_ref())
        .cloned()
        .ok_or("The paused workspace has no verified checkpoint.")?;
    if attempt.project.is_some()
        && state.chats[session_index]
            .project
            .as_ref()
            .and_then(|p| p.head.as_ref())
            != attempt.project_base.as_ref()
    {
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
    let index = state.runtime.runs.position(run.id()).unwrap();
    // Capture continuation inputs, not diagnostic histories or previous attempts.
    let actor = run.actor.clone();
    let action = run.action;
    let target = run.target.clone();
    let persona = crate::delegation::lineage(run);
    let context = run.context.clone();
    let lenses = run.lenses.clone();
    let queue_order = run.queue_order;
    let resources = run.resource_grants.clone();
    let objective = run
        .continuation
        .as_ref()
        .map(|c| c.objective.clone())
        .unwrap_or_else(|| run.objective.to_string());
    let continuation = Continuation {
        project_base: attempt.project_base.clone(),
        checkpoint,
        published_parent: baseline.clone(),
        objective,
        decision: question.input.clone(),
    };
    // Replacing the waiting run with its continuation keeps admission bounded.
    state.runtime.runs[index].status = RunStatus::Continued;
    let notice = state.runtime.notice.clone();
    if let Err(error) = state
        .runtime
        .enqueue_for(answer.clone(), RunKind::DesignAnswer, source, actor, action)
    {
        state.runtime.runs[index].status = RunStatus::Waiting;
        state.runtime.notice = notice;
        return Err(error);
    }
    let next = state.runtime.runs.last_mut().unwrap();
    next.target = target;
    next.resource_grants = resources;
    next.persona = Some(persona);
    next.context = context;
    next.lenses = lenses;
    next.chat_id = Some(chat_id.into());
    next.queue_order = queue_order;
    *next.continuation = Some(continuation);
    let next_id = next.id().to_string();
    state.runtime.runs[index].question = Some(Question {
        continuation: Some(next_id.clone()),
        ..answered
    });
    *state.runtime.runs[index].result = summary(state.runtime.runs[index].question.as_ref().unwrap());
    let session = &mut state.chats[session_index];
    session.messages.push(chat::Message {
        speaker: Default::default(),
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
                .find(|r| r.actor == actor && r.question.as_ref().is_some_and(|q| q.id == *id && q.pending()))
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
            html.push_str(&format!("<form method='post' action='/dm/{chat_id}/messages' target='_top'><button name='message' value='/answer {} {}'>{}. {}{}</button></form>",crate::escape(question.id.as_str()),i+1,i+1,crate::escape(option),if question.input.recommended == Some(i) {" · recommended"} else {""}));
        }
        html.push_str("<small>Or reply with your own direction.</small>");
    } else if let Some(outcome) = outcome(question) {
        html.push_str(&format!("<p class='muted preserve'>{}</p>", crate::escape(&outcome)));
    } else {
        html.push_str("<small>Saving the paused workspace…</small>");
    }
    html.push_str("</div>");
    html
}

#[cfg(test)]
pub(crate) mod tests;
