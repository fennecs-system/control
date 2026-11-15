//! Advisor notes: the one extra effect of an approved read-only judge bot. They
//! go only to the lead's steering chat; the lead decides what reaches the team.
use super::*;

/// The lead's task tree, linked to a room, with a builder and an advisor
/// (not yet approved) whose turn runs.
fn team() -> (State, String, String, String, String) {
    let (mut state, lead, builder, _) = family();
    state.runtime.runs[0].status = "completed".into();
    state.chats[0].room = Some("general".into());
    state.chats[0].lenses = Some(vec!["advisor".into()]);
    let advisor = spawned(&mut state, &lead);
    let index = crate::chat::latest_task(&state, &advisor, &state.chats[0].actor).unwrap();
    let mut persona = crate::delegation::lineage(&state.runtime.runs[index]);
    persona.role = crate::delegation::Role::Judge;
    state.runtime.runs[index].persona = Some(persona);
    let run = activate(&mut state, &advisor);
    (state, lead, builder, advisor, run)
}
fn note(state: &mut State, run: &str, nonce: &str, chat: &str, text: &str) -> io::Result<Value> {
    handle(state, &AttemptRef { run, generation: "peer-attempt", nonce },
        MessageCall { chat_id: chat.into(), text: text.into(), services: vec![] }, |_| Ok(()))
}

#[test]
fn an_approved_advisor_notes_only_the_lead_and_posts_nothing_to_the_room() {
    let (mut state, lead, _, advisor, run) = team();
    assert!(note(&mut state, &run, "unapproved", &lead, "Claim build is abandoned").is_err(),
        "an ordinary judge still cannot message agents");
    // The operator's approval (or /bot on) is the authority; a lens selection is not.
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    let sent = note(&mut state, &run, "n1", &lead, "Claim build is abandoned; its owner failed. Suggest reassigning it.").unwrap();
    assert_eq!((sent["queued"].as_bool(), sent["retained"].as_bool()), (Some(false), Some(true)), "retained, like peer evidence");
    let delivered = state.runtime.runs.last().unwrap();
    assert_eq!((delivered.kind, delivered.source.as_str(), delivered.status.as_str()), (RunKind::AgentMessage, "advisor-note", "completed"));
    assert_eq!(delivered.chat_id.as_deref(), Some(lead.as_str()));
    assert!(delivered.attempts.is_empty() && !crate::delegation::is_judge(delivered), "evidence, not a judge turn");
    assert!(delivered.objective.starts_with("Advisor note from") && delivered.objective.contains("not operator steering"));
    assert!(state.agent_messages.last().unwrap().note);
    let shown = state.chats[0].messages.last().unwrap();
    assert!(shown.text.starts_with("Advisor note from"));
    assert!(state.collective.posts.is_empty(), "the advisor steers; it never posts to the room");
    // The web transcript shows it as one distinct box, and the TUI detail marks it.
    let html = crate::chat::transcript(&state, &lead).unwrap();
    assert!(html.contains("<article class='advisor-note' aria-label='Advisor note'><p class='advisor-note-label'>Advisor note from"));
    assert!(html.contains("read-only evidence</p><p class='preserve'>Claim build is abandoned"));
    let detail = crate::chat::detail(&state, &lead).unwrap();
    let shown = detail["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(shown["advisor_note"], true);
    assert!(detail["messages"][0].get("advisor_note").is_none(), "ordinary messages are unchanged");
    // A retried request identity does not deliver twice.
    let runs = state.runtime.runs.len();
    note(&mut state, &run, "n1", &lead, "Claim build is abandoned; its owner failed. Suggest reassigning it.").unwrap();
    assert_eq!(state.runtime.runs.len(), runs);
}

#[test]
fn advisor_notes_refuse_members_judges_repeats_and_services() {
    let (mut state, lead, builder, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    let error = note(&mut state, &run, "n0", &builder, "Tester waits on build").unwrap_err();
    assert!(error.to_string().contains("steering chat"), "members hear advice only through the lead: {error}");
    note(&mut state, &run, "n1", &lead, "Tester waits on build; the builder already published it").unwrap();
    let runs = state.runtime.runs.len();
    let error = note(&mut state, &run, "n2", &lead, "Tester waits on build; the builder already published it").unwrap_err();
    assert!(error.to_string().contains("already delivered"), "identical notes are noise");
    assert_eq!(state.runtime.runs.len(), runs, "no second wake");
    note(&mut state, &run, "n3", &lead, "Tester now also waits on the schema").unwrap();
    assert_eq!(state.runtime.runs.len(), runs + 1);

    let other = spawned(&mut state, &lead);
    let index = crate::chat::latest_task(&state, &other, &state.chats[0].actor).unwrap();
    let mut persona = crate::delegation::lineage(&state.runtime.runs[index]);
    persona.role = crate::delegation::Role::Judge;
    state.runtime.runs[index].persona = Some(persona);
    assert!(note(&mut state, &run, "n5", &other, "Review this").is_err(), "advisors never message judges or advisors");
    let shared = handle(&mut state, &AttemptRef { run: &run, generation: "peer-attempt", nonce: "n6" },
        MessageCall { chat_id: lead.clone(), text: "Use this service".into(), services: vec![ResourceId::try_from("svc".to_owned()).unwrap()] }, |_| Ok(()));
    assert!(shared.is_err(), "advisors transfer no authority");
}

#[test]
fn an_active_lead_gets_one_callback_hint_per_note() {
    let (mut state, lead, _, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    let chat = state.chats.iter_mut().find(|chat| chat.id == lead).unwrap();
    chat.continuity = Some(Box::default());
    let through = chat.messages.len() as u64;
    let target = &mut state.runtime.runs[0];
    target.status = "running".into();
    let mut mailbox = control_context::turn_mailbox::Mailbox::default();
    mailbox.baseline.push(control_context::turn_mailbox::Range::Conversation { chat: lead.clone(), after: 0, through });
    target.attempts.push(crate::usage::Attempt { workspace_id: Some("lead-attempt".into()), turn_mailbox: Some(Box::new(mailbox)).into(), ..Default::default() });
    let target = target.id.clone();
    assert!(!crate::turn_context::callback_notice(&mut state, &target));
    note(&mut state, &run, "n1", &lead, "Schema claim was released; the tester can proceed").unwrap();
    assert!(crate::turn_context::callback_notice(&mut state, &target), "the note reaches the active lead");
    assert!(!crate::turn_context::callback_notice(&mut state, &target), "once");
}
