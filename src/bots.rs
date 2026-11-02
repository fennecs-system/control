//! Persistent bots: chats that sleep until events wake them. The pure
//! `control_bot::Supervisor` decides; this adapter feeds it room messages, turn
//! completions and time, and interprets its turn requests as ordinary queued
//! turns. Supervisor state, requested turns and queued runs share one checkpoint.
use crate::{State, identity};
use control_bot::{BotId, Budget, ChatId, Definition, Effect, Event, Reason, Source, Status, TurnId};
use control_effect::machine::{Machine, Now, Planned};
use serde::{Deserialize, Serialize};

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
        [] | ["on"] | ["on", "every", _] | ["every", _] => {
            let minutes = match words.last() {
                Some(value) if words.len() > 1 => Some(value.parse::<u64>().ok().filter(|m| (1..=MAX_INTERVAL_MINUTES).contains(m)).ok_or("Use /bot on every N, with N from 1 to 1440 minutes.")?),
                _ => None,
            };
            start(state, chat, minutes, now)
        }
        _ => Err("Use /bot on [every N], /bot pause, /bot resume or /bot off."),
    }
}

fn start(state: &mut State, chat: &str, minutes: Option<u64>, now: Now) -> Result<&'static str, &'static str> {
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
    if let Some(room) = session.room.as_deref().filter(|room| crate::rooms::valid(room)) {
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
    if state.bots.supervisor.definitions().next().is_none() && state.bots.requests.is_empty() {
        state.bots.room_cursor = state.collective.posts.len();
        return Ok(());
    }
    let previous = state.bots.clone();
    let (runs, notice) = (state.runtime.runs.len(), state.runtime.notice.clone());
    let chats = state.chats.iter().map(|session| (session.messages.len(), session.revision)).collect::<Vec<_>>();
    // Turns requested earlier and now finished; decided before new events so a
    // turn requested in this pass is never mistaken for a finished one.
    let index = state.runtime.runs.iter().enumerate().map(|(i, run)| (run.id.as_str(), i)).collect::<std::collections::HashMap<_, _>>();
    let tagged = state.runtime.runs.iter().enumerate()
        .filter_map(|(i, run)| run.bot_turn.as_ref().map(|turn| ((turn.bot.as_str(), turn.turn), i)))
        .collect::<std::collections::HashMap<_, _>>();
    // A turn that delegated or awaited a design answer continues in another run;
    // it finishes when that chain's leaf does. A broken chain releases the bot.
    let finished = state.bots.supervisor.outstanding()
        .filter(|(bot, _, turn)| match tagged.get(&(bot.0.as_str(), turn.0)) {
            Some(&run) => crate::rooms::continuation_leaf(state, run, &index).is_none_or(|leaf| terminal(&state.runtime.runs[leaf].status)),
            None => !state.bots.requests.iter().any(|request| &request.bot == *bot && &request.turn == *turn),
        })
        .map(|(bot, _, turn)| (bot.clone(), *turn))
        .collect::<Vec<_>>();
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
    if !(offered || completed || planned || queued) {
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
}
