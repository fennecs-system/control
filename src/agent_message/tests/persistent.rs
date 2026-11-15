//! A persistent bot's failed or interrupted turn (a transient provider error,
//! say) ends that turn, not the bot. Peer coordination sent in the gap before
//! its supervisor's next wake is retained for that wake, never dropped.
use super::*;

const TEXT: &str = "Your iron ingots are in the chest";

/// Alice (running) and Cass (an approved persistent bot) under one lead.
fn peers() -> (State, String, String, usize) {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    state.runtime.runs[0].status = "completed".into();
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
    run.status = status.into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("cass-attempt".into()),
        termination: Some(crate::attempt_control::Termination::ProviderError),
        ..Default::default()
    });
}
fn message(state: &mut State, run: &str, nonce: &str, chat: &str) -> io::Result<Value> {
    handle(state, &AttemptRef { run, generation: "peer-attempt", nonce },
        MessageCall { chat_id: chat.into(), text: TEXT.into(), services: vec![] }, |_| Ok(()))
}
fn refused(state: &mut State, run: &str, chat: &str) -> String {
    let before = state.runtime.runs.len();
    let error = handle(state, &AttemptRef { run, generation: "peer-attempt", nonce: "refused" },
        MessageCall { chat_id: chat.into(), text: TEXT.into(), services: vec![] },
        |_| panic!("refused messages must not save")).unwrap_err().to_string();
    assert_eq!(state.runtime.runs.len(), before);
    error
}

#[test]
fn a_persistent_bot_between_failed_turns_retains_peer_messages_for_its_next_wake() {
    for status in ["interrupted", "failed"] {
        let (mut state, alice_run, cass, index) = peers();
        end_turn(&mut state, index, status);
        let sent = message(&mut state, &alice_run, "ingots", &cass).unwrap();
        assert_eq!((sent["queued"].as_bool(), sent["retained"].as_bool()), (Some(false), Some(true)), "{status}");
        let delivered = state.runtime.runs.last().unwrap();
        assert_eq!((delivered.source.as_str(), delivered.status.as_str()), ("peer-message", "completed"),
            "{status}: retained evidence, never a restarted run");
        assert!(delivered.attempts.is_empty());
        assert_eq!(state.runtime.runs[index].status, status, "the ended turn is not revived");
        assert!(!state.runtime.runs.iter().any(|run| run.status == "queued"), "{status}: the message starts nothing");

        // The supervisor, not the message, owns the wake: the conversation topic
        // admits one turn whose context carries the note.
        crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
        let woken = state.runtime.runs.iter().position(|run| run.bot_turn.is_some()
            && run.chat_id.as_deref() == Some(cass.as_str()) && run.status == "queued")
            .unwrap_or_else(|| panic!("{status}: the retained message wakes the bot"));
        assert_eq!(crate::scheduler::next_in(&state), Some(woken));
        let trace = crate::turn_context::PlannedContext::prepare(&state, woken).unwrap().persist().unwrap();
        let context = crate::turn_context::read(&trace).unwrap().text;
        assert!(context.contains(TEXT), "{status}: the next wake sees the message");
        crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.runs.iter().filter(|run| run.bot_turn.is_some()).count(), 1, "one wake");
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
    state.runtime.runs[woken].status = "running".into();
    end_turn(&mut state, woken, "interrupted");
    let sent = message(&mut state, &alice_run, "second", &cass).unwrap();
    assert_eq!(sent["retained"], true);
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    assert!(!state.runtime.runs.iter().any(|run| run.status == "queued"), "the message itself starts nothing; the bot is in its turn cooldown");
    crate::bots::reconcile_after_cooldown(&mut state).unwrap();
    let next = state.runtime.runs.iter().rposition(|run| run.bot_turn.is_some() && run.status == "queued")
        .expect("the later message is admitted on the following wake");
    assert!(next > woken);
    let trace = crate::turn_context::PlannedContext::prepare(&state, next).unwrap().persist().unwrap();
    let context = crate::turn_context::read(&trace).unwrap().text;
    assert_eq!(context.matches(TEXT).count(), 2, "both notes are in the following wake's context");
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
        assert!(refused(&mut state, &alice_run, &cass).contains("cannot restart stopped work"), "{status}");
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
    state.runtime.runs[woken].status = "running".into();
    crate::bots::command(&mut state, &cass, "off").unwrap();
    assert!(crate::bots::defined(&state, &cass), "stopping until the running turn ends");
    state.runtime.runs[woken].status = "interrupted".into();
    assert!(refused(&mut state, &alice_run, &cass).contains("cannot restart stopped work"));
}
