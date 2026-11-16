//! Agent messages wake nobody unless urgent. Retained messages ride with the
//! recipient's next wake as an unread digest, and an unattended recipient gets
//! that digest within the node's bound instead of being starved.
use super::*;

const STATUS: &str = "Verified 090 deposited 5 oak logs in the shared chest";

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}
fn send(state: &mut State, run: &str, generation: &str, nonce: &str, chat: &str, text: &str, urgent: bool) -> Value {
    handle(state, &AttemptRef { run, generation, nonce },
        MessageCall { chat_id: chat.into(), text: text.into(), services: vec![], urgent }, |_| Ok(())).unwrap()
}
fn queued(state: &State) -> usize {
    state.runtime.runs.iter().filter(|run| run.status == "queued").count()
}
fn wakes<'a>(state: &'a State, chat: &str) -> Vec<&'a crate::runtime::Run> {
    state.runtime.runs.iter().filter(|run| run.bot_turn.is_some() && run.chat_id.as_deref() == Some(chat)).collect()
}
/// Alice (running) and Cass (an approved, idle persistent bot) under one lead.
fn bot_peer() -> (State, String, String) {
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    state.runtime.runs[0].status = "completed".into();
    let alice_run = activate(&mut state, &alice);
    for chat in &mut state.chats {
        chat.room = Some("general".into());
    }
    crate::bots::command(&mut state, &cass, "on").unwrap();
    (state, alice_run, cass)
}

#[test]
fn only_urgent_messages_wake_and_only_where_they_could_before() {
    // A descendant that is not a bot: retained unless urgent.
    let (mut state, lead, child, _) = family();
    let lead_run = state.runtime.runs[0].id.clone();
    let quiet = send(&mut state, &lead_run, "attempt-a", "quiet", &child, STATUS, false);
    assert_eq!((quiet["queued"].as_bool(), quiet["retained"].as_bool(), quiet.get("urgent")), (Some(false), Some(true), None));
    let record = state.runtime.runs.last().unwrap();
    assert_eq!((record.status.as_str(), record.source.as_str()), ("completed", "agent-message"));
    assert!(record.attempts.is_empty());
    assert_eq!(queued(&state), 0, "a status update queues no turn");
    let loud = send(&mut state, &lead_run, "attempt-a", "loud", &child, "Stop: the chest moved", true);
    assert_eq!((loud["queued"].as_bool(), loud["urgent"].as_bool()), (Some(true), Some(true)));
    assert_eq!(queued(&state), 1, "an urgent message to a descendant queues its follow-up");
    let _ = lead;

    // A sibling that is not a bot is never woken by a peer, urgent or not.
    let (mut state, lead, alice, _) = family();
    let cass = spawned(&mut state, &lead);
    state.runtime.runs[0].status = "completed".into();
    let alice_run = activate(&mut state, &alice);
    for (nonce, urgent) in [("a", false), ("b", true)] {
        let sent = send(&mut state, &alice_run, "peer-attempt", nonce, &cass, STATUS, urgent);
        assert_eq!(sent["retained"], true);
    }
    crate::bots::reconcile_at_ms(&mut state, now() + 3_600_000).unwrap();
    assert_eq!(queued(&state), 0, "peers cannot grant an idle non-bot a turn");

    // A persistent bot: non-urgent stays asleep, urgent wakes.
    let (mut state, alice_run, cass) = bot_peer();
    send(&mut state, &alice_run, "peer-attempt", "status", &cass, STATUS, false);
    let start = now();
    crate::bots::reconcile_at_ms(&mut state, start).unwrap();
    crate::bots::reconcile_at_ms(&mut state, start + 31_000).unwrap();
    assert!(wakes(&state, &cass).is_empty(), "a status record does not wake a bot");
    send(&mut state, &alice_run, "peer-attempt", "request", &cass, "Please smelt the ore now", true);
    crate::bots::reconcile_at_ms(&mut state, start + 32_000).unwrap();
    let woken = wakes(&state, &cass);
    assert_eq!(woken.len(), 1, "an urgent request wakes the bot");
    let objective = woken[0].objective.as_str();
    assert!(objective.contains("Unread agent messages (2;"), "the wake carries both as a digest: {objective}");
    assert!(objective.contains(STATUS) && objective.contains("(urgent): Please smelt"));
}

#[test]
fn a_sleeping_bot_gets_retained_messages_within_the_digest_bound() {
    let (mut state, alice_run, cass) = bot_peer();
    state.runtime.execution.peer_digest_seconds = 60;
    send(&mut state, &alice_run, "peer-attempt", "status", &cass, STATUS, false);
    let start = now();
    crate::bots::reconcile_at_ms(&mut state, start).unwrap();
    crate::bots::reconcile_at_ms(&mut state, start + 59_000).unwrap();
    assert!(wakes(&state, &cass).is_empty());
    crate::bots::reconcile_at_ms(&mut state, start + 60_000).unwrap();
    let woken = wakes(&state, &cass);
    assert_eq!(woken.len(), 1, "the bound delivers the digest");
    assert!(woken[0].objective.contains("Unread agent messages (1;") && woken[0].objective.contains(STATUS));
}

#[test]
fn an_unattended_descendant_gets_one_digest_turn_after_the_bound() {
    let (mut state, lead, child, _) = family();
    state.runtime.execution.peer_digest_seconds = 30;
    let lead_run = state.runtime.runs[0].id.clone();
    for (nonce, text) in [("one", STATUS), ("two", "Second update: furnace is lit")] {
        send(&mut state, &lead_run, "attempt-a", nonce, &child, text, false);
    }
    let sent = now();
    crate::bots::reconcile_at_ms(&mut state, sent).unwrap();
    assert_eq!(queued(&state), 0, "within the bound nothing is queued");

    // A failed checkpoint leaves the digest pending and the state unchanged.
    let before = serde_json::to_vec(&state).unwrap();
    let mut save = |_: &State| Err(std::io::Error::other("disk full"));
    assert!(crate::agent_message::digest::reconcile(&mut state, sent + 31_000, &mut save).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);

    crate::bots::reconcile_at_ms(&mut state, sent + 31_000).unwrap();
    let digest = state.runtime.runs.iter().filter(|run| run.source == "agent-digest").collect::<Vec<_>>();
    assert_eq!(digest.len(), 1, "one coalesced turn, not one per message");
    let run = digest[0];
    assert_eq!((run.status.as_str(), run.kind, run.chat_id.as_deref()), ("queued", RunKind::AgentMessage, Some(child.as_str())));
    assert!(run.objective.contains("(2 message(s)") && run.objective.contains(STATUS) && run.objective.contains("furnace is lit"));
    crate::bots::reconcile_at_ms(&mut state, sent + 3_600_000).unwrap();
    assert_eq!(state.runtime.runs.iter().filter(|run| run.source == "agent-digest").count(), 1, "delivered once");
    assert!(state.agent_messages.iter().all(|sent| !sent.digest), "delivered messages leave the overdue scan");

    // Any other turn of the recipient takes the messages instead.
    let (mut state, _, child, _) = family();
    state.runtime.execution.peer_digest_seconds = 30;
    let lead_run = state.runtime.runs[0].id.clone();
    send(&mut state, &lead_run, "attempt-a", "one", &child, STATUS, false);
    crate::chat::send(&mut state, &child, "Operator steering", "test").unwrap();
    assert_eq!(queued(&state), 1, "operator input always queues its own turn");
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    crate::bots::reconcile_at_ms(&mut state, now() + 3_600_000).unwrap();
    assert_eq!(queued(&state), 0, "the operator's turn already carried the message");
    assert!(state.agent_messages.iter().all(|sent| !sent.digest), "a message another turn took is retired");
    let _ = lead;
}

#[test]
fn digests_are_bounded_and_count_what_they_omit() {
    let (mut state, alice_run, cass) = bot_peer();
    let long = "x".repeat(1_900);
    for index in 0..20 {
        send(&mut state, &alice_run, "peer-attempt", &format!("n{index}"), &cass, &format!("{index} {long}"), false);
    }
    send(&mut state, &alice_run, "peer-attempt", "urgent", &cass, "Need the key now", true);
    crate::bots::reconcile_at_ms(&mut state, now()).unwrap();
    let woken = wakes(&state, &cass);
    assert_eq!(woken.len(), 1);
    let objective = woken[0].objective.as_str();
    let digest = &objective[objective.find("Unread agent messages").unwrap()..];
    assert!(digest.starts_with("Unread agent messages (21;"));
    assert_eq!(digest.lines().filter(|line| line.starts_with("- ")).count(), 9, "eight quotes and a remainder line");
    assert!(digest.contains("- and 13 more"));
    assert!(digest.len() < 2_048, "digest stays compact: {} bytes", digest.len());
    assert!(digest.contains("(urgent): Need the key now"), "urgent messages are quoted first");
    assert!(digest.contains(": 19 x") && !digest.contains(": 0 x"), "then the newest; the oldest are counted");
}
