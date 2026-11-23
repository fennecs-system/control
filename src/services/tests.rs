use super::*;
use crate::runtime::RunStatus;
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
    run.status = RunStatus::Running;
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt-a".into()),
        image: Some(format!("sha256:{}", "a".repeat(64))),
        ..Default::default()
    });
    let run = run.id().to_string();
    let time = now();
    (s, run, chat, time)
}
fn input(command: &str) -> Input {
    Input {
        command: command.to_owned().try_into().unwrap(),
        lease_seconds: 60,
        storage_limit_mb: None,
        shape: None,
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
        pids: s.pids_limit(),
        mounts: workspace::Workspace::of(s)
            .map(|workspace| {
                vec![control_isolation::ObservedMount {
                    kind: control_isolation::MountKind::Volume,
                    name: Some(workspace.name().into()),
                    driver: Some("local".into()),
                    source: String::new(),
                    destination: "/workspace".into(),
                    writable: true,
                }]
            })
            .unwrap_or_default(),
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
/// The scripted engine's containers and counters.
#[derive(Default)]
struct Engine {
    records: HashMap<String, control_isolation::Observed>,
    volumes: HashMap<String, Vec<(String, String)>>,
    hide_inventory: bool,
    unavailable: bool,
    /// Refuse creation; `leave` also leaves a dormant container behind.
    refuse: bool,
    leave: bool,
    creates: usize,
    starts: usize,
    removes: usize,
    inspections: usize,
}
/// An [`Isolation`] keeping containers in memory. What it reports for a
/// created container is read from the engine arguments the spec renders to,
/// as the engine itself would report them, so `verify` checks real specs.
#[derive(Default)]
struct Fake(Mutex<Engine>);
impl Fake {
    fn new(engine: Engine) -> Self {
        Self(Mutex::new(engine))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, Engine> {
        self.0.lock().unwrap()
    }
}
fn bytes(size: Size) -> u64 {
    match size {
        Size::Bytes(bytes) => bytes,
        Size::Mib(mib) => mib << 20,
        Size::Gib(gib) => gib << 30,
    }
}
fn observed_from(spec: &RunSpec, id: &str) -> control_isolation::Observed {
    use control_isolation::{MountKind, ObservedMount};
    let words = control_isolation::Docker::create_arguments(spec).unwrap();
    let values = |flag: &str| {
        words
            .windows(2)
            .filter(|pair| pair[0] == flag)
            .map(|pair| pair[1].clone())
            .collect::<Vec<_>>()
    };
    let pairs = |flag: &str, separator: char| {
        values(flag)
            .iter()
            .filter_map(|value| value.split_once(separator).map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let storage = pairs("--storage-opt", '=');
    control_isolation::Observed {
        id: id.into(),
        name: format!("/{}", spec.name),
        image: spec.image.as_str().into(),
        image_ref: spec.image.as_str().into(),
        labels: spec.labels.iter().cloned().collect(),
        status: "created".into(),
        user: spec
            .user
            .map(|user| format!("{}:{}", user.uid, user.gid))
            .unwrap_or_default(),
        entrypoint: spec.entrypoint.clone().map(|entrypoint| vec![entrypoint]),
        command: Some(spec.argv.clone()),
        network: if spec.network == Network::None {
            "none".into()
        } else {
            "bridge".into()
        },
        read_only_root: spec.root == Access::ReadOnly,
        restart: "no".into(),
        cap_drop: Some(vec!["ALL".into()]),
        security_opt: Some(vec!["no-new-privileges".into()]),
        memory: spec.limits.memory.map_or(0, bytes),
        nano_cpus: spec.limits.cpus.map_or(0, |cpus| cpus * 1_000_000_000),
        pids_limit: spec.limits.pids,
        tmpfs: pairs("--tmpfs", ':'),
        storage_opt: (!storage.is_empty()).then_some(storage),
        mounts: spec
            .mounts
            .iter()
            .filter_map(|mount| match &mount.source {
                Source::Volume(name) => Some(ObservedMount {
                    kind: MountKind::Volume,
                    name: Some(name.clone()),
                    driver: Some("local".into()),
                    source: String::new(),
                    destination: mount.target.clone(),
                    writable: mount.access == Access::ReadWrite,
                }),
                Source::Bind(_) => None,
            })
            .collect(),
        ..Default::default()
    }
}
fn unscripted() -> io::Error {
    fail("not scripted for services")
}
impl Isolation for Fake {
    fn ready(&self) -> bool {
        true
    }
    fn disk_quota(&self) -> io::Result<bool> {
        Ok(true)
    }
    fn image_labels(&self, _: &control_isolation::ImageId) -> io::Result<HashMap<String, String>> {
        Err(unscripted())
    }
    fn spawn(&self, _: &RunSpec, _: Streams) -> io::Result<std::process::Child> {
        Err(unscripted())
    }
    fn run_detached(&self, _: &RunSpec) -> io::Result<control_isolation::ContainerId> {
        Err(unscripted())
    }
    fn create(&self, spec: &RunSpec) -> io::Result<control_isolation::ContainerId> {
        let mut engine = self.state();
        engine.creates += 1;
        let id = "b".repeat(64);
        if !engine.refuse || engine.leave {
            engine.records.insert(id.clone(), observed_from(spec, &id));
        }
        if engine.refuse {
            return Err(fail(
                "Could not create capped service: --storage-opt is supported only for overlay over xfs",
            ));
        }
        Ok(id.into())
    }
    fn start(&self, id: &control_isolation::ContainerId) -> io::Result<()> {
        let mut engine = self.state();
        engine.starts += 1;
        engine.records.get_mut(id.as_str()).unwrap().status = "running".into();
        Ok(())
    }
    fn kill(&self, _: &control_isolation::ContainerId) -> io::Result<()> {
        Err(unscripted())
    }
    fn remove(&self, target: Target<'_>) -> io::Result<()> {
        let Target::Id(id) = target else {
            return Err(unscripted());
        };
        let mut engine = self.state();
        engine.removes += 1;
        engine.records.remove(id.as_str());
        Ok(())
    }
    fn inspect(&self, targets: &[Target<'_>]) -> io::Result<Vec<control_isolation::Observed>> {
        let engine = self.state();
        targets
            .iter()
            .map(|target| match target {
                Target::Id(id) => engine
                    .records
                    .get(id.as_str())
                    .cloned()
                    .ok_or_else(|| fail("No such container")),
                Target::Name(_) => Err(unscripted()),
            })
            .collect()
    }
    fn list(&self, _: States, select: &Select) -> io::Result<Vec<control_isolation::Listed>> {
        let mut engine = self.state();
        let listed = |engine: &Engine, keep: &dyn Fn(&str) -> bool| {
            engine
                .records
                .values()
                .filter(|record| keep(record.id.as_str()))
                .map(|record| control_isolation::Listed {
                    id: record.id.clone(),
                    name: record.name.clone(),
                    labels: record.labels.clone(),
                })
                .collect()
        };
        match select {
            Select::Labels(_) => {
                engine.inspections += 1;
                if engine.unavailable {
                    return Err(fail("Engine unavailable"));
                }
                Ok(if engine.hide_inventory {
                    Vec::new()
                } else {
                    listed(&engine, &|_| true)
                })
            }
            Select::Id(id) => Ok(listed(&engine, &|record| record == id.as_str())),
            Select::Name(_) | Select::Any => Err(unscripted()),
        }
    }
    fn exec(&self, _: &control_isolation::ExecSpec, _: Streams) -> io::Result<std::process::Child> {
        Err(unscripted())
    }
    fn wait(&self, _: &control_isolation::ContainerId) -> io::Result<std::process::Child> {
        Err(unscripted())
    }
    fn logs(&self, _: &control_isolation::ContainerId, _: u32) -> io::Result<std::process::Child> {
        std::process::Command::new("printf")
            .arg("retained final diagnostic")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
    }
    fn stats(&self, _: &[&control_isolation::ContainerId]) -> io::Result<HashMap<String, control_isolation::Stats>> {
        Err(unscripted())
    }
    fn top(&self, _: &control_isolation::ContainerId) -> io::Result<Vec<control_isolation::Process>> {
        Err(unscripted())
    }
    fn volume_create(&self, name: &str, labels: &[(&str, &str)]) -> io::Result<()> {
        self.state()
            .volumes
            .entry(name.to_owned())
            .or_insert_with(|| labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect());
        Ok(())
    }
    fn volume_inspect(&self, name: &str) -> io::Result<Vec<control_isolation::ObservedVolume>> {
        let engine = self.state();
        let labels = engine.volumes.get(name).ok_or_else(|| fail("No such volume"))?;
        Ok(vec![control_isolation::ObservedVolume {
            name: name.into(),
            driver: "local".into(),
            scope: "local".into(),
            labels: labels.iter().cloned().collect(),
            options: Default::default(),
        }])
    }
    fn volumes(&self, _: &[(&str, &str)]) -> io::Result<Vec<control_isolation::ListedVolume>> {
        Err(unscripted())
    }
    fn volume_remove(&self, _: &str) -> io::Result<()> {
        Err(unscripted())
    }
}
#[test]
fn fenced_missing_services_release_capacity_only_after_persisted_stop_and_observation() {
    let (mut state, run, _, time) = fixture();
    for slot in 0..default_slots() {
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
    assert!(matches!(state.services[0].resource.phase, Phase::Fenced { .. }));
    page::stop(&mut state, &id, time, |_| Ok(())).unwrap();
    assert!(admit_fixture(&mut state, &run, time, |_| Ok(())).is_err());
    // Restart preserves cleanup intent, without reviving the original work.
    let state: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let engine = Fake::new(Engine {
        unavailable: true,
        ..Default::default()
    });
    assert!(drive(&shared, &engine, "node-a", time, |_| Ok(())).is_err());
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopping { .. }
    ));
    engine.state().unavailable = false;
    drive(&shared, &engine, "node-a", time, |_| Err(fail("disk full"))).unwrap_err();
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopping { .. }
    ));
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    let mut state = shared.lock().unwrap();
    assert_eq!(state.services[0].resource.phase, Phase::Stopped(StopReason::Requested));
    assert!(
        state.services[1..]
            .iter()
            .all(|s| matches!(s.resource.phase, Phase::Fenced { .. }))
    );
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    assert_eq!(
        {
            let e = engine.state();
            (e.creates, e.starts, e.removes)
        },
        (0, 0, 0)
    );
}

#[test]
fn stopping_a_paused_service_reverifies_identity_and_retains_output() {
    for mismatch in [false, true] {
        let (mut state, run, _, time) = fixture();
        admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
        let shared = Arc::new(Mutex::new(state));
        let engine = Fake::default();
        for _ in 0..3 {
            drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
        }
        engine.state().records.values_mut().next().unwrap().status = "paused".into();
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
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
            engine.state().records.values_mut().next().unwrap().network = "bridge".into();
        }
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
        assert!(shared.lock().unwrap().services[0].resource.phase.reserves_capacity());
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
        let state = shared.lock().unwrap();
        if mismatch {
            assert!(matches!(
                state.services[0].resource.phase,
                Phase::Fenced {
                    fault: Fault::IdentityMismatch,
                    ..
                }
            ));
            assert_eq!(engine.state().removes, 0);
        } else {
            assert_eq!(state.services[0].resource.phase, Phase::Stopped(StopReason::Requested));
            assert_eq!(state.services[0].output, "retained final diagnostic");
            assert_eq!(engine.state().removes, 1);
        }
        assert_eq!(
            {
                let e = engine.state();
                (e.creates, e.starts)
            },
            (1, 1)
        );
    }
}

#[test]
fn missing_from_label_inventory_does_not_prove_a_recorded_container_is_gone() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let engine = Fake::default();
    for _ in 0..3 {
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    }
    {
        let mut state = shared.lock().unwrap();
        let id = state.services[0].resource.id.as_str().to_owned();
        page::stop(&mut state, &id, time, |_| Ok(())).unwrap();
    }
    engine.state().hide_inventory = true;
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Fenced {
            fault: Fault::IdentityMismatch,
            ..
        }
    ));
    assert_eq!(engine.state().removes, 0);
    assert_eq!(engine.state().records.len(), 1);
}

#[test]
fn exited_service_keeps_output_before_removal_and_failed_save_prevents_removal() {
    let (state, run, _, time) = fixture();
    let shared = Arc::new(Mutex::new(state));
    admit_fixture(&mut shared.lock().unwrap(), &run, time, |_| Ok(())).unwrap();
    let engine = Fake::default();
    for offset in 0..4 {
        drive(&shared, &engine, "node-a", time + offset, |_| Ok(())).unwrap();
    }
    let id = shared.lock().unwrap().services[0]
        .resource
        .phase
        .container()
        .unwrap()
        .as_str()
        .to_owned();
    engine.state().records.get_mut(&id).unwrap().status = "exited".into();
    engine.state().records.get_mut(&id).unwrap().exit_code = 7;
    drive(&shared, &engine, "node-a", time + 4, |state| {
        if state.services[0].output.is_empty() {
            Ok(())
        } else {
            Err(fail("disk full"))
        }
    })
    .unwrap();
    assert_eq!(engine.state().removes, 0);
    assert!(shared.lock().unwrap().services[0].output.is_empty());
    drive(&shared, &engine, "node-a", time + 5, |_| Ok(())).unwrap();
    assert_eq!(engine.state().removes, 1);
    drive(&shared, &engine, "node-a", time + 6, |_| Ok(())).unwrap();
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    assert!(matches!(restored.services[0].resource.phase, Phase::Stopped(_)));
    assert_eq!(restored.services[0].summary()["output"], "retained final diagnostic");
    assert_eq!(restored.services[0].summary()["exit_code"], 7);
}
#[test]
fn admission_is_bounded_deduplicated_and_rolls_back_before_any_effect() {
    let (mut s, run, _, time) = fixture();
    assert!(admit_fixture(&mut s, &run, time, |_| Err(fail("disk full"))).is_err());
    assert!(s.services.is_empty());
    let first = admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    assert_eq!(
        admit_fixture(&mut s, &run, time + 1, |_| panic!("Duplicate must not save")).unwrap(),
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
    for n in 1..default_slots() {
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
    let engine = Fake::default();
    assert!(drive(&shared, &engine, "node-a", time, |_| Err(fail("disk full"))).is_err());
    assert_eq!(engine.state().creates, 0);
    let writes = Cell::new(0);
    drive(&shared, &engine, "node-a", time, |_| {
        writes.set(writes.get() + 1);
        if writes.get() == 2 {
            Err(fail("lost created-ID save"))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(
        {
            let e = engine.state();
            (e.creates, e.starts)
        },
        (1, 0)
    );
    assert_eq!(shared.lock().unwrap().services[0].resource.phase, Phase::Creating);
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(restored));
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    assert_eq!(
        {
            let e = engine.state();
            (e.creates, e.starts)
        },
        (1, 0)
    );
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    assert_eq!(
        {
            let e = engine.state();
            (e.creates, e.starts)
        },
        (1, 1)
    );
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
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
    assert!(!service.owned_by(&service.resource.owner.node, "another-chat", &Actor::LocalOperator));
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
    let engine = Fake::default();
    for _ in 0..3 {
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    }
    shared.lock().unwrap().services[0].actor = Actor::Unknown;
    assert!(drive(&shared, &engine, "node-a", time, |_| Err(fail("disk full"))).is_err());
    assert_eq!(engine.state().removes, 0);
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
    for _ in 0..3 {
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    }
    assert_eq!(
        {
            let e = engine.state();
            (e.starts, e.removes)
        },
        (1, 1)
    );
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
    let engine = Fake::default();
    for _ in 0..3 {
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    }
    {
        let mut s = shared.lock().unwrap();
        s.runtime.runs[0].status = RunStatus::Completed;
        s.runtime.runs[0].attempts.clear();
    }
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(restored));
    for _ in 0..3 {
        drive(&shared, &engine, "node-a", time + 1, |_| {
            panic!("Unchanged Running services must not save")
        })
        .unwrap();
    }
    assert_eq!(
        {
            let e = engine.state();
            (e.creates, e.starts, e.removes)
        },
        (1, 1, 0)
    );
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
    drive(&shared, &engine, "node-a", time + 60, |_| Ok(())).unwrap();
    assert_eq!(engine.state().removes, 0, "Expiry persists stop intent first");
    drive(&shared, &engine, "node-a", time + 60, |_| Ok(())).unwrap();
    assert_eq!(engine.state().removes, 1);
    drive(&shared, &engine, "node-a", time + 60, |_| Ok(())).unwrap();
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
    for field in ["network", "mount", "image", "node", "entrypoint", "deadline", "pid"] {
        let mut o = inspection(&service, &"b".repeat(64));
        match field {
            "network" => o.network = "bridge".into(),
            "mount" => o.mounts.push(control_isolation::ObservedMount {
                kind: control_isolation::MountKind::Bind,
                name: None,
                driver: None,
                source: "/host".into(),
                destination: "/workspace".into(),
                writable: true,
            }),
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
    let engine = Fake::default();
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    engine.state().records.values_mut().next().unwrap().network = "bridge".into();
    drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
    assert_eq!(
        {
            let e = engine.state();
            (e.starts, e.removes)
        },
        (0, 0)
    );
    let s = shared.lock().unwrap();
    assert!(matches!(s.services[0].resource.phase, Phase::Fenced { .. }));
    assert!(
        s.services[0].resource.phase.container().is_some(),
        "Fencing must retain the known container identity"
    );
}
#[test]
fn refused_creation_fails_once_keeps_its_diagnostic_and_releases_capacity() {
    for (leave, stuck) in [(false, false), (true, false), (false, true)] {
        let (mut s, run, _, time) = fixture();
        admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
        if stuck {
            // A record an earlier build left retrying in Creating.
            s.services[0].resource.phase = Phase::Creating;
            s.services[0].diagnostic = "Could not create capped service: old".into();
        }
        let shared = Arc::new(Mutex::new(s));
        let engine = Fake::new(Engine {
            refuse: true,
            leave,
            ..Engine::default()
        });
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
        assert_eq!(
            shared.lock().unwrap().services[0].resource.phase,
            Phase::Stopping {
                container: None,
                reason: StopReason::Failed
            }
        );
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
        drive(&shared, &engine, "node-a", time, |_| Ok(())).unwrap();
        assert_eq!(engine.state().creates, 1, "A refusal is never retried");
        assert_eq!(
            {
                let e = engine.state();
                (e.starts, e.removes)
            },
            (0, usize::from(leave))
        );
        assert!(
            engine.state().records.is_empty(),
            "A container the refusal left is removed"
        );
        let state = shared.lock().unwrap();
        let service = &state.services[0];
        assert_eq!(service.resource.phase, Phase::Stopped(StopReason::Failed));
        assert_eq!(service.phase_name(), "Failed");
        assert!(
            service.summary()["diagnostic"]
                .as_str()
                .unwrap()
                .contains("--storage-opt is supported only")
        );
        assert_eq!(compute::reserved(&state.services).memory_bytes, 0);
    }
}
#[test]
fn unsupported_storage_quota_is_refused_before_admission() {
    let capped: Input = serde_json::from_value(json!({"command":"sleep 30","storage_limit_mb":16000})).unwrap();
    let error = quota_supported(&capped, || Ok(false)).unwrap_err().to_string();
    assert!(error.contains("Nothing was started or reserved"), "{error}");
    assert!(quota_supported(&capped, || Ok(true)).is_ok());
    assert!(quota_supported(&input("sleep 30"), || panic!("Uncapped starts never probe")).is_ok());
}
#[test]
fn idle_supervision_does_not_query_the_engine() {
    let shared = Arc::new(Mutex::new(State::default()));
    let engine = Fake::default();
    drive(&shared, &engine, "node-a", now(), |_| panic!("Idle must not save")).unwrap();
    assert_eq!(engine.state().inspections, 0);
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
            "mount" => bad.mounts.push(control_isolation::ObservedMount {
                kind: control_isolation::MountKind::Bind,
                name: None,
                driver: None,
                source: String::new(),
                destination: "/workspace".into(),
                writable: true,
            }),
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
    assert!(serde_json::to_value(&legacy).unwrap().get("workspace_exec").is_none());
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
    assert!(verify(&service, &observed).is_err(), "Missing quota must fail closed");
    let mut capped = inspection(&service, &"b".repeat(64));
    assert_eq!(capped.storage_opts.as_ref().unwrap()["size"], "4096M");
    assert!(verify(&service, &capped).is_ok());
    capped
        .storage_opts
        .as_mut()
        .unwrap()
        .insert("size".into(), "8192M".into());
    assert!(verify(&service, &capped).is_err());
    let restored: Service = serde_json::from_value(serde_json::to_value(&service).unwrap()).unwrap();
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
        assert!(serde_json::from_value::<Input>(json!({"command":"sleep 30","storage_limit_mb":limit})).is_err());
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
    assert!(serde_json::to_value(&service).unwrap().get("profile").is_none());
    service.resource.phase = Phase::Creating;
    let mut observed = inspection(&service, &"b".repeat(64));
    service.profile = crate::worker_profile::Profile::Full;
    assert!(verify(&service, &observed).is_err());
    observed.memory = service.profile.service_memory();
    observed.pids = service.pids_limit();
    observed.tmpfs = tmpfs(&service);
    observed.readonly = false;
    assert!(verify(&service, &observed).is_ok());
    let restored: Service = serde_json::from_value(serde_json::to_value(&service).unwrap()).unwrap();
    assert_eq!(restored.profile, crate::worker_profile::Profile::Full);
    assert!(verify(&restored, &observed).is_ok());
    service.profile = crate::worker_profile::Profile::Lite;
    assert!(verify(&service, &observed).is_err());
}
#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated cleanup probe, no models or credentials"]
fn live_fenced_paused_service_can_be_stopped_without_restart() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let profile = crate::worker_profile::Profile::inspect(crate::engine::isolation(), &image).unwrap();
    let (mut state, run, _, time) = fixture();
    let node = crate::test_support::unique("fenced-qa");
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
    let id = create_container(crate::engine::isolation(), &state.services[0]).unwrap();
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
        drive(&shared, crate::engine::isolation(), &node, time, |_| Ok(())).unwrap();
    }
    assert!(matches!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Running(_)
    ));
    crate::engine::query(&["pause", id.as_str()]).unwrap();
    drive(&shared, crate::engine::isolation(), &node, time, |_| Ok(())).unwrap();
    assert!(exists(crate::engine::isolation(), &id).unwrap());
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
    drive(&shared, crate::engine::isolation(), &node, time, |_| Ok(())).unwrap();
    assert!(!exists(crate::engine::isolation(), &id).unwrap());
    assert!(shared.lock().unwrap().services[0].resource.phase.reserves_capacity());
    drive(&shared, crate::engine::isolation(), &node, time, |_| Ok(())).unwrap();
    assert_eq!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopped(StopReason::Requested)
    );
}

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated storage probe, no models or credentials"]
fn live_workspace_survives_restart_exit_cleanup_and_container_replacement() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let profile = crate::worker_profile::Profile::inspect(crate::engine::isolation(), &image).unwrap();
    let (mut state, run, _, time) = fixture();
    let node = crate::test_support::unique("storage-qa");
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
    let id = create_container(crate::engine::isolation(), &service).unwrap();
    struct Container(ContainerId);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = crate::engine::query(&["rm", "-f", self.0.as_str()]);
        }
    }
    let cleanup = Container(id.clone());
    let observations = inventory(crate::engine::isolation(), &node).unwrap();
    verify(&service, &observations[id.as_str()]).unwrap();
    service.resource.phase = Phase::Starting(id.clone());
    let binding = verify(&service, &observations[id.as_str()]).unwrap();
    start_container(crate::engine::isolation(), &binding).unwrap();
    let output = crate::engine::query(&["exec", id.as_str(), "bash", "-c", "set -eu; test ! -w /bin; test ! -w /bin/control-tcp-relay; dd if=/dev/zero of=/workspace/large.bin bs=1048576 count=540; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; printf persisted > /tmp/marker; printf persisted > /home/agent/marker; printf disk-probe-ok"]).unwrap();
    assert!(output.contains("disk-probe-ok"));
    crate::engine::query(&["stop", "--time=1", id.as_str()]).unwrap();
    crate::engine::query(&["start", id.as_str()]).unwrap();
    let output = crate::engine::query(&["exec", id.as_str(), "bash", "-c", "set -eu; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; test ! -e /tmp/marker; test \"$(cat /home/agent/marker)\" = persisted; printf restart-ok"]).unwrap();
    assert_eq!(output.trim(), "restart-ok");
    let observations = inventory(crate::engine::isolation(), &node).unwrap();
    service.resource.phase = Phase::Running(id.clone());
    verify(&service, &observations[id.as_str()]).unwrap();
    assert!(
        observations[id.as_str()]
            .mounts
            .iter()
            .all(
                |m| (m.kind == control_isolation::MountKind::Tmpfs && m.destination == "/tmp")
                    || (m.kind == control_isolation::MountKind::Volume && m.destination == "/workspace")
            )
    );
    // Reproduce daemon shutdown's observable consequence without restarting
    // the operator's Docker daemon or touching other services.
    crate::engine::query(&["stop", "--time=1", id.as_str()]).unwrap();
    let shared = Arc::new(Mutex::new(State::default()));
    shared.lock().unwrap().services.push(service.clone());
    drive(&shared, crate::engine::isolation(), &node, time, |_| Ok(())).unwrap();
    assert!(
        !exists(crate::engine::isolation(), &id).unwrap(),
        "Exited containers are still removed"
    );
    drive(&shared, crate::engine::isolation(), &node, time, |_| Ok(())).unwrap();
    assert_eq!(
        shared.lock().unwrap().services[0].resource.phase,
        Phase::Stopped(StopReason::Exited)
    );
    drop(cleanup);
    assert!(inventory(crate::engine::isolation(), &node).unwrap().is_empty());
    // A different service handle under the same owner reopens the volume.
    // Serialize first, like a Control restart, rather than relying on memory.
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&*shared.lock().unwrap()).unwrap()).unwrap();
    service = restored.services[0].binding_snapshot();
    service.resource.id = "replacement-probe".to_owned().try_into().unwrap();
    service.resource.phase = Phase::Creating;
    let replacement = create_container(crate::engine::isolation(), &service).unwrap();
    let replacement_cleanup = Container(replacement.clone());
    let observations = inventory(crate::engine::isolation(), &node).unwrap();
    service.resource.phase = Phase::Starting(replacement.clone());
    start_container(
        crate::engine::isolation(),
        &verify(&service, &observations[replacement.as_str()]).unwrap(),
    )
    .unwrap();
    let output = crate::engine::query(&["exec", replacement.as_str(), "bash", "-c", "set -eu; test \"$(stat -c %s /workspace/large.bin)\" = 566231040; test ! -e /home/agent/marker; test ! -e /tmp/marker; test ! -w /bin; printf replacement-ok"]).unwrap();
    assert_eq!(output.trim(), "replacement-ok");
    drop(replacement_cleanup);
    service.resource.phase = Phase::Creating;
    service.storage = Storage::Workspace {
        limit_mb: NonZeroU32::new(1024),
    };
    match create_container(crate::engine::isolation(), &service) {
        Ok(id) => {
            let cleanup = Container(id.clone());
            let observations = inventory(crate::engine::isolation(), &node).unwrap();
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
                inventory(crate::engine::isolation(), &node).unwrap().is_empty(),
                "Rejected caps must not create an uncapped container"
            );
            println!("Engine rejected explicit cap; no uncapped fallback: {error}");
        }
    }
    // A pre-existing name alone must not authorize mounting foreign data.
    let workspace = workspace::Workspace::of(&service).unwrap();
    crate::engine::query(&["volume", "rm", workspace.name()]).unwrap();
    crate::engine::query(&[
        "volume",
        "create",
        "--label",
        "dev.control.chat=foreign",
        workspace.name(),
    ])
    .unwrap();
    assert!(
        create_container(crate::engine::isolation(), &service)
            .unwrap_err()
            .to_string()
            .contains("workspace does not match")
    );
    assert!(inventory(crate::engine::isolation(), &node).unwrap().is_empty());
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
    let root = crate::test_support::scratch("control-services-live");
    fs::create_dir(&root).unwrap();
    let state_dir = root.join("state");
    let config = root.join("config");
    fs::create_dir(&state_dir).unwrap();
    fs::create_dir(&config).unwrap();
    let node = ring::digest::digest(&ring::digest::SHA256, root.to_string_lossy().as_bytes()).as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    fs::write(state_dir.join("node-id"), &node).unwrap();
    fs::set_permissions(state_dir.join("node-id"), fs::Permissions::from_mode(0o600)).unwrap();
    let (mut s, run, _, time) = fixture();
    let profile = crate::worker_profile::Profile::inspect(crate::engine::isolation(), &image).unwrap();
    let value=admit_profile(&mut s,Request{run:&run,generation:"attempt-a",nonce:"live"},Input{command:"printf durable > /workspace/counter; if (exec 3<> /proc/1/mem) 2>/dev/null; then exit 41; fi; kill -STOP 1 2>/dev/null || true; echo ready; sleep 60".to_owned().try_into().unwrap(),lease_seconds:20,storage_limit_mb:None,shape:None},node.clone().try_into().unwrap(),time,profile,|_|Ok(())).unwrap();
    assert!(value["service_id"].is_string());
    s.services[0].resource.image = image.try_into().unwrap();
    let _workspace = WorkspaceCleanup::of(&s.services[0]);
    s.runtime.runs[0].status = RunStatus::Completed;
    fs::write(state_dir.join("state.json"), serde_json::to_vec(&s).unwrap()).unwrap();
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
    // The server appends saves to state.log; read the snapshot with its log replayed.
    let read = || {
        crate::config::read_state_in::<State>(&state_dir, isize::MAX as usize)
            .unwrap()
            .unwrap()
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
            if let Ok(Some(state)) =
                crate::config::read_state_in::<State>(&self.root.join("state"), isize::MAX as usize)
                && let Some(service) = state.services.into_iter().next()
            {
                self.service = service;
            }
            if let Ok(all) = inventory(crate::engine::isolation(), self.service.resource.owner.node.as_str())
                && let Some(o) = all.values().find(|o| o.name == format!("/{}", self.service.name()))
                && (self.service.resource.phase.container().is_some()
                    || (matches!(self.service.resource.phase, Phase::Creating) && o.status == "created"))
            {
                self.service.resource.phase = Phase::Stopping {
                    container: self.service.resource.phase.container().cloned(),
                    reason: StopReason::Requested,
                };
                if let Ok(binding) = verify(&self.service, o) {
                    let _ = remove_container(crate::engine::isolation(), &binding);
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
    let all = inventory(crate::engine::isolation(), &node).unwrap();
    verify(&cleanup.service, all.get(id.as_str()).unwrap()).unwrap();
    assert_eq!(all[id.as_str()].status, "running");
    // Simulate a crash after Docker start but before its Running receipt.
    let mut receipt_lost = read();
    receipt_lost.services[0].resource.phase = Phase::Starting(id.clone());
    fs::write(state_dir.join("state.json"), serde_json::to_vec(&receipt_lost).unwrap()).unwrap();
    let mut second = launch();
    let restored = wait_running(&mut second);
    assert_eq!(restored.resource.phase.container(), Some(&id));
    let contents = crate::engine::query(&["exec", id.as_str(), "cat", "/workspace/counter"]).unwrap();
    assert_eq!(contents, "durable");
    second.stop();
    let wait = Instant::now();
    loop {
        let all = inventory(crate::engine::isolation(), &node).unwrap();
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
/// Default Lite services the default node compute budget admits (CPU bound:
/// 8 CPUs x2 oversubscription at one CPU each). No fixed service count applies.
fn default_slots() -> usize {
    let capacity = compute::Capacity::DEFAULT;
    let each = compute::limits(None, crate::worker_profile::Profile::Lite);
    usize::try_from((capacity.admitted_cpus() / each.cpus).min(capacity.memory_bytes / each.memory_bytes)).unwrap()
}
#[test]
fn live_services_are_bounded_by_the_compute_budget_not_a_fixed_count() {
    assert_eq!(default_slots(), 16, "more than the former fixed eight");
}

#[test]
fn process_tree_services_link_to_their_own_card() {
    let (mut s, run, _chat, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let id = s.services[0].resource.id.as_str().to_owned();
    let tree = crate::process_tree::render(&s, "").unwrap();
    assert!(tree.contains(&format!("<a href='/tools/services#{id}'>")), "{tree}");
}

/// A chat spawned under `parent`, with a running run like the fixture's.
fn member(s: &mut State, parent: &str, run: &str) -> (String, crate::runtime::Run) {
    let chat = crate::chat::create(s);
    s.chats.last_mut().unwrap().parent_chat = Some(parent.to_owned());
    let mut member = s.runtime.runs.get(run).unwrap().clone();
    member = member.with_id(format!("run-{chat}"));
    member.chat_id = Some(chat.clone());
    member.resource_grants.clear();
    (chat, member)
}

#[test]
fn teammates_use_and_stop_their_teams_services_without_a_grant() {
    let (mut s, run, lead, time) = fixture();
    admit_fixture(&mut s, &run, time, |_| Ok(())).unwrap();
    let node: NodeId = "node-a".to_owned().try_into().unwrap();
    let id = s.services[0].resource.id.as_str().to_owned();
    let (worker, worker_run) = member(&mut s, &lead, &run);
    let (_, grandchild_run) = member(&mut s, &worker, &run);
    for teammate_run in [&worker_run, &grandchild_run] {
        assert!(
            !direct(&s.services[0], &node, teammate_run),
            "it holds no grant to share onward"
        );
        assert!(
            accessible(Lineage::of(&s.chats), &s.services[0], &node, teammate_run),
            "a team's service belongs to the team"
        );
        assert_eq!(
            target(ServiceAuthority::of(&s), &node, teammate_run, &id).unwrap(),
            0,
            "exec, attach, logs and stop need no grant"
        );
    }
    // The lead uses a service a worker started after it was spawned.
    let lead_run = s.runtime.runs[0].clone();
    s.services[0].resource.owner.chat = worker.clone().try_into().unwrap();
    assert!(target(ServiceAuthority::of(&s), &node, &lead_run, &id).is_ok());

    // Outside the tree, another actor, another node or a judge: refused.
    let stranger = crate::chat::create(&mut s);
    let mut outsider = worker_run.clone();
    outsider.chat_id = Some(stranger);
    let mut other_actor = worker_run.clone();
    other_actor.actor = crate::identity::Actor::DiscordDm {
        user_id: "7".parse().unwrap(),
        channel_id: "8".parse().unwrap(),
    };
    let mut judge = worker_run.clone();
    judge.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: lead.clone(),
        depth: 1,
        parent: Some(run.clone()),
        bundle: None,
    });
    for refused in [&outsider, &other_actor, &judge] {
        assert!(target(ServiceAuthority::of(&s), &node, refused, &id).is_err());
    }
    let elsewhere: NodeId = "node-b".to_owned().try_into().unwrap();
    assert!(target(ServiceAuthority::of(&s), &elsewhere, &worker_run, &id).is_err());
}

#[test]
fn service_create_vector_is_pinned() {
    use crate::engine::golden::{fill, pin_equivalent};
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let mut service = state.services[0].clone();
    let r = &service.resource;
    let (name, uid, gid, chat, id, generation, expires, seconds, command) = (
        service.name(),
        service.uid.to_string(),
        service.gid.to_string(),
        r.owner.chat.as_str().to_owned(),
        r.id.as_str().to_owned(),
        r.generation.get().to_string(),
        r.lease.expires().to_string(),
        r.lease.seconds().to_string(),
        service.command.0.to_string(),
    );
    let (node, image) = (r.owner.node.as_str().to_owned(), r.image.as_str().to_owned());
    let values = [
        ("name", name.as_str()),
        ("uid", &uid),
        ("gid", &gid),
        ("chat", &chat),
        ("id", &id),
        ("generation", &generation),
        ("expires", &expires),
        ("seconds", &seconds),
        ("command", &command),
        ("node", &node),
        ("image", &image),
    ];
    // The tmpfs set comes from a map, so its order was never fixed.
    pin_equivalent(
        &control_isolation::Docker::create_arguments(&container(&service, Some("control-ws-golden"))).unwrap(),
        &fill(GOLDEN_VOLUME, &values),
    );
    service.storage = Storage::Memory;
    pin_equivalent(
        &control_isolation::Docker::create_arguments(&container(&service, None)).unwrap(),
        &fill(GOLDEN_MEMORY, &values),
    );
}
const GOLDEN_VOLUME: &[&str] = &[
    "create",
    "--pull=never",
    "--restart=no",
    "--network=none",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=64",
    "--memory=536870912",
    "--cpus=1",
    "--log-driver=local",
    "--log-opt=max-size=1m",
    "--log-opt=max-file=2",
    "--name",
    "{name}",
    "--user",
    "{uid}:{gid}",
    "--env",
    "HOME=/home/agent",
    "--workdir=/workspace",
    "--entrypoint=control-service-runner",
    "--mount",
    "type=volume,source=control-ws-golden,target=/workspace",
    "--label",
    "dev.control.node={node}",
    "--label",
    "dev.control.purpose=service",
    "--label",
    "dev.control.resource={id}",
    "--label",
    "dev.control.chat={chat}",
    "--label",
    "dev.control.generation={generation}",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,uid={uid},gid={gid},size=128m",
    "{image}",
    "{expires}",
    "{seconds}",
    "{command}",
];
const GOLDEN_MEMORY: &[&str] = &[
    "create",
    "--pull=never",
    "--restart=no",
    "--network=none",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--pids-limit=64",
    "--memory=536870912",
    "--cpus=1",
    "--log-driver=local",
    "--log-opt=max-size=1m",
    "--log-opt=max-file=2",
    "--name",
    "{name}",
    "--user",
    "{uid}:{gid}",
    "--env",
    "HOME=/home/agent",
    "--workdir=/workspace",
    "--entrypoint=control-service-runner",
    "--read-only",
    "--label",
    "dev.control.node={node}",
    "--label",
    "dev.control.purpose=service",
    "--label",
    "dev.control.resource={id}",
    "--label",
    "dev.control.chat={chat}",
    "--label",
    "dev.control.generation={generation}",
    "--tmpfs",
    "/workspace:rw,exec,nosuid,nodev,uid={uid},gid={gid},size=64m",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,uid={uid},gid={gid},size=128m",
    "--tmpfs",
    "/home/agent:rw,nosuid,nodev,uid={uid},gid={gid},size=32m",
    "{image}",
    "{expires}",
    "{seconds}",
    "{command}",
];
