//! The advisor: a read-only judge persona run as a persistent bot whose only
//! source is its task tree's host lifecycle facts.
use super::*;

/// Appends one executed member turn with `status`.
pub(super) fn turn(state: &mut State, chat: &str, status: &str) -> usize {
    let mut run = state.runtime.runs[0].clone();
    run.id = format!("turn-{}", state.runtime.runs.len());
    run.chat_id = Some(chat.into());
    run.status = status.into();
    run.persona = None;
    run.bot_turn = None;
    run.attempts = vec![crate::usage::Attempt::default()];
    state.runtime.runs.push(run);
    state.runtime.runs.len() - 1
}
/// Ticks (or unticks) the advisor lens on the root chat.
pub(super) fn opt_in(state: &mut State, root: &str, on: bool) {
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().lenses = Some(if on { vec!["advisor".into()] } else { Vec::new() });
}
/// A completed read-only judge child of `root`, before it runs as a bot.
fn judge(state: &mut State, root: &str) -> String {
    let chat = spawned(state, root, "Advisor");
    let lead = state.runtime.runs[0].id.clone();
    state.runtime.runs.last_mut().unwrap().persona = Some(crate::delegation::Persona { role: crate::delegation::Role::Judge, root: lead, depth: 1, parent: None });
    state.runtime.runs.last_mut().unwrap().attempts = vec![crate::usage::Attempt { review_only: true, ..Default::default() }];
    chat
}
/// A judge child of `root`, opted in with the advisor lens and started as its advisor.
pub(super) fn advisor(state: &mut State, root: &str) -> String {
    opt_in(state, root, true);
    let chat = judge(state, root);
    command_at(state, &chat, "on", Now(0)).unwrap();
    chat
}
fn advisor_runs<'a>(state: &'a State, chat: &str) -> Vec<&'a crate::runtime::Run> {
    bot_runs(state).into_iter().filter(|run| run.chat_id.as_deref() == Some(chat)).collect()
}
fn facts(run: &crate::runtime::Run) -> Vec<Facts> {
    run.bot_turn.as_ref().unwrap().reasons.iter().filter_map(|reason| match reason {
        Reason::Info { info: Message::Lifecycle(facts), .. } => Some(facts.clone()),
        _ => None,
    }).collect()
}
fn ok(_: &State) -> io::Result<()> {
    Ok(())
}
fn finish(state: &mut State, chat: &str) {
    for run in state.runtime.runs.iter_mut().filter(|run| run.chat_id.as_deref() == Some(chat) && run.bot_turn.is_some()) {
        run.status = "completed".into();
        run.attempts = vec![crate::usage::Attempt { review_only: true, ..Default::default() }];
    }
}

#[test]
fn a_judge_advises_on_team_facts_only_without_timers_rooms_or_grants() {
    let (mut state, root) = fixture();
    let advisor = advisor(&mut state, &root);
    let definition = state.bots.supervisor.definitions().find(|d| d.chat.0 == advisor).unwrap();
    assert_eq!(definition.sources, [lifecycle::source(&root)], "no room inbox, watch or timer");
    assert!(state.bots.advises(&advisor) && !state.bots.advises(&root));
    assert!(status(&state, &advisor).unwrap().starts_with("Advisor · sleeping · team facts"));
    assert!(state.collective.room_watches.iter().all(|watch| watch.chat != advisor));
    assert!(command_at(&mut state, &advisor, "on every 5", Now(1)).is_err(), "facts trigger; no clock rule");
    // A judge that is not an approved advisor still cannot take a bot turn.
    let other = spawned(&mut state, &root, "Reviewer");
    state.runtime.runs.last_mut().unwrap().persona = state.runtime.runs.iter().find(|run| run.chat_id.as_deref() == Some(&advisor)).unwrap().persona.clone();
    let request = Request { bot: bot(&other), chat: ChatId(other.clone()), turn: TurnId(99), reasons: Vec::new() };
    assert!(queue(&mut state, &request).is_err());
    command_at(&mut state, &advisor, "off", Now(2)).unwrap();
    assert!(!state.bots.advises(&advisor));
}

#[test]
fn advisors_are_strictly_opt_in_through_the_advisor_lens() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    // The swarm preset does not include the advisor.
    state.chats[0].lenses = Some(vec!["swarm".into()]);
    assert!(!advisor_offered(&state, &root));
    let judge = judge(&mut state, &root);
    let error = command_at(&mut state, &judge, "on", Now(0)).unwrap_err();
    assert_eq!(error, ADVISOR_OFF, "without the lens there is no advisor option");
    assert!(!state.bots.advises(&judge));
    // Ticking the lens (with or beside swarm) makes the option exist; approval runs it.
    state.chats[0].lenses = Some(vec!["swarm".into(), "advisor".into()]);
    assert!(advisor_offered(&state, &root) && advisor_offered_for(&state, &worker));
    command_at(&mut state, &judge, "on", Now(1)).unwrap();
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(state.bots.teams.contains_key(&root));
    // Unticking it stops the lifecycle topic: no observation and no facts.
    opt_in(&mut state, &root, false);
    assert!(lifecycle::observe(&state).is_empty(), "the topic is not computed with the lens off");
    turn(&mut state, &worker, "completed");
    reconcile_at(&mut state, 3, ok).unwrap();
    assert!(advisor_runs(&state, &judge).is_empty());
}

#[test]
fn quiescence_fires_once_per_settled_activity_and_history_before_subscription_is_not_reported() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    turn(&mut state, &worker, "failed");
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    assert!(state.bots.teams.contains_key(&root), "the subscription snapshot is recorded");
    let mut saves = 0;
    reconcile_at(&mut state, 2, |_| { saves += 1; Ok(()) }).unwrap();
    assert_eq!(saves, 0, "an unchanged team does no work");
    assert!(advisor_runs(&state, &advisor).is_empty(), "earlier failures are not replayed");

    let active = turn(&mut state, &worker, "running");
    reconcile_at(&mut state, 3, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "an active team is not quiet");
    state.runtime.runs[active].status = "completed".into();
    reconcile_at(&mut state, 4, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1, "the edge wakes the advisor once");
    assert_eq!(facts(runs[0]), [Facts { quiescent: true, ..Facts::default() }]);
    assert!(crate::delegation::is_judge(runs[0]) && runs[0].resource_grants.is_empty());
    assert!(runs[0].objective.contains("read-only advisor") && runs[0].objective.contains("team went quiet"));
    finish(&mut state, &advisor);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "its own turn is not team activity");
}

#[test]
fn notes_and_lead_turns_woken_only_by_notes_do_not_re_trigger_the_advisor() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    // The lead runs as a bot, so a note to its steering chat wakes it.
    command_at(&mut state, &root, "on", Now(0)).unwrap();
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    turn(&mut state, &worker, "completed");
    reconcile_at(&mut state, 2, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1);
    assert!(advisor_runs(&state, &advisor)[0].objective.contains(&format!("steering chat {root}")), "the advisor advises the lead");
    finish(&mut state, &advisor);
    reconcile_at(&mut state, 3, ok).unwrap();
    let settled = state.bots.teams[&root].settled;

    // The advisor's note is retained evidence (a run without an attempt)...
    let cursor = state.chats.iter().find(|chat| chat.id == root).unwrap().messages.len() as u64 + 1;
    let mut note = state.runtime.runs[0].clone();
    (note.id, note.chat_id, note.status, note.kind, note.attempts) = ("note-1".into(), Some(root.clone()), "completed".into(), RunKind::AgentMessage, Vec::new());
    note.bot_turn = None;
    state.runtime.runs.push(note);
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().messages.push(crate::chat::Message { text: "Advisor note".into(), run_id: Some("note-1".into()), reply: String::new() });
    state.agent_messages.push(crate::application::messages::Sent { key: "advisor/note".into(), origin: "advisor-turn".into(), chat: root.clone(),
        run: "note-1".into(), services: vec![], peer_cursor: Some(cursor), note: true });
    // ...that wakes the approved lead once, as a `Note` reason.
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let woken = bot_runs(&state).into_iter().filter(|run| run.chat_id.as_deref() == Some(&root)).collect::<Vec<_>>();
    assert_eq!(woken.len(), 1);
    assert!(matches!(woken[0].bot_turn.as_ref().unwrap().reasons.as_slice(), [Reason::Info { info: Message::Note, .. }]));
    assert!(woken[0].objective.contains("advisor note"));
    let id = woken[0].id.clone();
    let woken = state.runtime.runs.iter_mut().find(|run| run.id == id).unwrap();
    woken.status = "completed".into();
    woken.attempts = vec![crate::usage::Attempt::default()];
    reconcile_at(&mut state, 80_000, ok).unwrap();
    reconcile_at(&mut state, 120_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "the lead's note-woken turn is not team activity");
    assert_eq!(state.bots.teams[&root].settled, settled);
}

#[test]
fn facts_coalesce_into_one_reason_and_respect_the_advisor_budget() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    let tester = spawned(&mut state, &root, "Tester");
    state.collective.claim_seq += 1;
    state.collective.claims.push(crate::rooms::claims::Claim { room: "general".into(), key: "build".into(), owner: worker.clone(),
        status: crate::rooms::claims::Status::Working, blocked_on: None, note: String::new(), revision: 1, incarnation: 1, abandoned: false });
    let advisor = advisor(&mut state, &root);
    let mut definition = state.bots.supervisor.definitions().find(|d| d.chat.0 == advisor).unwrap().clone();
    definition.budget.turns_per_window = 1;
    state.bots.supervisor.step(Event::Define(definition), Now(0));
    reconcile_at(&mut state, 1, ok).unwrap();
    turn(&mut state, &worker, "failed");
    turn(&mut state, &tester, "interrupted");
    turn(&mut state, &tester, "failed");
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1, "several facts in one pass make one turn");
    let observed = facts(runs[0]);
    assert_eq!(observed.len(), 1, "one coalesced lifecycle reason");
    assert_eq!((observed[0].repeated, observed[0].abandoned, observed[0].quiescent), (0, 1, true), "single failures are not facts");
    assert!(observed[0].chats.contains(&worker));
    finish(&mut state, &advisor);
    turn(&mut state, &tester, "failed");
    reconcile_at(&mut state, 40_000, ok).unwrap();
    // The third consecutive failure (the interruption neither extends nor breaks it).
    turn(&mut state, &tester, "blocked");
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "the existing turn budget bounds the advisor");
    reconcile_at(&mut state, WINDOW_MS + 10, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    let observed = facts(runs[1]);
    assert_eq!(observed.len(), 1, "pending facts merged while the budget was spent");
    assert_eq!((observed[0].repeated, observed[0].quiescent), (1, true));
    assert!(observed[0].chats.contains(&tester));
    assert!(runs[1].objective.contains("failed several turns in a row"));
    // A streak is reported once; it must break before it is reported again.
    finish(&mut state, &advisor);
    turn(&mut state, &tester, "failed");
    reconcile_at(&mut state, 2 * WINDOW_MS + 20, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).get(2).is_none_or(|run| facts(run)[0].repeated == 0));
}

#[test]
fn single_failures_and_exits_are_not_facts_but_a_retired_restart_loop_is() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    let tester = spawned(&mut state, &root, "Tester");
    watching(&mut state, &worker, "w-build");
    let advisor = advisor(&mut state, &root);
    // A running member keeps the team from going quiet.
    turn(&mut state, &tester, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    turn(&mut state, &worker, "failed");
    let exit = |gave_up| crate::watches::Exit { seq: 1, code: Some(1), signal: None, restarts: 0, gave_up };
    state.watches.iter_mut().find(|w| w.id == "w-build").unwrap().exits.push(exit(false));
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "one failed turn and one failed exit are ordinary work");
    turn(&mut state, &worker, "failed");
    reconcile_at(&mut state, 3, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "two in a row are below the default streak");
    state.watches.iter_mut().find(|w| w.id == "w-build").unwrap().exits.push(crate::watches::Exit { seq: 2, ..exit(true) });
    reconcile_at(&mut state, 4, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    assert_eq!(facts(runs[0]), [Facts { retired: 1, chats: vec![worker.clone()], ..Facts::default() }]);
}

#[test]
fn the_failure_streak_is_configurable() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    let tester = spawned(&mut state, &root, "Tester");
    state.runtime.execution.advisor_failure_streak = 2;
    let advisor = advisor(&mut state, &root);
    turn(&mut state, &tester, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    turn(&mut state, &worker, "failed");
    turn(&mut state, &worker, "completed");
    turn(&mut state, &worker, "failed");
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "a completed turn breaks the streak");
    turn(&mut state, &worker, "blocked");
    reconcile_at(&mut state, 3, ok).unwrap();
    assert_eq!(facts(advisor_runs(&state, &advisor)[0]), [Facts { repeated: 1, chats: vec![worker.clone()], ..Facts::default() }]);
}

#[test]
fn failed_saves_restore_the_team_snapshot_and_restart_keeps_the_cursor() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    turn(&mut state, &worker, "completed");
    let before = state.bots.teams[&root].clone();
    assert!(reconcile_at(&mut state, 2, |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(state.bots.teams[&root], before, "the fact stays pending");
    assert!(advisor_runs(&state, &advisor).is_empty());
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    reconcile_at(&mut restored, 3, ok).unwrap();
    assert_eq!(advisor_runs(&restored, &advisor).len(), 1);
    assert!(facts(advisor_runs(&restored, &advisor)[0])[0].quiescent);
}

#[test]
fn nodes_without_advisors_skip_team_observation() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    command_at(&mut state, &worker, "on", Now(0)).unwrap();
    turn(&mut state, &worker, "failed");
    assert!(lifecycle::observe(&state).is_empty());
    reconcile_at(&mut state, 1, ok).unwrap();
    assert!(state.bots.teams.is_empty());
    let saved = serde_json::to_value(&state.bots).unwrap();
    assert!(saved.get("teams").is_none(), "no persisted state without an advisor");
}

#[test]
fn a_lead_requests_its_judge_as_advisor_and_the_operator_approves_it() {
    let (mut state, lead, _) = requester();
    let judge = spawned(&mut state, &lead, "Advisor");
    let persona = crate::delegation::Persona { role: crate::delegation::Role::Judge, root: state.runtime.runs[0].id.clone(), depth: 1, parent: None };
    state.runtime.runs.last_mut().unwrap().persona = Some(persona);
    assert!(ask(&mut state, "off", &judge, None).is_err(), "no request without the advisor lens");
    opt_in(&mut state, &lead, true);
    assert!(ask(&mut state, "timer", &judge, Some(5)).is_err(), "an advisor takes no interval");
    let request = ask(&mut state, "advisor", &judge, None).unwrap();
    assert!(!state.bots.advises(&judge), "a request grants nothing");
    assert!(pending_panel(&state, &lead).contains("team facts (read-only advisor)"));
    let id = request["request_id"].as_str().unwrap().to_owned();
    decide(&mut state, &id, true, Some(&lead), Now(1)).unwrap();
    assert!(state.bots.advises(&judge));
    let definition = state.bots.supervisor.definitions().find(|d| d.chat.0 == judge).unwrap();
    assert_eq!(definition.sources, [lifecycle::source(&lead)]);
}
