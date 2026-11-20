//! Antibodies: opt-in read-only judges that hear pathological facts about
//! every team on the node.
use super::*;
use super::advisor::turn;

fn ok(_: &State) -> io::Result<()> {
    Ok(())
}
/// An operator-owned observer tree with the antibody lens and its judge child.
pub(super) fn observer(state: &mut State) -> (String, String) {
    let root = crate::chat::create(state);
    crate::chat::send(state, &root, "Watch the node's teams", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().lenses = Some(vec!["antibody".into()]);
    let judge = spawned(state, &root, "Antibody");
    let lead = state.runtime.runs[0].id.clone();
    let run = state.runtime.runs.last_mut().unwrap();
    run.persona = Some(crate::delegation::Persona { role: crate::delegation::Role::Judge, root: lead, depth: 1, parent: None, bundle: None });
    run.attempts = vec![crate::usage::Attempt { review_only: true, ..Default::default() }];
    (root, judge)
}
fn node_facts(state: &State, chat: &str) -> Vec<std::collections::BTreeMap<String, Facts>> {
    bot_runs(state).into_iter().filter(|run| run.chat_id.as_deref() == Some(chat))
        .flat_map(|run| run.bot_turn.as_ref().unwrap().reasons.iter().filter_map(|reason| match reason {
            Reason::Info { info: Message::Node(teams), .. } => Some(teams.clone()),
            _ => None,
        }).collect::<Vec<_>>()).collect()
}
/// A service holding finished commands of `run`: (command, exit code).
pub(super) fn service(state: &mut State, owner: &str, run: &str, commands: &[(&str, i32)]) {
    let executions = commands.iter().enumerate().map(|(index, (command, code))| json!({
        "id":format!("exec-{}-{index}", state.services.len()),"digest":"d","run":run,"attempt":"a","command":command,"timeout_seconds":60,
        "status":"completed","output":"","truncated":false,"exit_code":code,"diagnostic":""
    })).collect::<Vec<_>>();
    state.services.push(serde_json::from_value(json!({
        "resource":{"id":format!("svc-{}", state.services.len()),"owner":{"node":"node-a","chat":owner},"generation":1,
            "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},"lease":[1,u64::MAX],"phase":"Requested"},
        "origin":{"run":run,"attempt":"a0"},"actor":{"type":"local_operator"},"command":"true","uid":502,"gid":20,"executions":executions
    })).unwrap());
}

#[test]
fn antibodies_are_opt_in_operator_owned_and_hear_the_node_topic() {
    let mut state = State::default();
    let (root, judge) = observer(&mut state);
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().lenses = Some(Vec::new());
    assert_eq!(command_at(&mut state, &judge, "on", Now(0)).unwrap_err(), ADVISOR_OFF, "nothing runs without the lens");
    assert!(!antibodies_active(&state));
    // A root not owned by the local operator cannot watch other people's teams.
    let session = state.chats.iter_mut().find(|chat| chat.id == root).unwrap();
    session.lenses = Some(vec!["antibody".into()]);
    session.actor = identity::Actor::DiscordDm { user_id: "1".parse().unwrap(), channel_id: "2".parse().unwrap() };
    assert!(!antibody_offered(&state, &root));
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().actor = identity::Actor::LocalOperator;
    assert!(antibody_offered(&state, &root));
    // Approval (here the operator's /bot on) runs it, with an optional patrol timer.
    command_at(&mut state, &judge, "on every 60", Now(1)).unwrap();
    let definition = state.bots.supervisor.definitions().find(|d| d.chat.0 == judge).unwrap();
    assert_eq!(definition.sources, [lifecycle::node_source(), Source::Timer { every_ms: 3_600_000 }]);
    assert!(state.bots.antibody(&judge) && state.bots.advises(&judge) && antibodies_active(&state));
    // Standing parity: the pre-persona judge reads as the built-in antibody.
    let run = state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(judge.as_str())).unwrap();
    assert_eq!(crate::personas::of(&state.bots, run), crate::personas::Effective::builtin("antibody", true));
    assert!(status(&state, &judge).unwrap().starts_with("Antibody · sleeping · node pathology facts"));
    assert!(state.collective.room_watches.iter().all(|watch| watch.chat != judge), "no room inbox");
    command_at(&mut state, &judge, "off", Now(2)).unwrap();
    assert!(!state.bots.antibody(&judge));
}

#[test]
fn an_antibody_hears_pathologies_of_every_team_but_not_ordinary_coordination() {
    let (mut state, team) = fixture();
    let worker = spawned(&mut state, &team, "Builder");
    let (observer_root, judge) = observer(&mut state);
    command_at(&mut state, &judge, "on", Now(0)).unwrap();
    reconcile_at(&mut state, 1, ok).unwrap();
    assert!(state.bots.teams.contains_key(&team), "every team is observed, without an advisor");
    assert!(!state.bots.teams.contains_key(&observer_root), "the observer tree is not a team");
    assert!(crate::bots::conditions(&state, &team).unwrap().is_empty());

    // A quiet team is ordinary: the advisor topic hears it, the node topic does not.
    let active = turn(&mut state, &worker, "running");
    reconcile_at(&mut state, 2, ok).unwrap();
    state.runtime.runs[active].status = "completed".into();
    reconcile_at(&mut state, 3, ok).unwrap();
    assert!(node_facts(&state, &judge).is_empty());

    // The same command failing again and again is a repetition loop.
    let run = state.runtime.runs[active].id.clone();
    service(&mut state, &worker, &run, &[("./place-wall.sh 10 64", 1), ("./place-wall.sh 11 64", 1), ("./place-wall.sh 12 64", 1)]);
    reconcile_at(&mut state, 100_000, ok).unwrap();
    let heard = node_facts(&state, &judge);
    assert_eq!(heard.len(), 1, "{heard:?}");
    let facts = &heard[0][&team];
    assert_eq!(facts.loops, [pathology::Loop { chat: worker.clone(), command: "./place-wall.sh 12 64".into(), failures: 3 }]);
    assert!(!facts.quiescent && facts.blocked.is_empty());
    assert!(describe(&state, &[Reason::Info { topic: lifecycle::node_topic(), through: 1, info: Message::Node(heard[0].clone()) }]).contains("repetition loop"));
    assert_eq!(crate::bots::conditions(&state, &team).unwrap(), ["1 repetition loop(s)"]);
    // Unchanged evidence is not offered again.
    finish_bot_turns(&mut state);
    reconcile_at(&mut state, 200_000, ok).unwrap();
    assert_eq!(node_facts(&state, &judge).len(), 1);

    // Stopping the last antibody drops the snapshots of unadvised teams.
    command_at(&mut state, &judge, "off", Now(300_000)).unwrap();
    assert!(state.bots.teams.is_empty() && !antibodies_active(&state));
    assert!(lifecycle::observe(&state).is_empty(), "nodes without an antibody or advisor do no work");
}

#[test]
fn cost_burn_reaches_the_antibody_and_progress_rearms_it() {
    let (mut state, team) = fixture();
    let worker = spawned(&mut state, &team, "Builder");
    let (_, judge) = observer(&mut state);
    state.runtime.execution.pathology_burn_tokens = 10_000;
    command_at(&mut state, &judge, "on", Now(0)).unwrap();
    reconcile_at(&mut state, 1, ok).unwrap();
    let spent = turn(&mut state, &worker, "completed");
    state.runtime.runs[spent].attempts[0].tokens = Some(control_ai::Tokens { input_tokens: 9_000, output_tokens: 2_000, ..Default::default() });
    reconcile_at(&mut state, 100_000, ok).unwrap();
    let heard = node_facts(&state, &judge);
    assert_eq!(heard.last().unwrap()[&team].burn, Some(pathology::Burn { tokens: 11_000 }));
}

fn finish_bot_turns(state: &mut State) {
    for run in state.runtime.runs.iter_mut().filter(|run| run.bot_turn.is_some()) {
        run.status = "completed".into();
        run.attempts = vec![crate::usage::Attempt { review_only: true, ..Default::default() }];
    }
}

#[test]
fn a_captured_observer_persona_decides_its_mode_and_keeps_its_gene_gate() {
    use control_context::persona::{builtin, Captured};
    let mut state = State::default();
    let (root, judge) = observer(&mut state);
    let capture = |id: &str| Some(Box::new(Captured::of(&builtin(id).unwrap(), &[]).unwrap()));
    // An advisor persona in an antibody-only tree: its own gate (the advisor gene) is closed.
    state.runtime.runs.last_mut().unwrap().persona.as_mut().unwrap().bundle = capture("advisor");
    assert_eq!(command_at(&mut state, &judge, "on", Now(0)).unwrap_err(), ADVISOR_OFF);
    // An antibody persona runs on the node topic and may patrol.
    state.runtime.runs.last_mut().unwrap().persona.as_mut().unwrap().bundle = capture("antibody");
    command_at(&mut state, &judge, "on every 30", Now(1)).unwrap();
    let definition = state.bots.supervisor.definitions().find(|d| d.chat.0 == judge).unwrap();
    assert_eq!(definition.sources, [lifecycle::node_source(), Source::Timer { every_ms: 1_800_000 }]);
    let run = state.runtime.runs.iter().rev().find(|run| run.chat_id.as_deref() == Some(judge.as_str())).unwrap();
    let standing = crate::personas::of(&state.bots, run);
    assert!(standing.captured && standing.antibody() && standing.notifies());
    command_at(&mut state, &judge, "off", Now(2)).unwrap();
    // Without the antibody gene on the root, the persona cannot run at all.
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().lenses = Some(Vec::new());
    assert_eq!(command_at(&mut state, &judge, "on", Now(3)).unwrap_err(), ANTIBODY_OFF);
}

#[test]
fn a_quiet_patrol_settles_without_a_model_turn_and_activity_still_wakes_one() {
    let mut state = State::default();
    let (_, judge) = observer(&mut state);
    command_at(&mut state, &judge, "on every 1", Now(1)).unwrap();
    // Nothing happens on the node: the timer fires, but the turn settles at once.
    reconcile_at(&mut state, 61_000, ok).unwrap();
    let patrols = bot_runs(&state).into_iter().filter(|run| run.chat_id.as_deref() == Some(judge.as_str())).collect::<Vec<_>>();
    assert_eq!(patrols.len(), 1);
    assert_eq!((patrols[0].status.as_str(), patrols[0].attempts.len()), ("completed", 0), "no model call");
    assert!(patrols[0].result.contains("Patrol skipped"));
    // Another team starts work: the next patrol is a real turn.
    let other = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &other, "Build the wall", "test").unwrap();
    reconcile_at(&mut state, 122_000, ok).unwrap();
    let last = bot_runs(&state).into_iter().filter(|run| run.chat_id.as_deref() == Some(judge.as_str())).last().unwrap();
    assert_eq!(last.status, "queued", "activity on the node wakes a real patrol");
}
