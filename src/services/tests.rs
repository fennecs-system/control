
use super::*;
use std::cell::Cell;
// Only the uniquely owned QA workspace, removed after its test containers.
struct WorkspaceCleanup(Option<String>);
impl WorkspaceCleanup {
    fn of(service: &Service) -> Self {
        Self(workspace::Workspace::of(service).map(|workspace| workspace.name().to_owned()))
    }
}
impl Drop for WorkspaceCleanup {
    fn drop(&mut self) {
        if let Some(name) = &self.0 {
            let _ = crate::engine::query(&["volume", "rm", name]);
        }
    }
}
pub(super) fn fixture() -> (State, String, String, u64) {
    let mut s = State::default();
    let chat = crate::chat::create(&mut s);
    crate::chat::send(&mut s, &chat, "Run a persistent counter", "test").unwrap();
    let run = &mut s.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        image: Some(format!("sha256:{}", "a".repeat(64))),
        ..Default::default()
    });
    let run = run.id.clone();
    let time = now();
    (s, run, chat, time)
}
fn input(command: &str) -> Input {
    Input {
        command: command.to_owned().try_into().unwrap(),
        lease_seconds: 60,
        storage_limit_mb: None,
    }
}
pub(super) fn admit_fixture(
    s: &mut State,
    run: &str,
    time: u64,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<Value> {
    admit(
        s,
        Request {
            run,
            generation: "attempt-a",
            nonce: "nonce-a",
        },
        input("printf counter; sleep 30"),
        "node-a".to_owned().try_into().unwrap(),
        time,
        save,
    )
}
pub(super) fn inspection(s: &Service, id: &str) -> Inspection {
    Inspection {
        id: id.into(),
        name: format!("/{}", s.name()),
        image: s.resource.image.as_str().into(),
        node: Some(s.resource.owner.node.as_str().into()),
        resource: Some(s.resource.id.as_str().into()),
        chat: Some(s.resource.owner.chat.as_str().into()),
        generation: Some(s.resource.generation.get().to_string()),
        status: "created".into(),
        exit_code: None,
        network: "none".into(),
        readonly: s.readonly_root(),
        privileged: false,
        user: format!("{}:{}", s.uid, s.gid),
        pid_mode: String::new(),
        restart: "no".into(),
        caps: vec!["ALL".into()],
        security: vec!["no-new-privileges".into()],
        memory: 512 * 1024 * 1024,
        cpu: 1_000_000_000,
        pids: 64,
        mounts: workspace::Workspace::of(s).map(|workspace| vec![json!({"Type":"volume","Name":workspace.name(),"Driver":"local","Destination":"/workspace","RW":true})]).unwrap_or_default(),
        tmpfs: tmpfs(s),
        storage_opts: Some(s.storage.options()),
        entrypoint: vec!["control-service-runner".into()],
        command: vec![
            s.resource.lease.expires().to_string(),
            s.resource.lease.seconds().to_string(),
            s.command.0.to_string(),
        ],
    }
}
#[derive(Default)]
struct Fake {
    records: HashMap<String, Inspection>,
    hide_inventory: bool,
    unavailable: bool,
    creates: usize,
    starts: usize,
    removes: usize,
    inspections: usize,
}
impl Engine for Fake {
    fn logs(&mut self, _: &Verified<'_>) -> io::Result<String> {
        Ok("retained final diagnostic".into())
    }
    fn inventory(&mut self, _: &str) -> io::Result<HashMap<String, Inspection>> {
        self.inspections += 1;
        if self.unavailable {
            return Err(fail("Engine unavailable"));
        }
        Ok(if self.hide_inventory {
            HashMap::new()
        } else {
            self.records.clone()
        })
    }
    fn exists(&mut self, id: &ContainerId) -> io::Result<bool> {
        Ok(self.records.contains_key(id.as_str()))
    }
    fn create(&mut self, s: &Service) -> io::Result<ContainerId> {
        self.creates += 1;
        let id = "b".repeat(64);
        self.records.insert(id.clone(), inspection(s, &id));
        id.try_into().map_err(fail)
    }
    fn start(&mut self, binding: &Verified<'_>) -> io::Result<()> {
        self.starts += 1;
        self.records.get_mut(binding.id.as_str()).unwrap().status = "running".into();
        Ok(())
    }
    fn remove(&mut self, binding: &Verified<'_>) -> io::Result<()> {
        self.removes += 1;
        self.records.remove(binding.id.as_str());
        Ok(())
    }
}
#[test]
fn fenced_missing_services_release_capacity_only_after_persisted_stop_and_observation() {
    let (mut state, run, _, time) = fixture();
    for slot in 0..MAX_ACTIVE {
        admit(
            &mut state,
            Request {
                run: &run,
                generation: "attempt-a",
                nonce: &format!("slot-{slot}"),
            },
            input("sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Ok(()),
        )
        .unwrap();
        state.services[slot].resource.phase = Phase::Fenced {
            fault: Fault::UnexpectedEngineState,
            container: Some(format!("{slot:064x}").try_into().unwrap()),
        };
    }
    assert!(
        admit_fixture(&mut state, &run, time, |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("capacity")
    );
    let id = state.services[0].resource.id.as_str().to_owned();
    assert!(page::stop(&mut state, &id, time, |_| Err(fail("disk full"))).is_err());
    assert!(matches!(
        state.services[0].resource.phase,
        Phase::Fenced { .. }
    ));
    page::stop(&mut state, &id, time, |_| Ok(())).unwrap();
    assert!(admit_fixture(&mut state, &run, time, |_| Ok(())).is_err());
    // Restart preserves cleanup intent, without reviving the original work.
    let state: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let mut engine = Fake {
        unavailable: true,
        ..Default::default()
    };
    assert!(drive(&shared, &mut engine, "node-a", time, |_| Ok(())).is_err());
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopping { .. }
    ));
    engine.unavailable = false;
    drive(&shared, &mut engine, "node-a", time, |_| {
        Err(fail("disk full"))
    })
    .unwrap_err();
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopping { .. }
    ));
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    let mut state = shared.lock().unwrap();
    assert_eq!(
        state.services[0].resource.phase,
        Phase::Stopped(StopReason::Requested)
    );
    assert!(
        state.services[1..]
            .iter()
            .all(|s| matches!(s.resource.phase, Phase::Fenced { .. }))
    );
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    assert_eq!((engine.creates, engine.starts, engine.removes), (0, 0, 0));
}

#[test]
fn stopping_a_paused_service_reverifies_identity_and_retains_output() {
    for mismatch in [false, true] {
        let (mut state, run, _, time) = fixture();
        admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
        let shared = Arc::new(Mutex::new(state));
        let mut engine = Fake::default();
        for _ in 0..3 {
            drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        }
        engine.records.values_mut().next().unwrap().status = "paused".into();
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        {
            let mut state = shared.lock().unwrap();
            assert!(matches!(
                state.services[0].resource.phase,
                Phase::Fenced {
                    fault: Fault::UnexpectedEngineState,
                    ..
                }
            ));
            let id = state.services[0].resource.id.as_str().to_owned();
            page::stop(&mut state, &id, time, |_| Ok(())).unwrap();
        }
        if mismatch {
            engine.records.values_mut().next().unwrap().network = "bridge".into();
        }
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        assert!(
            shared.lock().unwrap().services[0]
                .resource
                .phase
                .reserves_capacity()
        );
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
        let state = shared.lock().unwrap();
        if mismatch {
            assert!(matches!(
                state.services[0].resource.phase,
                Phase::Fenced {
                    fault: Fault::IdentityMismatch,
                    ..
                }
            ));
            assert_eq!(engine.removes, 0);
        } else {
            assert_eq!(
                state.services[0].resource.phase,
                Phase::Stopped(StopReason::Requested)
            );
            assert_eq!(state.services[0].output, "retained final diagnostic");
            assert_eq!(engine.removes, 1);
        }
        assert_eq!((engine.creates, engine.starts), (1, 1));
    }
}

#[test]
fn missing_from_label_inventory_does_not_prove_a_recorded_container_is_gone() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let mut engine = Fake::default();
    for _ in 0..3 {
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    }
    {
        let mut state = shared.lock().unwrap();
        let id = state.services[0].resource.id.as_str().to_owned();
        page::stop(&mut state, &id, time, |_| Ok(())).unwrap();
    }
    engine.hide_inventory = true;
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Fenced {
            fault: Fault::IdentityMismatch,
            ..
        }
    ));
    assert_eq!(engine.removes, 0);
    assert_eq!(engine.records.len(), 1);
}

#[test]
fn exited_service_keeps_output_before_removal_and_failed_save_prevents_removal() {
    let (state, run, _, time) = fixture();
    let shared = Arc::new(Mutex::new(state));
    admit_fixture(&mut shared.lock().unwrap(), &run, time, |_| Ok(())).unwrap();
    let mut engine = Fake::default();
    for offset in 0..4 {
        drive(&shared, &mut engine, "node-a", time + offset, |_| Ok(())).unwrap();
    }
    let id = shared.lock().unwrap().services[0]
        .resource
        .phase
        .container()
        .unwrap()
        .as_str()
        .to_owned();
    engine.records.get_mut(&id).unwrap().status = "exited".into();
    engine.records.get_mut(&id).unwrap().exit_code = Some(7);
    drive(&shared, &mut engine, "node-a", time + 4, |state| {
        if state.services[0].output.is_empty() {
            Ok(())
        } else {
            Err(fail("disk full"))
        }
    })
    .unwrap();
    assert_eq!(engine.removes, 0);
    assert!(shared.lock().unwrap().services[0].output.is_empty());
    drive(&shared, &mut engine, "node-a", time + 5, |_| Ok(())).unwrap();
    assert_eq!(engine.removes, 1);
    drive(&shared, &mut engine, "node-a", time + 6, |_| Ok(())).unwrap();
    let restored: State =
        serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    assert!(matches!(
        restored.services[0].resource.phase,
        Phase::Stopped(_)
    ));
    assert_eq!(
        restored.services[0].summary()["output"],
        "retained final diagnostic"
    );
    assert_eq!(restored.services[0].summary()["exit_code"], 7);
}
#[test]
fn admission_is_bounded_deduplicated_and_rolls_back_before_any_effect() {
    let (mut s, run, _, time) = fixture();
    assert!(admit_fixture(&mut s, &run, time, |_| Err(fail("disk full"))).is_err());
    assert!(s.services.is_empty());
    let first = admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    assert_eq!(
        admit_fixture(&mut s, &run, time + 1, |_| panic!(
            "Duplicate must not save"
        ))
        .unwrap(),
        first
    );
    assert_eq!(s.services.len(), 1);
    assert!(
        admit(
            &mut s,
            Request {
                run: &run,
                generation: "attempt-a",
                nonce: "nonce-a"
            },
            input("different"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Ok(())
        )
        .is_err()
    );
    assert!(
        admit(
            &mut s,
            Request {
                run: &run,
                generation: "stale",
                nonce: "other"
            },
            input("different"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Ok(())
        )
        .is_err()
    );
    for n in 1..MAX_ACTIVE {
        admit(
            &mut s,
            Request {
                run: &run,
                generation: "attempt-a",
                nonce: &format!("nonce-{n}"),
            },
            input("sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Ok(()),
        )
        .unwrap();
    }
    assert!(
        admit(
            &mut s,
            Request {
                run: &run,
                generation: "attempt-a",
                nonce: "overflow"
            },
            input("sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Ok(())
        )
        .is_err()
    );
    let mut poisoned = serde_json::to_value(&s).unwrap();
    poisoned["services"][0]["resource"]["owner"]["chat"] = json!("../../host");
    assert!(serde_json::from_value::<State>(poisoned).is_err());
    let mut poisoned = serde_json::to_value(&s).unwrap();
    poisoned["services"][0]["uid"] = json!(0);
    assert!(serde_json::from_value::<State>(poisoned).is_err());
}
#[test]
fn persistence_failures_cannot_launch_and_created_id_is_recovered_before_start() {
    let (mut s, run, _, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let shared = Arc::new(Mutex::new(s));
    let mut engine = Fake::default();
    assert!(
        drive(&shared, &mut engine, "node-a", time, |_| Err(fail(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(engine.creates, 0);
    let writes = Cell::new(0);
    drive(&shared, &mut engine, "node-a", time, |_| {
        writes.set(writes.get() + 1);
        if writes.get() == 2 {
            Err(fail("lost created-ID save"))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!((engine.creates, engine.starts), (1, 0));
    assert_eq!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Creating
    );
    let restored: State =
        serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(restored));
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    assert_eq!((engine.creates, engine.starts), (1, 0));
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    assert_eq!((engine.creates, engine.starts), (1, 1));
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
}
#[test]
fn completed_history_is_bounded_without_replaying_live_attempt_receipts() {
    let (mut s, run, chat, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let service = s.services[0].clone();
    assert!(Arc::ptr_eq(&service.command.0, &service.clone().command.0));
    assert!(service.owned_by(&service.resource.owner.node, &chat, &Actor::LocalOperator));
    assert!(!service.owned_by(
        &service.resource.owner.node,
        "another-chat",
        &Actor::LocalOperator
    ));
    assert!(!service.owned_by(&service.resource.owner.node, &chat, &Actor::Unknown));
    assert!(!service.owned_by(
        &"another-node".to_owned().try_into().unwrap(),
        &chat,
        &Actor::LocalOperator
    ));
    s.services = (0..MAX_RECORDS)
        .map(|n| {
            let mut service = service.clone();
            service.resource.id = format!("old-{n}").try_into().unwrap();
            service.resource.phase = Phase::Stopped(StopReason::Exited);
            service
        })
        .collect();
    // The still-active origin retains every receipt, including stopped ones.
    assert!(admit_fixture(&mut s, &run, time, |_| Ok(())).is_err());
    let before = serde_json::to_vec(&s).unwrap();
    s.runtime.runs[0].attempts.last_mut().unwrap().workspace_id = Some("attempt-b".into());
    let request = Request {
        run: &run,
        generation: "attempt-b",
        nonce: "nonce-a",
    };
    assert!(
        admit(
            &mut s,
            request,
            input("sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| Err(fail("disk full"))
        )
        .is_err()
    );
    assert_eq!(s.services.len(), MAX_RECORDS);
    assert_eq!(s.services[0].resource.id.as_str(), "old-0");
    let first = admit(
        &mut s,
        request,
        input("sleep 30"),
        "node-a".to_owned().try_into().unwrap(),
        time,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(s.services.len(), MAX_RECORDS);
    assert_eq!(s.services[0].resource.id.as_str(), "old-1");
    assert_ne!(first["service_id"], json!(service.resource.id));
    assert_eq!(
        first,
        admit(
            &mut s,
            request,
            input("sleep 30"),
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| panic!("Duplicate must not save")
        )
        .unwrap()
    );
    assert!(
        admit_fixture(&mut s, &run, time, |_| Ok(())).is_err(),
        "Old attempts remain fenced even after receipt eviction"
    );
    let restored: State = serde_json::from_slice(&before).unwrap();
    assert_eq!(restored.services.len(), MAX_RECORDS);
}
#[test]
fn revocation_quiesces_a_running_service_and_failed_stop_save_cannot_remove_it() {
    let (mut s, run, _, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let shared = Arc::new(Mutex::new(s));
    let mut engine = Fake::default();
    for _ in 0..3 {
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    }
    shared.lock().unwrap().services[0].actor = Actor::Unknown;
    assert!(
        drive(&shared, &mut engine, "node-a", time, |_| Err(fail(
            "disk full"
        )))
        .is_err()
    );
    assert_eq!(engine.removes, 0);
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
    for _ in 0..3 {
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    }
    assert_eq!((engine.starts, engine.removes), (1, 1));
    assert_eq!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopped(StopReason::Revoked)
    );
}
#[test]
fn services_survive_turns_and_restoration_without_a_second_start() {
    let (mut s, run, _, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let shared = Arc::new(Mutex::new(s));
    let mut engine = Fake::default();
    for _ in 0..3 {
        drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    }
    {
        let mut s = shared.lock().unwrap();
        s.runtime.runs[0].status = "completed".into();
        s.runtime.runs[0].attempts.clear();
    }
    let restored: State =
        serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(restored));
    for _ in 0..3 {
        drive(&shared, &mut engine, "node-a", time + 1, |_| {
            panic!("Unchanged Running services must not save")
        })
        .unwrap();
    }
    assert_eq!((engine.creates, engine.starts, engine.removes), (1, 1, 0));
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
    drive(&shared, &mut engine, "node-a", time + 60, |_| Ok(())).unwrap();
    assert_eq!(engine.removes, 0, "Expiry persists stop intent first");
    drive(&shared, &mut engine, "node-a", time + 60, |_| Ok(())).unwrap();
    assert_eq!(engine.removes, 1);
    drive(&shared, &mut engine, "node-a", time + 60, |_| Ok(())).unwrap();
    assert_eq!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopped(StopReason::Expired)
    );
}
#[test]
fn scope_or_identity_mismatch_never_authorizes_start_or_removal() {
    let (mut s, run, _, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let mut service = s.services[0].clone();
    service.resource.phase = Phase::Creating;
    assert!(verify(&service, &inspection(&service, &"b".repeat(64))).is_ok());
    for field in [
        "network",
        "mount",
        "image",
        "node",
        "entrypoint",
        "deadline",
        "pid",
    ] {
        let mut o = inspection(&service, &"b".repeat(64));
        match field {
            "network" => o.network = "bridge".into(),
            "mount" => o
                .mounts
                .push(json!({"Type":"bind","Source":"/host","Destination":"/workspace"})),
            "image" => o.image = "latest".into(),
            "node" => o.node = Some("other".into()),
            "entrypoint" => o.entrypoint = vec!["bash".into()],
            "deadline" => o.command[0] = "99999999999".into(),
            "pid" => o.pid_mode = "host".into(),
            _ => unreachable!(),
        }
        assert!(verify(&service, &o).is_err(), "{field} must be enforced");
    }
    let shared = Arc::new(Mutex::new(s));
    let mut engine = Fake::default();
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    engine.records.values_mut().next().unwrap().network = "bridge".into();
    drive(&shared, &mut engine, "node-a", time, |_| Ok(())).unwrap();
    assert_eq!((engine.starts, engine.removes), (0, 0));
    let s = shared.lock().unwrap();
    assert!(matches!(s.services[0].resource.phase, Phase::Fenced { .. }));
    assert!(
        s.services[0].resource.phase.container().is_some(),
        "Fencing must retain the known container identity"
    );
}
#[test]
fn idle_supervision_does_not_query_the_engine() {
    let shared = Arc::new(Mutex::new(State::default()));
    let mut engine = Fake::default();
    drive(&shared, &mut engine, "node-a", now(), |_| {
        panic!("Idle must not save")
    })
    .unwrap();
    assert_eq!(engine.inspections, 0);
}
#[test]
fn requested_dispatch_snapshot_classifies_only_matching_dormant_creation() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let requested = state.services[0].clone();
    assert!(matches!(requested.resource.phase, Phase::Requested));
    let mut creating = requested.clone();
    creating.resource.phase = Phase::Creating;
    let observation = inspection(&creating, &"b".repeat(64));
    assert!(
        verify(&requested, &observation).is_err(),
        "Execution capabilities must remain strict"
    );
    let observations = HashMap::from([(observation.id.clone(), observation.clone())]);
    assert_eq!(
        verified_discovery_names(std::slice::from_ref(&requested), &observations).unwrap(),
        vec![requested.name()]
    );
    assert!(matches!(requested.resource.phase, Phase::Requested));
    for field in ["running", "exited", "image", "node", "mount", "name"] {
        let mut bad = observation.clone();
        match field {
            "running" => bad.status = "running".into(),
            "exited" => bad.status = "exited".into(),
            "image" => bad.image = format!("sha256:{}", "c".repeat(64)),
            "node" => bad.node = Some("foreign".into()),
            "mount" => bad
                .mounts
                .push(json!({"Type":"bind","Destination":"/workspace"})),
            _ => bad.name = "/unregistered".into(),
        };
        assert!(
            verified_discovery_names(
                std::slice::from_ref(&requested),
                &HashMap::from([(bad.id.clone(), bad)])
            )
            .is_err(),
            "{field}"
        );
    }
}
#[test]
fn executable_workspace_policy_is_captured_and_legacy_services_remain_unchanged() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let mut current = state.services[0].clone();
    assert!(current.workspace_exec);
    current.resource.phase = Phase::Creating;
    current.storage = Storage::Memory;
    let mut old = serde_json::to_value(&current).unwrap();
    assert_eq!(old["workspace_exec"], true);
    old.as_object_mut().unwrap().remove("workspace_exec");
    let legacy: Service = serde_json::from_value(old).unwrap();
    assert!(!legacy.workspace_exec);
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("workspace_exec")
            .is_none()
    );
    assert!(tmpfs(&current)["/workspace"].contains("rw,exec,nosuid,nodev"));
    assert!(!tmpfs(&legacy)["/workspace"].contains("exec"));
    assert_eq!(tmpfs(&current)["/tmp"], tmpfs(&legacy)["/tmp"]);
    let old_observation = inspection(&legacy, &"b".repeat(64));
    let new_observation = inspection(&current, &"b".repeat(64));
    assert!(verify(&legacy, &old_observation).is_ok());
    assert!(verify(&current, &new_observation).is_ok());
    assert!(verify(&legacy, &new_observation).is_err());
    assert!(verify(&current, &old_observation).is_err());
}
#[test]
fn disk_storage_defaults_and_captured_quota_are_verified() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let mut service = state.services[0].clone();
    service.resource.phase = Phase::Creating;
    assert_eq!(service.storage, Storage::Workspace { limit_mb: None });
    assert_eq!(tmpfs(&service).len(), 1);
    assert!(tmpfs(&service).contains_key("/tmp"));
    assert!(!service.readonly_root());
    let observed = inspection(&service, &"b".repeat(64));
    assert!(verify(&service, &observed).is_ok());
    service.storage = Storage::Workspace {
        limit_mb: NonZeroU32::new(4096),
    };
    assert!(
        verify(&service, &observed).is_err(),
        "Missing quota must fail closed"
    );
    let mut capped = inspection(&service, &"b".repeat(64));
    assert_eq!(capped.storage_opts.as_ref().unwrap()["size"], "4096M");
    assert!(verify(&service, &capped).is_ok());
    capped
        .storage_opts
        .as_mut()
        .unwrap()
        .insert("size".into(), "8192M".into());
    assert!(verify(&service, &capped).is_err());
    let restored: Service =
        serde_json::from_value(serde_json::to_value(&service).unwrap()).unwrap();
    assert_eq!(restored.storage, service.storage);
    assert_eq!(service.binding_snapshot().storage, service.storage);
    let mut old = serde_json::to_value(&service).unwrap();
    old.as_object_mut().unwrap().remove("storage");
    let legacy: Service = serde_json::from_value(old).unwrap();
    assert_eq!(legacy.storage, Storage::Memory);
    assert!(legacy.readonly_root());
    assert!(verify(&legacy, &inspection(&legacy, &"b".repeat(64))).is_ok());
    assert!(verify(&legacy, &observed).is_err());
    for limit in [json!(0), json!(-1), json!(4294967296u64), json!("1g")] {
        assert!(
            serde_json::from_value::<Input>(json!({"command":"sleep 30","storage_limit_mb":limit}))
                .is_err()
        );
    }
    let request = Request {
        run: &run,
        generation: "attempt-a",
        nonce: "nonce-a",
    };
    let mut retry = input("printf counter; sleep 30");
    retry.storage_limit_mb = NonZeroU32::new(4096);
    assert!(
        admit(
            &mut state,
            request,
            retry,
            "node-a".to_owned().try_into().unwrap(),
            time,
            |_| panic!("Quota changes must not save")
        )
        .is_err()
    );
}
#[test]
fn full_profile_captures_private_store_policy_and_legacy_stays_readonly() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let mut service = state.services[0].clone();
    service.storage = Storage::Memory;
    assert!(
        serde_json::to_value(&service)
            .unwrap()
            .get("profile")
            .is_none()
    );
    service.resource.phase = Phase::Creating;
    let mut observed = inspection(&service, &"b".repeat(64));
    service.profile = crate::worker_profile::Profile::Full;
    assert!(verify(&service, &observed).is_err());
    observed.memory = service.profile.service_memory();
    observed.pids = service.profile.service_pids();
    observed.tmpfs = tmpfs(&service);
    observed.readonly = false;
    assert!(verify(&service, &observed).is_ok());
    let restored: Service =
        serde_json::from_value(serde_json::to_value(&service).unwrap()).unwrap();
    assert_eq!(restored.profile, crate::worker_profile::Profile::Full);
    assert!(verify(&restored, &observed).is_ok());
    service.profile = crate::worker_profile::Profile::Lite;
    assert!(verify(&service, &observed).is_err());
}
#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated cleanup probe, no models or credentials"]
fn live_fenced_paused_service_can_be_stopped_without_restart() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
    let (mut state, run, _, time) = fixture();
    let node = format!(
        "fenced-qa-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut request = input("sleep infinity");
    request.lease_seconds = -1;
    admit_profile(
        &mut state,
        Request {
            run: &run,
            generation: "attempt-a",
            nonce: "fenced-probe",
        },
        request,
        node.clone().try_into().unwrap(),
        time,
        profile,
        |_| Ok(()),
    )
    .unwrap();
    state.services[0].resource.image = image.try_into().unwrap();
    state.services[0].resource.phase = Phase::Creating;
    let _workspace = WorkspaceCleanup::of(&state.services[0]);
    let id = Docker.create(&state.services[0]).unwrap();
    struct Container(ContainerId);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = crate::engine::query(&["rm", "-f", self.0.as_str()]);
        }
    }
    let _cleanup = Container(id.clone());
    state.services[0].resource.phase = Phase::Starting(id.clone());
    let shared = Arc::new(Mutex::new(state));
    for _ in 0..2 {
        drive(&shared, &mut Docker, &node, time, |_| Ok(())).unwrap();
    }
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
    crate::engine::query(&["pause", id.as_str()]).unwrap();
    drive(&shared, &mut Docker, &node, time, |_| Ok(())).unwrap();
    assert!(Docker.exists(&id).unwrap());
    {
        let mut state = shared.lock().unwrap();
        assert!(matches!(
            state.services[0].resource.phase,
            Phase::Fenced {
                fault: Fault::UnexpectedEngineState,
                ..
            }
        ));
        let service_id = state.services[0].resource.id.as_str().to_owned();
        page::stop(&mut state, &service_id, time, |_| Ok(())).unwrap();
    }
    drive(&shared, &mut Docker, &node, time, |_| Ok(())).unwrap();
    assert!(!Docker.exists(&id).unwrap());
    assert!(
        shared.lock().unwrap().services[0]
            .resource
            .phase
            .reserves_capacity()
    );
    drive(&shared, &mut Docker, &node, time, |_| Ok(())).unwrap();
    assert_eq!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopped(StopReason::Requested)
    );
}

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated storage probe, no models or credentials"]
fn live_workspace_survives_restart_exit_cleanup_and_container_replacement() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
    let (mut state, run, _, time) = fixture();
    let node = format!(
        "storage-qa-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    admit_profile(
        &mut state,
        Request {
            run: &run,
            generation: "attempt-a",
            nonce: "disk-probe",
        },
        input("sleep 120"),
        node.clone().try_into().unwrap(),
        time,
        profile,
        |_| Ok(()),
    )
    .unwrap();
    let mut service = state.services.remove(0);
    service.resource.image = image.try_into().unwrap();
    service.resource.phase = Phase::Creating;
    let _workspace = WorkspaceCleanup::of(&service);
    let id = Docker.create(&service).unwrap();
    struct Container(ContainerId);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = crate::engine::query(&["rm", "-f", self.0.as_str()]);
        }
    }
    let cleanup = Container(id.clone());
    let observations = Docker.inventory(&node).unwrap();
    verify(&service, &observations[id.as_str()]).unwrap();
    service.resource.phase = Phase::Starting(id.clone());
    let binding = verify(&service, &observations[id.as_str()]).unwrap();
    Docker.start(&binding).unwrap();
    let output = crate::engine::query(&["exec", id.as_str(), "bash", "-c", "set -eu; test ! -w /bin; test ! -w /bin/control-tcp-relay; dd if=/dev/zero of=/workspace/large.bin bs=1048576 count=540; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; printf persisted > /tmp/marker; printf persisted > /home/agent/marker; printf disk-probe-ok"]).unwrap();
    assert!(output.contains("disk-probe-ok"));
    crate::engine::query(&["stop", "--time=1", id.as_str()]).unwrap();
    crate::engine::query(&["start", id.as_str()]).unwrap();
    let output = crate::engine::query(&["exec", id.as_str(), "bash", "-c", "set -eu; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; test ! -e /tmp/marker; test \"$(cat /home/agent/marker)\" = persisted; printf restart-ok"]).unwrap();
    assert_eq!(output.trim(), "restart-ok");
    let observations = Docker.inventory(&node).unwrap();
    service.resource.phase = Phase::Running(id.clone());
    verify(&service, &observations[id.as_str()]).unwrap();
    assert!(
        observations[id.as_str()]
            .mounts
            .iter()
            .all(|m| (m["Type"] == "tmpfs" && m["Destination"] == "/tmp") || (m["Type"] == "volume" && m["Destination"] == "/workspace"))
    );
    // Reproduce daemon shutdown's observable consequence without restarting
    // the operator's Docker daemon or touching other services.
    crate::engine::query(&["stop", "--time=1", id.as_str()]).unwrap();
    let shared = Arc::new(Mutex::new(State::default()));
    shared.lock().unwrap().services.push(service.clone());
    drive(&shared, &mut Docker, &node, time, |_| Ok(())).unwrap();
    assert!(!Docker.exists(&id).unwrap(), "Exited containers are still removed");
    drive(&shared, &mut Docker, &node, time, |_| Ok(())).unwrap();
    assert_eq!(shared.lock().unwrap().services[0].resource.phase, Phase::Stopped(StopReason::Exited));
    drop(cleanup);
    assert!(Docker.inventory(&node).unwrap().is_empty());
    // A different service handle under the same owner reopens the volume.
    // Serialize first, like a Control restart, rather than relying on memory.
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    service = restored.services[0].binding_snapshot();
    service.resource.id = "replacement-probe".to_owned().try_into().unwrap();
    service.resource.phase = Phase::Creating;
    let replacement = Docker.create(&service).unwrap();
    let replacement_cleanup = Container(replacement.clone());
    let observations = Docker.inventory(&node).unwrap();
    service.resource.phase = Phase::Starting(replacement.clone());
    Docker.start(&verify(&service, &observations[replacement.as_str()]).unwrap()).unwrap();
    let output = crate::engine::query(&["exec", replacement.as_str(), "bash", "-c", "set -eu; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; test ! -e /home/agent/marker; test ! -e /tmp/marker; test ! -w /bin; printf replacement-ok"]).unwrap();
    assert_eq!(output.trim(), "replacement-ok");
    drop(replacement_cleanup);
    service.resource.phase = Phase::Creating;
    service.storage = Storage::Workspace {
        limit_mb: NonZeroU32::new(1024),
    };
    match Docker.create(&service) {
        Ok(id) => {
            let cleanup = Container(id.clone());
            let observations = Docker.inventory(&node).unwrap();
            verify(&service, &observations[id.as_str()]).unwrap();
            println!("Engine accepted and retained the explicit 1024 MiB quota");
            drop(cleanup);
        }
        Err(error) => {
            assert!(
                error
                    .to_string()
                    .contains("storage_limit_mb requires Docker storage-driver quota support"),
                "Unexpected engine failure: {error}"
            );
            assert!(
                Docker.inventory(&node).unwrap().is_empty(),
                "Rejected caps must not create an uncapped container"
            );
            println!("Engine rejected explicit cap; no uncapped fallback: {error}");
        }
    }
    // A pre-existing name alone must not authorize mounting foreign data.
    let workspace = workspace::Workspace::of(&service).unwrap();
    crate::engine::query(&["volume", "rm", workspace.name()]).unwrap();
    crate::engine::query(&["volume", "create", "--label", "dev.control.chat=foreign", workspace.name()]).unwrap();
    assert!(Docker.create(&service).unwrap_err().to_string().contains("workspace does not match"));
    assert!(Docker.inventory(&node).unwrap().is_empty());
}
#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE (new pinned worker image), CONTROL_QA_CONTROL_BIN and a local Linux engine; no credentials/models"]
fn live_service_survives_server_restart_and_expires_without_control() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
        time::Instant,
    };
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let binary = std::fs::canonicalize(std::env::var("CONTROL_QA_CONTROL_BIN").unwrap()).unwrap();
    let root = std::env::temp_dir().join(format!(
        "control-services-live-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let state_dir = root.join("state");
    let config = root.join("config");
    fs::create_dir(&state_dir).unwrap();
    fs::create_dir(&config).unwrap();
    let node = ring::digest::digest(&ring::digest::SHA256, root.to_string_lossy().as_bytes())
        .as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    fs::write(state_dir.join("node-id"), &node).unwrap();
    fs::set_permissions(state_dir.join("node-id"), fs::Permissions::from_mode(0o600)).unwrap();
    let (mut s, run, _, time) = fixture();
    let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
    let value=admit_profile(&mut s,Request{run:&run,generation:"attempt-a",nonce:"live"},Input{command:"printf durable > /workspace/counter; if (exec 3<> /proc/1/mem) 2>/dev/null; then exit 41; fi; kill -STOP 1 2>/dev/null || true; echo ready; sleep 60".to_owned().try_into().unwrap(),lease_seconds:20,storage_limit_mb:None},node.clone().try_into().unwrap(),time,profile,|_|Ok(())).unwrap();
    assert!(value["service_id"].is_string());
    s.services[0].resource.image = image.try_into().unwrap();
    let _workspace = WorkspaceCleanup::of(&s.services[0]);
    s.runtime.runs[0].status = "completed".into();
    fs::write(
        state_dir.join("state.json"),
        serde_json::to_vec(&s).unwrap(),
    )
    .unwrap();
    struct Server(std::process::Child);
    impl Server {
        fn stop(&mut self) {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            let start = Instant::now();
            while self.0.try_wait().unwrap().is_none() {
                if start.elapsed() > Duration::from_secs(10) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                    panic!("Server did not stop");
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let launch = || {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("server.log"))
            .unwrap();
        Server(
            Command::new(&binary)
                .arg("serve")
                .env("CONTROL_ADDR", addr)
                .env("CONTROL_CONFIG_DIR", &config)
                .env("CONTROL_STATE_DIR", &state_dir)
                .env_remove("CONTROL_WORKER_IMAGE")
                .env_remove("CONTROL_CLAUDE_IMAGE")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        )
    };
    let read = || {
        serde_json::from_slice::<State>(&fs::read(state_dir.join("state.json")).unwrap()).unwrap()
    };
    let wait_running = |server: &mut Server| {
        let started = Instant::now();
        loop {
            let state = read();
            if matches!(state.services[0].resource.phase, Phase::Running(_)) {
                return state.services[0].clone();
            }
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "Server exited; inspect {}",
                root.display()
            );
            assert!(
                started.elapsed() < Duration::from_secs(12),
                "Service did not run: {}",
                state.services[0].summary()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    struct Cleanup {
        service: Service,
        root: std::path::PathBuf,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(bytes) = fs::read(self.root.join("state/state.json"))
                && let Ok(state) = serde_json::from_slice::<State>(&bytes)
                && let Some(service) = state.services.into_iter().next()
            {
                self.service = service;
            }
            if let Ok(all) = Docker.inventory(self.service.resource.owner.node.as_str())
                && let Some(o) = all
                    .values()
                    .find(|o| o.name == format!("/{}", self.service.name()))
                && (self.service.resource.phase.container().is_some()
                    || (matches!(self.service.resource.phase, Phase::Creating)
                        && o.status == "created"))
            {
                self.service.resource.phase = Phase::Stopping {
                    container: self.service.resource.phase.container().cloned(),
                    reason: StopReason::Requested,
                };
                if let Ok(binding) = verify(&self.service, o) {
                    let _ = Docker.remove(&binding);
                }
            }
            if !std::thread::panicking() {
                let _ = fs::remove_dir_all(&self.root);
            }
        }
    }
    let mut cleanup = Cleanup {
        service: s.services[0].clone(),
        root: root.clone(),
    };
    let mut first = launch();
    let service = wait_running(&mut first);
    let id = service.resource.phase.container().unwrap().clone();
    cleanup.service = service;
    first.stop();
    let all = Docker.inventory(&node).unwrap();
    verify(&cleanup.service, all.get(id.as_str()).unwrap()).unwrap();
    assert_eq!(all[id.as_str()].status, "running");
    // Simulate a crash after Docker start but before its Running receipt.
    let mut receipt_lost = read();
    receipt_lost.services[0].resource.phase = Phase::Starting(id.clone());
    fs::write(
        state_dir.join("state.json"),
        serde_json::to_vec(&receipt_lost).unwrap(),
    )
    .unwrap();
    let mut second = launch();
    let restored = wait_running(&mut second);
    assert_eq!(restored.resource.phase.container(), Some(&id));
    let contents =
        crate::engine::query(&["exec", id.as_str(), "cat", "/workspace/counter"]).unwrap();
    assert_eq!(contents, "durable");
    second.stop();
    let wait = Instant::now();
    loop {
        let all = Docker.inventory(&node).unwrap();
        let observation = all.get(id.as_str()).unwrap();
        if observation.status == "exited" {
            break;
        }
        assert!(
            wait.elapsed() < Duration::from_secs(24),
            "PID-1 deadline must survive Control shutdown and child signals"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let log = crate::engine::query(&["logs", id.as_str()]).unwrap();
    assert!(
        log.contains("ready"),
        "Supervisor memory isolation failed or command did not run"
    );
}
