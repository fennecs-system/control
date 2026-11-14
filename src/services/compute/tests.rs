use super::*;
use crate::services::tests::{admit_fixture, fixture, inspection};

fn request(
    state: &mut State,
    run: &str,
    time: u64,
    nonce: &str,
    shape: Option<Shape>,
) -> io::Result<Value> {
    super::super::admit(
        state,
        Request {
            run,
            generation: "attempt-a",
            nonce,
        },
        Input {
            command: "sleep 30".to_owned().try_into().unwrap(),
            shape,
            lease_seconds: 60,
            storage_limit_mb: None,
        },
        "node-a".to_owned().try_into().unwrap(),
        time,
        |_| Ok(()),
    )
}

#[test]
fn admission_counts_pending_and_fenced_resources_retries_do_not_charge_again() {
    let (mut state, run, _, time) = fixture();
    let first = request(&mut state, &run, time, "one", Some(Shape::Large)).unwrap();
    request(&mut state, &run, time, "two", Some(Shape::Large)).unwrap();
    assert_eq!(
        request(&mut state, &run, time, "one", Some(Shape::Large)).unwrap(),
        first
    );
    assert!(request(&mut state, &run, time, "one", Some(Shape::Small)).is_err());
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        request(&mut state, &run, time, "three", None)
            .unwrap_err()
            .to_string()
            .contains("compute capacity")
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    state.services[0].resource.phase = Phase::Fenced {
        fault: Fault::UnexpectedEngineState,
        container: None,
    };
    assert!(request(&mut state, &run, time, "three", None).is_err());
    state.services[0].resource.phase = Phase::Stopped(StopReason::Requested);
    request(&mut state, &run, time, "three", Some(Shape::Medium)).unwrap();
    assert_eq!(
        status(&state.services)["capacity"]["reserved"]["memory_bytes"],
        12 * GIB
    );
    // Durable restart retains compute reservations and the captured shape.
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(status(&restored.services), status(&state.services));
    assert!(request(&mut restored, &run, time, "four", Some(Shape::Large)).is_err());
}

#[test]
fn defaults_keep_legacy_limits_and_named_shapes_are_independent_of_image_capabilities() {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let mut service = state.services.remove(0);
    for (profile, memory) in [
        (crate::worker_profile::Profile::Lite, GIB / 2),
        (crate::worker_profile::Profile::Full, 2 * GIB),
    ] {
        service.profile = profile;
        let legacy = serde_json::to_value(&service).unwrap();
        assert!(legacy.get("shape").is_none());
        let restored: Service = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            restored.limits(),
            Limits {
                memory_bytes: memory,
                cpus: 1
            }
        );
        service.shape = Some(Shape::Large);
        assert_eq!(service.limits(), Shape::Large.limits());
        assert_eq!(service.binding_snapshot().limits(), service.limits());
        service.resource.phase = Phase::Creating;
        let mut observed = inspection(&service, &"b".repeat(64));
        observed.memory = 8 * GIB;
        observed.cpu = 2_000_000_000;
        observed.pids = profile.service_pids();
        assert!(verify(&service, &observed).is_ok());
        observed.cpu = 1_000_000_000;
        assert!(verify(&service, &observed).is_err());
        service.shape = None;
    }
    assert!(serde_json::from_value::<Input>(json!({"command":"true","shape":"huge"})).is_err());
    assert!(
        serde_json::from_value::<Input>(json!({"command":"true","memory_bytes":u64::MAX})).is_err()
    );
}

#[test]
fn five_default_workers_and_an_eight_gib_server_fit_the_pool() {
    let (mut state, run, _, time) = fixture();
    for slot in 0..5 {
        request(&mut state, &run, time, &format!("worker-{slot}"), None).unwrap();
    }
    request(&mut state, &run, time, "server", Some(Shape::Large)).unwrap();
    assert_eq!(status(&state.services)["capacity"]["reserved"]["cpus"], 7);
    assert_eq!(
        status(&state.services)["capacity"]["reserved"]["memory_bytes"],
        10 * GIB + GIB / 2
    );
}

#[test]
fn concurrent_requests_cannot_oversubscribe_the_memory_pool() {
    let (state, run, _, time) = fixture();
    let shared = Arc::new(Mutex::new(state));
    let jobs = (0..4)
        .map(|slot| {
            let shared = Arc::clone(&shared);
            let run = run.clone();
            std::thread::spawn(move || {
                request(
                    &mut shared.lock().unwrap(),
                    &run,
                    time,
                    &format!("parallel-{slot}"),
                    Some(Shape::Large),
                )
                .is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        jobs.into_iter()
            .map(|j| usize::from(j.join().unwrap()))
            .sum::<usize>(),
        2
    );
    assert_eq!(
        status(&shared.lock().unwrap().services)["capacity"]["available"]["memory_bytes"],
        0
    );
}

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated create/inspect probes, no models"]
fn live_docker_enforces_all_shapes_without_changing_image_capabilities() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
    for shape in [Shape::Small, Shape::Medium, Shape::Large] {
        let (mut state, run, _, time) = fixture();
        request(&mut state, &run, time, "probe", Some(shape)).unwrap();
        let service = &mut state.services[0];
        service.resource.image = image.clone().try_into().unwrap();
        service.resource.owner.node = format!("qa-shapes-{}-{time}", std::process::id())
            .try_into()
            .unwrap();
        service.profile = profile;
        service.resource.phase = Phase::Creating;
        struct Cleanup {
            container: ContainerId,
            volume: String,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = crate::engine::query(&["rm", "-f", self.container.as_str()]);
                let _ = crate::engine::query(&["volume", "rm", &self.volume]);
            }
        }
        let container = Docker.create(service).unwrap();
        let _cleanup = Cleanup {
            container: container.clone(),
            volume: workspace::Workspace::of(service).unwrap().name().to_owned(),
        };
        let inventory = Docker
            .inventory(service.resource.owner.node.as_str())
            .unwrap();
        let observed = &inventory[container.as_str()];
        verify(service, observed).unwrap();
        assert_eq!(observed.memory, shape.limits().memory_bytes);
        assert_eq!(observed.cpu, shape.limits().cpus * 1_000_000_000);
        assert_eq!(observed.pids, profile.service_pids());
    }
}
