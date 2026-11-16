use super::*;

/// Chats named by `names`, each owning one claim in `general`: `(key, blocked_on)`.
fn team(claims: &[(&str, Option<&str>)]) -> (State, Vec<String>) {
    let mut state = State::default();
    let chats = claims.iter().map(|_| crate::chat::create(&mut state)).collect::<Vec<_>>();
    for ((key, on), chat) in claims.iter().zip(&chats) {
        let mut args = json!({"key":key,"action":"acquire"});
        if let Some(on) = on {
            args["status"] = json!("blocked");
            args["blocked_on"] = json!(on);
        }
        let (value, _) = claims::call(&mut state, chat, "general", &args).unwrap();
        assert_eq!(value["applied"], true, "{value}");
    }
    (state, chats)
}
fn keys(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(|entry| entry.as_str().or(entry["key"].as_str()).unwrap()).collect()
}
fn done(state: &mut State, chat: &str, key: &str) {
    let revision = claims::find(state, "general", key).unwrap().revision;
    claims::call(state, chat, "general", &json!({"key":key,"action":"update","status":"done","revision":revision})).unwrap();
}

#[test]
fn the_plan_finds_the_critical_path_bottlenecks_and_parallel_frontier() {
    let (state, chats) = team(&[("axe", None), ("wood", Some("axe")), ("house", Some("wood")), ("roof", Some("wood")), ("paint", Some("primer")), ("fence", None)]);
    let plan = plan(&state, "general");
    assert_eq!(plan["room"], "general");
    assert_eq!(keys(&plan["open"]), ["axe", "wood", "house", "roof", "paint", "fence"]);
    assert_eq!(keys(&plan["critical_path"]), ["axe", "wood", "house"], "producer first; ties go to the first key");
    assert_eq!(plan["cycles"], json!([]));
    assert_eq!(keys(&plan["unclaimed"]), ["primer"]);
    // axe holds up wood, house and roof (three members); wood two; primer one.
    assert_eq!(keys(&plan["bottlenecks"]), ["axe", "wood", "primer"]);
    assert_eq!(plan["bottlenecks"][0]["dependents"], 3);
    let mut members = vec![chats[1].as_str(), chats[2].as_str(), chats[3].as_str()];
    members.sort_unstable();
    assert_eq!(plan["bottlenecks"][0]["members"], json!(members));
    assert_eq!((&plan["bottlenecks"][2]["owner"], &plan["bottlenecks"][2]["status"]), (&Value::Null, &json!("unclaimed")));
    // The frontier: the critical producer first, then by what each unblocks.
    assert_eq!(keys(&plan["ready"]), ["axe", "primer", "fence"]);
    assert_eq!((&plan["ready"][0]["critical"], &plan["ready"][0]["unblocks"], &plan["ready"][0]["why"]), (&json!(true), &json!(3), &json!("working")));
    assert_eq!((&plan["ready"][1]["why"], plan["ready"][1].get("owner")), (&json!("unclaimed"), None));
    // Only-blocked members can take ready work; working owners cannot.
    let available = plan["available"].as_array().unwrap().iter().map(|member| member["chat"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(available.len(), 4);
    assert!(!available.contains(&chats[0].as_str()) && !available.contains(&chats[5].as_str()));
    assert_eq!(plan["available"][0]["blocked_on"], json!(["axe"]));
    assert_eq!(plan["available"][0]["idle"], true, "no active turn");
}

#[test]
fn cycles_are_reported_and_kept_off_the_critical_path() {
    let (state, _) = team(&[("x", Some("y")), ("y", Some("x")), ("z", Some("x")), ("w", None)]);
    let plan = plan(&state, "general");
    let cycles = plan["cycles"].as_array().unwrap();
    assert_eq!(cycles.len(), 1);
    let mut cycle = keys(&cycles[0]);
    cycle.sort_unstable();
    assert_eq!(cycle, ["x", "y"]);
    assert_eq!(keys(&plan["critical_path"]), ["w"], "chains into a cycle never finish");
    assert_eq!(keys(&plan["ready"]), ["w"]);
    let x = plan["bottlenecks"].as_array().unwrap().iter().find(|neck| neck["key"] == "x").unwrap();
    assert_eq!((&x["cycle"], &x["dependents"]), (&json!(true), &json!(2)));
}

#[test]
fn finished_and_released_dependencies_change_what_is_ready() {
    let (mut state, chats) = team(&[("axe", None), ("wood", Some("axe")), ("house", Some("wood"))]);
    done(&mut state, &chats[0], "axe");
    let plan = plan(&state, "general");
    assert_eq!(plan["done"], 1);
    assert_eq!(keys(&plan["open"]), ["wood", "house"]);
    assert_eq!(keys(&plan["ready"]), ["wood"]);
    assert_eq!(plan["ready"][0]["why"], "resumable", "its dependency is done; the owner can resume");
    assert_eq!(keys(&plan["critical_path"]), ["wood", "house"]);
    assert_eq!(keys(&plan["bottlenecks"]), ["wood"], "a done producer is no bottleneck");
    // A released dependency is unclaimed work again; an abandoned claim is ready to reassign.
    let revision = claims::find(&state, "general", "wood").unwrap().revision;
    claims::call(&mut state, &chats[1], "general", &json!({"key":"wood","action":"release","status":"working","revision":revision})).unwrap();
    let plan = super::plan(&state, "general");
    assert_eq!(keys(&plan["unclaimed"]), ["wood"]);
    assert_eq!(keys(&plan["critical_path"]), ["wood", "house"]);
    state.collective.claims.iter_mut().find(|claim| claim.key == "house").unwrap().abandoned = true;
    let plan = super::plan(&state, "general");
    assert_eq!(plan["open"][0]["status"], "abandoned");
    assert!(plan["bottlenecks"].as_array().unwrap().is_empty(), "a lost owner's claim waits on nothing live");
}

#[test]
fn other_rooms_are_not_planned() {
    let (mut state, chats) = team(&[("axe", None)]);
    claims::call(&mut state, &chats[0], "research", &json!({"key":"secret","action":"acquire"})).unwrap();
    let plan = plan(&state, "general");
    assert_eq!(keys(&plan["open"]), ["axe"]);
    assert!(!plan.to_string().contains("secret"));
    let empty = super::plan(&state, "engineering");
    assert_eq!((&empty["open"], &empty["critical_path"], &empty["available"]), (&json!([]), &json!([]), &json!([])));
}
