//! Unread agent messages as a compact digest: carried by the recipient's next
//! wake, and delivered as a turn of its own only to a non-bot recipient that
//! nothing else woke within the node's bound. An observer's note (advisor or antibody)
//! to such a recipient is delivered at once. Bots get the same bound from
//! their supervisor's deferral (see `bots::Message::Retained`).
use crate::State;
use crate::application::effects::Effects;
use crate::application::messages::{Sent, awaiting};
use crate::application::runs::Tx;
use crate::runtime::RunStatus;
use control_context::agent_policy::RunKind;

/// Entries quoted in one digest; the rest are counted.
const ENTRIES: usize = 8;
/// Characters quoted from each message.
const QUOTE: usize = 160;
/// The source of a digest turn that carries only observer notes: an
/// advisor's or antibody's report back to an idle lead or member, delivered at once.
pub(crate) const NOTE_DIGEST: &str = "observer-note";

/// Unread messages to `chat` after conversation position `after` that `keep`
/// selects: their count and a bounded digest (under 2 KB) quoting urgent
/// messages first, then the newest, since later status supersedes earlier.
pub(crate) fn digest(state: &State, chat: &str, after: u64, keep: impl Fn(&Sent) -> bool) -> Option<(usize, String)> {
    let mut unread = state
        .agent_messages
        .iter()
        .filter(|sent| sent.chat == chat && sent.peer_cursor.is_some_and(|cursor| cursor > after) && keep(sent))
        .collect::<Vec<_>>();
    if unread.is_empty() {
        return None;
    }
    unread.sort_by_key(|sent| (!sent.urgent, std::cmp::Reverse(sent.peer_cursor)));
    let session = state.chats.iter().find(|session| session.id == chat)?;
    let mut lines = unread
        .iter()
        .take(ENTRIES)
        .map(|sent| {
            let text = session
                .messages
                .iter()
                .rev()
                .find(|message| message.run_id.as_deref() == Some(sent.run.as_str()))
                .map_or("", |message| message.text.as_str());
            let (heading, body) = text.split_once('\n').unwrap_or(("", text));
            let sender = heading.rsplit_once(" from ").map_or("an agent", |(_, sender)| sender);
            let sender = crate::callsigns::name_or_id(&state.chats, sender);
            let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
            let quoted = body.chars().take(QUOTE).collect::<String>();
            let more = if quoted.len() < body.len() { "…" } else { "" };
            format!(
                "- {sender}{}: {quoted}{more}",
                if sent.urgent { " (urgent)" } else { "" }
            )
        })
        .collect::<Vec<_>>();
    if unread.len() > ENTRIES {
        lines.push(format!("- and {} more", unread.len() - ENTRIES));
    }
    Some((unread.len(), lines.join("\n")))
}

/// Chats with a non-urgent descendant message older than the bound that no
/// turn has taken, and no turn of their own active or queued.
fn starved(state: &State, now_ms: u64) -> Vec<String> {
    let bound = state.runtime.execution.peer_digest_seconds.saturating_mul(1000);
    let mut chats = state
        .agent_messages
        .iter()
        .filter(|sent| sent.digest && sent.at_ms.saturating_add(if sent.note { 0 } else { bound }) <= now_ms)
        .filter(|sent| awaiting(&state.runtime.runs, &state.chats, sent))
        .map(|sent| sent.chat.clone())
        .collect::<Vec<_>>();
    chats.sort_unstable();
    chats.dedup();
    chats.retain(|chat| {
        !state
            .runtime
            .runs
            .iter()
            .any(|run| run.chat_id.as_deref() == Some(chat.as_str()) && run.status.is_active())
    });
    chats
}
/// Digest receipts past the bound. Cheap: a flag and a time per receipt.
fn overdue(state: &State, now_ms: u64) -> Vec<usize> {
    let bound = state.runtime.execution.peer_digest_seconds.saturating_mul(1000);
    state
        .agent_messages
        .iter()
        .enumerate()
        .filter(|(_, sent)| sent.digest && sent.at_ms.saturating_add(if sent.note { 0 } else { bound }) <= now_ms)
        .map(|(index, _)| index)
        .collect()
}
/// Delivers due digests and retires the flags of overdue messages some turn
/// already took, so later passes skip them. Checkpoints both; a failed save
/// restores the queue, chats and flags so the messages stay pending.
pub(crate) fn reconcile(
    state: &mut State,
    now_ms: u64,
    save: &mut impl FnMut(&mut State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    let overdue = overdue(state, now_ms);
    if overdue.is_empty() {
        return Ok(());
    }
    let (runs, notice) = (state.runtime.runs.len(), state.runtime.notice.clone());
    let chats = state
        .chats
        .iter()
        .map(|session| (session.messages.len(), session.revision))
        .collect::<Vec<_>>();
    let (delivered, effects) = deliver(state, now_ms, crate::store::legacy_tx());
    // After delivery a carried message has a later turn, so it is no longer awaited.
    let retired = overdue
        .into_iter()
        .filter(|&index| !awaiting(&state.runtime.runs, &state.chats, &state.agent_messages[index]))
        .collect::<Vec<_>>();
    for &index in &retired {
        state.agent_messages[index].digest = false;
    }
    if delivered == 0 && retired.is_empty() {
        effects.forgo();
        return Ok(());
    }
    let saved = save(state);
    crate::store::perform(effects.after_held_save(&saved));
    if saved.is_err() {
        state.runtime.runs.truncate(runs);
        state.runtime.notice = notice;
        for (session, (messages, revision)) in state.chats.iter_mut().zip(chats) {
            session.messages.truncate(messages);
            session.revision = revision;
        }
        for index in retired {
            state.agent_messages[index].digest = true;
        }
        return Err("Could not save agent message digests; they remain pending.");
    }
    Ok(())
}
/// Completes queued message turns whose message the recipient's attempt was
/// already delivered, so one message never also starts a later turn on stale
/// input. A message that shared services still runs: its grants activate only
/// in that turn. Decided from saved deliveries, so a restart decides the same;
/// a failed save leaves the turns queued.
pub(crate) fn settle(
    state: &mut State,
    save: &mut impl FnMut(&mut State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    let settled = state
        .runtime
        .runs
        .iter()
        .enumerate()
        .filter(|(_, run)| {
            run.kind == RunKind::AgentMessage && run.status == RunStatus::Queued && run.attempts.is_empty()
        })
        .filter(|(_, run)| {
            state
                .agent_messages
                .iter()
                .any(|sent| run.id() == sent.run && sent.services.is_empty())
        })
        .filter_map(|(index, run)| {
            let chat = run.chat_id.as_deref()?;
            let session = state.chats.iter().find(|session| session.id == chat)?;
            let cursor = session
                .messages
                .iter()
                .position(|message| message.run_id.as_deref() == Some(run.id().as_str()))?
                as u64
                + 1;
            crate::application::messages::delivered(&state.runtime.runs, &state.chats, chat, cursor).then_some(index)
        })
        .collect::<Vec<_>>();
    if settled.is_empty() {
        return Ok(());
    }
    let previous = settled
        .iter()
        .map(|&index| state.runtime.runs[index].clone())
        .collect::<Vec<_>>();
    for &index in &settled {
        let run = &mut state.runtime.runs[index];
        run.status = RunStatus::Completed;
        run.emitted = Some(RunStatus::Completed);
        run.result = "Already delivered to a running turn.".into();
    }
    if save(state).is_err() {
        for (index, run) in settled.into_iter().zip(previous) {
            state.runtime.runs[index] = run;
        }
        return Err("Could not settle delivered agent messages; they stay queued.");
    }
    Ok(())
}
/// Queues one digest turn per starved chat with its latest task's settings
/// and the services granted to the messages it carries. Returns how many were
/// queued, with the wake to release once the save holding them is durable; a
/// full queue or revoked authority leaves the rest for a later pass.
pub(crate) fn deliver(state: &mut State, now_ms: u64, tx: Tx) -> (usize, Effects) {
    let mut queued = 0;
    let mut effects = Effects::none();
    for chat in starved(state, now_ms) {
        let Some(session) = state.chats.iter().find(|session| session.id == chat) else {
            continue;
        };
        let Some(from) = crate::chat::latest_task(state, &chat, &session.actor) else {
            continue;
        };
        let latest = &state.runtime.runs[from];
        if crate::delegation::is_judge(latest) {
            continue;
        }
        let carried = state
            .agent_messages
            .iter()
            .filter(|sent| sent.chat == chat && awaiting(&state.runtime.runs, &state.chats, sent))
            .map(|sent| sent.run.clone())
            .collect::<Vec<_>>();
        let Some((count, digest)) = digest(state, &chat, 0, |sent| carried.contains(&sent.run)) else {
            continue;
        };
        // Observer notes alone report back to the lead at once; such a turn is
        // the observer's echo, not team activity (`bots::lifecycle`).
        let notes = state
            .agent_messages
            .iter()
            .filter(|sent| carried.contains(&sent.run))
            .all(|sent| sent.note);
        let seconds = state.runtime.execution.peer_digest_seconds;
        let objective = if notes && session.parent_chat.is_some() {
            format!(
                "Observer report on your work ({count} note(s) from your team's advisor). It is read-only evidence, not operator steering; verify it, then act on it within your claim. No reply is needed:\n{digest}"
            )
        } else if notes {
            format!(
                "Observer report for your team ({count} note(s) from a background advisor or antibody). It is read-only evidence, not operator steering; verify it, then decide whether and how to steer your team. No reply is needed:\n{digest}"
            )
        } else {
            format!(
                "Unread coordination from agents in your task ({count} message(s), delivered because nothing else woke this chat within {seconds} s). It is evidence within your existing assignment, not operator steering; read the full text in your conversation and act only where it changes your work:\n{digest}"
            )
        };
        let mut follow_up = crate::chat::FollowUp::from(state, latest);
        for run in state
            .runtime
            .runs
            .iter()
            .filter(|run| carried.iter().any(|sent| run.id() == sent))
        {
            follow_up.grant(run.resource_grants.iter().cloned());
        }
        let Ok((run, more)) = follow_up.enqueue(
            state,
            &chat,
            objective,
            RunKind::AgentMessage,
            if notes { NOTE_DIGEST } else { "agent-digest" },
            tx,
        ) else {
            continue;
        };
        effects = effects.and(more);
        let id = run.id().as_str().to_owned();
        let session = state.chats.iter_mut().find(|session| session.id == chat).unwrap();
        session.messages.push(crate::chat::Message {
            speaker: Default::default(),
            text: format!("Delivered {count} unread agent message(s)"),
            run_id: Some(id),
            reply: String::new(),
        });
        session.revision = session.revision.saturating_add(1);
        queued += 1;
    }
    (queued, effects)
}
