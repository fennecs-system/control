//! Persistent bots: chats that sleep until events wake them. The pure
//! `control_bot::Supervisor` decides; this adapter feeds it room messages, turn
//! completions and time, and interprets its turn requests as ordinary queued
//! turns. Supervisor state, requested turns and queued runs share one checkpoint.
mod tools;
pub use tools::{BotCall, BotOps, Tools};
use crate::{State, identity, tool_registry::AttemptRef};
use control_bot::{BotId, Budget, ChatId, Definition, Effect, Event, Reason, Source, Status, TurnId};
use control_effect::machine::{Machine, Now, Planned};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, sync::{Arc, Mutex}};

const COOLDOWN_MS: u64 = 30_000;
const WINDOW_MS: u64 = 3_600_000;
const TURNS_PER_WINDOW: u32 = 60;
const MAX_INTERVAL_MINUTES: u64 = 1_440;
/// Room posts offered per pass, bounding work under the state lock.
const POSTS_PER_PASS: usize = 256;

/// Marks a run as a bot turn so its completion releases the supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub bot: String,
    pub turn: u64,
}

/// A requested turn not yet queued, retried until queued or no longer eligible.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Request {
    bot: BotId,
    chat: ChatId,
    turn: TurnId,
    reasons: Vec<Reason>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bots {
    supervisor: control_bot::Supervisor,
    /// Room posts before this index were already offered to the supervisor.
    room_cursor: usize,
    requests: Vec<Request>,
    /// Bot requests from agents, decided by the operator.
    approvals: Vec<Approval>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis().min(u64::MAX as u128) as u64)
}
/// Each chat runs at most one bot, named after the chat.
fn bot(chat: &str) -> BotId {
    BotId(chat.into())
}
/// Records every effect of a supervisor step: turn requests go to the outbox so
/// no step, including a command's, can drop one.
fn record(state: &mut State, effects: Vec<Planned<BotId, Effect>>) {
    for Planned { effect, .. } in effects {
        match effect {
            Effect::RequestTurn { bot, chat, turn, reasons } => state.bots.requests.push(Request { bot, chat, turn, reasons }),
            Effect::BudgetExhausted { bot, .. } => {
                state.runtime.notice = format!("Bot in chat {} reached its turn budget; events stay pending.", bot.0);
            }
        }
    }
}

pub(crate) fn defined(state: &State, chat: &str) -> bool {
    state.bots.supervisor.status(&bot(chat)).is_some()
}

pub(crate) fn status(state: &State, chat: &str) -> Option<String> {
    let id = bot(chat);
    let status = state.bots.supervisor.status(&id)?;
    let definition = state.bots.supervisor.definitions().find(|definition| definition.bot == id)?;
    let mut sources = definition.sources.iter().filter_map(|source| match source {
        Source::Room { room } => Some(format!("room #{room}")),
        Source::Timer { every_ms } => Some(format!("every {} min", every_ms / 60_000)),
        Source::Service { .. } => None,
    }).collect::<Vec<_>>().join(", ");
    if sources.is_empty() {
        sources = "no sources".into();
    }
    let state = match status {
        Status::Ready => "sleeping",
        Status::WaitingTurn => "working",
        Status::Paused => "paused",
        Status::Stopping => "stopping",
    };
    Some(format!("Bot · {state} · {sources}"))
}

/// `/bot on [every N]`, `/bot off`, `/bot pause` and `/bot resume`. Reached only
/// through the authenticated chat command route, never from room text or tools.
pub(crate) fn command(state: &mut State, chat: &str, option: &str) -> Result<&'static str, &'static str> {
    command_at(state, chat, option, Now(now_ms()))
}
fn command_at(state: &mut State, chat: &str, option: &str, now: Now) -> Result<&'static str, &'static str> {
    let session = state.chats.iter().find(|session| session.id == chat).ok_or("Chat not found.")?;
    identity::authorize(&session.actor, identity::Action::Task)?;
    let words = option.split_whitespace().collect::<Vec<_>>();
    let id = bot(chat);
    match words.as_slice() {
        ["off"] => {
            let effects = state.bots.supervisor.step(Event::Stop(id), now);
            record(state, effects);
            Ok("Bot stopped. A running turn finishes first.")
        }
        ["pause"] if defined(state, chat) => {
            let effects = state.bots.supervisor.step(Event::Pause(id), now);
            record(state, effects);
            Ok("Bot paused. Events are kept for when it resumes.")
        }
        ["resume"] if defined(state, chat) => {
            let effects = state.bots.supervisor.step(Event::Resume(id), now);
            record(state, effects);
            Ok("Bot resumed.")
        }
        ["pause" | "resume"] => Err("This chat has no bot. Start one with /bot on."),
        ["allow", id] => decide(state, id, true, Some(chat), now),
        ["deny", id] => decide(state, id, false, Some(chat), now),
        [] | ["on"] | ["on", "every", _] | ["every", _] => {
            let minutes = match words.last() {
                Some(value) if words.len() > 1 => Some(value.parse::<u64>().ok().filter(|m| (1..=MAX_INTERVAL_MINUTES).contains(m)).ok_or("Use /bot on every N, with N from 1 to 1440 minutes.")?),
                _ => None,
            };
            start(state, chat, minutes, true, now)
        }
        _ => Err("Use /bot on [every N], /bot pause, /bot resume or /bot off."),
    }
}

fn start(state: &mut State, chat: &str, minutes: Option<u64>, room: bool, now: Now) -> Result<&'static str, &'static str> {
    if crate::rooms::listening(state, chat) {
        return Err("Room listening is on. Stop it with /listen off before starting a bot.");
    }
    let session = state.chats.iter().find(|session| session.id == chat).ok_or("Chat not found.")?;
    let origin = state.runtime.runs.iter().rev()
        .find(|run| run.chat_id.as_deref() == Some(chat) && run.actor == session.actor)
        .ok_or("Give this chat a task before starting a bot.")?;
    identity::authorize(&session.actor, origin.action)?;
    if crate::delegation::is_judge(origin) {
        return Err("Read-only judges cannot run as bots.");
    }
    let mut sources = Vec::new();
    if let Some(room) = session.room.as_deref().filter(|linked| room && crate::rooms::valid(linked)) {
        sources.push(Source::Room { room: room.into() });
    }
    if let Some(minutes) = minutes {
        sources.push(Source::Timer { every_ms: minutes * 60_000 });
    }
    if sources.is_empty() {
        return Err("Link a room or give an interval, for example /bot on every 10.");
    }
    crate::rooms::ensure_inbox(state, chat)?;
    if state.bots.supervisor.definitions().next().is_none() {
        state.bots.room_cursor = state.collective.posts.len();
    }
    let definition = Definition {
        bot: bot(chat),
        chat: ChatId(chat.into()),
        sources,
        budget: Budget { cooldown_ms: COOLDOWN_MS, window_ms: WINDOW_MS, turns_per_window: TURNS_PER_WINDOW },
    };
    definition.validate()?;
    let effects = state.bots.supervisor.step(Event::Define(definition), now);
    record(state, effects);
    if state.bots.supervisor.status(&bot(chat)) == Some(Status::Stopping) {
        return Err("The previous bot is still finishing its turn. Try again shortly.");
    }
    Ok("Bot started. It sleeps until room messages or its timer wake it.")
}

fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted" | "blocked")
}

fn describe(reasons: &[Reason]) -> String {
    reasons.iter().map(|reason| match reason {
        Reason::Room { room, .. } => format!("new messages in room #{room}"),
        Reason::Service { service, kind, .. } => format!("service {} reported {}", service.0, kind.chars().take(80).collect::<String>()),
        Reason::Timer => "your scheduled check-in".into(),
        Reason::Overflow { dropped, sources } => format!("{dropped} more events than fit; re-read {} source(s) directly", sources.len()),
    }).collect::<Vec<_>>().join("; ")
}

/// Queues one bot turn in its chat, reusing the chat's latest task context like
/// room listening does. Errors leave the request for a later attempt.
fn queue(state: &mut State, request: &Request) -> Result<(), &'static str> {
    let chat = request.chat.0.as_str();
    let session = state.chats.iter().find(|session| session.id == chat).ok_or("Bot chat is missing.")?;
    let latest = state.runtime.runs.iter().rev()
        .find(|run| run.chat_id.as_deref() == Some(chat) && run.actor == session.actor)
        .ok_or("Bot chat has no task.")?;
    if crate::delegation::is_judge(latest) {
        return Err("Read-only judges cannot run as bots.");
    }
    let (actor, action, backend, model, context, lenses) =
        (latest.actor.clone(), latest.action, latest.backend, latest.model.clone(), latest.context.clone(), latest.lenses.clone());
    let persona = crate::delegation::lineage(latest);
    let grants = crate::services::grants(state, latest);
    let woke = describe(&request.reasons);
    let objective = format!(
        "You are running as a persistent bot. You woke because of: {woke}. Read what changed (control_room_notifications for room messages, service status for your services), act on it, coordinate with other agents through control_room_post when useful, and acknowledge processed room updates. Then end your turn; new events will wake you again. Do not loop, poll or sleep inside commands."
    );
    let objective = objective.chars().take(2000).collect::<String>();
    state.runtime.enqueue_for(objective, "chat", "bot", actor, action)?;
    let run = state.runtime.runs.last_mut().unwrap();
    run.chat_id = Some(chat.into());
    run.backend = backend;
    run.model = model;
    run.context = context;
    run.lenses = lenses;
    run.persona = Some(persona);
    run.resource_grants = grants;
    run.bot_turn = Some(Turn { bot: request.bot.0.clone(), turn: request.turn.0 });
    let id = run.id.clone();
    let session = state.chats.iter_mut().find(|session| session.id == chat).unwrap();
    session.messages.push(crate::chat::Message { text: format!("Bot woke: {woke}"), run_id: Some(id), reply: String::new() });
    session.revision = session.revision.saturating_add(1);
    Ok(())
}

/// Runs in the existing worker supervisor. Feeds new room posts, finished bot
/// turns and the current time to the supervisor, records its turn requests and
/// queues them, then saves everything in one checkpoint.
pub fn reconcile(state: &mut State, save: impl FnMut(&State) -> std::io::Result<()>) -> Result<(), &'static str> {
    reconcile_at(state, now_ms(), save)
}

fn reconcile_at(state: &mut State, now: u64, mut save: impl FnMut(&State) -> std::io::Result<()>) -> Result<(), &'static str> {
    let resumable = state.bots.approvals.iter().any(|approval| approval.decision != Decision::Pending && !approval.acknowledged);
    if state.bots.supervisor.definitions().next().is_none() && state.bots.requests.is_empty() && !resumable {
        state.bots.room_cursor = state.collective.posts.len();
        return Ok(());
    }
    let outstanding = state.bots.supervisor.outstanding().next().is_some();
    if !outstanding && state.bots.requests.is_empty() && state.bots.room_cursor == state.collective.posts.len()
        && state.bots.supervisor.next_deadline(Now(now)).is_none_or(|due| due.0 > now)
    {
        return Ok(());
    }
    let previous = state.bots.clone();
    let (runs, notice) = (state.runtime.runs.len(), state.runtime.notice.clone());
    let chats = state.chats.iter().map(|session| (session.messages.len(), session.revision)).collect::<Vec<_>>();
    let decided = advance(state);
    // Turns requested earlier and now finished; decided before new events so a
    // turn requested in this pass is never mistaken for a finished one.
    let finished = if outstanding {
        let index = state.runtime.runs.iter().enumerate().map(|(i, run)| (run.id.as_str(), i)).collect::<std::collections::HashMap<_, _>>();
        let tagged = state.runtime.runs.iter().enumerate()
            .filter_map(|(i, run)| run.bot_turn.as_ref().map(|turn| ((turn.bot.as_str(), turn.turn), i)))
            .collect::<std::collections::HashMap<_, _>>();
        // A turn that delegated or awaited a design answer continues in another run;
        // it finishes when that chain's leaf does. A broken chain releases the bot.
        state.bots.supervisor.outstanding()
            .filter(|(bot, _, turn)| match tagged.get(&(bot.0.as_str(), turn.0)) {
                Some(&run) => crate::rooms::continuation_leaf(state, run, &index).is_none_or(|leaf| terminal(&state.runtime.runs[leaf].status)),
                None => !state.bots.requests.iter().any(|request| &request.bot == *bot && &request.turn == *turn),
            })
            .map(|(bot, _, turn)| (bot.clone(), *turn))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let completed = !finished.is_empty();
    let mut effects = Vec::new();
    for (bot, turn) in finished {
        effects.extend(state.bots.supervisor.step(Event::TurnCompleted { bot, turn }, Now(now)));
    }
    let start = state.bots.room_cursor.min(state.collective.posts.len());
    let end = start.saturating_add(POSTS_PER_PASS).min(state.collective.posts.len());
    for (index, post) in state.collective.posts.iter().enumerate().take(end).skip(start) {
        let event = Event::RoomMessage { room: post.room.clone(), from: post.chat.clone().map(ChatId), cursor: index as u64 + 1 };
        effects.extend(state.bots.supervisor.step(event, Now(now)));
    }
    let offered = state.bots.room_cursor != end;
    state.bots.room_cursor = end;
    effects.extend(state.bots.supervisor.step(Event::Tick, Now(now)));
    let planned = !effects.is_empty();
    record(state, effects);
    let mut queued = false;
    let requests = std::mem::take(&mut state.bots.requests);
    for request in requests {
        if !state.bots.supervisor.eligible(&request.bot, &request.turn) {
            queued = true;
            continue;
        }
        match queue(state, &request) {
            Ok(()) => queued = true,
            Err(_) => state.bots.requests.push(request),
        }
    }
    if !(offered || completed || planned || queued || decided) {
        return Ok(());
    }
    if save(state).is_err() {
        state.bots = previous;
        state.runtime.runs.truncate(runs);
        state.runtime.notice = notice;
        for (session, (messages, revision)) in state.chats.iter_mut().zip(chats) {
            session.messages.truncate(messages);
            session.revision = revision;
        }
        return Err("Could not save bot progress; events remain pending.");
    }
    Ok(())
}

const MAX_APPROVALS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Decision {
    Pending,
    Approved,
    Denied,
}

/// An agent's request to run a chat as a bot. Granting it is the operator's
/// decision; the request itself carries no authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Approval {
    id: String,
    chat: String,
    requester: String,
    origin: String,
    every_minutes: Option<u64>,
    room: bool,
    decision: Decision,
    acknowledged: bool,
}
impl Approval {
    fn summary(&self) -> Value {
        json!({"request_id":self.id,"chat_id":self.chat,"every_minutes":self.every_minutes,"room":self.room,
            "decision":match self.decision { Decision::Pending => "pending", Decision::Approved => "approved", Decision::Denied => "denied" }})
    }
}

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn digest(parts: &[&str]) -> String {
    let joined = parts.join("\u{0}");
    ring::digest::digest(&ring::digest::SHA256, joined.as_bytes()).as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}
/// Whether `chat` is `ancestor` or was spawned from it, directly or through children.
fn within(state: &State, chat: &str, ancestor: &str) -> bool {
    let mut current = Some(chat);
    for _ in 0..64 {
        let Some(id) = current else { return false };
        if id == ancestor {
            return true;
        }
        current = state.chats.iter().find(|session| session.id == id).and_then(|session| session.parent_chat.as_deref());
    }
    false
}

/// Worker tool entry point: agents request bots and read their state.
pub(crate) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: BotCall) -> io::Result<Value> {
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    handle(&mut state, attempt, call, crate::storage::save)
}
fn handle(state: &mut State, attempt: &AttemptRef<'_>, call: BotCall, save: impl Fn(&State) -> io::Result<()>) -> io::Result<Value> {
    let run = state.runtime.runs.iter()
        .find(|run| run.id == attempt.run && run.status == "running"
            && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation))
        .ok_or_else(|| fail("Attempt is not active"))?;
    identity::authorize(&run.actor, run.action).map_err(fail)?;
    if crate::delegation::is_judge(run) {
        return Err(fail("Judges cannot request bots"));
    }
    let requester = run.chat_id.clone().ok_or_else(|| fail("A chat is required"))?;
    let (origin, actor) = (run.id.clone(), run.actor.clone());
    match call {
        BotCall::Status { chat_id } => {
            let chat = chat_id.unwrap_or_else(|| requester.clone());
            if !within(state, &chat, &requester) {
                return Err(fail("Read bot state only for this chat or chats it spawned"));
            }
            let requests = state.bots.approvals.iter().filter(|a| a.requester == requester).map(Approval::summary).collect::<Vec<_>>();
            Ok(json!({"chat_id":chat,"bot":status(state, &chat),"requests":requests}))
        }
        BotCall::Request { chat_id, every_minutes, room } => {
            if !attempt.nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') || attempt.nonce.is_empty() || attempt.nonce.len() > 128 {
                return Err(fail("Invalid request identity"));
            }
            let id = format!("bot-{}", digest(&[attempt.run, attempt.generation, attempt.nonce, &chat_id]));
            if let Some(existing) = state.bots.approvals.iter().find(|a| a.id == id) {
                return Ok(existing.summary());
            }
            let target = state.chats.iter().find(|session| session.id == chat_id).ok_or_else(|| fail("Chat not found"))?;
            if target.actor != actor || !within(state, &chat_id, &requester) {
                return Err(fail("Request bots only for this chat or chats it spawned"));
            }
            if every_minutes.is_some_and(|m| !(1..=MAX_INTERVAL_MINUTES).contains(&m)) {
                return Err(fail("every_minutes must be 1–1440"));
            }
            let linked = target.room.as_deref().is_some_and(crate::rooms::valid);
            if !(room && linked) && every_minutes.is_none() {
                return Err(fail("The chat has no room to listen to; give every_minutes"));
            }
            if !state.runtime.runs.iter().any(|run| run.chat_id.as_deref() == Some(chat_id.as_str())) {
                return Err(fail("The chat has no task yet; give it one before it becomes a bot"));
            }
            if crate::rooms::listening(state, &chat_id) {
                return Err(fail("That chat listens to its room; it must turn off /listen first"));
            }
            if defined(state, &chat_id) || state.bots.approvals.iter().any(|a| a.chat == chat_id && a.decision == Decision::Pending) {
                return Err(fail("That chat already runs a bot or has a pending request"));
            }
            if state.bots.approvals.iter().filter(|a| a.decision == Decision::Pending).count() >= MAX_APPROVALS {
                return Err(fail("Too many pending bot requests; wait for the operator"));
            }
            let previous = state.bots.clone();
            state.bots.approvals.retain(|a| !(a.acknowledged && a.decision != Decision::Pending));
            let approval = Approval { id, chat: chat_id, requester: requester.clone(), origin, every_minutes, room: room && linked, decision: Decision::Pending, acknowledged: false };
            let mut value = approval.summary();
            value["approval"] = json!(format!("Waiting for the operator: approve in this chat or with /bot allow {} (also over Discord). End your turn; you will be resumed with the decision.", approval.id));
            state.bots.approvals.push(approval);
            bump(state, &requester);
            if let Err(error) = save(state) {
                state.bots = previous;
                return Err(error);
            }
            Ok(value)
        }
    }
}
fn bump(state: &mut State, chat: &str) {
    if let Some(session) = state.chats.iter_mut().find(|session| session.id == chat) {
        session.revision = session.revision.saturating_add(1);
    }
}

/// The operator's decision, from the chat panel or `/bot allow|deny ID` in the
/// requesting or target chat. Approval starts the bot; the requester resumes later.
fn decide(state: &mut State, id: &str, allow: bool, chat: Option<&str>, now: Now) -> Result<&'static str, &'static str> {
    let index = state.bots.approvals.iter()
        .position(|a| a.id == id && a.decision == Decision::Pending && chat.is_none_or(|chat| a.chat == chat || a.requester == chat))
        .ok_or("No pending bot request with that ID in this conversation.")?;
    let approval = state.bots.approvals[index].clone();
    let session = state.chats.iter().find(|session| session.id == approval.requester).ok_or("Requesting chat is missing.")?;
    identity::authorize(&session.actor, identity::Action::Task)?;
    if allow {
        start(state, &approval.chat, approval.every_minutes, approval.room, now)?;
    }
    state.bots.approvals[index].decision = if allow { Decision::Approved } else { Decision::Denied };
    bump(state, &approval.requester);
    Ok(if allow { "Bot request approved; the chat now runs as a bot." } else { "Bot request denied." })
}

/// Resumes each requesting agent once all its requests are decided, if the
/// requesting turn (or its continuation) is still the latest in that chat;
/// otherwise records the decisions in the chat without a new turn.
fn advance(state: &mut State) -> bool {
    let mut changed = false;
    let requesters = state.bots.approvals.iter()
        .filter(|a| a.decision != Decision::Pending && !a.acknowledged)
        .map(|a| a.requester.clone()).collect::<std::collections::BTreeSet<_>>();
    for requester in requesters {
        let mine = |a: &&Approval| a.requester == requester;
        if state.bots.approvals.iter().filter(mine).any(|a| a.decision == Decision::Pending)
            || state.runtime.runs.iter().any(|run| run.chat_id.as_deref() == Some(requester.as_str()) && matches!(run.status.as_str(), "queued" | "running" | "waiting" | "cancelling" | "continued"))
        {
            continue;
        }
        let decided = state.bots.approvals.iter().filter(mine).filter(|a| a.decision != Decision::Pending && !a.acknowledged).cloned().collect::<Vec<_>>();
        let latest = state.runtime.runs.iter().rposition(|run| run.chat_id.as_deref() == Some(requester.as_str()));
        let index = state.runtime.runs.iter().enumerate().map(|(index, run)| (run.id.as_str(), index)).collect::<std::collections::HashMap<_, _>>();
        let resume = latest.filter(|&latest| state.runtime.runs[latest].status == "completed" && decided.iter().any(|a| {
            index.get(a.origin.as_str()).and_then(|&origin| crate::rooms::continuation_leaf(state, origin, &index)) == Some(latest)
        }));
        let lines = decided.iter().map(|a| {
            let title = state.chats.iter().find(|session| session.id == a.chat).map_or(a.chat.as_str(), |session| session.title.as_str());
            format!("{title} ({}): {}", a.chat, if a.decision == Decision::Approved { "approved, now a persistent bot" } else { "denied" })
        }).collect::<Vec<_>>().join("\n");
        let mut run_id = None;
        if let Some(latest) = resume {
            if crate::scheduler::admit(&state.runtime).is_err() {
                continue;
            }
            let latest = &state.runtime.runs[latest];
            let objective = format!("Operator bot decisions:\n{lines}\nContinue the original task. Approved chats now sleep until room messages or their timers wake them. Original task:\n{}", latest.objective).chars().take(2000).collect::<String>();
            let (actor, action, backend, model, context, lenses) = (latest.actor.clone(), latest.action, latest.backend, latest.model.clone(), latest.context.clone(), latest.lenses.clone());
            let persona = crate::delegation::lineage(latest);
            let grants = crate::services::grants(state, latest);
            if state.runtime.enqueue_for(objective, "bot-decision", "bot", actor, action).is_err() {
                continue;
            }
            let run = state.runtime.runs.last_mut().unwrap();
            run.chat_id = Some(requester.clone());
            run.backend = backend;
            run.model = model;
            run.context = context;
            run.lenses = lenses;
            run.persona = Some(persona);
            run.resource_grants = grants;
            run_id = Some(run.id.clone());
        }
        if let Some(session) = state.chats.iter_mut().find(|session| session.id == requester) {
            session.messages.push(crate::chat::Message { text: format!("Bot decisions\n{lines}"), run_id, reply: String::new() });
            session.revision = session.revision.saturating_add(1);
        }
        for approval in state.bots.approvals.iter_mut().filter(|a| a.requester == requester && a.decision != Decision::Pending) {
            approval.acknowledged = true;
        }
        changed = true;
    }
    changed
}

/// Pending requests shown to the operator in the requesting chat.
pub(crate) fn pending_panel(state: &State, chat: &str) -> String {
    state.bots.approvals.iter().filter(|a| a.requester == chat && a.decision == Decision::Pending).map(|a| {
        let title = state.chats.iter().find(|session| session.id == a.chat).map_or(a.chat.as_str(), |session| session.title.as_str());
        let mut sources = Vec::new();
        if a.room { sources.push("room messages".to_owned()); }
        if let Some(minutes) = a.every_minutes { sources.push(format!("every {minutes} min")); }
        format!("<article class='assistant-message port-request' data-transcript-key='bot-{}'><h3>Bot request · {}</h3><p>Run this chat as a persistent bot, woken by {}. Up to {TURNS_PER_WINDOW} turns per hour.</p><form method='post' action='/tools/bots' target='_top'><input type='hidden' name='id' value='{}'><input type='hidden' name='return_chat' value='{}'><button class='primary' name='allow' value='yes'>Allow bot</button><button name='deny' value='yes'>Deny</button><p role='alert' hidden></p></form><p class='muted'>Or reply /bot allow {} · /bot deny {}</p></article>",
            crate::escape(&a.id), crate::escape(title), crate::escape(&sources.join(" and ")), crate::escape(&a.id), crate::escape(chat), crate::escape(&a.id), crate::escape(&a.id))
    }).collect()
}
pub(crate) fn pending_json(state: &State, chat: &str) -> Value {
    Value::from(state.bots.approvals.iter().filter(|a| a.requester == chat && a.decision == Decision::Pending).map(Approval::summary).collect::<Vec<_>>())
}

/// `POST /tools/bots` from the chat panel: allow or deny, then return to the chat.
pub fn http(stream: &mut impl io::Write, shared: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8]) -> bool {
    http_with_save(stream, shared, method, path, body, crate::storage::save)
}
fn http_with_save(stream: &mut impl io::Write, shared: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8], save: impl Fn(&State) -> io::Result<()>) -> bool {
    if path != "/tools/bots" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let asynchronous = fields.get("response").is_some_and(|value| value == "json");
    let chat = fields.get("return_chat").filter(|id| crate::chat::valid_id(id)).cloned();
    let result = (|| -> Result<(), &'static str> {
        if method != "POST" {
            return Err("Use POST");
        }
        let id = fields.get("id").ok_or("Missing request ID")?;
        let chat = chat.as_deref().ok_or("Missing chat")?;
        let mut state = shared.lock().map_err(|_| "State unavailable")?;
        let (bots, watches) = (state.bots.clone(), state.collective.room_watches.clone());
        let revisions = state.chats.iter().map(|session| session.revision).collect::<Vec<_>>();
        let result = decide(&mut state, id, fields.contains_key("allow"), Some(chat), Now(now_ms()));
        if result.is_err() || save(&state).is_err() {
            state.bots = bots;
            state.collective.room_watches = watches;
            for (session, revision) in state.chats.iter_mut().zip(revisions) {
                session.revision = revision;
            }
            return result.and(Err("Could not save the decision. Try again."));
        }
        Ok(())
    })();
    let _ = match (result, chat) {
        (result, _) if asynchronous => {
            let (status, value) = match result { Ok(()) => ("200 OK", json!({"ok":true})), Err(error) => ("400 Bad Request", json!({"error":error})) };
            control_http::respond(stream, status, "application/json", value.to_string().as_bytes(), "", control_http::Policy::Page)
        }
        (Ok(()), Some(chat)) => control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: /chat/{chat}\r\n"), control_http::Policy::Page),
        (Ok(()), None) => control_http::respond(stream, "204 No Content", "text/plain", b"", "", control_http::Policy::Page),
        (Err(error), back) => control_http::respond(stream, "400 Bad Request", "text/html; charset=utf-8", crate::error_page(error, &back.map_or("/".to_owned(), |chat| format!("/chat/{chat}"))).as_bytes(), "", control_http::Policy::Page),
    };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (State, String) {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        state.chats[0].room = Some("general".into());
        crate::chat::send(&mut state, &chat, "Watch the game and coordinate", "test").unwrap();
        state.runtime.runs[0].status = "completed".into();
        (state, chat)
    }
    fn post(state: &mut State, chat: Option<&str>, text: &str) {
        state.collective.posts.push(crate::rooms::Post { room: "general".into(), text: text.into(), chat: chat.map(Into::into), run: None, key: format!("k{}", state.collective.posts.len()) });
    }
    fn bot_runs(state: &State) -> Vec<&crate::runtime::Run> {
        state.runtime.runs.iter().filter(|run| run.bot_turn.is_some()).collect()
    }

    #[test]
    fn room_messages_wake_a_bot_once_and_its_finished_turn_releases_the_next() {
        let (mut state, chat) = fixture();
        assert_eq!(command(&mut state, &chat, "on"), Ok("Bot started. It sleeps until room messages or its timer wake it."));
        assert_eq!(status(&state, &chat).as_deref(), Some("Bot · sleeping · room #general"));
        reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
        assert!(bot_runs(&state).is_empty(), "no events yet");
        post(&mut state, Some("c-other"), "Alice found iron");
        reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
        let runs = bot_runs(&state);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].source.as_str(), runs[0].chat_id.as_deref()), ("bot", Some(chat.as_str())));
        assert!(runs[0].objective.contains("new messages in room #general"));
        assert!(state.chats[0].messages.last().unwrap().text.starts_with("Bot woke:"));
        post(&mut state, Some("c-other"), "Cass needs wood");
        post(&mut state, Some(&chat), "own post");
        reconcile_at(&mut state, 3_000, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1, "one turn at a time");
        let first = state.runtime.runs.iter().position(|run| run.bot_turn.is_some()).unwrap();
        state.runtime.runs[first].status = "completed".into();
        reconcile_at(&mut state, 40_000, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 2, "pending messages wake the next turn after the cooldown");
        assert_eq!(status(&state, &chat).as_deref(), Some("Bot · working · room #general"));
    }

    #[test]
    fn timers_wake_bots_without_rooms_and_commands_validate() {
        let (mut state, chat) = fixture();
        state.chats[0].room = None;
        assert!(command(&mut state, &chat, "on").is_err(), "no room and no interval");
        assert!(command(&mut state, &chat, "on every 0").is_err());
        assert!(command(&mut state, &chat, "on every 2000").is_err());
        assert!(command(&mut state, &chat, "pause").is_err());
        assert!(command(&mut state, &chat, "dance").is_err());
        command(&mut state, &chat, "on every 5").unwrap();
        let started = now_ms();
        reconcile_at(&mut state, started + 60_000, |_| Ok(())).unwrap();
        assert!(bot_runs(&state).is_empty());
        reconcile_at(&mut state, started + 300_000, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1);
        assert!(bot_runs(&state)[0].objective.contains("scheduled check-in"));
    }

    #[test]
    fn listening_and_bots_are_exclusive_and_stop_waits_for_the_turn() {
        let (mut state, chat) = fixture();
        crate::rooms::listen(&mut state, &chat, true).unwrap();
        assert!(command(&mut state, &chat, "on").is_err());
        crate::rooms::listen(&mut state, &chat, false).unwrap();
        command(&mut state, &chat, "on").unwrap();
        post(&mut state, None, "operator: can any agent scout the cave?");
        reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
        command(&mut state, &chat, "off").unwrap();
        assert_eq!(status(&state, &chat).as_deref(), Some("Bot · stopping · room #general"));
        let turn = state.runtime.runs.iter().position(|run| run.bot_turn.is_some()).unwrap();
        state.runtime.runs[turn].status = "completed".into();
        reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
        assert!(!defined(&state, &chat));
        post(&mut state, None, "after stop");
        reconcile_at(&mut state, 50_000, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1);
    }

    #[test]
    fn failed_saves_roll_back_and_full_queues_keep_the_request() {
        let (mut state, chat) = fixture();
        command(&mut state, &chat, "on").unwrap();
        post(&mut state, None, "hello");
        let runs = state.runtime.runs.len();
        assert!(reconcile_at(&mut state, 1_000, |_| Err(std::io::Error::other("disk"))).is_err());
        assert_eq!(state.runtime.runs.len(), runs);
        assert_eq!(state.bots.room_cursor, 0, "the post is offered again");
        for index in 0..crate::scheduler::MAX_PENDING {
            state.runtime.enqueue(format!("filler {index}"), "chat", "test");
        }
        reconcile_at(&mut state, 1_000, |_| Ok(())).unwrap();
        assert!(bot_runs(&state).is_empty());
        assert_eq!(state.bots.requests.len(), 1, "the requested turn waits for queue space");
        for run in &mut state.runtime.runs {
            if run.source == "test" { run.status = "completed".into(); }
        }
        reconcile_at(&mut state, 2_000, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1);
        assert!(state.bots.requests.is_empty());
    }

    #[test]
    fn command_steps_never_drop_turn_requests_for_any_bot() {
        let (mut state, chat) = fixture();
        command_at(&mut state, &chat, "on", Now(0)).unwrap();
        command_at(&mut state, &chat, "pause", Now(1)).unwrap();
        post(&mut state, Some("c-other"), "while paused");
        reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
        assert!(bot_runs(&state).is_empty());
        command_at(&mut state, &chat, "resume", Now(3)).unwrap();
        assert_eq!(state.bots.requests.len(), 1, "resuming with pending events records the turn");
        reconcile_at(&mut state, 4, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&state).len(), 1);

        let other = crate::chat::create(&mut state);
        let index = state.chats.iter().position(|session| session.id == other).unwrap();
        state.chats[index].room = None;
        crate::chat::send(&mut state, &other, "Check the farm", "test").unwrap();
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        command_at(&mut state, &other, "on every 1", Now(10)).unwrap();
        command_at(&mut state, &chat, "pause", Now(60_010)).unwrap();
        assert!(state.bots.requests.iter().any(|request| request.chat.0 == other), "another bot's due timer turn is recorded during this command");
        reconcile_at(&mut state, 60_020, |_| Ok(())).unwrap();
        assert!(bot_runs(&state).iter().any(|run| run.chat_id.as_deref() == Some(other.as_str())));
    }

    #[test]
    fn requested_turns_survive_a_restart_and_are_queued_once() {
        let (mut state, chat) = fixture();
        command_at(&mut state, &chat, "on", Now(0)).unwrap();
        for index in 0..crate::scheduler::MAX_PENDING {
            state.runtime.enqueue(format!("filler {index}"), "chat", "test");
        }
        post(&mut state, Some("c-other"), "while the queue is full");
        reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
        assert_eq!(state.bots.requests.len(), 1);
        let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        for run in &mut restored.runtime.runs {
            if run.source == "test" { run.status = "completed".into(); }
        }
        reconcile_at(&mut restored, 2, |_| Ok(())).unwrap();
        reconcile_at(&mut restored, 3, |_| Ok(())).unwrap();
        assert_eq!(bot_runs(&restored).len(), 1, "the persisted request is queued exactly once");
        assert!(restored.bots.requests.is_empty());
    }

    #[test]
    fn large_room_backlogs_are_offered_in_bounded_passes() {
        let (mut state, chat) = fixture();
        command_at(&mut state, &chat, "on", Now(0)).unwrap();
        for index in 0..600 {
            post(&mut state, Some("c-other"), &format!("message {index}"));
        }
        reconcile_at(&mut state, 1, |_| Ok(())).unwrap();
        assert_eq!(state.bots.room_cursor, POSTS_PER_PASS);
        reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
        reconcile_at(&mut state, 3, |_| Ok(())).unwrap();
        assert_eq!(state.bots.room_cursor, 600);
        assert_eq!(bot_runs(&state).len(), 1, "the backlog coalesces into one turn");
    }

    #[test]
    fn bot_commands_through_chat_roll_back_when_the_save_fails() {
        let (mut state, chat) = fixture();
        let send = serde_json::json!({"id": chat, "message": "/bot on every 10"});
        assert!(crate::session_http::change_with_save(&mut state, "control.chat.send", &send, |_| Err(std::io::Error::other("disk"))).is_err());
        assert!(!defined(&state, &chat), "a failed save leaves no bot behind");
        crate::session_http::change_with_save(&mut state, "control.chat.send", &send, |_| Ok(())).unwrap();
        assert_eq!(status(&state, &chat).as_deref(), Some("Bot · sleeping · room #general, every 10 min"));
        let listen = serde_json::json!({"id": chat, "message": "/listen"});
        assert!(crate::session_http::change_with_save(&mut state, "control.chat.send", &listen, |_| Ok(())).is_err(), "/listen refuses while a bot runs");
        let off = serde_json::json!({"id": chat, "message": "/listen off"});
        assert!(crate::session_http::change_with_save(&mut state, "control.chat.send", &off, |_| Ok(())).is_err(), "/listen off cannot remove a bot's inbox");
        assert!(state.collective.room_watches.iter().any(|watch| watch.chat == chat));
    }

    #[test]
    fn idle_passes_skip_work_until_an_event_or_deadline() {
        let (mut state, chat) = fixture();
        command_at(&mut state, &chat, "on every 5", Now(0)).unwrap();
        let mut saves = 0;
        reconcile_at(&mut state, 60_000, |_| { saves += 1; Ok(()) }).unwrap();
        assert_eq!(saves, 0, "a sleeping bot with nothing due does no work");
        post(&mut state, Some("c-other"), "news");
        reconcile_at(&mut state, 61_000, |_| { saves += 1; Ok(()) }).unwrap();
        assert_eq!(saves, 1, "new room posts are offered");
    }

    fn requester() -> (State, String, String) {
        let (mut state, parent) = fixture();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        run.attempts.push(crate::usage::Attempt { workspace_id: Some("attempt-a".into()), ..Default::default() });
        let child = spawned(&mut state, &parent, "Farm agent");
        (state, parent, child)
    }
    fn spawned(state: &mut State, parent: &str, title: &str) -> String {
        let child = crate::chat::create(state);
        let session = state.chats.iter_mut().find(|session| session.id == child).unwrap();
        session.parent_chat = Some(parent.into());
        session.room = Some("general".into());
        crate::chat::send(state, &child, "Farm wheat", "test").unwrap();
        state.chats.iter_mut().find(|session| session.id == child).unwrap().title = title.into();
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        child
    }
    fn ask(state: &mut State, nonce: &str, chat: &str, every_minutes: Option<u64>) -> io::Result<Value> {
        let run = state.runtime.runs[0].id.clone();
        let attempt = AttemptRef { run: &run, generation: "attempt-a", nonce };
        handle(state, &attempt, BotCall::Request { chat_id: chat.into(), every_minutes, room: true }, |_| Ok(()))
    }
    fn finish_requesting_turn(state: &mut State) {
        state.runtime.runs[0].status = "completed".into();
    }

    #[test]
    fn an_agent_requests_bots_for_its_children_and_resumes_with_the_operators_decisions() {
        let (mut state, parent, child) = requester();
        let other = spawned(&mut state, &parent, "Mine agent");
        let first = ask(&mut state, "n1", &child, None).unwrap();
        let id = first["request_id"].as_str().unwrap().to_owned();
        assert_eq!(ask(&mut state, "n1", &child, None).unwrap()["request_id"], first["request_id"], "retries are idempotent");
        assert!(ask(&mut state, "n2", &child, None).is_err(), "one pending request per chat");
        let second = ask(&mut state, "n3", &other, Some(10)).unwrap()["request_id"].as_str().unwrap().to_owned();
        assert!(!defined(&state, &child), "a request grants nothing");
        assert!(pending_panel(&state, &parent).contains(&format!("/bot allow {id}")));
        assert_eq!(pending_json(&state, &parent).as_array().unwrap().len(), 2);

        crate::chat::send(&mut state, &parent, &format!("/bot allow {id}"), "discord").unwrap();
        assert!(defined(&state, &child));
        finish_requesting_turn(&mut state);
        let runs = state.runtime.runs.len();
        reconcile_at(&mut state, 10, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(), runs, "the requester waits until every request is decided");

        command_at(&mut state, &other, &format!("deny {second}"), Now(20)).unwrap();
        assert!(!defined(&state, &other));
        reconcile_at(&mut state, 30, |_| Ok(())).unwrap();
        let resumed = state.runtime.runs.last().unwrap();
        assert_eq!((resumed.kind.as_str(), resumed.chat_id.as_deref()), ("bot-decision", Some(parent.as_str())));
        assert!(resumed.objective.contains("Farm agent") && resumed.objective.contains("approved") && resumed.objective.contains("denied"));
        assert!(pending_json(&state, &parent).as_array().unwrap().is_empty());
        let runs = state.runtime.runs.len();
        reconcile_at(&mut state, 40, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(), runs, "decisions resume the requester once");
    }

    #[test]
    fn bots_may_request_bots_only_within_their_own_lineage() {
        let (mut state, parent, child) = requester();
        let grandchild = spawned(&mut state, &child, "Scout");
        assert!(ask(&mut state, "n1", &grandchild, None).is_ok(), "descendants through children");
        let stranger = crate::chat::create(&mut state);
        state.chats.last_mut().unwrap().room = Some("general".into());
        crate::chat::send(&mut state, &stranger, "Unrelated", "test").unwrap();
        assert!(ask(&mut state, "n2", &stranger, None).is_err(), "unrelated chats");
        state.chats.iter_mut().find(|session| session.id == child).unwrap().room = None;
        assert!(ask(&mut state, "n3", &child, None).is_err(), "no room and no timer");
        assert!(ask(&mut state, "n4", &child, Some(0)).is_err());
        assert!(ask(&mut state, "n5", &child, Some(5)).is_ok());
        assert!(ask(&mut state, "bad nonce", &parent, Some(5)).is_err());
        state.runtime.runs[0].attempts.last_mut().unwrap().workspace_id = Some("attempt-b".into());
        assert!(ask(&mut state, "n6", &parent, Some(5)).is_err(), "stale attempts");
        let first = format!("allow {}", state.bots.approvals[0].id);
        assert!(command_at(&mut state, &stranger, &first, Now(1)).is_err(), "decided only from the requesting or target chat");
    }

    #[test]
    fn decisions_for_a_superseded_requester_are_recorded_without_a_new_turn() {
        let (mut state, parent, child) = requester();
        let id = ask(&mut state, "n1", &child, None).unwrap()["request_id"].as_str().unwrap().to_owned();
        finish_requesting_turn(&mut state);
        crate::chat::send(&mut state, &parent, "Something else", "test").unwrap();
        state.runtime.runs.last_mut().unwrap().status = "completed".into();
        command_at(&mut state, &parent, &format!("deny {id}"), Now(1)).unwrap();
        let runs = state.runtime.runs.len();
        reconcile_at(&mut state, 2, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.runs.len(), runs);
        let session = state.chats.iter().find(|session| session.id == parent).unwrap();
        assert!(session.messages.last().unwrap().text.contains("denied"));
        assert!(state.bots.approvals.iter().all(|a| a.acknowledged));
    }

    #[test]
    fn the_chat_panel_route_decides_and_rolls_back_failed_saves() {
        let (mut state, parent, child) = requester();
        let id = ask(&mut state, "n1", &child, None).unwrap()["request_id"].as_str().unwrap().to_owned();
        let shared = Arc::new(Mutex::new(state));
        let body = format!("id={id}&return_chat={parent}&allow=yes&response=json");
        let mut out = Vec::new();
        assert!(http_with_save(&mut out, &shared, "POST", "/tools/bots", body.as_bytes(), |_| Err(io::Error::other("disk full"))));
        assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"));
        assert!(!defined(&shared.lock().unwrap(), &child), "a failed save leaves no bot behind");
        let mut out = Vec::new();
        http_with_save(&mut out, &shared, "POST", "/tools/bots", body.as_bytes(), |_| Ok(()));
        assert!(String::from_utf8_lossy(&out).contains("\"ok\":true"));
        assert!(defined(&shared.lock().unwrap(), &child));
        let mut out = Vec::new();
        http_with_save(&mut out, &shared, "POST", "/tools/bots", format!("id={id}&return_chat={parent}&deny=yes").as_bytes(), |_| Ok(()));
        assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"), "decided requests are not pending");
        assert!(!http_with_save(&mut Vec::new(), &shared, "POST", "/tools/ports", b"", |_| Ok(())));
    }
}
