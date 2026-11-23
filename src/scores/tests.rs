use super::*;

fn run(id: &str, chat: &str, status: &str) -> crate::runtime::Run {
    serde_json::from_value(
        json!({"id":id,"objective":"work","kind":"task","status":status,"source":"test","result":"",
        "chat_id":chat,"attempts":[{"status":status}]}),
    )
    .unwrap()
}
fn claim(owner: &str, status: &str) -> crate::rooms::claims::Claim {
    serde_json::from_value(json!({"room":"team","key":owner,"owner":owner,"status":status,"revision":1})).unwrap()
}
fn probe(chat: &str, name: &str) -> Probe {
    Probe {
        chat: chat.into(),
        name: name.into(),
        service: "svc-gone".into(),
        generation: 1,
        command: "cat score.json".into(),
        interval_seconds: 300,
        declared_unix: 1_000,
        last_unix: 1_000,
        milestones: 0,
        error: String::new(),
        samples: VecDeque::new(),
    }
}
fn team() -> (State, String, String) {
    let mut state = State::default();
    let root = crate::chat::create(&mut state);
    let member = crate::chat::create(&mut state);
    state.chats[1].parent_chat = Some(root.clone());
    (state, root, member)
}

#[test]
fn scores_are_the_last_line_as_a_small_object_of_finite_numbers() {
    assert_eq!(
        parse("connecting…\n{\"blocks_placed\":75,\"beds\":2}\n\n").unwrap(),
        BTreeMap::from([("beds".into(), 2.0), ("blocks_placed".into(), 75.0)])
    );
    assert_eq!(parse("{\"items\":120.5}").unwrap()["items"], 120.5);
    for (output, error) in [
        ("", "printed nothing"),
        ("items: 120", "JSON object"),
        ("[1,2]", "JSON object"),
        ("{}", "1 to 8"),
        (
            "{\"a\":1,\"b\":2,\"c\":3,\"d\":4,\"e\":5,\"f\":6,\"g\":7,\"h\":8,\"i\":9}",
            "1 to 8",
        ),
        ("{\"alerts\":\"3\"}", "not a number"),
        ("{\"ok\":true}", "not a number"),
        ("{\"two words\":1}", "Score names"),
    ] {
        assert!(parse(output).unwrap_err().contains(error), "{output:?}");
    }
    assert!(
        parse(&format!("{{\"x\":{}}}", "1".repeat(MAX_LINE_BYTES)))
            .unwrap_err()
            .contains("exceeds")
    );
}

#[test]
fn a_full_series_thins_its_older_half_and_keeps_its_whole_span() {
    let mut samples = VecDeque::new();
    for unix in 0..10 * MAX_SAMPLES as u64 {
        push(
            &mut samples,
            Sample {
                unix,
                trigger: Trigger::Schedule,
                values: BTreeMap::from([("n".into(), unix as f64)]),
            },
        );
        assert!(samples.len() <= MAX_SAMPLES);
    }
    assert_eq!(samples.front().unwrap().unix, 0, "the first sample stays");
    assert_eq!(
        samples.back().unwrap().unix,
        10 * MAX_SAMPLES as u64 - 1,
        "the newest is last"
    );
    assert!(
        samples.iter().zip(samples.iter().skip(1)).all(|(a, b)| a.unix < b.unix),
        "in time order"
    );
    let recent = samples.iter().rev().take(MAX_SAMPLES / 4).collect::<Vec<_>>();
    assert!(
        recent.windows(2).all(|pair| pair[0].unix == pair[1].unix + 1),
        "recent samples keep full resolution"
    );
    // Serialized size at the caps stays small.
    let mut probe = probe("c", "score");
    probe.samples = (0..MAX_SAMPLES as u64)
        .map(|unix| Sample {
            unix: 1_800_000_000 + unix,
            trigger: Trigger::Schedule,
            values: (0..MAX_KEYS).map(|key| (format!("metric_{key}"), 123_456.0)).collect(),
        })
        .collect();
    let bytes = serde_json::to_vec(&probe).unwrap().len();
    assert!(bytes * MAX_PROBES < 4 << 20, "{bytes} bytes per full probe");
}

#[test]
fn milestones_count_completed_turns_and_done_claims_across_the_team() {
    let (mut state, root, member) = team();
    let outsider = crate::chat::create(&mut state);
    state.runtime.runs = vec![
        run("r1", &root, "completed"),
        run("r2", &member, "completed"),
        run("r3", &member, "failed"),
        run("r4", &outsider, "completed"),
    ];
    state.collective.claims = vec![
        claim(&member, "done"),
        claim(&root, "working"),
        claim(&outsider, "done"),
    ];
    assert_eq!(milestones(&state, &root), 3);
    assert_eq!(milestones(&state, &member), 3, "any chat names its team");
    assert_eq!(milestones(&state, &outsider), 2);
}

#[test]
fn probes_sample_on_schedule_and_on_milestones_and_record_failures() {
    let (mut state, root, member) = team();
    state.scores = vec![probe(&root, "score")];
    assert!(due(&state, 1_100).is_empty(), "not due and no milestone");
    state.runtime.runs.push(run("r1", &member, "completed"));
    assert!(due(&state, 1_010).is_empty(), "milestones are rate limited");
    let pending = due(&state, 1_100);
    assert_eq!(
        (pending.len(), pending[0].trigger, pending[0].milestones),
        (1, Trigger::Milestone, 1)
    );
    // The service is gone: the failure is recorded, no sample is invented.
    let item = pending.into_iter().next().unwrap();
    let outcome = item.target.map(|_| BTreeMap::new());
    assert!(apply(
        &mut state,
        &item.chat,
        &item.name,
        1_100,
        item.trigger,
        item.milestones,
        outcome
    ));
    assert!(state.scores[0].samples.is_empty() && state.scores[0].error.contains("declare the probe again"));
    assert!(due(&state, 1_200).is_empty(), "a milestone seen once does not repeat");
    let pending = due(&state, 1_400);
    assert_eq!(pending[0].trigger, Trigger::Schedule);
    assert!(apply(
        &mut state,
        &root,
        "score",
        1_400,
        Trigger::Schedule,
        1,
        Ok(BTreeMap::from([("items".into(), 120.0)]))
    ));
    assert!(state.scores[0].error.is_empty());
    assert_eq!(state.scores[0].samples.back().unwrap().values["items"], 120.0);
    assert!(
        !apply(
            &mut state,
            &root,
            "other",
            1_400,
            Trigger::Schedule,
            1,
            Ok(BTreeMap::new())
        ),
        "a cleared probe is not recreated"
    );
}

#[test]
fn the_operator_reads_every_probe_in_the_team_and_the_chat_view_shows_the_latest() {
    let (mut state, root, member) = team();
    let outsider = crate::chat::create(&mut state);
    let mut lead = probe(&root, "build");
    push(
        &mut lead.samples,
        Sample {
            unix: 1_000,
            trigger: Trigger::Declared,
            values: BTreeMap::from([("blocks_placed".into(), 75.0), ("beds".into(), 2.0)]),
        },
    );
    let mut worker = probe(&member, "<items>");
    worker.error = "The probe exited with status 1 <b>".into();
    state.scores = vec![lead, worker, probe(&outsider, "elsewhere")];
    let read_root = read(&state, &member).unwrap();
    assert_eq!(read_root["root"], root.as_str());
    let names = read_root["probes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["build", "<items>"]);
    assert_eq!(read_root["probes"][0]["samples"][0]["values"]["blocks_placed"], 75.0);
    assert!(read(&state, "c-missing").is_err());
    let line = line(&state, &root);
    assert!(line.contains("build: beds 2, blocks_placed 75 (1 samples)"), "{line}");
    let failing = super::line(&state, &member);
    assert!(
        failing.contains("&lt;items&gt;: no sample yet")
            && failing.contains("status 1 &lt;b&gt;")
            && !failing.contains("<b>")
    );
    assert_eq!(super::line(&state, "c-none"), "");
    // Probes of deleted chats are retired.
    state.chats.retain(|chat| chat.id != outsider);
    assert!(retire(&mut state) && state.scores.len() == 2);
}

/// The supervisor's due check runs every 5 s under the state lock: it counts
/// team milestones once per probe chat. `cargo test --release --locked --bin
/// control scores::tests::due_profile -- --ignored --nocapture`
#[test]
#[ignore = "local timing"]
fn due_profile() {
    for (teams, members, runs_per_chat) in [(4, 8, 20), (32, 8, 20), (32, 8, 100)] {
        let mut state = State::default();
        for _ in 0..teams {
            let root = crate::chat::create(&mut state);
            for _ in 0..members {
                let member = crate::chat::create(&mut state);
                state.chats.last_mut().unwrap().parent_chat = Some(root.clone());
                for index in 0..runs_per_chat {
                    state
                        .runtime
                        .runs
                        .push(run(&format!("{member}-{index}"), &member, "completed"));
                }
                state.collective.claims.push(claim(&member, "done"));
            }
            state.scores.push(probe(&root, "score"));
        }
        let rounds = 200;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(due(std::hint::black_box(&state), 1_100));
        }
        eprintln!(
            "{teams} probes, {} chats, {} runs: {:?} per pass",
            state.chats.len(),
            state.runtime.runs.len(),
            start.elapsed() / rounds
        );
    }
}

#[test]
fn declarations_are_validated_before_any_service_runs() {
    assert!(
        matches!(decode(&json!({"service_id":"svc-1","command":"cat s.json"})).unwrap(),
        ScoreCall::Declare { name, interval_seconds: DEFAULT_INTERVAL, .. } if name == "score")
    );
    assert!(
        matches!(decode(&json!({"name":"game","clear":true})).unwrap(), ScoreCall::Clear { name } if name == "game")
    );
    for (args, error) in [
        (json!({"service_id":"svc-1"}), "needs service_id and command"),
        (json!({"service_id":"svc-1","command":" "}), "1 to 2000 bytes"),
        (
            json!({"service_id":"svc-1","command":"x","interval_seconds":5}),
            "interval_seconds",
        ),
        (json!({"name":"two words","clear":true}), "probe name"),
        (json!({"service_id":"svc-1","command":"x","unexpected":1}), "Expected"),
    ] {
        assert!(decode(&args).err().unwrap().to_string().contains(error), "{args}");
    }
    let names = Tools
        .definitions()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .all(|name| control_context::worker_mcp_names().contains(&name.as_str())),
        "advertised to managed providers"
    );
    assert!(
        !control_context::tool_policy::after_handoff("control_score_probe"),
        "fenced after a handoff"
    );
}

#[test]
fn a_probe_samples_only_the_generation_it_was_declared_with() {
    let (mut state, root, _) = team();
    state.scores = vec![probe(&root, "score")];
    let service = |generation: u64| {
        serde_json::from_value::<crate::services::Service>(json!({
        "resource":{"id":"svc-gone","owner":{"node":"node-a","chat":root},"generation":generation,
            "image":format!("sha256:{}", "a".repeat(64)),"scope":{"network":"Offline","storage":"PrivateMemory"},"lease":[1,u64::MAX],"phase":"Requested"},
        "origin":{"run":"r0","attempt":"a0"},"actor":{"type":"local_operator"},"command":"true","uid":502,"gid":20
    })).unwrap()
    };
    state.services.push(service(2));
    let replaced = due(&state, 1_400).into_iter().next().unwrap();
    assert!(replaced.target.err().unwrap().contains("stopped or was replaced"));
    state.services.push(service(1));
    let Ok((target, command)) = due(&state, 1_400).into_iter().next().unwrap().target else {
        panic!("the declared generation is sampled")
    };
    assert_eq!(
        (target.resource.generation.get(), command.as_str()),
        (1, "cat score.json")
    );
}
