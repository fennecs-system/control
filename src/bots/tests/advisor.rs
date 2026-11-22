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
    state.runtime.runs.last_mut().unwrap().persona = Some(crate::delegation::Persona { role: crate::delegation::Role::Judge, root: lead, depth: 1, parent: None, bundle: None });
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
        Reason::Info { info: Message::Lifecycle(facts), .. } => Some((**facts).clone()),
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
    // Standing parity: the pre-persona judge reads as the built-in advisor.
    let run = state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(advisor.as_str())).unwrap();
    assert_eq!(crate::personas::of(&state.bots, run), crate::personas::Effective::builtin("advisor", true));
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
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().messages.push(crate::chat::Message { speaker: Default::default(), text: "Advisor note".into(), run_id: Some("note-1".into()), reply: String::new() });
    state.agent_messages.push(crate::application::messages::Sent { key: "advisor/note".into(), origin: "advisor-turn".into(), chat: root.clone(),
        run: "note-1".into(), services: vec![], peer_cursor: Some(cursor), note: true, urgent: false, digest: false, at_ms: 0 });
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
        status: crate::rooms::claims::Status::Working, blocked_on: None, note: String::new(), revision: 1, incarnation: 1, abandoned: false, blocked_at: 0, advanced_at: 0 });
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
    let persona = crate::delegation::Persona { role: crate::delegation::Role::Judge, root: state.runtime.runs[0].id.clone(), depth: 1, parent: None, bundle: None };
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

/// Acquires `key` for `chat` in `general`, or updates it to `status`.
fn claim(state: &mut State, chat: &str, key: &str, status: &str, on: Option<&str>) {
    let live = crate::rooms::claims::find(state, "general", key).filter(|c| c.status != crate::rooms::claims::Status::Released).map(|c| c.revision);
    let mut args = match live {
        None => json!({"key":key,"action":"acquire"}),
        Some(revision) => json!({"key":key,"action":"update","status":status,"revision":revision,"note":format!("r{revision}")}),
    };
    if let Some(on) = on { args["blocked_on"] = json!(on); }
    let (value, _) = crate::rooms::claims::call(state, chat, "general", &args).unwrap();
    assert_eq!(value["applied"], true, "{value}");
}
fn blocked(on: &str, producer: &str, status: &str, waiting: &[(&str, &str)]) -> lifecycle::Blocked {
    lifecycle::Blocked { room: "general".into(), on: on.into(), producer: Some(producer.into()), status: status.into(),
        waiting: waiting.iter().map(|(key, chat)| lifecycle::Waiter { key: (*key).into(), chat: (*chat).into() }).collect() }
}

#[test]
fn a_member_becoming_blocked_is_one_fact_per_transition() {
    let (mut state, root) = fixture();
    let producer = spawned(&mut state, &root, "Producer");
    let miner = spawned(&mut state, &root, "Miner");
    claim(&mut state, &producer, "wood-tools", "working", None);
    claim(&mut state, &miner, "mine", "working", None);
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    // working → blocked: one fact naming the claim, its producer and status.
    claim(&mut state, &miner, "mine", "blocked", Some("wood-tools"));
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    assert_eq!(facts(runs[0]), [Facts { blocked: vec![blocked("wood-tools", &producer, "working", &[("mine", &miner)])], chats: vec![miner.clone()], ..Facts::default() }]);
    assert!(runs[0].objective.contains("1 claim(s) newly blocked on general/wood-tools") && runs[0].objective.contains("following the Team advisor guidance"));
    finish(&mut state, &advisor);
    // Staying blocked, even with a new note and revision, is the same state.
    let since = crate::rooms::claims::find(&state, "general", "mine").unwrap().blocked_at;
    claim(&mut state, &miner, "mine", "blocked", Some("wood-tools"));
    assert_eq!(crate::rooms::claims::find(&state, "general", "mine").unwrap().blocked_at, since);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "an unchanged blocked state does not re-fire");
    // blocked → working → blocked is a new transition, even between passes.
    claim(&mut state, &miner, "mine", "working", None);
    claim(&mut state, &miner, "mine", "blocked", Some("wood-tools"));
    reconcile_at(&mut state, 80_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    assert_eq!(facts(runs[1])[0].blocked, [blocked("wood-tools", &producer, "working", &[("mine", &miner)])]);
}

#[test]
fn members_blocked_on_one_claim_coalesce_into_one_bottleneck() {
    let (mut state, root) = fixture();
    let producer = spawned(&mut state, &root, "Producer");
    let members = ["Miner", "Builder", "Smith", "Scout"].map(|name| spawned(&mut state, &root, name));
    claim(&mut state, &producer, "wood-tools", "working", None);
    for (member, key) in members.iter().zip(["mine", "build", "smelt", "scout"]) {
        claim(&mut state, member, key, "working", None);
    }
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    for (member, key) in members.iter().zip(["mine", "build", "smelt"]) {
        claim(&mut state, member, key, "blocked", Some("wood-tools"));
    }
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1, "one wake");
    let observed = facts(runs[0]);
    assert_eq!(observed[0].blocked, [blocked("wood-tools", &producer, "working", &[("mine", &members[0]), ("build", &members[1]), ("smelt", &members[2])])]);
    assert!(runs[0].objective.contains("3 claim(s) newly blocked on general/wood-tools (owner"));
    // A later member blocked during the cooldown merges into the same entry,
    // with the producer's newer state.
    finish(&mut state, &advisor);
    claim(&mut state, &members[3], "scout", "blocked", Some("wood-tools"));
    claim(&mut state, &producer, "wood-tools", "blocked", Some("mine"));
    reconcile_at(&mut state, 3, ok).unwrap();
    claim(&mut state, &members[0], "mine", "working", None);
    claim(&mut state, &members[0], "mine", "blocked", Some("wood-tools"));
    reconcile_at(&mut state, 4, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "the cooldown holds the facts");
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    let observed = facts(runs[1]);
    assert_eq!(observed.len(), 1);
    // The producer now waits on a member's claim too: a circular wait the advisor can see.
    assert_eq!(observed[0].blocked, [
        blocked("mine", &members[0], "blocked", &[("wood-tools", &producer)]),
        blocked("wood-tools", &producer, "blocked", &[("scout", &members[3]), ("mine", &members[0])]),
    ]);
}

#[test]
fn blocked_members_are_not_observed_with_the_lens_off() {
    let (mut state, root) = fixture();
    let producer = spawned(&mut state, &root, "Producer");
    let miner = spawned(&mut state, &root, "Miner");
    claim(&mut state, &producer, "wood-tools", "working", None);
    claim(&mut state, &miner, "mine", "working", None);
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    let before = state.bots.teams[&root].clone();
    opt_in(&mut state, &root, false);
    claim(&mut state, &miner, "mine", "blocked", Some("wood-tools"));
    assert!(lifecycle::observe(&state).is_empty());
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty());
    assert_eq!(state.bots.teams[&root], before);
}

#[test]
fn turns_without_claim_progress_are_one_stall_fact_until_a_claim_advances() {
    let (mut state, root) = fixture();
    let miner = spawned(&mut state, &root, "Miner");
    let smith = spawned(&mut state, &root, "Smith");
    state.runtime.execution.advisor_stall_turns = 4;
    let advisor = advisor(&mut state, &root);
    // A running member keeps the team from going quiet.
    turn(&mut state, &smith, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    for _ in 0..6 {
        turn(&mut state, &miner, "completed");
    }
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "without open claims there is nothing to stall");
    claim(&mut state, &miner, "mine", "working", None);
    claim(&mut state, &smith, "smelt", "working", None);
    reconcile_at(&mut state, 3, ok).unwrap();
    for _ in 0..3 {
        turn(&mut state, &miner, "completed");
    }
    claim(&mut state, &miner, "mine", "working", None);
    reconcile_at(&mut state, 4, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "below the window; a working update is not progress");
    turn(&mut state, &smith, "completed");
    reconcile_at(&mut state, 5, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    assert_eq!(facts(runs[0]), [Facts { stalled: Some(lifecycle::Stall { turns: 4, open: 2 }), ..Facts::default() }]);
    assert!(runs[0].objective.contains("busy but stalled: 4 team turns settled while 2 claim(s) stayed open"));
    finish(&mut state, &advisor);
    for _ in 0..8 {
        turn(&mut state, &miner, "completed");
    }
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "a stall is reported once");
    // A claim resuming from blocked re-arms it: progress itself is no fact.
    claim(&mut state, &smith, "smelt", "blocked", Some("mine"));
    claim(&mut state, &smith, "smelt", "working", None);
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1);
    for _ in 0..4 {
        turn(&mut state, &miner, "completed");
    }
    reconcile_at(&mut state, 120_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    assert_eq!(facts(runs[1])[0].stalled, Some(lifecycle::Stall { turns: 4, open: 2 }));
    finish(&mut state, &advisor);
    // So does a claim becoming done; the remaining open claim can stall again.
    for _ in 0..3 {
        turn(&mut state, &miner, "completed");
    }
    claim(&mut state, &miner, "mine", "done", None);
    reconcile_at(&mut state, 160_000, ok).unwrap();
    for _ in 0..4 {
        turn(&mut state, &miner, "completed");
    }
    reconcile_at(&mut state, 200_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 3);
    assert_eq!(facts(runs[2])[0].stalled, Some(lifecycle::Stall { turns: 4, open: 1 }), "the done claim restarted the window");
}

/// Marks `chat`'s live claim `key` blocked on `on` with a free-text `note`.
fn blocked_with(state: &mut State, chat: &str, key: &str, on: &str, note: &str) {
    let revision = crate::rooms::claims::find(state, "general", key).unwrap().revision;
    let args = json!({"key":key,"action":"update","status":"blocked","blocked_on":on,"note":note,"revision":revision});
    let (value, _) = crate::rooms::claims::call(state, chat, "general", &args).unwrap();
    assert_eq!(value["applied"], true, "{value}");
}

#[test]
fn members_reporting_a_similar_blocker_are_one_shared_fact() {
    let (mut state, root) = fixture();
    let members = ["Miner", "Builder", "Smith", "Scout"].map(|name| spawned(&mut state, &root, name));
    for (member, key) in members.iter().zip(["mine", "build", "smelt", "scout"]) {
        claim(&mut state, member, key, "working", None);
    }
    let advisor = advisor(&mut state, &root);
    turn(&mut state, &members[3], "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    let host = "Host error 502: the shared service at 10.0.0.5 refused the connection";
    blocked_with(&mut state, &members[0], "mine", "ore", host);
    blocked_with(&mut state, &members[1], "build", "planks", "host error 503 - shared service at 10.0.0.7 refused the connection");
    blocked_with(&mut state, &members[2], "smelt", "fuel", "Need coal from the scout before smelting can start");
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    let observed = facts(runs[0]);
    assert_eq!(observed[0].shared, [lifecycle::Shared { reason: host.into(), members: vec![members[0].clone(), members[1].clone()] }]);
    assert_eq!(observed[0].blocked.len(), 3, "claims blocked on different keys stay separate entries");
    assert!(runs[0].objective.contains("2 members report a similar blocker"));
    finish(&mut state, &advisor);
    // An unchanged group is not repeated, even with a new note revision.
    blocked_with(&mut state, &members[0], "mine", "ore", "Host error 502: the shared service at 10.0.0.6 refused the connection");
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1);
    // A member joining is reported with the whole group.
    blocked_with(&mut state, &members[3], "scout", "map", "HOST ERROR 504: shared service refused the connection (10.0.0.9)");
    reconcile_at(&mut state, 80_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    let observed = facts(runs[1]);
    assert_eq!(observed[0].shared.len(), 1);
    assert_eq!(observed[0].shared[0].members, [members[0].clone(), members[1].clone(), members[3].clone()]);
}

#[test]
fn members_failing_with_the_same_error_share_a_blocker() {
    let (mut state, root) = fixture();
    let miner = spawned(&mut state, &root, "Miner");
    let smith = spawned(&mut state, &root, "Smith");
    let scout = spawned(&mut state, &root, "Scout");
    let advisor = advisor(&mut state, &root);
    turn(&mut state, &scout, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    for member in [&miner, &smith] {
        for attempt in 0..3 {
            let run = turn(&mut state, member, "failed");
            state.runtime.runs[run].result = format!("Could not reach the build host (attempt {attempt}): connection reset").into();
        }
    }
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    let observed = facts(runs[0]);
    assert_eq!(observed[0].repeated, 2);
    assert_eq!(observed[0].shared, [lifecycle::Shared { reason: "Could not reach the build host (attempt 2): connection reset".into(), members: vec![miner.clone(), smith.clone()] }]);
}

/// The bottlenecks of an advisor run's single fact.
fn necks(run: &crate::runtime::Run) -> Vec<lifecycle::Bottleneck> {
    facts(run).into_iter().flat_map(|facts| facts.bottlenecks).collect()
}

#[test]
fn a_claim_several_members_wait_on_transitively_is_one_bottleneck_fact() {
    let (mut state, root) = fixture();
    let [owner, recovery, stone, builder] = ["Player1", "Player5", "Stone", "Builder"].map(|name| spawned(&mut state, &root, name));
    for (member, key) in [(&owner, "covered-chest-access"), (&recovery, "player5-drop-recovery"), (&stone, "stone-stock"), (&builder, "m3")] {
        claim(&mut state, member, key, "working", None);
    }
    let advisor = advisor(&mut state, &root);
    turn(&mut state, &owner, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    // One member waits directly: a dependency, not yet a bottleneck.
    claim(&mut state, &recovery, "player5-drop-recovery", "blocked", Some("covered-chest-access"));
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(necks(advisor_runs(&state, &advisor)[0]).is_empty());
    finish(&mut state, &advisor);
    // A second member waits through the first: transitive dependents count.
    claim(&mut state, &stone, "stone-stock", "blocked", Some("player5-drop-recovery"));
    claim(&mut state, &builder, "m3", "blocked", Some("covered-chest-access"));
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    let observed = necks(runs[1]);
    assert_eq!(observed.len(), 1, "player5-drop-recovery holds up only one member");
    let neck = &observed[0];
    assert_eq!((neck.key.as_str(), neck.owner.as_str(), neck.status.as_str(), neck.dependents, neck.cycle), ("covered-chest-access", owner.as_str(), "working", 3, false));
    let mut members = vec![recovery.clone(), stone.clone(), builder.clone()];
    members.sort();
    assert_eq!(neck.members, members);
    assert_eq!(neck.waiting.iter().map(|waiter| waiter.key.as_str()).collect::<Vec<_>>(), ["player5-drop-recovery", "m3", "stone-stock"], "nearest first");
    let incarnation = crate::rooms::claims::find(&state, "general", "covered-chest-access").unwrap().incarnation;
    assert_eq!((neck.open, neck.idle), (state.collective.claim_seq - incarnation, neck.open), "claim updates since it was acquired; it never advanced");
    assert_eq!(neck.open, 6);
    assert!(runs[1].objective.contains("bottleneck general/covered-chest-access (owner") && runs[1].objective.contains("holds up 3 claim(s) of 3 member(s)"));
    finish(&mut state, &advisor);
    // Unchanged dependents are not repeated, even across claim updates.
    claim(&mut state, &builder, "m3", "blocked", Some("covered-chest-access"));
    claim(&mut state, &owner, "covered-chest-access", "working", None);
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 2);
    // A new dependent, even a transitive one, re-reports it with the whole set.
    let scout = spawned(&mut state, &root, "Scout");
    claim(&mut state, &scout, "scout", "working", None);
    claim(&mut state, &scout, "scout", "blocked", Some("stone-stock"));
    reconcile_at(&mut state, 120_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 3);
    let observed = necks(runs[2]);
    assert_eq!(observed.iter().map(|neck| (neck.key.as_str(), neck.dependents)).collect::<Vec<_>>(), [("covered-chest-access", 4), ("player5-drop-recovery", 2)]);
    finish(&mut state, &advisor);
    // The owner finishing it clears that bottleneck alone.
    claim(&mut state, &owner, "covered-chest-access", "done", None);
    reconcile_at(&mut state, 160_000, ok).unwrap();
    assert!(state.bots.teams[&root].necks.len() == 1, "only player5-drop-recovery still holds up two members");
}

#[test]
fn a_circular_wait_is_reported_without_looping() {
    let (mut state, root) = fixture();
    let members = ["Miner", "Smith", "Builder"].map(|name| spawned(&mut state, &root, name));
    let keys = ["ore", "ingots", "tools"];
    for (member, key) in members.iter().zip(keys) {
        claim(&mut state, member, key, "working", None);
    }
    let advisor = advisor(&mut state, &root);
    turn(&mut state, &members[0], "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    // ore ← ingots ← tools ← ore.
    claim(&mut state, &members[0], "ore", "blocked", Some("tools"));
    claim(&mut state, &members[1], "ingots", "blocked", Some("ore"));
    claim(&mut state, &members[2], "tools", "blocked", Some("ingots"));
    reconcile_at(&mut state, 2, ok).unwrap();
    let observed = necks(advisor_runs(&state, &advisor)[0]);
    assert_eq!(observed.len(), 3, "every claim in the cycle holds up the other two members");
    assert!(observed.iter().all(|neck| neck.cycle && neck.dependents == 2 && neck.status == "blocked" && !neck.members.contains(&neck.owner)));
    assert!(advisor_runs(&state, &advisor)[0].objective.contains("in a circular wait"));
}

#[test]
fn the_bottleneck_threshold_counts_distinct_members_other_than_the_owner() {
    let (mut state, root) = fixture();
    state.runtime.execution.advisor_bottleneck_members = 3;
    let [owner, miner, smith, scout] = ["Producer", "Miner", "Smith", "Scout"].map(|name| spawned(&mut state, &root, name));
    for (member, key) in [(&owner, "wood-tools"), (&owner, "own-follow-up"), (&miner, "mine"), (&miner, "mine-2"), (&smith, "smelt"), (&scout, "scout")] {
        claim(&mut state, member, key, "working", None);
    }
    let advisor = advisor(&mut state, &root);
    turn(&mut state, &owner, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    // Four dependent claims, but only two members besides the owner.
    for (member, key) in [(&owner, "own-follow-up"), (&miner, "mine"), (&miner, "mine-2"), (&smith, "smelt")] {
        claim(&mut state, member, key, "blocked", Some("wood-tools"));
    }
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(necks(advisor_runs(&state, &advisor)[0]).is_empty());
    finish(&mut state, &advisor);
    claim(&mut state, &scout, "scout", "blocked", Some("wood-tools"));
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let observed = necks(advisor_runs(&state, &advisor)[1]);
    assert_eq!(observed.len(), 1);
    assert_eq!((observed[0].members.len(), observed[0].dependents), (3, 5));
}

#[test]
fn the_room_plan_reports_the_advisor_bottlenecks() {
    let mut state = State::default();
    let spec = [("hub", None), ("a", Some("hub")), ("b", Some("a")), ("c", Some("hub")), ("d", Some("b")), ("x", Some("y")), ("y", Some("x")), ("z", Some("x"))];
    for (key, on) in spec {
        let chat = crate::chat::create(&mut state);
        let mut args = json!({"key":key,"action":"acquire"});
        if let Some(on) = on { args["status"] = json!("blocked"); args["blocked_on"] = json!(on); }
        crate::rooms::claims::call(&mut state, &chat, "general", &args).unwrap();
    }
    let all = state.collective.claims.iter().collect::<Vec<_>>();
    let plan = crate::rooms::plan::plan(&state, "general");
    for threshold in 1..=4 {
        let advisor = lifecycle::bottlenecks(&state, &all, threshold).into_iter()
            .map(|(_, neck)| (neck.key, u64::from(neck.dependents), neck.members, neck.cycle)).collect::<Vec<_>>();
        let planned = plan["bottlenecks"].as_array().unwrap().iter()
            .filter(|neck| !neck["owner"].is_null() && neck["members"].as_array().unwrap().len() >= threshold)
            .map(|neck| (neck["key"].as_str().unwrap().to_owned(), neck["dependents"].as_u64().unwrap(),
                serde_json::from_value::<Vec<String>>(neck["members"].clone()).unwrap(), neck["cycle"] == true))
            .collect::<Vec<_>>();
        assert!(!advisor.is_empty() || threshold == 4, "threshold {threshold}");
        assert_eq!(advisor, planned, "threshold {threshold}");
    }
}

#[test]
fn a_member_idle_with_a_working_claim_while_peers_work_is_one_fact_per_idle_spell() {
    let (mut state, root) = fixture();
    let (idler, worker) = (spawned(&mut state, &root, "Builder"), spawned(&mut state, &root, "Worker"));
    let (watcher, queued, waiter) = (spawned(&mut state, &root, "Watcher"), spawned(&mut state, &root, "Queued"), spawned(&mut state, &root, "Waiter"));
    state.runtime.execution.advisor_stall_turns = 12;
    claim(&mut state, &idler, "castle", "working", None);
    // A running watch or a queued turn will wake its member; a blocked claim is the blocked fact's.
    claim(&mut state, &watcher, "farm", "working", None);
    watching(&mut state, &watcher, "w-farm");
    claim(&mut state, &queued, "mine", "working", None);
    turn(&mut state, &queued, "queued");
    claim(&mut state, &waiter, "smelt", "working", None);
    claim(&mut state, &waiter, "smelt", "blocked", Some("castle"));
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    let work = |state: &mut State, turns: usize| for _ in 0..turns { turn(state, &worker, "completed"); };
    work(&mut state, 3);
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(advisor_runs(&state, &advisor).is_empty(), "below the window of 12 / 3 team turns");
    // A peer's message landing in the idler's chat is not a turn of its own.
    let mut message = state.runtime.runs.iter().rfind(|run| run.chat_id.as_deref() == Some(idler.as_str())).unwrap().clone();
    (message.id, message.status, message.kind, message.attempts) = ("peer-message-1".into(), "completed".into(), RunKind::AgentMessage, Vec::new());
    state.runtime.runs.push(message);
    work(&mut state, 1);
    reconcile_at(&mut state, 3, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    let idle = vec![lifecycle::Waiter { key: "castle".into(), chat: idler.clone() }];
    assert_eq!(facts(runs[0]), [Facts { idle: idle.clone(), chats: vec![idler.clone()], ..Facts::default() }]);
    assert_eq!(facts(runs[0])[0].pathological().idle, idle, "antibodies hear it too");
    assert!(runs[0].objective.contains(&format!("idle claim holder: member {idler} holds working claim castle")));
    finish(&mut state, &advisor);
    work(&mut state, 4);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1, "one fact per idle spell");
    // The member's next turn re-arms it; a new idle spell is a new fact.
    turn(&mut state, &idler, "completed");
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &advisor).len(), 1);
    work(&mut state, 4);
    reconcile_at(&mut state, 120_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    assert_eq!(facts(runs[1])[0].idle, idle);
}

#[test]
fn a_persistent_member_with_no_claim_while_the_team_has_open_work_is_an_idle_fact() {
    let (mut state, root) = fixture();
    let (finished, worker, helper) = (spawned(&mut state, &root, "Finished"), spawned(&mut state, &root, "Worker"), spawned(&mut state, &root, "Helper"));
    state.runtime.execution.advisor_stall_turns = 12;
    // Finished did its slice and holds nothing; Worker still has open work.
    claim(&mut state, &finished, "wall", "working", None);
    claim(&mut state, &finished, "wall", "done", None);
    claim(&mut state, &worker, "tower", "working", None);
    command_at(&mut state, &finished, "on", Now(0)).unwrap();
    let advisor = advisor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    for _ in 0..4 { turn(&mut state, &worker, "completed"); }
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    assert_eq!(facts(runs[0])[0].idle, vec![lifecycle::Waiter { key: String::new(), chat: finished.clone() }], "a one-off helper that is not a bot is not reported: {helper}");
    assert!(runs[0].objective.contains(&format!("idle member: member {finished} holds no claim")));
}

/// A judge child of `root` capturing the acceptance persona, started as its
/// team's acceptance check.
fn acceptance(state: &mut State, root: &str) -> String {
    let chat = judge(state, root);
    let captured = control_context::persona::Captured::of(&control_context::persona::builtin("acceptance").unwrap(), &[]).unwrap();
    state.runtime.runs.last_mut().unwrap().persona.as_mut().unwrap().bundle = Some(Box::new(captured));
    assert_eq!(command_at(state, &chat, "on", Now(0)).unwrap(), "Acceptance check started. It sleeps until its team goes quiet with every claim done.");
    chat
}

#[test]
fn an_acceptance_check_wakes_only_when_the_team_goes_quiet_with_every_claim_done() {
    // Run V: a member called the goal complete, the team went quiet and
    // nobody compared the result with the request.
    let (mut state, root) = fixture();
    let (builder, miner) = (spawned(&mut state, &root, "Builder"), spawned(&mut state, &root, "Miner"));
    let advisor = advisor(&mut state, &root);
    state.chats[0].lenses = Some(vec!["advisor".into(), "acceptance".into()]);
    let check = acceptance(&mut state, &root);
    let definition = state.bots.supervisor.definitions().find(|d| d.chat.0 == check).unwrap();
    assert_eq!(definition.sources, [lifecycle::done_source(&root)], "completion only: no team facts, room, watch or timer");
    assert!(status(&state, &check).unwrap().starts_with("Acceptance check · sleeping · team completion"));
    assert!(command_at(&mut state, &check, "on every 5", Now(1)).is_err(), "no timer");
    // It has an advisor's standing in its own team, but members ask the advisor.
    assert_eq!(state.bots.advised_team(&check), Some(root.as_str()));
    assert!(state.bots.advises(&check) && state.bots.accepts(&check) && !state.bots.accepts(&advisor));
    assert_eq!(state.bots.team_advisor(&root), Some(advisor.as_str()));
    reconcile_at(&mut state, 1, ok).unwrap();

    // Quiet with no claims: nothing was called finished.
    turn(&mut state, &builder, "completed");
    reconcile_at(&mut state, 2, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 1);
    assert_eq!(facts(runs[0]), [Facts { quiescent: true, ..Facts::default() }]);
    assert!(advisor_runs(&state, &check).is_empty());
    finish(&mut state, &advisor);

    // Quiet with a blocked claim: the advisor's facts, not completion.
    claim(&mut state, &builder, "wall", "working", None);
    claim(&mut state, &miner, "mine", "working", None);
    claim(&mut state, &miner, "mine", "blocked", Some("wall"));
    turn(&mut state, &miner, "completed");
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 2);
    let heard = &facts(runs[1])[0];
    assert!(heard.quiescent && !heard.finished && heard.blocked.len() == 1, "{heard:?}");
    assert!(advisor_runs(&state, &check).is_empty(), "other team facts never wake it");
    finish(&mut state, &advisor);

    // Every claim done and the team quiet: both hear it, each on its own topic.
    claim(&mut state, &builder, "wall", "done", None);
    claim(&mut state, &miner, "mine", "done", None);
    turn(&mut state, &builder, "completed");
    reconcile_at(&mut state, 80_000, ok).unwrap();
    let runs = advisor_runs(&state, &advisor);
    assert_eq!(runs.len(), 3, "no extra advisor wake: the quiet fact carries it");
    assert!(facts(runs[2])[0].quiescent && facts(runs[2])[0].finished);
    let runs = advisor_runs(&state, &check);
    assert_eq!(runs.len(), 1);
    assert_eq!(facts(runs[0]), [Facts { finished: true, ..Facts::default() }], "completion alone");
    assert!(crate::delegation::is_judge(runs[0]) && runs[0].resource_grants.is_empty());
    assert!(runs[0].objective.contains("read-only acceptance check") && runs[0].objective.contains("went quiet with every claim done")
        && runs[0].objective.contains(&root), "{}", runs[0].objective);
    assert!(Facts { finished: true, ..Facts::default() }.pathological() == Facts::default(), "not a pathology");
    finish(&mut state, &advisor);
    finish(&mut state, &check);
    reconcile_at(&mut state, 120_000, ok).unwrap();
    reconcile_at(&mut state, 160_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &check).len(), 1, "its own turn is not team activity");

    // Run AA: finished members kept checking in on timers, and each quiet
    // moment announced the same completion again (seven wakes).
    turn(&mut state, &builder, "completed");
    reconcile_at(&mut state, 180_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &check).len(), 1, "an unchanged completion is announced once");
    finish(&mut state, &advisor);

    // Reopened work that finishes again is a new completion.
    claim(&mut state, &builder, "keep", "working", None);
    turn(&mut state, &builder, "completed");
    reconcile_at(&mut state, 200_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &check).len(), 1, "an open claim is not finished");
    claim(&mut state, &builder, "keep", "done", None);
    turn(&mut state, &builder, "completed");
    reconcile_at(&mut state, 240_000, ok).unwrap();
    assert_eq!(advisor_runs(&state, &check).len(), 2);

    // Stopping one observer keeps the team's snapshot for the other.
    for step in 1..=3 {
        finish(&mut state, &advisor);
        finish(&mut state, &check);
        reconcile_at(&mut state, 240_000 + step * 40_000, ok).unwrap();
    }
    command_at(&mut state, &advisor, "off", Now(400_001)).unwrap();
    assert!(state.bots.teams.contains_key(&root));
    assert_eq!(state.bots.team_advisor(&root), None, "an acceptance check takes no questions");
    command_at(&mut state, &check, "off", Now(400_002)).unwrap();
    assert!(!state.bots.teams.contains_key(&root));
}

#[test]
fn the_acceptance_gene_alone_offers_an_acceptance_check() {
    let (mut state, root) = fixture();
    let builder = spawned(&mut state, &root, "Builder");
    state.chats[0].lenses = Some(vec!["swarm".into()]);
    let chat = judge(&mut state, &root);
    let captured = control_context::persona::Captured::of(&control_context::persona::builtin("acceptance").unwrap(), &[]).unwrap();
    state.runtime.runs.last_mut().unwrap().persona.as_mut().unwrap().bundle = Some(Box::new(captured));
    assert_eq!(command_at(&mut state, &chat, "on", Now(0)).unwrap_err(), ADVISOR_OFF, "opt-in");
    state.chats[0].lenses = Some(vec!["swarm".into(), "acceptance".into()]);
    command_at(&mut state, &chat, "on", Now(1)).unwrap();
    assert!(state.bots.accepts(&chat));
    reconcile_at(&mut state, 2, ok).unwrap();
    assert!(state.bots.teams.contains_key(&root), "facts are computed for a team with only an acceptance check");
    claim(&mut state, &builder, "wall", "working", None);
    claim(&mut state, &builder, "wall", "done", None);
    turn(&mut state, &builder, "completed");
    reconcile_at(&mut state, 3, ok).unwrap();
    assert_eq!(advisor_runs(&state, &chat).len(), 1);
}
