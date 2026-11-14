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
    let won = claim(&mut state, &a, json!({"key":"tick-end","action":"acquire","note":"debugging the kick"}));
    assert_eq!(won["applied"], true);
    assert_eq!(won["claim"]["status"], "working");
    let revision = won["claim"]["revision"].as_u64().unwrap();

    let lost = claim(&mut state, &b, json!({"key":"tick-end","action":"acquire"}));
    assert_eq!(lost["applied"], false, "a second acquire reports the owner without taking it");
    assert_eq!(lost["claim"]["owner"], a.as_str());
    let foreign = claim(&mut state, &b, json!({"key":"tick-end","action":"release","revision":revision}));
    assert_eq!(foreign["applied"], false);
    let stale = claim(&mut state, &a, json!({"key":"tick-end","action":"update","status":"done","revision":revision + 7}));
    assert_eq!(stale["applied"], false);

    let done = claim(&mut state, &a, json!({"key":"tick-end","action":"update","status":"done","note":"fix verified on 5 clients","revision":revision}));
    assert_eq!(done["claim"]["status"], "done");
    let released = claim(&mut state, &a, json!({"key":"tick-end","action":"release","revision":done["claim"]["revision"]}));
    assert_eq!(released["claim"]["status"], "released");
    assert!(listed(&state, "general").is_empty(), "released claims are not shown as live");

    let taken = claim(&mut state, &b, json!({"key":"tick-end","action":"acquire"}));
    assert_eq!(taken["applied"], true);
    assert!(taken["claim"]["revision"].as_u64().unwrap() > released["claim"]["revision"].as_u64().unwrap(),
        "revisions never repeat, so they can serve as wait cursors");
}

#[test]
fn blocked_claims_name_a_different_dependency() {
    let (mut state, a, _) = setup();
    assert!(call(&mut state, &a, "general", &json!({"key":"house","action":"acquire","status":"blocked"})).is_err());
    assert!(call(&mut state, &a, "general", &json!({"key":"house","action":"acquire","status":"blocked","blocked_on":"house"})).is_err());
    assert!(call(&mut state, &a, "general", &json!({"key":"house","action":"acquire","blocked_on":"wood"})).is_err());
    let blocked = claim(&mut state, &a, json!({"key":"house","action":"acquire","status":"blocked","blocked_on":"wood"}));
    assert_eq!(blocked["claim"]["blocked_on"], "wood");
    assert!(call(&mut state, &a, "general", &json!({"key":"bad/key","action":"acquire"})).is_err());
}

#[test]
fn operators_release_only_abandoned_claims_at_the_shown_revision() {
    let (mut state, a, _) = setup();
    let revision = claim(&mut state, &a, json!({"key":"server","action":"acquire"}))["claim"]["revision"].as_u64().unwrap();
    assert_eq!(listed(&state, "general")[0]["abandoned"], true, "an owner with no active turn or bot cannot act");
    crate::chat::send(&mut state, &a, "Keep the server running", "test").unwrap();
    assert_eq!(listed(&state, "general")[0]["abandoned"], false);
    assert!(operator_release(&mut state, "general", "server", revision).is_err());

    state.runtime.runs.iter_mut().for_each(|run| run.status = "completed".into());
    assert!(operator_release(&mut state, "general", "server", revision + 1).is_err());
    let released = operator_release(&mut state, "general", "server", revision).unwrap();
    assert!(released > revision);
    assert_eq!(find(&state, "general", "server").unwrap().status, Status::Released);
    assert!(render(&state, "general").contains("No live claims"));
}

#[test]
fn claim_waits_record_the_observed_revision_and_refuse_finished_claims() {
    let (mut state, a, b) = setup();
    let room = crate::rooms::resolve(&state, &b, "general").unwrap();
    assert_eq!(wait_topic(&state, &b, "general/wood").unwrap(), format!("claim:{room}/wood@0"), "an unclaimed key waits for its first claim");
    let revision = call(&mut state, &a, &room, &json!({"key":"wood","action":"acquire"})).unwrap().0["claim"]["revision"].as_u64().unwrap();
    assert_eq!(wait_topic(&state, &b, "general/wood").unwrap(), format!("claim:{room}/wood@{revision}"));
    assert!(wait_topic(&state, &b, &format!("general/wood@{}", revision - 1)).is_err(), "a change since the agent looked is reported now");
    call(&mut state, &a, &room, &json!({"key":"wood","action":"update","status":"done","revision":revision})).unwrap();
    assert!(wait_topic(&state, &b, "general/wood").unwrap_err().contains("already done"));
    assert!(wait_topic(&state, &b, "general/no keys here").is_err());
}
