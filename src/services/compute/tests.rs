use super::*;
use crate::services::tests::{admit_fixture, fixture, inspection};

fn request(state: &mut State, run: &str, time: u64, nonce: &str, shape: Option<Shape>) -> io::Result<Value> {
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
    request(&mut state, &run, time, "filler", Some(Shape::Medium)).unwrap();
    assert_eq!(
        request(&mut state, &run, time, "one", Some(Shape::Large)).unwrap(),
        first
    );
    assert!(request(&mut state, &run, time, "one", Some(Shape::Small)).is_err());
    let before = serde_json::to_vec(&state).unwrap();
    let refused = request(&mut state, &run, time, "three", None).unwrap_err().to_string();
    assert!(refused.contains("compute capacity"));
    assert!(
        refused.contains("3 reserving service(s) are not running (Requested, Requested, Requested)"),
        "{refused}"
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
        16 * GIB
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
    // A record from before shape-derived process limits has no captured value.
    service.pids_limit = None;
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
    assert!(serde_json::from_value::<Input>(json!({"command":"true","memory_bytes":u64::MAX})).is_err());
}

#[test]
fn five_default_workers_and_an_eight_gib_server_fit_the_pool() {
    for profile in [
        crate::worker_profile::Profile::Lite,
        crate::worker_profile::Profile::Full,
    ] {
        let (mut state, run, _, time) = fixture();
        for slot in 0..6 {
            admit_profile(
                &mut state,
                Request {
                    run: &run,
                    generation: "attempt-a",
                    nonce: &format!("service-{slot}"),
                },
                Input {
                    command: "sleep 30".to_owned().try_into().unwrap(),
                    shape: (slot == 5).then_some(Shape::Large),
                    lease_seconds: 60,
                    storage_limit_mb: None,
                },
                "node-a".to_owned().try_into().unwrap(),
                time,
                profile,
                |_| Ok(()),
            )
            .unwrap();
        }
        assert_eq!(status(&state.services)["capacity"]["reserved"]["cpus"], 7);
        assert_eq!(
            status(&state.services)["capacity"]["reserved"]["memory_bytes"],
            5 * profile.service_memory() + 8 * GIB
        );
    }
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
        jobs.into_iter().map(|j| usize::from(j.join().unwrap())).sum::<usize>(),
        2
    );
    assert_eq!(
        status(&shared.lock().unwrap().services)["capacity"]["available"]["memory_bytes"],
        4 * GIB
    );
}

#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE and local Linux Docker; isolated create/inspect probes, no models"]
fn live_docker_enforces_all_shapes_without_changing_image_capabilities() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let profile = crate::worker_profile::Profile::inspect(crate::engine::isolation(), &image).unwrap();
    for shape in [Shape::Small, Shape::Medium, Shape::Large] {
        let (mut state, run, _, time) = fixture();
        request(&mut state, &run, time, "probe", Some(shape)).unwrap();
        let service = &mut state.services[0];
        service.resource.image = image.clone().try_into().unwrap();
        service.resource.owner.node = format!("qa-shapes-{}-{time}", std::process::id()).try_into().unwrap();
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
        let container = create_container(crate::engine::isolation(), service).unwrap();
        let _cleanup = Cleanup {
            container: container.clone(),
            volume: workspace::Workspace::of(service).unwrap().name().to_owned(),
        };
        let inventory = inventory(crate::engine::isolation(), service.resource.owner.node.as_str()).unwrap();
        let observed = &inventory[container.as_str()];
        verify(service, observed).unwrap();
        assert_eq!(observed.memory, shape.limits().memory_bytes);
        assert_eq!(observed.cpu, shape.limits().cpus * 1_000_000_000);
        assert_eq!(observed.pids, service.pids_limit());
        assert_eq!(
            observed.pids,
            pids_limit(shape.limits().memory_bytes, profile, Capacity::DEFAULT)
        );
    }
}

#[test]
fn cpu_limits_oversubscribe_while_memory_stays_a_strict_sum() {
    let capacity = Capacity::DEFAULT;
    let cpus = |cpus| Limits {
        memory_bytes: GIB,
        cpus,
    };
    assert!(
        admit_within(&[], cpus(12), capacity).is_ok(),
        "CPU caps beyond the 8 physical CPUs are admitted"
    );
    assert!(admit_within(&[], cpus(16), capacity).is_ok());
    assert!(
        admit_within(&[], cpus(17), capacity)
            .unwrap_err()
            .to_string()
            .contains("x2 oversubscription")
    );
    assert!(
        admit_within(
            &[],
            Limits {
                memory_bytes: 21 * GIB,
                cpus: 1
            },
            capacity
        )
        .is_err(),
        "memory is never oversubscribed"
    );
    let strict = Capacity {
        cpu_oversubscription: 1,
        ..capacity
    };
    assert!(admit_within(&[], cpus(9), strict).is_err());
}

#[test]
fn node_settings_size_the_pool_and_invalid_values_keep_defaults() {
    let settings = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    assert_eq!(Capacity::from_settings(|_| None), Capacity::DEFAULT);
    let sized = Capacity::from_settings(settings(&[
        ("service-memory-gib", "40\n"),
        ("service-cpus", "14"),
        ("service-cpu-oversubscription", "3"),
    ]));
    assert_eq!(
        sized,
        Capacity {
            memory_bytes: 40 * GIB,
            cpus: 14,
            cpu_oversubscription: 3,
            ..Capacity::DEFAULT
        }
    );
    assert_eq!(sized.admitted_cpus(), 42);
    let invalid = Capacity::from_settings(settings(&[
        ("service-memory-gib", "0"),
        ("service-cpus", "many"),
        ("service-cpu-oversubscription", "100"),
    ]));
    assert_eq!(invalid, Capacity::DEFAULT);
}

#[test]
fn process_limits_scale_with_memory_between_profile_floor_and_ceiling() {
    use crate::worker_profile::Profile::{Full, Lite};
    let capacity = Capacity::DEFAULT;
    // Floor: the profile default never shrinks.
    assert_eq!(pids_limit(Lite.service_memory(), Lite, capacity), 64);
    assert_eq!(pids_limit(Full.service_memory(), Full, capacity), 128);
    assert_eq!(pids_limit(GIB, Full, capacity), 128);
    // Scaling: 64 per GiB, so small keeps 128 and large gets 512.
    assert_eq!(pids_limit(Shape::Small.limits().memory_bytes, Lite, capacity), 128);
    assert_eq!(pids_limit(Shape::Medium.limits().memory_bytes, Lite, capacity), 256);
    assert_eq!(pids_limit(Shape::Large.limits().memory_bytes, Full, capacity), 512);
    // Ceiling: large memory and extreme inputs stop at pids_max.
    assert_eq!(pids_limit(64 * GIB, Full, capacity), 1024);
    assert_eq!(
        pids_limit(
            u64::MAX,
            Full,
            Capacity {
                pids_per_gib: 4096,
                ..capacity
            }
        ),
        1024
    );
    // The floor wins over a low scale.
    assert_eq!(
        pids_limit(
            8 * GIB,
            Full,
            Capacity {
                pids_per_gib: 1,
                ..capacity
            }
        ),
        128
    );
}

#[test]
fn node_settings_override_process_scale_and_ceiling() {
    let settings = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    let sized = Capacity::from_settings(settings(&[
        ("service-pids-per-gib", "256"),
        ("service-pids-max", "1536\n"),
    ]));
    assert_eq!((sized.pids_per_gib, sized.pids_max), (256, 1536));
    assert_eq!(pids_limit(4 * GIB, crate::worker_profile::Profile::Lite, sized), 1024);
    assert_eq!(pids_limit(8 * GIB, crate::worker_profile::Profile::Lite, sized), 1536);
    let invalid = Capacity::from_settings(settings(&[("service-pids-per-gib", "0"), ("service-pids-max", "64")]));
    assert_eq!(invalid, Capacity::DEFAULT, "a ceiling below the Full floor is rejected");
}

#[test]
fn admission_captures_process_limit_and_create_and_verify_share_it() {
    let (mut state, run, _, time) = fixture();
    request(&mut state, &run, time, "large", Some(Shape::Large)).unwrap();
    let service = &mut state.services[0];
    assert_eq!(service.pids_limit, Some(512));
    assert_eq!(service.summary()["pids_limit"], 512);
    assert_eq!(service.binding_snapshot().pids_limit(), 512);
    service.resource.phase = Phase::Creating;
    let mut observed = inspection(service, &"b".repeat(64));
    observed.memory = 8 * GIB;
    observed.cpu = 2_000_000_000;
    assert_eq!(observed.pids, 512);
    assert!(verify(service, &observed).is_ok());
    observed.pids = 128;
    assert!(
        verify(service, &observed).is_err(),
        "a container with a different limit is not this service"
    );
    // The captured value survives restart and is independent of later settings.
    let restored: Service = serde_json::from_value(serde_json::to_value(&*service).unwrap()).unwrap();
    assert_eq!(restored.pids_limit(), 512);
}

#[test]
fn drift_check_accepts_live_services_created_under_the_profile_rule() {
    let (mut state, run, _, time) = fixture();
    request(&mut state, &run, time, "legacy", Some(Shape::Large)).unwrap();
    let service = &mut state.services[0];
    service.profile = crate::worker_profile::Profile::Full;
    // Serialized before this change: no `pids_limit` field.
    let mut legacy = serde_json::to_value(&*service).unwrap();
    legacy.as_object_mut().unwrap().remove("pids_limit");
    let mut legacy: Service = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.pids_limit, None);
    legacy.resource.phase = Phase::Running(ContainerId::try_from("c".repeat(64)).unwrap());
    let mut observed = inspection(&legacy, &"c".repeat(64));
    observed.status = "running".into();
    observed.memory = 8 * GIB;
    observed.cpu = 2_000_000_000;
    observed.readonly = legacy.readonly_root();
    observed.tmpfs = tmpfs(&legacy);
    // The old rule gave every Full service 128, whatever its shape.
    assert_eq!(observed.pids, 128);
    assert!(
        verify(&legacy, &observed).is_ok(),
        "a live legacy service is not fenced by the new formula"
    );
    assert!(
        serde_json::to_value(&legacy).unwrap().get("pids_limit").is_none(),
        "legacy records are not rewritten"
    );
    observed.pids = 512;
    assert!(verify(&legacy, &observed).is_err());
}
