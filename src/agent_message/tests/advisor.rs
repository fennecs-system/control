//! Advisor notes: the one extra effect of an approved read-only judge bot.
use super::*;

/// The lead's task tree with a builder and an approved advisor whose turn runs.
fn team() -> (State, String, String, String) {
    let (mut state, lead, builder, _) = family();
    state.runtime.runs[0].status = "completed".into();
    let advisor = spawned(&mut state, &lead);
    let index = crate::chat::latest_task(&state, &advisor, &state.chats[0].actor).unwrap();
    let mut persona = crate::delegation::lineage(&state.runtime.runs[index]);
    persona.role = crate::delegation::Role::Judge;
    state.runtime.runs[index].persona = Some(persona);
    let run = activate(&mut state, &advisor);
    (state, builder, advisor, run)
}
fn note(state: &mut State, run: &str, nonce: &str, chat: &str, text: &str) -> io::Result<Value> {
    handle(state, &AttemptRef { run, generation: "peer-attempt", nonce },
        MessageCall { chat_id: chat.into(), text: text.into(), services: vec![] }, |_| Ok(()))
}

#[test]
fn only_an_approved_advisor_judge_sends_evidence_notes() {
    let (mut state, builder, advisor, run) = team();
    assert!(note(&mut state, &run, "unapproved", &builder, "Claim build is abandoned").is_err(),
        "an ordinary judge still cannot message agents");
    // The operator's approval (or /bot on) is the authority; a lens selection is not.
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    let sent = note(&mut state, &run, "n1", &builder, "Claim build is abandoned; its owner failed").unwrap();
    assert_eq!((sent["queued"].as_bool(), sent["retained"].as_bool()), (Some(false), Some(true)), "retained, like peer evidence");
    let delivered = state.runtime.runs.last().unwrap();
    assert_eq!((delivered.kind, delivered.source.as_str(), delivered.status.as_str()), (RunKind::AgentMessage, "advisor-note", "completed"));
    assert!(delivered.attempts.is_empty() && !crate::delegation::is_judge(delivered), "evidence, not a judge turn");
    assert!(delivered.objective.starts_with("Advisor note from") && delivered.objective.contains("not operator steering"));
    assert!(state.agent_messages.last().unwrap().note);
    let shown = state.chats.iter().find(|chat| chat.id == builder).unwrap().messages.last().unwrap();
    assert!(shown.text.starts_with("Advisor note from"));
}

#[test]
fn advisor_notes_refuse_repeats_services_judges_and_the_steering_chat() {
    let (mut state, builder, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    note(&mut state, &run, "n1", &builder, "Tester waits on build; you already published it").unwrap();
    let runs = state.runtime.runs.len();
    let error = note(&mut state, &run, "n2", &builder, "Tester waits on build; you already published it").unwrap_err();
    assert!(error.to_string().contains("already delivered"), "identical notes are noise");
    assert_eq!(state.runtime.runs.len(), runs);
    note(&mut state, &run, "n3", &builder, "Tester now also waits on the schema").unwrap();
    // Retrying the same request identity is idempotent, not a repeat.
    note(&mut state, &run, "n3", &builder, "Tester now also waits on the schema").unwrap();
    assert_eq!(state.runtime.runs.len(), runs + 1);

    let lead = state.chats[0].id.clone();
    assert!(note(&mut state, &run, "n4", &lead, "The team is stuck").is_err(), "the steering chat is not a peer");
    let other = spawned(&mut state, &lead);
    let index = crate::chat::latest_task(&state, &other, &state.chats[0].actor).unwrap();
    let mut persona = crate::delegation::lineage(&state.runtime.runs[index]);
    persona.role = crate::delegation::Role::Judge;
    state.runtime.runs[index].persona = Some(persona);
    assert!(note(&mut state, &run, "n5", &other, "Review this").is_err(), "advisors never message judges or advisors");
    let shared = handle(&mut state, &AttemptRef { run: &run, generation: "peer-attempt", nonce: "n6" },
        MessageCall { chat_id: builder.clone(), text: "Use this service".into(), services: vec![ResourceId::try_from("svc".to_owned()).unwrap()] }, |_| Ok(()));
    assert!(shared.is_err(), "advisors transfer no authority");
}

#[test]
fn an_active_recipient_gets_one_callback_hint_per_note() {
    let (mut state, builder, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    let index = crate::chat::latest_task(&state, &builder, &state.chats[0].actor).unwrap();
    let chat = state.chats.iter_mut().find(|chat| chat.id == builder).unwrap();
    chat.continuity = Some(Box::default());
    let through = chat.messages.len() as u64;
    let target = &mut state.runtime.runs[index];
    target.status = "running".into();
    let mut mailbox = control_context::turn_mailbox::Mailbox::default();
    mailbox.baseline.push(control_context::turn_mailbox::Range::Conversation { chat: builder.clone(), after: 0, through });
    target.attempts.push(crate::usage::Attempt { workspace_id: Some("builder-attempt".into()), turn_mailbox: Some(Box::new(mailbox)).into(), ..Default::default() });
    let target = target.id.clone();
    assert!(!crate::turn_context::callback_notice(&mut state, &target));
    note(&mut state, &run, "n1", &builder, "Schema claim was released; you can proceed").unwrap();
    assert!(crate::turn_context::callback_notice(&mut state, &target), "the note reaches the active turn");
    assert!(!crate::turn_context::callback_notice(&mut state, &target), "once");
}
