use super::*;

fn setup() -> (State, String, String) {
    let mut state = State::default();
    let a = crate::chat::create(&mut state);
    let b = crate::chat::create(&mut state);
    (state, a, b)
}
fn claim(state: &mut State, chat: &str, args: Value) -> Value {
    call(state, chat, "general", &args).unwrap().0
}

#[test]
fn acquisition_is_exclusive_and_changes_need_owner_and_current_revision() {
    let (mut state, a, b) = setup();
    let won = claim(
        &mut state,
        &a,
        json!({"key":"tick-end","action":"acquire","note":"debugging the kick"}),
    );
    assert_eq!(won["applied"], true);
    assert_eq!(won["claim"]["status"], "working");
    let revision = won["claim"]["revision"].as_u64().unwrap();
    // An applied change returns what the host assigned, not the caller's own note, room or name.
    assert_eq!(
        won["claim"],
        json!({"key":"tick-end","status":"working","revision":revision,"incarnation":revision})
    );

    let lost = claim(&mut state, &b, json!({"key":"tick-end","action":"acquire"}));
    assert_eq!(
        lost["applied"], false,
        "a second acquire reports the owner without taking it"
    );
    assert_eq!(lost["claim"]["owner"], a.as_str());
    assert_eq!(
        lost["claim"]["note"], "debugging the kick",
        "a refusal shows the full live claim"
    );
    let foreign = claim(
        &mut state,
        &b,
        json!({"key":"tick-end","action":"release","revision":revision}),
    );
    assert_eq!(foreign["applied"], false);
    let stale = claim(
        &mut state,
        &a,
        json!({"key":"tick-end","action":"update","status":"done","revision":revision + 7}),
    );
    assert_eq!(stale["applied"], false);

    let done = claim(
        &mut state,
        &a,
        json!({"key":"tick-end","action":"update","status":"done","note":"fix verified on 5 clients","revision":revision}),
    );
    assert_eq!(done["claim"]["status"], "done");
    let released = claim(
        &mut state,
        &a,
        json!({"key":"tick-end","action":"release","revision":done["claim"]["revision"]}),
    );
    assert_eq!(released["claim"]["status"], "released");
    assert!(
        listed(&state, "general").is_empty(),
        "released claims are not shown as live"
    );

    let taken = claim(&mut state, &b, json!({"key":"tick-end","action":"acquire"}));
    assert_eq!(taken["applied"], true);
    assert!(
        taken["claim"]["revision"].as_u64().unwrap() > released["claim"]["revision"].as_u64().unwrap(),
        "revisions never repeat, so they can serve as wait cursors"
    );
}

#[test]
fn blocked_claims_name_a_different_dependency() {
    let (mut state, a, _) = setup();
    assert!(
        call(
            &mut state,
            &a,
            "general",
            &json!({"key":"house","action":"acquire","status":"blocked"})
        )
        .is_err()
    );
    assert!(
        call(
            &mut state,
            &a,
            "general",
            &json!({"key":"house","action":"acquire","status":"blocked","blocked_on":"house"})
        )
        .is_err()
    );
    assert!(
        call(
            &mut state,
            &a,
            "general",
            &json!({"key":"house","action":"acquire","blocked_on":"wood"})
        )
        .is_err()
    );
    let blocked = claim(
        &mut state,
        &a,
        json!({"key":"house","action":"acquire","status":"blocked","blocked_on":"wood"}),
    );
    assert_eq!(blocked["claim"]["blocked_on"], "wood");
    assert!(call(&mut state, &a, "general", &json!({"key":"bad/key","action":"acquire"})).is_err());
}

#[test]
fn operators_release_only_abandoned_claims_at_the_shown_revision() {
    let (mut state, a, _) = setup();
    let revision = claim(&mut state, &a, json!({"key":"server","action":"acquire"}))["claim"]["revision"]
        .as_u64()
        .unwrap();
    assert_eq!(
        listed(&state, "general")[0]["owner_idle"],
        true,
        "an owner with no active turn or bot cannot act now"
    );
    assert_eq!(listed(&state, "general")[0]["abandoned"], false, "idle is not lost");
    crate::chat::send(&mut state, &a, "Keep the server running", "test").unwrap();
    assert_eq!(listed(&state, "general")[0]["owner_idle"], false);
    assert!(operator_release(&mut state, "general", "server", revision).is_err());

    state
        .runtime
        .runs
        .iter_mut()
        .for_each(|run| run.status = "completed".into());
    assert!(operator_release(&mut state, "general", "server", revision + 1).is_err());
    let released = operator_release(&mut state, "general", "server", revision)
        .unwrap()
        .revision;
    assert!(released > revision);
    assert_eq!(find(&state, "general", "server").unwrap().status, Status::Released);
    assert!(render(&state, "general").contains("No live claims"));
}

#[test]
fn claim_waits_name_a_live_incarnation_and_refuse_resolved_or_unowned_keys() {
    let (mut state, a, b) = setup();
    let room = crate::rooms::resolve(&state, &b, "general").unwrap();
    assert!(
        wait_topic(&state, &b, "general/wood")
            .unwrap_err()
            .contains("nobody has committed"),
        "waiting on a key nobody produces would sleep forever"
    );
    let first = call(&mut state, &a, &room, &json!({"key":"wood","action":"acquire"}))
        .unwrap()
        .0["claim"]["incarnation"]
        .as_u64()
        .unwrap();
    assert_eq!(
        wait_topic(&state, &b, "general/wood").unwrap(),
        format!("claim:{room}/wood#{first}")
    );
    let (_, resolved) = call(
        &mut state,
        &a,
        &room,
        &json!({"key":"wood","action":"update","status":"blocked","blocked_on":"axe","revision":first}),
    )
    .unwrap();
    assert!(resolved.is_none(), "progress updates are not resolutions");
    let revision = find(&state, &room, "wood").unwrap().revision;
    let (_, resolved) = call(
        &mut state,
        &a,
        &room,
        &json!({"key":"wood","action":"update","status":"done","revision":revision}),
    )
    .unwrap();
    assert_eq!(resolved.unwrap().outcome, Outcome::Done);
    assert!(
        wait_topic(&state, &b, "general/wood")
            .unwrap_err()
            .contains("already resolved as done")
    );

    // Reusing the key starts a new incarnation; the old one stays resolved.
    let revision = find(&state, &room, "wood").unwrap().revision;
    let (_, resolved) = call(
        &mut state,
        &a,
        &room,
        &json!({"key":"wood","action":"release","revision":revision}),
    )
    .unwrap();
    assert!(resolved.is_none(), "releasing after done is not a second outcome");
    let second = call(&mut state, &b, &room, &json!({"key":"wood","action":"acquire"}))
        .unwrap()
        .0["claim"]["incarnation"]
        .as_u64()
        .unwrap();
    assert!(second > first);
    assert_eq!(resolution(&state, &room, "wood", first).unwrap().0, Outcome::Released);
    assert!(resolution(&state, &room, "wood", second).is_none());
    assert!(wait_topic(&state, &a, &format!("general/wood#{first}")).is_err());
}

#[test]
fn acquisition_reports_related_live_claims_without_blocking() {
    let (mut state, a, b) = setup();
    claim(
        &mut state,
        &a,
        json!({"key":"client-movement-diagnostic","action":"acquire","note":"invalid_player_movement kick after spawn"}),
    );
    claim(&mut state, &a, json!({"key":"farm-wall","action":"acquire"}));
    let mine = claim(
        &mut state,
        &b,
        json!({"key":"client-invalid-movement-diagnostic","action":"acquire"}),
    );
    assert_eq!(
        mine["applied"], true,
        "similarity is advisory; exact keys decide ownership"
    );
    let related = mine["related"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["key"], "client-movement-diagnostic");
    let by_note = claim(
        &mut state,
        &b,
        json!({"key":"kick-debug","action":"acquire","note":"spawn invalid_player_movement kick"}),
    );
    assert_eq!(
        by_note["related"][0]["key"], "client-movement-diagnostic",
        "notes count as well as keys"
    );
    assert!(
        claim(&mut state, &b, json!({"key":"bread-supply","action":"acquire"}))["related"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn owners_that_cannot_act_resolve_their_claims_once_as_abandoned() {
    let (mut state, a, _) = setup();
    crate::chat::send(&mut state, &a, "Build the farm", "test").unwrap();
    claim(&mut state, &a, json!({"key":"farm","action":"acquire"}));
    assert!(
        observe_abandoned(&mut state).is_empty(),
        "an active owner keeps its claim"
    );
    state
        .runtime
        .runs
        .iter_mut()
        .for_each(|run| run.status = "failed".into());
    let lost = observe_abandoned(&mut state);
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].outcome, Outcome::Abandoned);
    assert!(observe_abandoned(&mut state).is_empty(), "abandonment is reported once");
    let claim = find(&state, "general", "farm").unwrap();
    assert_eq!(
        resolution(&state, "general", "farm", claim.incarnation).unwrap().0,
        Outcome::Abandoned
    );
}

#[test]
fn an_owner_idle_after_a_completed_turn_is_not_lost_but_a_stopped_one_is() {
    let (mut state, a, _) = setup();
    crate::chat::send(&mut state, &a, "Steer the team", "test").unwrap();
    claim(&mut state, &a, json!({"key":"server","action":"acquire"}));
    state
        .runtime
        .runs
        .iter_mut()
        .for_each(|run| run.status = "completed".into());
    assert!(
        observe_abandoned(&mut state).is_empty(),
        "a steering chat between turns keeps its claims"
    );
    assert_eq!(listed(&state, "general")[0]["owner_idle"], true);
    let stopped = abandon_owned(&mut state, &a);
    assert_eq!(stopped.len(), 1, "an explicit stop resolves the owner's claims at once");
    assert_eq!(stopped[0].outcome, Outcome::Abandoned);
    assert!(abandon_owned(&mut state, &a).is_empty());
}

#[test]
fn rule_messages_state_the_enforced_limits() {
    assert!(KEY_RULE.contains(&format!("1–{MAX_KEY} ")));
    assert!(NOTE_RULE.contains(&format!(" {MAX_NOTE} ")));
}

#[test]
fn an_operator_release_ends_the_blocked_state() {
    let (mut state, a, _) = setup();
    let revision = claim(
        &mut state,
        &a,
        json!({"key":"house","action":"acquire","status":"blocked","blocked_on":"wood"}),
    )["claim"]["revision"]
        .as_u64()
        .unwrap();
    assert_eq!(find(&state, "general", "house").unwrap().blocked_at, revision);
    operator_release(&mut state, "general", "house", revision).unwrap();
    let released = find(&state, "general", "house").unwrap();
    assert_eq!(
        (released.status, released.blocked_on.as_deref(), released.blocked_at),
        (Status::Released, None, 0)
    );
}

#[test]
fn a_claim_advances_when_it_becomes_done_or_resumes_from_blocked() {
    let (mut state, a, _) = setup();
    let at = |state: &State| find(state, "general", "house").unwrap().advanced_at;
    let mut revision = claim(&mut state, &a, json!({"key":"house","action":"acquire"}))["claim"]["revision"]
        .as_u64()
        .unwrap();
    assert_eq!(at(&state), 0, "acquiring is not progress");
    let mut update = |state: &mut State, mut args: Value| {
        (args["key"], args["action"], args["revision"]) = (json!("house"), json!("update"), json!(revision));
        revision = claim(state, &a, args)["claim"]["revision"].as_u64().unwrap();
        revision
    };
    update(&mut state, json!({"status":"working","note":"still going"}));
    assert_eq!(at(&state), 0, "a working note is not progress");
    update(&mut state, json!({"status":"blocked","blocked_on":"wood"}));
    assert_eq!(at(&state), 0);
    let resumed = update(&mut state, json!({"status":"working"}));
    assert_eq!(at(&state), resumed, "blocked → working advances");
    update(&mut state, json!({"status":"working","note":"walls up"}));
    assert_eq!(at(&state), resumed);
    let done = update(&mut state, json!({"status":"done"}));
    assert_eq!(at(&state), done);
    update(&mut state, json!({"status":"done","note":"verified"}));
    assert_eq!(at(&state), done, "re-reporting done is the same advance");
    update(&mut state, json!({"status":"working"}));
    assert_eq!(at(&state), done);
    claim(
        &mut state,
        &a,
        json!({"key":"house","action":"release","revision":revision}),
    );
    assert_eq!(at(&state), done, "a release keeps the last advance");
    claim(&mut state, &a, json!({"key":"house","action":"acquire"}));
    assert_eq!(at(&state), 0, "a new incarnation starts unadvanced");
}

/// A wrong argument shape names the field or value and shows one valid
/// acquire, which is itself the fix.
#[test]
fn a_wrong_claim_shape_names_the_field_and_shows_a_valid_acquire() {
    let (mut state, a, _) = setup();
    for (args, names) in [
        (json!({"key":"wall","action":"claim"}), "unknown variant `claim`"),
        (
            json!({"key":"wall","action":"acquire","owner":"me"}),
            "unknown field `owner`",
        ),
        (json!({"action":"acquire"}), "missing field `key`"),
    ] {
        let shown = call(&mut state, &a, "general", &args).unwrap_err().into_owned();
        assert!(
            shown.contains(names) && shown.contains(r#"Example: control_room_claim {"key":"#),
            "{shown}"
        );
        assert_eq!(
            crate::worker_tools::fixes::actionable("control_room_claim", &shown),
            shown
        );
    }
    assert!(state.collective.claims.is_empty());
}
