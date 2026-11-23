//! A persistent bot's failed or interrupted turn (a transient provider error,
//! say) ends that turn, not the bot. Peer coordination sent in the gap before
//! its supervisor's next wake is retained for that wake, never dropped.
use super::*;
use crate::runtime::RunStatus;

const TEXT: &str = "Your iron ingots are in the chest";

/// Alice (running) and Cass (an approved persistent bot) under one lead.
fn peers() -> (State, String, String, usize) {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    state.runtime.runs[0].status = RunStatus::Completed;
    let alice_run = activate(&mut state, &alice);
    for chat in &mut state.chats {
        chat.room = Some("general".into());
    }
    crate::bots::command(&mut state, &cass, "on").unwrap();
    let index = crate::chat::latest_task(&state, &cass, &state.chats[0].actor).unwrap();
    (state, alice_run, cass, index)
}
fn end_turn(state: &mut State, index: usize, status: &str) {
    let run = &mut state.runtime.runs[index];
    run.status = RunStatus::named(status);
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("cass-attempt".into()),
        termination: Some(crate::attempt_control::Termination::ProviderError),
        ..Default::default()
    });
}
fn message(state: &mut State, run: &str, nonce: &str, chat: &str) -> io::Result<Value> {
    handle(
        state,
        &AttemptRef {
            run,
            generation: "peer-attempt",
            nonce,
        },
        MessageCall {
            chat_id: chat.into(),
            text: TEXT.into(),
            services: vec![],
            urgent: true,
        },
        |_| Ok(()),
    )
}
fn refused(state: &mut State, run: &str, chat: &str) -> String {
    let before = state.runtime.runs.len();
    let error = handle(
        state,
        &AttemptRef {
            run,
            generation: "peer-attempt",
            nonce: "refused",
        },
        MessageCall {
            chat_id: chat.into(),
            text: TEXT.into(),
            services: vec![],
            urgent: true,
        },
        |_| panic!("refused messages must not save"),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(state.runtime.runs.len(), before);
    error
}

#[test]
fn a_persistent_bot_between_failed_turns_retains_peer_messages_for_its_next_wake() {
    for status in ["interrupted", "failed"] {
        let (mut state, alice_run, cass, index) = peers();
        end_turn(&mut state, index, status);
        let sent = message(&mut state, &alice_run, "ingots", &cass).unwrap();
        assert_eq!(
            (sent["queued"].as_bool(), sent["retained"].as_bool()),
            (Some(false), Some(true)),
            "{status}"
        );
        let delivered = state.runtime.runs.last().unwrap();
        assert_eq!(
            (delivered.source.as_str(), delivered.status.as_str()),
            ("peer-message", "completed"),
            "{status}: retained evidence, never a restarted run"
        );
        assert!(delivered.attempts.is_empty());
        assert_eq!(
            state.runtime.runs[index].status,
            RunStatus::named(status),
            "the ended turn is not revived"
        );
        assert!(
            !state.runtime.runs.iter().any(|run| run.status == RunStatus::Queued),
            "{status}: the message starts nothing"
        );

        // The supervisor, not the message, owns the wake: the conversation topic
        // admits one turn whose context carries the note.
        crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
        let woken = state
            .runtime
            .runs
            .iter()
            .position(|run| {
                run.bot_turn.is_some()
                    && run.chat_id.as_deref() == Some(cass.as_str())
                    && run.status == RunStatus::Queued
            })
            .unwrap_or_else(|| panic!("{status}: the retained message wakes the bot"));
        assert_eq!(crate::scheduler::next_in(&state), Some(woken));
        let trace = crate::turn_context::PlannedContext::prepare(&state, woken)
            .unwrap()
            .persist()
            .unwrap();
        let context = crate::turn_context::read(&trace).unwrap().text;
        assert!(context.contains(TEXT), "{status}: the next wake sees the message");
        crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
        assert_eq!(
            state.runtime.runs.iter().filter(|run| run.bot_turn.is_some()).count(),
            1,
            "one wake"
        );
    }
}

#[test]
fn a_message_after_another_provider_error_reaches_the_following_wake() {
    let (mut state, alice_run, cass, index) = peers();
    end_turn(&mut state, index, "interrupted");
    message(&mut state, &alice_run, "first", &cass).unwrap();
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    let woken = crate::scheduler::next_in(&state).expect("the bot wakes");
    assert!(state.runtime.runs[woken].bot_turn.is_some());
    // That wake hits another provider error; a peer writes in the gap.
    state.runtime.runs[woken].status = RunStatus::Running;
    end_turn(&mut state, woken, "interrupted");
    let sent = message(&mut state, &alice_run, "second", &cass).unwrap();
    assert_eq!(sent["retained"], true);
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(
        !state.runtime.runs.iter().any(|run| run.status == RunStatus::Queued),
        "the message itself starts nothing; the bot is in its turn cooldown"
    );
    crate::bots::reconcile_after_cooldown(&mut state).unwrap();
    let next = state
        .runtime
        .runs
        .iter()
        .rposition(|run| run.bot_turn.is_some() && run.status == RunStatus::Queued)
        .expect("the later message is admitted on the following wake");
    assert!(next > woken);
    let trace = crate::turn_context::PlannedContext::prepare(&state, next)
        .unwrap()
        .persist()
        .unwrap();
    let context = crate::turn_context::read(&trace).unwrap().text;
    assert_eq!(
        context.matches(TEXT).count(),
        2,
        "both notes are in the following wake's context"
    );
}

#[test]
fn stopped_cancelled_rescoped_and_ordinary_recipients_still_refuse() {
    // An ordinary worker's failure is still stopped work.
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    let alice_run = activate(&mut state, &alice);
    let index = crate::chat::latest_task(&state, &cass, &state.chats[0].actor).unwrap();
    for status in ["interrupted", "failed"] {
        end_turn(&mut state, index, status);
        assert!(
            refused(&mut state, &alice_run, &cass).contains("cannot restart stopped work"),
            "{status}"
        );
    }

    // Cancelled work stays cancelled, even for a bot.
    let (mut state, alice_run, cass, index) = peers();
    end_turn(&mut state, index, "cancelled");
    assert!(refused(&mut state, &alice_run, &cass).contains("cannot restart stopped work"));

    // An action-scope change is refused.
    end_turn(&mut state, index, "interrupted");
    state.runtime.runs[index].action = identity::Action::Improve;
    assert!(refused(&mut state, &alice_run, &cass).contains("action scope"));
    state.runtime.runs[index].action = identity::Action::Task;

    // A judge recipient is refused before bot liveness is considered.
    let mut persona = crate::delegation::lineage(&state.runtime.runs[index]);
    persona.role = crate::delegation::Role::Judge;
    state.runtime.runs[index].persona = Some(persona);
    assert!(refused(&mut state, &alice_run, &cass).contains("judges"));
    state.runtime.runs[index].persona = None;

    // `/bot off` withdraws the exception immediately.
    crate::bots::command(&mut state, &cass, "off").unwrap();
    assert!(refused(&mut state, &alice_run, &cass).contains("cannot restart stopped work"));

    // So does a stop that is still waiting for its outstanding turn.
    let (mut state, alice_run, cass, index) = peers();
    end_turn(&mut state, index, "interrupted");
    message(&mut state, &alice_run, "wake", &cass).unwrap();
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    let woken = crate::scheduler::next_in(&state).unwrap();
    state.runtime.runs[woken].status = RunStatus::Running;
    crate::bots::command(&mut state, &cass, "off").unwrap();
    assert!(
        crate::bots::defined(&state, &cass),
        "stopping until the running turn ends"
    );
    state.runtime.runs[woken].status = RunStatus::Interrupted;
    assert!(refused(&mut state, &alice_run, &cass).contains("cannot restart stopped work"));
}

#[test]
fn a_lead_messaging_its_running_persistent_bot_member_queues_no_turns() {
    // One live run: sixteen lead messages to a busy member bot became sixteen
    // queued turns behind its running one, unseen for the whole turn.
    let (mut state, lead, child, _) = family();
    for chat in &mut state.chats {
        chat.room = Some("general".into());
    }
    crate::bots::command(&mut state, &child, "on").unwrap();
    let index = crate::chat::latest_task(&state, &child, &state.chats[0].actor).unwrap();
    state.runtime.runs[index].status = RunStatus::Running;
    let lead_run = state.runtime.runs[0].id().as_str().to_owned();
    let before = state.runtime.runs.len();
    for nonce in ["one", "two", "three"] {
        let sent = handle(
            &mut state,
            &AttemptRef {
                run: &lead_run,
                generation: "attempt-a",
                nonce,
            },
            MessageCall {
                chat_id: child.clone(),
                text: TEXT.into(),
                services: vec![],
                urgent: true,
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            (sent["queued"].as_bool(), sent["retained"].as_bool()),
            (Some(false), Some(true))
        );
    }
    let delivered = &state.runtime.runs[before..];
    assert_eq!(delivered.len(), 3);
    assert!(
        delivered
            .iter()
            .all(|run| run.source == "agent-message" && run.status == RunStatus::Completed && run.attempts.is_empty()),
        "retained evidence for the running session and the next wake, not queued turns"
    );
    let messages = &state.chats.iter().find(|chat| chat.id == child).unwrap().messages;
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.text.starts_with(&format!("Message from {lead}")))
            .count(),
        3
    );
    // Once the running turn ends, the supervisor coalesces them into one wake.
    state.runtime.runs[index].status = RunStatus::Completed;
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert_eq!(
        state
            .runtime
            .runs
            .iter()
            .filter(|run| run.bot_turn.is_some() && run.chat_id.as_deref() == Some(child.as_str()))
            .count(),
        1,
        "one wake"
    );
    // A member that is not a persistent bot still gets its requested turn.
    let (mut state, _, child, _) = family();
    let lead_run = state.runtime.runs[0].id().as_str().to_owned();
    handle(
        &mut state,
        &AttemptRef {
            run: &lead_run,
            generation: "attempt-a",
            nonce: "plain",
        },
        MessageCall {
            chat_id: child,
            text: TEXT.into(),
            services: vec![],
            urgent: true,
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(state.runtime.runs.last().unwrap().status, RunStatus::Queued);
}
