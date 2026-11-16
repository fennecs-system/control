//! What the common checkpoint costs: a persistent bot woken by room posts
//! processes them and waits for more. A scripted agent reads callbacks only
//! when its wake did not carry them, hands off with the topic name it knows
//! (the room's bare name, the commonest refusal in run -128) and follows each
//! refusal's fix by re-reading the context for wake_topics. The counts are
//! the calls that protocol requires, not a model's behaviour.
use super::*;
use serde_json::json;

#[derive(Debug, Default)]
struct Calls {
    turns: usize,
    context: usize,
    handoff: usize,
    refused: usize,
}
fn ok(_: &State) -> io::Result<()> {
    Ok(())
}
/// Admits the queued wake, runs the scripted agent and publishes its handoff
/// as the worker's finish transaction would.
fn turn(state: &mut State, chat: &str, posts: &[String], calls: &mut Calls) -> Value {
    let index = crate::worker::checkpoint_next(state, ok).expect("a wake was queued");
    assert_eq!(state.runtime.runs[index].kind, RunKind::BotWake);
    let run = state.runtime.runs[index].id.clone();
    let attempt = state.runtime.runs[index].attempts.last().unwrap();
    let generation = attempt.workspace_id.clone().unwrap();
    let projection = crate::turn_context::input(attempt).unwrap().unwrap();
    let mut nonce = 0;
    let mut call = |state: &mut State, name: &str, args: Value| {
        nonce += 1;
        crate::turn_context::tool_call(state, &run, &generation, &format!("call-{nonce}"), name, &args)
    };
    let mut receipts = Vec::new();
    if !posts.iter().all(|post| projection.contains(post.as_str())) {
        calls.context += 1;
        let page = call(state, "control_turn_context", json!({"source":"room"})).unwrap();
        receipts.push(page["receipt"].clone());
    }
    let chat_index = state.chats.iter().position(|c| c.id == chat).unwrap();
    let revision = state.chats[chat_index].continuity.as_ref().unwrap().working.as_ref().map_or(0, |w| w.revision);
    let payload = json!({"subgoal":"Coordinate with the room","findings":[],"tried":[],"pending":[],"next":[]});
    // Send only what the advertised schema requires, plus the wait.
    let required = {
        use crate::tool_registry::AttemptTools;
        crate::turn_context::Tools.definitions().into_iter().find(|d| d["name"] == "control_turn_handoff").unwrap()["inputSchema"]["required"].clone()
    };
    let requires = |field: &str| required.as_array().unwrap().iter().any(|r| r == field);
    let mut topics = json!(["general"]);
    let accepted = loop {
        calls.handoff += 1;
        let mut handoff = json!({"disposition":{"kind":"wait","topics":topics}});
        for (field, value) in [("base_revision", json!(revision)), ("payload", payload.clone()), ("processed", json!(receipts))] {
            if requires(field) {
                handoff[field] = value;
            }
        }
        match call(state, "control_turn_handoff", handoff) {
            Ok(accepted) => break accepted,
            Err(_) => {
                calls.refused += 1;
                assert!(calls.refused < 16, "the scripted agent cannot recover");
                calls.context += 1;
                let page = call(state, "control_turn_context", json!({"source":"room"})).unwrap();
                topics = json!(page["wake_topics"].as_array().unwrap().iter().filter(|t| t.as_str().unwrap().ends_with("general")).collect::<Vec<_>>());
            }
        }
    };
    let mailbox = state.runtime.runs[index].attempts.last().unwrap().turn_mailbox.as_ref().cloned().unwrap();
    let previous = state.chats[chat_index].continuity.as_ref().unwrap().working.clone();
    let working = mailbox.publication(previous.as_ref(), &run, &generation, "published", |_| true).unwrap().unwrap();
    crate::rooms::processed(state, chat, &working.processed);
    state.chats[chat_index].continuity.as_mut().unwrap().working = Some(working);
    state.chats[chat_index].workspace = Some("published".into());
    state.runtime.runs[index].status = "completed".into();
    calls.turns += 1;
    accepted
}

#[test]
fn a_room_wake_needs_one_handoff_and_no_context_read() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    state.chats[0].continuity = Some(Box::default());
    let mut calls = Calls::default();
    let mut now = 1_000;
    let mut last = Value::Null;
    for round in 0..5 {
        let posts: Vec<String> = (0..3).map(|i| format!("peer update {round}.{i}")).collect();
        for text in &posts {
            post(&mut state, Some("peer"), text);
        }
        reconcile_at(&mut state, now, ok).unwrap();
        last = turn(&mut state, &chat, &posts, &mut calls);
        now += 31_000;
        reconcile_at(&mut state, now, ok).unwrap();
        // Every post the wake carried is committed; none wakes the bot again.
        let processed = state.chats[0].continuity.as_ref().unwrap().working.as_ref().unwrap().processed.clone();
        assert!(processed.iter().any(|r| matches!(r, control_context::turn_mailbox::Range::Room { through, .. } if *through as usize == state.collective.posts.len())), "{processed:?}");
        assert!(state.runtime.runs.iter().all(|r| r.status != "queued"), "processed posts woke the bot again");
    }
    eprintln!(
        "handoff cost over {} turns: control_turn_context {} ({:.1}/turn), control_turn_handoff {} ({:.1}/turn), refused {}",
        calls.turns, calls.context, calls.context as f64 / calls.turns as f64, calls.handoff, calls.handoff as f64 / calls.turns as f64, calls.refused
    );
    assert_eq!((calls.turns, calls.context, calls.handoff, calls.refused), (5, 0, 5, 0), "{calls:?}");
    // The bare room name was read as its subscription, and the result says so.
    assert_eq!(last["disposition"]["topics"], json!(["room:general"]), "{last}");
    assert!(last["adjusted"].to_string().contains("read general as room:general"), "{last}");
    // One receipt was acknowledged: the wake's own delivery.
    assert_eq!(last["processed"], 1, "{last}");
}
