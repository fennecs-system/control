//! Turns stopped at their attempt time limit: the advisor hears of them once,
//! the owner keeps its claims and their notes, and a bot's next wake says so.
use super::advisor::{advisor, turn};
use super::*;

fn ok(_: &State) -> io::Result<()> {
    Ok(())
}
/// Marks `runs[index]` as stopped at its time limit, as the worker records it.
fn time_out(state: &mut State, index: usize) {
    let run = &mut state.runtime.runs[index];
    run.status = "failed".into();
    *run.result = crate::worker::timeout_message(std::time::Duration::from_secs(600));
    run.attempts.last_mut().unwrap().termination = Some(crate::attempt_control::Termination::TimedOut);
}
fn lifecycle_facts(state: &State, advisor: &str) -> Vec<Facts> {
    bot_runs(state).into_iter().filter(|run| run.chat_id.as_deref() == Some(advisor))
        .flat_map(|run| run.bot_turn.as_ref().unwrap().reasons.iter())
        .filter_map(|reason| match reason {
            Reason::Info { info: Message::Lifecycle(facts), .. } => Some(facts.clone()),
            _ => None,
        })
        .collect()
}
fn finish_advisor(state: &mut State, advisor: &str) {
    for run in state.runtime.runs.iter_mut().filter(|run| run.chat_id.as_deref() == Some(advisor) && run.bot_turn.is_some()) {
        run.status = "completed".into();
        run.attempts = vec![crate::usage::Attempt { review_only: true, ..Default::default() }];
    }
}

#[test]
fn a_timed_out_turn_is_one_advisor_fact_until_the_member_settles_otherwise() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    let tester = spawned(&mut state, &root, "Tester");
    let advisor = advisor(&mut state, &root);
    // A running member keeps the team from going quiet.
    turn(&mut state, &tester, "running");
    reconcile_at(&mut state, 1, ok).unwrap();
    // An ordinary single failure is not a fact; a timeout is.
    turn(&mut state, &worker, "failed");
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert!(lifecycle_facts(&state, &advisor).is_empty());
    let index = turn(&mut state, &worker, "failed");
    time_out(&mut state, index);
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert_eq!(lifecycle_facts(&state, &advisor), [Facts { timed_out: vec![worker.clone()], chats: vec![worker.clone()], ..Facts::default() }]);
    finish_advisor(&mut state, &advisor);
    reconcile_at(&mut state, 120_000, ok).unwrap();
    assert_eq!(lifecycle_facts(&state, &advisor).len(), 1, "unchanged evidence is not reported again");
    // A later turn that settles otherwise re-arms it; the lead timing out is
    // named as the lead.
    turn(&mut state, &worker, "completed");
    reconcile_at(&mut state, 160_000, ok).unwrap();
    let index = turn(&mut state, &worker, "failed");
    time_out(&mut state, index);
    let index = turn(&mut state, &root, "failed");
    time_out(&mut state, index);
    reconcile_at(&mut state, 200_000, ok).unwrap();
    let facts = lifecycle_facts(&state, &advisor);
    assert_eq!(facts.len(), 2, "{facts:?}");
    let mut timed = facts[1].timed_out.clone();
    timed.sort();
    let mut expected = vec![root.clone(), worker.clone()];
    expected.sort();
    assert_eq!(timed, expected);
    let text = facts[1].describe(&root);
    assert!(text.contains(&format!("the lead {root}'s turn reached its time limit and was stopped before it checkpointed")), "{text}");
    assert!(text.contains(&format!("member {worker}'s turn reached its time limit")), "{text}");
    // The advisor lens (inline for the advisor) carries the split-or-delegate
    // suggestion; the wake names the facts and points at that procedure.
    let run = bot_runs(&state).into_iter().rfind(|run| run.chat_id.as_deref() == Some(advisor.as_str())).unwrap();
    assert!(run.objective.contains("turn reached its time limit") && run.objective.contains("following the Team advisor guidance"), "{}", run.objective);
    assert!(state.lens_library.get("advisor").unwrap().instructions.contains("For a turn stopped at its time limit, suggest splitting the work"));
}

#[test]
fn a_timed_out_owner_keeps_its_claim_and_note_and_can_resume_it() {
    let (mut state, root) = fixture();
    let worker = spawned(&mut state, &root, "Builder");
    let (acquired, _) = crate::rooms::claims::call(&mut state, &worker, "general", &json!({"key":"skills-library","action":"acquire","note":"half of the library written"})).unwrap();
    assert_eq!(acquired["applied"], true);
    let index = turn(&mut state, &worker, "failed");
    time_out(&mut state, index);
    // The owner will not act again without new input, so dependents learn of
    // it as an abandoned claim; ownership and the note stay.
    crate::rooms::claims::observe_abandoned(&mut state);
    let claim = crate::rooms::claims::find(&state, "general", "skills-library").unwrap().clone();
    assert!(claim.abandoned);
    assert_eq!((claim.owner.as_str(), claim.note.as_str()), (worker.as_str(), "half of the library written"));
    // Nobody else can take it over; the owner's next turn resumes it.
    let other = spawned(&mut state, &root, "Other");
    let (taken, _) = crate::rooms::claims::call(&mut state, &other, "general", &json!({"key":"skills-library","action":"acquire"})).unwrap();
    assert_eq!(taken["applied"], false);
    let (again, _) = crate::rooms::claims::call(&mut state, &worker, "general", &json!({"key":"skills-library","action":"acquire"})).unwrap();
    assert_eq!(again["reason"], "You already own this claim; use update with its revision.");
    let (resumed, _) = crate::rooms::claims::call(&mut state, &worker, "general", &json!({"key":"skills-library","action":"update","revision":claim.revision,"note":"half of the library written"})).unwrap();
    assert_eq!(resumed["applied"], true, "{resumed}");
    let claim = crate::rooms::claims::find(&state, "general", "skills-library").unwrap();
    assert!(!claim.abandoned && claim.status == crate::rooms::claims::Status::Working);
    assert_eq!(claim.note, "half of the library written");
}

#[test]
fn a_bot_woken_after_a_timed_out_turn_is_told_to_split_or_delegate() {
    let (mut state, chat) = fixture();
    command_at(&mut state, &chat, "on", Now(0)).unwrap();
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    let index = state.runtime.runs.iter().rposition(|run| run.bot_turn.is_some() && run.status == "queued").unwrap();
    state.runtime.runs[index].attempts = vec![crate::usage::Attempt::default()];
    time_out(&mut state, index);
    post(&mut state, Some("peer"), "Another callback");
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let run = bot_runs(&state).into_iter().last().unwrap();
    assert_eq!(run.status, "queued");
    assert!(run.objective.contains(TIMEOUT_NOTE), "{}", run.objective);
    let woke = &state.chats[0].messages.last().unwrap().text;
    assert!(woke.starts_with("Bot woke: new messages in room #general. Your previous turn hit its time limit"), "{woke}");
    // A turn that ended otherwise carries no such note.
    let index = state.runtime.runs.iter().rposition(|run| run.bot_turn.is_some() && run.status == "queued").unwrap();
    state.runtime.runs[index].status = "completed".into();
    state.runtime.runs[index].attempts = vec![crate::usage::Attempt::default()];
    post(&mut state, Some("peer"), "A third callback");
    reconcile_at(&mut state, 80_000, ok).unwrap();
    let run = bot_runs(&state).into_iter().last().unwrap();
    assert!(!run.objective.contains("time limit"), "{}", run.objective);
}
