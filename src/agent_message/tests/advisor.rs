//! Advisor notes: the one extra effect of an approved read-only judge bot. They
//! go to the member a fact is about, or to the lead's steering chat.
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
        MessageCall { chat_id: chat.into(), text: text.into(), services: vec![], urgent: true }, |_| Ok(()))
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
fn an_observer_note_reports_back_to_an_idle_lead_as_a_turn_at_once() {
    // Like a background agent's completion: the lead (not a bot, its turn
    // finished) is woken by the note without waiting for the digest bound,
    // and that turn is the observer's echo, not team activity.
    let (mut state, lead, _, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    note(&mut state, &run, "n1", &lead, "The chest is covered by a block; mine it, then reassign the chest claim.").unwrap();
    assert!(state.agent_messages.last().unwrap().digest, "the idle lead is owed a turn");
    let sent_ms = state.agent_messages.last().unwrap().at_ms;
    crate::agent_message::digest::reconcile(&mut state, sent_ms, &mut |_| Ok(())).unwrap();
    let reports = state.runtime.runs.iter().filter(|r| r.chat_id.as_deref() == Some(lead.as_str()) && r.source == crate::agent_message::digest::NOTE_DIGEST).collect::<Vec<_>>();
    assert_eq!(reports.len(), 1, "woken at once");
    assert_eq!((reports[0].status.as_str(), reports[0].kind), ("queued", RunKind::AgentMessage));
    assert!(reports[0].objective.starts_with("Observer report") && reports[0].objective.contains("mine it"), "{}", reports[0].objective);
    // Delivered once.
    crate::agent_message::digest::reconcile(&mut state, sent_ms + 1_000_000, &mut |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.iter().filter(|r| r.source == crate::agent_message::digest::NOTE_DIGEST).count(), 1);
}

#[test]
fn an_advisor_notes_the_member_a_fact_is_about_without_the_lead() {
    let (mut state, lead, builder, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    let before = state.chats.iter().find(|chat| chat.id == lead).unwrap().messages.len();
    note(&mut state, &run, "n0", &builder, "Your wall routine failed three times on the same support check; dig stone below you instead.").unwrap();
    let delivered = state.runtime.runs.last().unwrap();
    assert_eq!((delivered.chat_id.as_deref(), delivered.source.as_str()), (Some(builder.as_str()), "advisor-note"));
    assert!(delivered.objective.starts_with("Advisor note from") && delivered.objective.contains("about your own work"), "{}", delivered.objective);
    assert!(!delivered.objective.contains("steer your team"), "a member is not asked to steer");
    assert_eq!(state.chats.iter().find(|chat| chat.id == lead).unwrap().messages.len(), before, "the lead is not a relay");
    // An idle member is woken by the note at once, as a lead is.
    let sent_ms = state.agent_messages.last().unwrap().at_ms;
    crate::agent_message::digest::reconcile(&mut state, sent_ms, &mut |_| Ok(())).unwrap();
    let report = state.runtime.runs.iter().find(|r| r.source == crate::agent_message::digest::NOTE_DIGEST).expect("woken at once");
    assert_eq!(report.chat_id.as_deref(), Some(builder.as_str()));
    assert!(report.objective.starts_with("Observer report on your work"), "{}", report.objective);
    // Another team's member is out of reach.
    let elsewhere = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &elsewhere, "Another task", "test").unwrap();
    let error = note(&mut state, &run, "n9", &elsewhere, "Unrelated team").unwrap_err();
    assert!(error.to_string().contains("own team"), "{error}");
}

#[test]
fn advisor_notes_refuse_judges_repeats_and_services() {
    let (mut state, lead, _, advisor, run) = team();
    crate::bots::command(&mut state, &advisor, "on").unwrap();
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
        MessageCall { chat_id: lead.clone(), text: "Use this service".into(), services: vec![ResourceId::try_from("svc".to_owned()).unwrap()], urgent: true }, |_| Ok(()));
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
    assert!(!crate::turn_context::callback_notice(&mut state, &target, true));
    note(&mut state, &run, "n1", &lead, "Schema claim was released; the tester can proceed").unwrap();
    assert!(crate::turn_context::callback_notice(&mut state, &target, true), "the note reaches the active lead");
    assert!(!crate::turn_context::callback_notice(&mut state, &target, true), "once");
}

#[test]
fn a_member_asks_its_teams_advisor_and_the_question_wakes_it() {
    let (mut state, _, builder, advisor, run) = team();
    let asking = activate(&mut state, &builder);
    let ask = |state: &mut State, nonce: &str, services: Vec<ResourceId>| handle(state, &AttemptRef { run: &asking, generation: "peer-attempt", nonce },
        MessageCall { chat_id: advisor.clone(), text: "My wall routine keeps stopping on a support check; is one long routine better?".into(), services, urgent: true }, |_| Ok(()));
    assert!(ask(&mut state, "q0", vec![]).is_err(), "an unapproved judge takes no questions");
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    assert!(ask(&mut state, "q1", vec![ResourceId::try_from("svc".to_owned()).unwrap()]).is_err(), "a question carries no services");
    let sent = ask(&mut state, "q2", vec![]).unwrap();
    assert_eq!((sent["queued"].as_bool(), sent["retained"].as_bool()), (Some(false), Some(true)));
    let asked = state.runtime.runs.last().unwrap();
    assert_eq!((asked.chat_id.as_deref(), asked.source.as_str()), (Some(advisor.as_str()), "peer-message"));
    assert!(crate::delegation::is_judge(asked), "the advisor stays a read-only judge");
    // Once its current turn settles, the question wakes the advisor.
    state.runtime.runs.iter_mut().find(|r| r.id == run).unwrap().status = "completed".into();
    crate::bots::reconcile_after_cooldown(&mut state).unwrap();
    let woken = state.runtime.runs.iter().rfind(|r| r.chat_id.as_deref() == Some(advisor.as_str()) && r.status == "queued").expect("the advisor is woken");
    assert!(woken.objective.contains("read-only advisor") && woken.objective.contains("one long routine better"), "{}", woken.objective);
}

/// Run E refused 31 helper spawns from players wanting a second opinion and
/// told them to report to the operator. With an approved advisor the refusal
/// names it as a callable message instead; without one it is unchanged.
#[test]
fn a_spawn_rate_refusal_sends_members_to_their_approved_advisor() {
    let (mut state, lead, builder, advisor, _) = team();
    let limit = crate::delegation::HELPERS_SPENT;
    assert_eq!(crate::delegation::refusal(&state, Some(&builder), limit), limit, "no approved advisor yet");
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    for chat in [&builder, &lead] {
        let shown = crate::delegation::refusal(&state, Some(chat), limit);
        assert!(shown.starts_with("This chat, woken by Control events, has spawned enough helpers this hour."), "{shown}");
        assert!(shown.contains(&format!("control_agent_message {{\"chat_id\":\"{advisor}\"")) && !shown.contains("operator"), "{shown}");
        assert!(crate::worker_tools::fixes::names_fix(&shown));
    }
    let other = "Delegation depth limit reached (2).";
    assert_eq!(crate::delegation::refusal(&state, Some(&builder), other), other);
    // The advisor takes that question: see a_member_asks_its_teams_advisor_and_the_question_wakes_it.
}
