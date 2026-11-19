
use super::*;

#[test]
fn cards_escape_commands_and_show_live_data_only_when_observed() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    let input = Input {
        command: "<script>alert(1)</script>\nsecond line"
            .to_owned()
            .try_into()
            .unwrap(),
        lease_seconds: -1,
        storage_limit_mb: None,
        shape: Some(compute::Shape::Large),
    };
    admit(
        &mut state,
        Request {
            run: &run,
            generation: "attempt-a",
            nonce: "n1",
        },
        input,
        "node-a".to_owned().try_into().unwrap(),
        time,
        |_| Ok(()),
    )
    .unwrap();
    let service = state.services[0].binding_snapshot();
    let plain = card(&service);
    assert!(plain.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !plain.contains("<script>"));
    assert!(!plain.contains("second line"));
    assert!(
        plain.contains("Requested") && plain.contains("until stopped") && plain.contains("/dm/")
    );
    assert!(plain.contains("action='/tools/services'") && plain.contains("Stop service"));
    assert!(plain.contains("/workspace retained for this chat"));
    assert!(
        !plain.contains("data-live="),
        "only running services wait for live usage"
    );
    assert!(plain.contains("<dt>Size</dt><dd><strong>large</strong> <span class='muted'>· 8 GiB RAM · 2 CPUs · 512 processes</span></dd>") && !plain.contains("<table>"));
    assert!(plain.contains("<dt>Image</dt><dd><strong>Lite</strong>"), "the image is its own field, apart from the size");
    let live = Live {
        stats: Some(crate::engine::Stats {
            cpu: "3.5%".into(),
            memory: "512MiB / 2GiB".into(),
            pids: "14".into(),
        }),
        processes: vec![crate::engine::Process {
            pid: "77".into(),
            user: "502".into(),
            command: "java -jar server.jar <nogui>".into(),
        }],
    };
    let observed = usage(&live);
    assert!(observed.contains("CPU 3.5% · Memory 512MiB / 2GiB · 14 processes"));
    assert!(
        observed.contains("<td>77</td>") && observed.contains("java -jar server.jar &lt;nogui&gt;")
    );
}

#[test]
fn page_snapshots_keep_escaped_diagnostics() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    let input = Input {
        command: "sleep infinity".to_owned().try_into().unwrap(),
        lease_seconds: -1,
        storage_limit_mb: None,
        shape: None,
    };
    admit(
        &mut state,
        Request {
            run: &run,
            generation: "attempt-a",
            nonce: "n1",
        },
        input,
        "node-a".to_owned().try_into().unwrap(),
        time,
        |_| Ok(()),
    )
    .unwrap();
    state.services[0].diagnostic = "Engine said <no>".into();
    assert!(state.services[0].binding_snapshot().diagnostic.is_empty());
    let shown = card(&snapshot(&state.services[0]));
    assert!(shown.contains("Engine said &lt;no&gt;"));
}

#[test]
fn gather_keeps_results_that_arrive_before_the_deadline() {
    let jobs: Vec<(String, Job<u32>)> = vec![
        ("fast".into(), Box::new(|| 1)),
        (
            "slow".into(),
            Box::new(|| {
                std::thread::sleep(Duration::from_secs(2));
                2
            }),
        ),
        ("also-fast".into(), Box::new(|| 3)),
    ];
    let started = Instant::now();
    let results = gather(jobs, Instant::now() + Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        (
            results.get("fast"),
            results.get("also-fast"),
            results.get("slow")
        ),
        (Some(&1), Some(&3), None)
    );
    assert!(gather::<u32>(Vec::new(), Instant::now() + Duration::from_secs(5)).is_empty());
}

#[test]
fn operators_forget_stopped_services_and_keep_workspaces_of_live_chats() {
    let (mut state, run, chat, time) = super::super::tests::fixture();
    for nonce in ["live", "old-a", "old-b"] {
        admit(&mut state, Request { run: &run, generation: "attempt-a", nonce },
            Input { command: "sleep 30".to_owned().try_into().unwrap(), lease_seconds: -1, storage_limit_mb: None, shape: None },
            "node-a".to_owned().try_into().unwrap(), time, |_| Ok(())).unwrap();
    }
    for index in [1, 2] {
        state.services[index].resource.phase = Phase::Stopped(StopReason::Requested);
    }
    let (live, old) = (state.services[0].resource.id.as_str().to_owned(), state.services[1].resource.id.as_str().to_owned());
    assert!(forget(&mut state, Some(&live), |_| Ok(())).is_err(), "live services are stopped first, never deleted");
    assert!(forget(&mut state, Some(&old), |_| Err(io::Error::other("disk full"))).is_err());
    assert_eq!(state.services.len(), 3, "a failed save restores the records");

    let (keep, target) = forget(&mut state, Some(&old), |_| Ok(())).unwrap();
    assert_eq!(state.services.len(), 2);
    assert_eq!(target.as_deref(), Some(chat.as_str()));
    assert!(keep.contains(&chat), "the chat still has services, so its workspace stays");
    assert!(card(&state.services[1].binding_snapshot()).contains("value='delete'"));

    let (keep, target) = forget(&mut state, None, |_| Ok(())).unwrap();
    assert_eq!(state.services.len(), 1, "cleanup removes every stopped record and only those");
    assert!(target.is_none() && keep.contains(&chat));
}

#[test]
fn card_actions_return_to_their_card_or_its_neighbour() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    for nonce in ["a", "b", "c"] {
        admit(&mut state, Request { run: &run, generation: "attempt-a", nonce },
            Input { command: "sleep 30".to_owned().try_into().unwrap(), lease_seconds: -1, storage_limit_mb: None, shape: None },
            "node-a".to_owned().try_into().unwrap(), time, |_| Ok(())).unwrap();
    }
    let ids = state.services.iter().map(|s| s.resource.id.as_str().to_owned()).collect::<Vec<_>>();
    for id in &ids {
        assert!(id.starts_with("svc-"));
        assert!(card(&state.services.iter().find(|s| s.resource.id.as_str() == id).unwrap().binding_snapshot()).contains(&format!("<article class='record service-record' id='{id}'>")),
            "each card carries its service ID as a stable anchor");
    }
    // Page order is newest first: c, b, a.
    let place = nearby(&state, &ids[1]);
    assert_eq!(place, [ids[1].clone(), ids[0].clone(), ids[2].clone()]);
    stop(&mut state, &ids[1], time, |_| Ok(())).unwrap();
    assert_eq!(fragment(&state, &place), format!("#{}", ids[1]), "a stopped service's card stays in view");

    state.services[1].resource.phase = Phase::Stopped(StopReason::Requested);
    let place = nearby(&state, &ids[1]);
    forget(&mut state, Some(&ids[1]), |_| Ok(())).unwrap();
    assert_eq!(fragment(&state, &place), format!("#{}", ids[0]), "a deleted card returns to the card that takes its place");

    assert!(nearby(&state, "svc-unknown'\r\nSet-Cookie: x").is_empty(), "anchors come from state, never the request");
    assert_eq!(fragment(&state, &[]), "", "page-wide actions return to the top");
}

/// One sized Lite service, one unsized Full service and a stopped one. Writes an
/// optional local preview from this synthetic state and repository CSS only.
#[test]
fn preview_mixed_images_and_sizes() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    let start = |state: &mut State, nonce, shape, profile| {
        let input = Input { command: "sleep infinity".to_owned().try_into().unwrap(), lease_seconds: -1, storage_limit_mb: None, shape };
        admit_profile(state, Request { run: &run, generation: "attempt-a", nonce }, input, "node-a".to_owned().try_into().unwrap(), time, profile, |_| Ok(())).unwrap();
    };
    start(&mut state, "lite-small", Some(compute::Shape::Small), crate::worker_profile::Profile::Lite);
    start(&mut state, "full-default", None, crate::worker_profile::Profile::Full);
    start(&mut state, "old", Some(compute::Shape::Large), crate::worker_profile::Profile::Lite);
    state.services[2].resource.phase = Phase::Stopped(StopReason::Requested);
    let (sized, default_sized) = (card(&state.services[0].binding_snapshot()), card(&state.services[1].binding_snapshot()));
    assert!(sized.contains("<dt>Image</dt><dd><strong>Lite</strong> <span class='muted'>· Rust, Git"));
    assert!(sized.contains("<dt>Size</dt><dd><strong>small</strong> <span class='muted'>· 2 GiB RAM · 1 CPU · 128 processes"));
    assert!(default_sized.contains("<dt>Image</dt><dd><strong>Full</strong> <span class='muted'>· Lite plus Nix"));
    assert!(default_sized.contains("<dt>Size</dt><dd><strong>image default</strong> <span class='muted'>· 2 GiB RAM · 1 CPU · 128 processes"));
    assert!(!sized.contains(" · </p>"), "no dangling separator after the links");

    let panel = compute_panel(&state.services);
    let (images, sizes) = (panel.find("<h4>Image · which tools</h4>").unwrap(), panel.find("<h4>Size · how much it may use</h4>").unwrap());
    assert!(images < sizes, "images and sizes are separate tables");
    let (image_table, size_table) = (&panel[images..sizes], &panel[sizes..]);
    assert!(image_table.contains("<td><strong>Full</strong></td><td>Lite plus Nix") && image_table.contains("512 MiB RAM · 1 CPU · 64 processes"));
    assert!(!image_table.contains("<code>small</code>") && size_table.contains("<td><code>small</code></td><td>2 GiB</td>"));
    assert!(!size_table.split("<h4>Total").next().unwrap().contains("Lite</strong>"), "no image rows among the sizes");
    let page = render(&Arc::new(Mutex::new(state))).unwrap();
    if let Some(path) = std::env::var_os("CONTROL_QA_SERVICES_PREVIEW") {
        let mut page = page.clone();
        for (name, css) in [("style", include_str!("../../../assets/style.css")), ("navigation", include_str!("../../../assets/navigation.css")), ("workspace", include_str!("../../../assets/workspace.css")), ("runtime", include_str!("../../../assets/runtime.css")), ("collective", include_str!("../../../assets/collective.css"))] {
            page = page.replace(&format!("<link rel=\"stylesheet\" href=\"/{name}.css\">"), &format!("<style>{css}</style>"));
            page = page.replace(&format!("<link rel='stylesheet' href='/{name}.css'>"), &format!("<style>{css}</style>"));
        }
        std::fs::write(path, page).unwrap();
    }
}

/// The client re-reads the `data-live-refresh` region on each state-change
/// hint, so every state-derived control must be inside it: when the last
/// service finishes stopping, its Delete and the page-wide cleanup appear
/// without a reload.
#[test]
fn state_derived_controls_sit_inside_the_refreshed_region() {
    let (mut state, run, _chat, time) = super::super::tests::fixture();
    admit(&mut state, Request { run: &run, generation: "attempt-a", nonce: "only" },
        Input { command: "sleep 30".to_owned().try_into().unwrap(), lease_seconds: -1, storage_limit_mb: None, shape: None },
        "node-a".to_owned().try_into().unwrap(), time, |_| Ok(())).unwrap();
    let shared = Arc::new(Mutex::new(state));
    let region = || {
        let page = render(&shared).unwrap();
        let (outside, inside) = page.split_once("<div data-live-src='/tools/services/live' data-live-refresh>").expect("one refreshable region");
        assert!(!outside.contains("decision-form") && !outside.contains("<table"), "state-derived content is inside the region");
        inside.to_owned()
    };
    let running = region();
    assert!(running.contains("Stop service") && !running.contains("value='delete'"));
    shared.lock().unwrap().services[0].resource.phase = Phase::Stopped(StopReason::Requested);
    let stopped = region();
    assert!(stopped.contains("value='delete'") && stopped.contains("Clean up 1 stopped service"), "{stopped}");
}
