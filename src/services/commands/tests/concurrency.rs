//! Several commands share one service; copies take it exclusively.
use super::*;

fn submit(state: &mut State, run: &str, node: &NodeId, id: &str, nonce: &str, command: &str) -> io::Result<String> {
    admit(state, run, "attempt-a", nonce, node, input(id, command), |_| Ok(()))
        .map(|value| value["execution_id"].as_str().unwrap().to_owned())
}
fn copy_work(id: &str) -> Work {
    Work {
        service_id: id.into(),
        digest: hash(b"copy"),
        command: String::new(),
        display: "copy /workspace/tool -> service /workspace/tool".into(),
        timeout_seconds: 60,
        copy: Some(copy::Transfer::new("tool", "tool").unwrap()),
    }
}
fn launchable_ids(state: &State) -> Vec<String> {
    launchable(&state.services[0].executions).into_iter().map(|e| handle(&e.id).to_owned()).collect()
}
fn set(state: &mut State, id: &str, status: Status) {
    state.services[0].executions.iter_mut().find(|e| names(&e.id, id)).unwrap().status = status;
}

#[test]
fn concurrent_commands_launch_together_and_report_independently() {
    let (mut state, run, node, service) = fixture();
    let first = submit(&mut state, &run, &node, &service, "player-a", "status --watch").unwrap();
    let second = submit(&mut state, &run, &node, &service, "player-b", "status").unwrap();
    assert_ne!(first, second);
    // Both launch in one dispatch pass; one running never holds back another.
    assert_eq!(launchable_ids(&state), [first.clone(), second.clone()]);
    set(&mut state, &first, Status::Running);
    assert_eq!(launchable_ids(&state), std::slice::from_ref(&second));
    let snapshot = state.services[0].clone();
    let shared = Arc::new(Mutex::new(state));
    let finish = |id: &str, output: &str, code: i32| {
        update(&shared, &snapshot, id, |e| {
            e.status = if code == 0 { Status::Completed } else { Status::Failed };
            e.output = output.into();
            e.exit_code = Some(code);
        }, false).unwrap();
    };
    update(&shared, &snapshot, &second, |e| e.status = Status::Running, false).unwrap();
    finish(&second, "ok\n", 0);
    let read = |id: &str| status(&shared, &run, "attempt-a", &node, Handle { service_id: service.clone(), execution_id: id.into(), wait_seconds: 0 }).unwrap();
    let (a, b) = (read(&first), read(&second));
    assert_eq!((a["status"].as_str(), b["status"].as_str()), (Some("running"), Some("completed")));
    assert_eq!((a["output"].as_str(), b["output"].as_str()), (None, Some("ok\n")));
    finish(&first, "lost\n", 2);
    let a = read(&first);
    assert_eq!((a["status"].as_str(), a["exit_code"].as_i64(), a["output"].as_str()), (Some("failed"), Some(2), Some("lost\n")));
    assert_eq!(read(&second)["exit_code"], 0);
    // Each command keeps its own request identity: a retry returns its own record.
    let mut state = shared.lock().unwrap();
    assert_eq!(submit(&mut state, &run, &node, &service, "player-a", "status --watch").unwrap(), first);
    assert_eq!(state.services[0].running_commands(), Vec::<String>::new());
}

#[test]
fn the_limit_refuses_with_active_handles_and_frees_on_completion() {
    let (mut state, run, node, service) = fixture();
    state.services[0].pids_limit = Some(4096);
    let limit = Concurrency::DEFAULT.0;
    let ids = (0..limit)
        .map(|n| submit(&mut state, &run, &node, &service, &format!("c-{n}"), &format!("job {n}")).unwrap())
        .collect::<Vec<_>>();
    set(&mut state, &ids[0], Status::Running);
    let before = serde_json::to_vec(&state).unwrap();
    let refused = submit(&mut state, &run, &node, &service, "over", "one more").unwrap_err().to_string();
    assert_eq!(serde_json::to_vec(&state).unwrap(), before, "a refusal saves nothing");
    assert!(refused.contains(&format!("running {limit} of {limit} concurrent commands")), "{refused}");
    assert!(refused.contains("service-commands-max") && refused.contains("nothing was submitted"), "{refused}");
    let chat = state.chats[0].id.clone();
    assert!(refused.contains(&format!("{} (running, chat {chat}: job 0)", ids[0])), "{refused}");
    assert!(ids.iter().all(|id| refused.contains(id.as_str())), "{refused}");
    set(&mut state, &ids[3], Status::Completed);
    submit(&mut state, &run, &node, &service, "over", "one more").unwrap();
    assert_eq!(state.services[0].executions.iter().filter(|e| e.status.active()).count(), limit);
}

#[test]
fn a_small_service_admits_no_more_commands_than_its_process_limit() {
    let (mut state, run, node, service) = fixture();
    state.services[0].pids_limit = Some(3);
    for n in 0..3 {
        submit(&mut state, &run, &node, &service, &format!("p-{n}"), "true").unwrap();
    }
    let refused = submit(&mut state, &run, &node, &service, "p-3", "true").unwrap_err().to_string();
    assert!(refused.contains("running 3 of 3") && refused.contains("process limit of 3") && refused.contains("larger shape"), "{refused}");
    // Every admitted service shape allows far more than the old fixed eight.
    let (state, ..) = fixture();
    assert_eq!(Concurrency::DEFAULT.for_service(&state.services[0]), 64);
}

#[test]
fn persisted_command_records_stay_bounded() {
    // The largest admissible command: 16,000-byte text, 2,000-character label
    // and full 16,000-byte output. Launch clears the command text.
    let (mut state, run, node, service) = fixture();
    let text = "x".repeat(16_000);
    let id = submit(&mut state, &run, &node, &service, "big", &text).unwrap();
    let size = |state: &State| serde_json::to_vec(state.services[0].executions.iter().find(|e| names(&e.id, &id)).unwrap()).unwrap().len();
    // A request holds its text only until dispatch launches it (normally < 2 s).
    let requested = size(&state);
    let snapshot = state.services[0].clone();
    let shared = Arc::new(Mutex::new(state));
    // What dispatch records at launch; the launched snapshot keeps the text.
    update(&shared, &snapshot, &id, |e| {
        e.status = Status::Running;
        e.command.clear();
        e.output = "y".repeat(OUTPUT);
    }, false).unwrap();
    let running = size(&shared.lock().unwrap());
    update(&shared, &snapshot, &id, |e| e.status = Status::Completed, false).unwrap();
    let finished = size(&shared.lock().unwrap());
    println!("requested {requested} B, running {running} B, finished {finished} B");
    assert!(requested < 19_000 && running < 19_000 && finished < 19_000, "{requested} {running} {finished}");
}

#[test]
fn settings_bound_concurrency() {
    let read = |value: Option<&str>| Concurrency::from_settings(|name| value.filter(|_| name == "service-commands-max").map(str::to_owned));
    assert_eq!(read(None), Concurrency(64));
    assert_eq!(read(Some("3\n")), Concurrency(3));
    assert_eq!(read(Some("1024")), Concurrency(1024));
    for invalid in ["0", "1025", "many", "-1"] {
        assert_eq!(read(Some(invalid)), Concurrency::DEFAULT);
    }
}

#[test]
fn a_copy_waits_for_running_commands_and_holds_the_service() {
    let (mut state, run, node, service) = fixture();
    let running = submit(&mut state, &run, &node, &service, "build", "cargo build").unwrap();
    let queued = submit(&mut state, &run, &node, &service, "probe", "ls").unwrap();
    set(&mut state, &running, Status::Running);
    let copy = admit_work(&mut state, &run, "attempt-a", "copy", &node, copy_work(&service), |_| Ok(())).unwrap();
    assert_eq!(copy["status"], "requested");
    assert!(copy["diagnostic"].as_str().unwrap().contains("Waiting for 2 running command(s)"));
    let copy = copy["execution_id"].as_str().unwrap().to_owned();
    // Nothing new is admitted behind the copy, and the refusal names it.
    let refused = submit(&mut state, &run, &node, &service, "late", "cat tool/x").unwrap_err().to_string();
    assert!(refused.contains("reserved for copy") && refused.contains(&copy) && refused.contains("waiting"), "{refused}");
    assert!(admit_work(&mut state, &run, "attempt-a", "copy-2", &node, copy_work(&service), |_| panic!("no save")).is_err());
    // A command admitted before the copy still launches; the copy waits for it.
    assert_eq!(launchable_ids(&state), std::slice::from_ref(&queued));
    set(&mut state, &queued, Status::Running);
    assert!(launchable_ids(&state).is_empty());
    set(&mut state, &running, Status::Completed);
    assert!(launchable_ids(&state).is_empty(), "one command still runs");
    set(&mut state, &queued, Status::Failed);
    assert_eq!(launchable_ids(&state), std::slice::from_ref(&copy));
    set(&mut state, &copy, Status::Running);
    assert!(launchable_ids(&state).is_empty());
    assert!(submit(&mut state, &run, &node, &service, "late", "cat tool/x").unwrap_err().to_string().contains("reserved for copy"));
    set(&mut state, &copy, Status::Completed);
    submit(&mut state, &run, &node, &service, "late", "cat tool/x").unwrap();
    // A copy into an idle service launches at once, alone.
    let (mut state, run, node, service) = fixture();
    let copy = admit_work(&mut state, &run, "attempt-a", "copy", &node, copy_work(&service), |_| Ok(())).unwrap();
    assert!(copy.get("diagnostic").is_none(), "an unset diagnostic is omitted");
    assert_eq!(launchable_ids(&state), [copy["execution_id"].as_str().unwrap().to_owned()]);
}

#[test]
fn a_copy_admitted_behind_unlaunched_commands_does_not_launch_with_them() {
    let (mut state, run, node, service) = fixture();
    let first = submit(&mut state, &run, &node, &service, "a", "true").unwrap();
    admit_work(&mut state, &run, "attempt-a", "copy", &node, copy_work(&service), |_| Ok(())).unwrap();
    assert_eq!(launchable_ids(&state), [first], "the copy waits for commands launched in the same pass");
}

#[test]
fn finished_outputs_are_retained_beside_active_commands() {
    let (mut state, run, node, service) = fixture();
    for n in 0..MAX_EXECUTIONS {
        let id = submit(&mut state, &run, &node, &service, &format!("done-{n}"), "true").unwrap();
        set(&mut state, &id, Status::Completed);
    }
    let active = (0..4).map(|n| submit(&mut state, &run, &node, &service, &format!("live-{n}"), "sleep 1").unwrap()).collect::<Vec<_>>();
    let executions = &state.services[0].executions;
    assert_eq!(executions.iter().filter(|e| !e.status.active()).count(), MAX_EXECUTIONS - 1, "one finished output made room; the rest stay");
    assert!(active.iter().all(|id| executions.iter().any(|e| names(&e.id, id))), "active commands are never evicted");
    assert_eq!(state.services[0].command_receipts.len(), 1, "the evicted output keeps a retry receipt");
}

#[test]
fn restart_interrupts_every_running_command_once_and_reads_legacy_records() {
    let (mut state, run, node, service) = fixture();
    let ids = (0..3).map(|n| submit(&mut state, &run, &node, &service, &format!("r-{n}"), "sleep 9").unwrap()).collect::<Vec<_>>();
    set(&mut state, &ids[0], Status::Running);
    set(&mut state, &ids[1], Status::Running);
    // state.json from before concurrency has the same shape: a list with at
    // most one active record. Both round-trip through the current schema.
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(restored));
    let saves = std::cell::Cell::new(0);
    recover(&shared, |_| {
        saves.set(saves.get() + 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(saves.get(), 1, "one durable stop decision per service");
    let state = shared.lock().unwrap();
    assert!(matches!(state.services[0].resource.phase, Phase::Stopping { .. }));
    let by_id = |id: &str| state.services[0].executions.iter().find(|e| names(&e.id, id)).unwrap();
    for id in &ids[..2] {
        assert_eq!(by_id(id).status, Status::Interrupted);
        assert!(by_id(id).command.is_empty() && by_id(id).diagnostic.contains("no replay"));
    }
    assert_eq!(by_id(&ids[2]).status, Status::Requested, "dispatch cancels it because the service is stopping");
    assert!(launchable_ids(&state).contains(&ids[2]) && !matches!(state.services[0].resource.phase, Phase::Running(_)));
    drop(state);
    recover(&shared, |_| panic!("Recovery must not repeat")).unwrap();
    // A pre-concurrency record with one running command recovers the same way.
    let (mut legacy, run, node, service) = fixture();
    let only = submit(&mut legacy, &run, &node, &service, "only", "true").unwrap();
    set(&mut legacy, &only, Status::Running);
    let mut json = serde_json::to_value(&legacy).unwrap();
    assert!(json["services"][0]["executions"][0].get("copy").is_none(), "no new persisted fields");
    json["services"][0]["executions"][0].as_object_mut().unwrap().remove("display_command");
    let shared = Arc::new(Mutex::new(serde_json::from_value::<State>(json).unwrap()));
    recover(&shared, |_| Ok(())).unwrap();
    assert_eq!(shared.lock().unwrap().services[0].executions[0].status, Status::Interrupted);
}

#[test]
fn results_show_short_handles_and_every_lookup_takes_handle_or_full_id() {
    let (mut state, run, node, service) = fixture();
    let shown = submit(&mut state, &run, &node, &service, "short", "true").unwrap();
    let full = state.services[0].executions[0].id.clone();
    assert_eq!((shown.len(), full.len()), (HANDLE, 64));
    assert!(full.starts_with(&shown));
    // A retry returns the same handle: request identity is unchanged.
    assert_eq!(submit(&mut state, &run, &node, &service, "short", "true").unwrap(), shown);
    let shared = Arc::new(Mutex::new(state));
    let read = |id: &str| status(&shared, &run, "attempt-a", &node, Handle { service_id: service.clone(), execution_id: id.into(), wait_seconds: 0 });
    let (by_handle, by_full) = (read(&shown).unwrap(), read(&full).unwrap());
    assert_eq!(by_handle, by_full);
    // Only fields that are set: no output, exit code, diagnostic or constants yet.
    assert_eq!(by_handle, json!({"execution_id": shown, "status": "requested"}));
    assert!(read(&shown[..HANDLE - 1]).unwrap_err().to_string().contains("not found"), "shorter prefixes name nothing");
    // A handle that matches two records is refused rather than guessed.
    {
        let mut state = shared.lock().unwrap();
        let mut twin = state.services[0].executions[0].clone();
        twin.id = format!("{shown}{}", "f".repeat(64 - HANDLE));
        assert_ne!(twin.id, full);
        state.services[0].executions.push(twin);
    }
    assert!(read(&shown).unwrap_err().to_string().contains("pass the full execution_id"));
    assert_eq!(read(&full).unwrap()["status"], "requested", "the full ID stays exact");
    // An evicted record's receipt answers its handle with the outcome, minus constants.
    let receipt = Receipt { id: format!("{}{}", "e".repeat(HANDLE), "0".repeat(64 - HANDLE)), digest: String::new(), run: run.clone(), attempt: "attempt-a".into(), status: Status::Failed, exit_code: Some(3), diagnostic: String::new() };
    shared.lock().unwrap().services[0].command_receipts.push(receipt);
    assert_eq!(read(&"e".repeat(HANDLE)).unwrap(), json!({"execution_id": "e".repeat(HANDLE), "status": "failed", "exit_code": 3, "output_evicted": true}));
}
