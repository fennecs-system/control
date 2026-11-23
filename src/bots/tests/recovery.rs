//! Bounded recovery of persistent bots whose turns end without an accepted
//! handoff, and collection of finished, fully processed watch records.
use super::*;
use crate::runtime::RunStatus;
use control_context::{
    turn_mailbox::{Disposition, Range, WorkingState},
    working_state::Payload,
};

fn ok(_: &State) -> io::Result<()> {
    Ok(())
}
/// A persistent bot in `chat` with managed continuity and a published head.
fn managed_bot(state: &mut State, chat: &str) {
    command_at(state, chat, "on", Now(0)).unwrap();
    let session = state.chats.iter_mut().find(|c| c.id == chat).unwrap();
    session.continuity = Some(Box::default());
    session.workspace = Some("published".into());
}
fn chat_runs<'a>(state: &'a State, chat: &str) -> Vec<&'a crate::runtime::Run> {
    bot_runs(state)
        .into_iter()
        .filter(|run| run.chat_id.as_deref() == Some(chat))
        .collect()
}
/// Completes the chat's queued bot turn as a published session without an
/// accepted handoff; with `rejection`, the host refused its handoff.
fn fail_queued(state: &mut State, chat: &str, rejection: Option<&str>) -> bool {
    let Some(index) = state.runtime.runs.iter().rposition(|run| {
        run.chat_id.as_deref() == Some(chat) && run.bot_turn.is_some() && run.status == RunStatus::Queued
    }) else {
        return false;
    };
    let generation = format!("{}-attempt", state.runtime.runs[index].id);
    let run = &mut state.runtime.runs[index];
    run.status = RunStatus::Completed;
    *run.result = "Re-attached, re-checked health and started a new watch".into();
    run.attempts = vec![crate::usage::Attempt {
        context_trace: Some(Box::new(crate::turn_context::TraceRef {
            generation: generation.clone(),
            at: 0,
            sha256: String::new(),
            bytes: 0,
            version: 1,
        })),
        workspace_id: Some(generation),
        artifact: Some("published".into()),
        turn_mailbox: Some(Box::default()).into(),
        ..Default::default()
    }];
    if let Some(error) = rejection {
        reject_handoff(state, index, error);
    }
    true
}
fn continues(run: &crate::runtime::Run) -> bool {
    run.bot_turn.as_ref().unwrap().reasons.contains(&Reason::Continue)
}
fn checkpointing(state: &State, chat: &str) -> crate::turn_context::Checkpointing {
    state
        .chats
        .iter()
        .find(|c| c.id == chat)
        .unwrap()
        .continuity
        .as_ref()
        .unwrap()
        .checkpointing
        .clone()
}
/// Publishes an accepted handoff from the chat's latest run.
fn publish(state: &mut State, chat: &str, revision: u64, processed: Vec<Range>) {
    let index = state
        .runtime
        .runs
        .iter()
        .rposition(|run| run.chat_id.as_deref() == Some(chat))
        .unwrap();
    state.runtime.runs[index].status = RunStatus::Completed;
    let run = state.runtime.runs[index].id.clone();
    state
        .chats
        .iter_mut()
        .find(|c| c.id == chat)
        .unwrap()
        .continuity
        .as_mut()
        .unwrap()
        .working = Some(WorkingState {
        revision,
        run,
        generation: format!("published-{revision}"),
        checkpoint: "published".into(),
        payload: Payload {
            subgoal: "Keep the farm running".into(),
            ..Default::default()
        },
        processed,
        disposition: Disposition::Wait {
            topics: vec!["room:general".into()],
            recheck_seconds: None,
        },
    });
}

/// Runs -121's loop shape for `minutes`: every turn the bot gets ends with a
/// rejected handoff (or none), and the worker finishes turns at once. Returns
/// (turns, continue wakes).
fn persistent_failure(minutes: u64, rejection: Option<&str>) -> (State, String, usize, usize) {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "The farm needs checking");
    let mut now = 1_000;
    reconcile_at(&mut state, now, ok).unwrap();
    while now < minutes * 60_000 {
        fail_queued(&mut state, &chat, rejection);
        now += 15_000;
        reconcile_at(&mut state, now, ok).unwrap();
    }
    let runs = chat_runs(&state, &chat);
    let (turns, continued) = (runs.len(), runs.iter().filter(|run| continues(run)).count());
    (state, chat, turns, continued)
}

#[test]
fn persistent_handoff_rejection_is_bounded_instead_of_continuing_every_turn() {
    // -121: 217 continue wakes in ~3 h from one rejection that never cleared.
    let (state, chat, turns, continued) = persistent_failure(180, Some("Processed source budget exceeded"));
    println!(
        "{}",
        json!({"simulation":"persistent_rejection","minutes":180,"turns":turns,"continue_wakes":continued})
    );
    assert_eq!(continued, 2, "the default allowance of two immediate recoveries");
    // Initial wake, two continues, then rechecks after 5, 10, 20, 40 and 80
    // minutes; the next would come after 160.
    assert_eq!(turns, 8);
    let streak = checkpointing(&state, &chat);
    assert_eq!(streak.failures, 8);
    assert!(streak.reported);
    assert!(status(&state, &chat).unwrap().ends_with(" · cannot checkpoint"));
    let latest = chat_runs(&state, &chat).into_iter().last().unwrap();
    assert_eq!(
        latest.bot_turn.as_ref().unwrap().reasons,
        [Reason::Timer],
        "rechecks are timers, not continues"
    );
}

#[test]
fn turns_without_any_handoff_wait_with_a_recheck_instead_of_looping() {
    // -122 looped about once a minute on turns that never called the handoff.
    let (_, _, turns, continued) = persistent_failure(30, None);
    println!(
        "{}",
        json!({"simulation":"no_handoff","minutes":30,"turns":turns,"continue_wakes":continued})
    );
    assert_eq!(continued, 0, "a turn without a handoff is waiting, never runnable work");
    // Initial wake, then rechecks after 5, 5, 5 and 10 minutes.
    assert_eq!(turns, 5);
}

#[test]
fn a_turn_without_any_handoff_waits_instead_of_continuing() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    assert!(fail_queued(&mut state, &chat, None));
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(chat_runs(&state, &chat).len(), 1, "no immediate continue");
    assert!(status(&state, &chat).unwrap().contains("sleeping"));
    reconcile_at(&mut state, 40_000 + 299_000, ok).unwrap();
    assert_eq!(chat_runs(&state, &chat).len(), 1, "nothing before the recheck");
    reconcile_at(&mut state, 40_000 + 300_000, ok).unwrap();
    let runs = chat_runs(&state, &chat);
    assert_eq!(runs.len(), 2, "the recheck wakes it");
    let turn = runs[1].bot_turn.as_ref().unwrap();
    assert_eq!(turn.reasons, [Reason::Timer]);
    assert_eq!(
        turn.recovery.as_deref(),
        Some(runs[0].id.as_str()),
        "the report remains recovery evidence"
    );
    assert!(runs[1].objective.contains("ended without calling control_turn_handoff"));
    assert!(!checkpointing(&state, &chat).reported, "one waiting turn is not a fact");
}

#[test]
fn a_parked_bot_wakes_on_real_events_before_its_recheck() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    for at in [40_000, 80_000, 120_000] {
        assert!(fail_queued(
            &mut state,
            &chat,
            Some("Cannot process an unseen event range")
        ));
        reconcile_at(&mut state, at, ok).unwrap();
    }
    assert!(!fail_queued(&mut state, &chat, None), "parked after two continues");
    assert!(
        state
            .runtime
            .notice
            .starts_with(&format!("Bot in chat {chat} cannot checkpoint: 3 turn(s)")),
        "{}",
        state.runtime.notice
    );
    reconcile_at(&mut state, 200_000, ok).unwrap();
    assert_eq!(chat_runs(&state, &chat).len(), 3);
    post(&mut state, Some("peer"), "A new finding");
    reconcile_at(&mut state, 201_000, ok).unwrap();
    let runs = chat_runs(&state, &chat);
    assert_eq!(runs.len(), 4, "a subscribed room post releases the parked wait");
    assert!(runs[3].bot_turn.as_ref().unwrap().reasons.iter().any(|reason| matches!(
        reason,
        Reason::Info {
            info: Message::Post,
            ..
        }
    )));
    // A direct message (own conversation) also releases it.
    assert!(fail_queued(
        &mut state,
        &chat,
        Some("Cannot process an unseen event range")
    ));
    reconcile_at(&mut state, 240_000, ok).unwrap();
    assert_eq!(chat_runs(&state, &chat).len(), 4);
    peer_message_at(&mut state, &chat, 7, Now(250_000));
    reconcile_at(&mut state, 251_000, ok).unwrap();
    assert_eq!(
        chat_runs(&state, &chat).len(),
        5,
        "own-conversation evidence releases it"
    );
}

#[test]
fn an_accepted_handoff_resets_the_streak() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    for at in [40_000, 80_000, 120_000] {
        fail_queued(
            &mut state,
            &chat,
            Some("Retained handoff refers to an unavailable receipt"),
        );
        reconcile_at(&mut state, at, ok).unwrap();
    }
    assert_eq!(checkpointing(&state, &chat).failures, 3);
    assert!(checkpointing(&state, &chat).reported);
    // A real event wakes it and that turn's handoff publishes.
    post(&mut state, Some("peer"), "A new finding");
    reconcile_at(&mut state, 160_000, ok).unwrap();
    publish(&mut state, &chat, 1, vec![]);
    reconcile_at(&mut state, 200_000, ok).unwrap();
    let streak = checkpointing(&state, &chat);
    assert_eq!(
        (streak.failures, streak.reported),
        (0, false),
        "the published handoff ends the streak"
    );
    assert!(!status(&state, &chat).unwrap().contains("cannot checkpoint"));
    // A later rejection gets the full allowance again.
    post(&mut state, Some("peer"), "Another finding");
    reconcile_at(&mut state, 240_000, ok).unwrap();
    let before = chat_runs(&state, &chat).len();
    fail_queued(
        &mut state,
        &chat,
        Some("Working state revision changed before publication"),
    );
    reconcile_at(&mut state, 280_000, ok).unwrap();
    let runs = chat_runs(&state, &chat);
    assert_eq!(runs.len(), before + 1);
    assert!(
        continues(runs.last().unwrap()),
        "a new streak starts with an immediate recovery"
    );
    assert_eq!(checkpointing(&state, &chat).failures, 1);
}

#[test]
fn recovery_wake_text_carries_the_last_rejection_not_a_generic_continue() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    fail_queued(
        &mut state,
        &chat,
        Some("Cannot process an unseen event range. No draft was accepted and no cursor advanced."),
    );
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let run = chat_runs(&state, &chat).into_iter().last().unwrap();
    assert!(continues(run));
    assert!(
        run.objective
            .contains("Host error: \"Cannot process an unseen event range\""),
        "{}",
        run.objective
    );
    assert!(
        run.objective.contains("Omit processed from your handoff"),
        "{}",
        run.objective
    );
    assert!(run.objective.contains("do not re-verify them or start new watches"));
    assert!(
        !run.objective
            .contains("unfinished runnable work from your committed checkpoint")
    );
    let woke = &state.chats[0].messages.last().unwrap().text;
    assert!(
        woke.starts_with("Bot woke: a turn that ended without an accepted handoff. Checkpoint recovery"),
        "{woke}"
    );
    // A rejection recorded for an earlier attempt is not this turn's.
    fail_queued(&mut state, &chat, None);
    reconcile_at(&mut state, 80_000, ok).unwrap();
    reconcile_at(&mut state, 80_000 + 300_000, ok).unwrap();
    let run = chat_runs(&state, &chat).into_iter().last().unwrap();
    assert!(
        run.objective.contains("ended without calling control_turn_handoff"),
        "{}",
        run.objective
    );
}

#[test]
fn a_member_that_cannot_checkpoint_is_reported_to_the_advisor_once() {
    let (mut state, root) = fixture();
    let member = spawned(&mut state, &root, "Farmer");
    let advisor = super::advisor::monitor(&mut state, &root);
    reconcile_at(&mut state, 1, ok).unwrap();
    managed_bot(&mut state, &member);
    post(&mut state, Some("peer"), "The farm needs checking");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    let mut now = 1_000;
    while now < 120 * 60_000 {
        fail_queued(&mut state, &member, Some("Processed source budget exceeded"));
        now += 15_000;
        reconcile_at(&mut state, now, ok).unwrap();
        // The advisor's turns finish at once.
        for run in state
            .runtime
            .runs
            .iter_mut()
            .filter(|run| run.chat_id.as_deref() == Some(advisor.as_str()) && run.bot_turn.is_some())
        {
            run.status = RunStatus::Completed;
            run.attempts = vec![crate::usage::Attempt {
                review_only: true,
                ..Default::default()
            }];
        }
    }
    let reports = bot_runs(&state)
        .into_iter()
        .filter(|run| run.chat_id.as_deref() == Some(advisor.as_str()))
        .flat_map(|run| run.bot_turn.as_ref().unwrap().reasons.iter())
        .filter_map(|reason| match reason {
            Reason::Info {
                info: Message::Lifecycle(facts),
                ..
            } => Some(facts.uncheckpointed.clone()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        reports.len(),
        1,
        "one fact per streak, however long it lasts: {reports:?}"
    );
    assert_eq!(reports[0].chat, member);
    assert_eq!(reports[0].turns, 3);
    assert_eq!(
        reports[0].rejection.as_deref(),
        Some("Processed source budget exceeded")
    );
    let text = lifecycle::Facts {
        uncheckpointed: reports,
        ..Default::default()
    }
    .describe(&root);
    assert!(text.contains(&format!("member {member} cannot checkpoint: 3 turn(s) ended without an accepted handoff; last rejection: \"Processed source budget exceeded\"")), "{text}");
}

#[test]
fn failed_saves_roll_back_the_streak() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    fail_queued(&mut state, &chat, Some("Processed source budget exceeded"));
    let before = serde_json::to_vec(&state).unwrap();
    assert!(reconcile_at(&mut state, 40_000, |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(checkpointing(&state, &chat).failures, 1);
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        checkpointing(&restored, &chat),
        checkpointing(&state, &chat),
        "the streak is durable"
    );
}

/// A watch of `chat` whose last frame is a final exit at sequence 2.
fn finished(state: &mut State, chat: &str, id: &str) {
    watching(state, chat, id);
    let watch = state.watches.last_mut().unwrap();
    watch.permanent = false;
    watch.observe_for_test(1, None);
    watch.observe_for_test(2, Some(0));
    assert!(!watch.active());
}

#[test]
fn finished_watches_are_collected_once_processed_without_redelivery() {
    use crate::watches::delivery::{live, prepare, record};
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    finished(&mut state, &chat, "w-done");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    assert_eq!(chat_runs(&state, &chat).len(), 1, "the exit wakes the bot");
    reconcile_at(&mut state, 2_000, ok).unwrap();
    assert!(
        state.watches.iter().any(|w| w.id == "w-done"),
        "kept while its wake is outstanding"
    );
    let range = Range::Watch {
        watch: "w-done".into(),
        after: 0,
        through: 2,
    };
    publish(&mut state, &chat, 1, vec![range.clone()]);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert!(
        !state.watches.iter().any(|w| w.id == "w-done"),
        "terminal and processed through its last frame"
    );
    assert!(
        !live(&state.watches)(&range),
        "the next publication retires its watermark"
    );
    // Nothing can read it again, so nothing can be redelivered.
    let id = state.runtime.runs[0].id.clone();
    state.runtime.runs[0].status = RunStatus::Running;
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        workspace_id: Some("later".into()),
        turn_mailbox: Some(Box::default()).into(),
        ..Default::default()
    });
    let later = AttemptRef {
        run: &id,
        generation: "later",
        nonce: "read",
    };
    let read = prepare(&state, &later, "w-done", None).unwrap();
    assert_eq!(
        record(&mut state, &later, "w-done", &read, 2, false, |_| panic!("no receipt")).unwrap(),
        None
    );
}

#[test]
fn unfinished_or_unprocessed_watches_are_kept() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    finished(&mut state, &chat, "w-partial");
    finished(&mut state, &chat, "w-unread");
    // Restarted after a transient exit: a later line supersedes the exit.
    finished(&mut state, &chat, "w-restarted");
    state.watches.last_mut().unwrap().observe_for_test(3, None);
    watching(&mut state, &chat, "w-running");
    state.watches.last_mut().unwrap().observe_for_test(1, None);
    // Given up permanently, processed, but one more frame observed after.
    finished(&mut state, &chat, "w-late");
    let late = state.watches.last_mut().unwrap();
    late.permanent = true;
    late.last_exit.as_mut().unwrap().gave_up = true;
    late.observe(&crate::watch_frame::Frame {
        seq: 3,
        event: crate::watch_frame::Event::Notification(b"late".to_vec()),
    });
    reconcile_at(&mut state, 1_000, ok).unwrap();
    let processed = |id: &str, through| Range::Watch {
        watch: id.into(),
        after: 0,
        through,
    };
    publish(
        &mut state,
        &chat,
        1,
        vec![
            processed("w-partial", 1),
            processed("w-restarted", 3),
            processed("w-running", 1),
            processed("w-late", 2),
        ],
    );
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let kept = state.watches.iter().map(|w| w.id.as_str()).collect::<Vec<_>>();
    assert_eq!(
        kept,
        ["w-partial", "w-unread", "w-restarted", "w-running", "w-late"],
        "none is both terminal and fully processed"
    );

    // A stopped watch processed through its last frame is collected, unless a
    // turn in its chat is still active.
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    watching(&mut state, &chat, "w-stopped");
    state.watches[0].observe_for_test(1, None);
    state.watches[0].stopped = true;
    reconcile_at(&mut state, 1_000, ok).unwrap();
    publish(&mut state, &chat, 1, vec![processed("w-stopped", 1)]);
    state.runtime.runs.push(state.runtime.runs[0].clone());
    state.runtime.runs.last_mut().unwrap().id = "active".into();
    state.runtime.runs.last_mut().unwrap().status = RunStatus::Running;
    reconcile_at(&mut state, 40_000, ok).unwrap();
    assert_eq!(state.watches.len(), 1, "an active attempt may hold a receipt");
    state.runtime.runs.last_mut().unwrap().status = RunStatus::Completed;
    publish(&mut state, &chat, 2, vec![processed("w-stopped", 1)]);
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert!(state.watches.is_empty());
}

/// A journal-style save: the patch from the last committed encoding, applied
/// to that document, must reproduce the state; a failed save commits nothing.
struct Journal {
    base: control_journal::snapshot::Encoded,
    document: serde_json::Value,
    patches: usize,
}
impl Journal {
    fn new(state: &State) -> Self {
        let base = crate::history::persisting(|| control_journal::snapshot::encode(state)).unwrap();
        let document = serde_json::from_slice(base.bytes()).unwrap();
        Self {
            base,
            document,
            patches: 0,
        }
    }
    fn save(&mut self, state: &State) -> io::Result<()> {
        let encoded = crate::history::persisting(|| control_journal::snapshot::encode(state))?;
        if let Some(patch) = control_journal::snapshot::diff(&self.base, &encoded) {
            control_journal::snapshot::apply(&mut self.document, control_journal::snapshot::edits(&patch)?)?;
            self.patches += 1;
        }
        let fresh: serde_json::Value =
            crate::history::uncached(|| crate::history::persisting(|| serde_json::to_value(state))).unwrap();
        assert_eq!(self.document, fresh, "the patch reproduces the saved state");
        self.base = encoded;
        Ok(())
    }
}

#[test]
fn the_streak_and_collected_watches_are_journal_patches_and_failed_saves_commit_none() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    finished(&mut state, &chat, "w-done");
    post(&mut state, Some("peer"), "The farm needs checking");
    let mut journal = Journal::new(&state);
    reconcile_at(&mut state, 1_000, |s| journal.save(s)).unwrap();
    // A failed recovery save leaves nothing new to journal.
    fail_queued(&mut state, &chat, Some("Processed source budget exceeded"));
    journal.save(&state).unwrap();
    assert!(reconcile_at(&mut state, 40_000, |_| Err(io::Error::other("disk full"))).is_err());
    let unchanged = crate::history::persisting(|| control_journal::snapshot::encode(&state)).unwrap();
    assert!(
        control_journal::snapshot::diff(&journal.base, &unchanged).is_none(),
        "rollback leaves nothing to journal"
    );
    // Recovery, parking and the reported streak are ordinary patches; mutated
    // runs drop their cached bytes, so the patch still reproduces the state.
    let mut now = 40_000;
    while now < 30 * 60_000 {
        reconcile_at(&mut state, now, |s| journal.save(s)).unwrap();
        fail_queued(&mut state, &chat, Some("Processed source budget exceeded"));
        journal.save(&state).unwrap();
        now += 15_000;
    }
    assert!(checkpointing(&state, &chat).reported);
    assert_eq!(
        journal.document["chats"][0]["continuity"]["checkpointing"]["reported"],
        true
    );
    // The publication ends the streak and deletes the drained watch in one patch.
    publish(
        &mut state,
        &chat,
        1,
        vec![Range::Watch {
            watch: "w-done".into(),
            after: 0,
            through: 2,
        }],
    );
    journal.save(&state).unwrap();
    let before = journal.patches;
    reconcile_at(&mut state, now + 1, |s| journal.save(s)).unwrap();
    assert_eq!(journal.patches, before + 1);
    assert!(state.watches.is_empty());
    assert_eq!(journal.document["watches"], json!([]));
    assert_eq!(
        journal.document["chats"][0]["continuity"]["checkpointing"]["failures"],
        json!(0)
    );
}

/// Fails the chat's queued bot turn as stopped at its time limit: no handoff,
/// no publication, the previous workspace head unchanged.
fn time_out_queued(state: &mut State, chat: &str) -> bool {
    if !fail_queued(state, chat, None) {
        return false;
    }
    let index = state
        .runtime
        .runs
        .iter()
        .rposition(|run| run.chat_id.as_deref() == Some(chat) && run.bot_turn.is_some())
        .unwrap();
    let run = &mut state.runtime.runs[index];
    run.status = RunStatus::Failed;
    *run.result = crate::worker::timeout_message(std::time::Duration::from_secs(600));
    let attempt = run.attempts.last_mut().unwrap();
    attempt.termination = Some(crate::attempt_control::Termination::TimedOut);
    attempt.artifact = None;
    true
}

#[test]
fn a_provider_failure_resumes_from_its_host_checkpoint_like_a_timeout() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    assert!(time_out_queued(&mut state, &chat));
    let index = state
        .runtime
        .runs
        .iter()
        .rposition(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[index].attempts[0].termination = Some(crate::attempt_control::Termination::ProviderError);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let runs = chat_runs(&state, &chat);
    assert_eq!(runs.len(), 2, "a turn cut off by its provider is runnable work");
    assert!(continues(runs[1]));
    assert!(
        runs[1].objective.contains(crate::bots::PROVIDER_NOTE),
        "{}",
        runs[1].objective
    );
    let woke = &state.chats[0].messages.last().unwrap().text;
    assert!(
        woke.starts_with("Bot woke: a turn ended by a provider failure."),
        "{woke}"
    );
    assert_eq!(
        checkpointing(&state, &chat).failures,
        1,
        "it shares the bounded recovery streak"
    );
}
#[test]
fn a_local_turn_stopped_at_its_step_cap_resumes_from_its_host_checkpoint() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    assert!(time_out_queued(&mut state, &chat));
    let index = state
        .runtime
        .runs
        .iter()
        .rposition(|run| run.bot_turn.is_some())
        .unwrap();
    state.runtime.runs[index].attempts[0].termination = Some(crate::attempt_control::Termination::StepLimit);
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let runs = chat_runs(&state, &chat);
    assert_eq!(runs.len(), 2, "a turn cut off at its step cap is runnable work");
    assert!(continues(runs[1]));
    assert!(
        runs[1].objective.contains(crate::bots::STEP_LIMIT_NOTE),
        "{}",
        runs[1].objective
    );
    let woke = &state.chats[0].messages.last().unwrap().text;
    assert!(
        woke.starts_with("Bot woke: a turn stopped at its model-step limit."),
        "{woke}"
    );
}
#[test]
fn a_timed_out_turn_continues_promptly_within_the_bounded_streak() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    assert!(time_out_queued(&mut state, &chat));
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let runs = chat_runs(&state, &chat);
    assert_eq!(runs.len(), 2, "cut off mid-work is runnable, not a parked recheck");
    let run = runs[1];
    assert!(continues(run));
    assert!(run.objective.contains(crate::bots::TIMEOUT_NOTE), "{}", run.objective);
    assert!(
        !run.objective
            .contains("unfinished runnable work from your committed checkpoint"),
        "{}",
        run.objective
    );
    assert!(
        run.bot_turn.as_ref().unwrap().recovery.is_none(),
        "no published report to recover from"
    );
    let woke = &state.chats[0].messages.last().unwrap().text;
    assert!(
        woke.starts_with("Bot woke: a turn stopped at its time limit. Your previous turn hit its time limit"),
        "{woke}"
    );
    assert_eq!(
        checkpointing(&state, &chat).failures,
        1,
        "it counts toward the recovery streak"
    );
    // Repeated timeouts stay bounded: two prompt continues, then a recheck.
    assert!(time_out_queued(&mut state, &chat));
    reconcile_at(&mut state, 80_000, ok).unwrap();
    assert!(continues(chat_runs(&state, &chat).into_iter().last().unwrap()));
    assert!(time_out_queued(&mut state, &chat));
    reconcile_at(&mut state, 120_000, ok).unwrap();
    assert_eq!(chat_runs(&state, &chat).len(), 3, "past the allowance it waits");
    assert!(checkpointing(&state, &chat).reported);
    reconcile_at(&mut state, 120_000 + 300_000, ok).unwrap();
    let latest = chat_runs(&state, &chat).into_iter().last().unwrap();
    assert_eq!(latest.bot_turn.as_ref().unwrap().reasons, [Reason::Timer]);
}

#[test]
fn an_unseen_range_rejection_says_how_to_resynchronise() {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    let omitted = format!(
        "Cannot process an unseen event range. No draft was accepted and no cursor advanced. Retained receipts omitted from this handoff (up to 32): [{}]",
        "\"r\",".repeat(120)
    );
    fail_queued(&mut state, &chat, Some(&omitted));
    reconcile_at(&mut state, 40_000, ok).unwrap();
    let run = chat_runs(&state, &chat).into_iter().last().unwrap();
    assert!(continues(run));
    assert!(
        run.objective
            .contains("Host error: \"Cannot process an unseen event range\""),
        "{}",
        run.objective
    );
    assert!(
        run.objective.contains(crate::bots::UNSEEN_RANGE.trim()),
        "the guidance is not truncated: {}",
        run.objective
    );
    assert!(
        !run.objective.contains("Retained receipts omitted"),
        "the previous turn's receipt list is stale"
    );
    // Other rejections quote the host error in full and get no such guidance.
    let other = recovery_wake("Processed source budget exceeded");
    assert!(
        other.contains("Host error: \"Processed source budget exceeded\"")
            && !other.contains(crate::bots::UNSEEN_RANGE.trim())
    );
}
fn recovery_wake(error: &str) -> String {
    let (mut state, chat) = fixture();
    managed_bot(&mut state, &chat);
    post(&mut state, Some("peer"), "A relevant callback");
    reconcile_at(&mut state, 1_000, ok).unwrap();
    fail_queued(&mut state, &chat, Some(error));
    reconcile_at(&mut state, 40_000, ok).unwrap();
    chat_runs(&state, &chat)
        .into_iter()
        .last()
        .unwrap()
        .objective
        .to_string()
}
