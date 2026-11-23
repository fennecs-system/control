use super::*;

fn auto() -> AutoApproval {
    AutoApproval {
        mode: Mode::Auto,
        ..Default::default()
    }
}
fn egress<'a>(actor: &'a Actor, host: &'a str, port: u16) -> PortFacts<'a> {
    PortFacts {
        actor,
        ingress: false,
        service_port: 31000,
        target: Some((host, port)),
        peer_in_task: None,
        service_in_task: true,
        task_ports: 1,
    }
}
fn port(policy: &AutoApproval, facts: PortFacts<'_>) -> Verdict {
    classify(policy, &Request::Port(facts))
}
fn bot_facts(actor: &Actor) -> BotFacts<'_> {
    BotFacts {
        actor,
        operator_tree: true,
        observer: false,
        task_chats: 3,
        spawn_limit: 64,
        task_bots: 0,
        node_bots: 0,
    }
}

#[test]
fn public_web_targets_are_automatic_in_auto_mode_only() {
    let actor = Actor::LocalOperator;
    assert_eq!(
        port(&auto(), egress(&actor, "registry.npmjs.org", 443)),
        Verdict::Approve("public-web")
    );
    assert_eq!(
        port(&auto(), egress(&actor, "example.com", 80)),
        Verdict::Approve("public-web")
    );
    assert_eq!(
        port(&AutoApproval::default(), egress(&actor, "example.com", 443)),
        Verdict::Human("strict mode"),
        "new nodes are strict"
    );
    assert_eq!(
        port(
            &AutoApproval {
                port: KindMode::Ask,
                ..auto()
            },
            egress(&actor, "example.com", 443)
        ),
        Verdict::Human("ports set to ask")
    );
    assert_eq!(
        port(&auto(), egress(&actor, "example.com", 8443)),
        Verdict::Ask("unusual port")
    );
    assert_eq!(
        port(&auto(), egress(&actor, "93.184.216.34", 443)),
        Verdict::Ask("literal public address")
    );
}

#[test]
fn private_and_unlisted_loopback_targets_always_ask() {
    let actor = Actor::LocalOperator;
    for (host, target) in [
        ("10.0.0.5", 443),
        ("192.168.1.2", 80),
        ("172.16.0.1", 443),
        ("169.254.169.254", 80),
        ("fe80::1", 443),
        ("127.0.0.1", 25565),
        ("::1", 443),
        ("0.0.0.0", 80),
    ] {
        assert_eq!(
            port(&auto(), egress(&actor, host, target)),
            Verdict::Human("unlisted private target"),
            "{host}:{target}"
        );
    }
    for host in ["localhost", "printer.local", "db.internal", "box.lan", "intranet"] {
        assert_eq!(
            port(&auto(), egress(&actor, host, 443)),
            Verdict::Human("unlisted private target"),
            "{host}"
        );
    }
}

#[test]
fn listed_standing_targets_are_automatic() {
    let actor = Actor::LocalOperator;
    let policy = AutoApproval {
        standing_targets: vec!["127.0.0.1:25565".into(), "mirror.example.com:8080".into()],
        ..auto()
    };
    assert_eq!(
        port(&policy, egress(&actor, "127.0.0.1", 25565)),
        Verdict::Approve("standing-target")
    );
    assert_eq!(
        port(&policy, egress(&actor, "mirror.example.com", 8080)),
        Verdict::Approve("standing-target")
    );
    assert_eq!(
        port(&policy, egress(&actor, "127.0.0.1", 25566)),
        Verdict::Human("unlisted private target"),
        "exact port only"
    );
    assert_eq!(
        port(
            &AutoApproval {
                mode: Mode::Strict,
                ..policy
            },
            egress(&actor, "127.0.0.1", 25565)
        ),
        Verdict::Human("strict mode")
    );
}

#[test]
fn quota_scope_and_ingress_boundaries() {
    let actor = Actor::LocalOperator;
    let over = PortFacts {
        task_ports: TASK_PORTS + 1,
        ..egress(&actor, "example.com", 443)
    };
    assert_eq!(port(&auto(), over), Verdict::Human("task port quota"));
    let at = PortFacts {
        task_ports: TASK_PORTS,
        ..egress(&actor, "example.com", 443)
    };
    assert_eq!(port(&auto(), at), Verdict::Approve("public-web"));
    let foreign = PortFacts {
        service_in_task: false,
        ..egress(&actor, "example.com", 443)
    };
    assert_eq!(port(&auto(), foreign), Verdict::Ask("service outside the task"));
    let ingress = |service_port| PortFacts {
        ingress: true,
        service_port,
        target: None,
        ..egress(&actor, "", 0)
    };
    assert_eq!(port(&auto(), ingress(1025)), Verdict::Approve("ingress-high-port"));
    assert_eq!(port(&auto(), ingress(1024)), Verdict::Ask("privileged ingress port"));
    let peer = |in_task| PortFacts {
        target: None,
        peer_in_task: Some(in_task),
        ..egress(&actor, "", 0)
    };
    assert_eq!(port(&auto(), peer(true)), Verdict::Approve("peer-route-in-task"));
    assert_eq!(port(&auto(), peer(false)), Verdict::Ask("peer outside the task"));
}

#[test]
fn folder_access_is_never_automatic() {
    // There is no folder setting: a file that tries one is invalid, and an
    // invalid file reads as the strict default.
    let file = r#"{"auto_approval":{"mode":"auto","folder":"auto"}}"#;
    assert!(serde_json::from_str::<control::permissions::Permissions>(file).is_err());
    // A folder request stays pending in Auto mode: nothing on its path consults the policy.
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::projects::request(
        &mut state,
        &chat,
        "/srv/project",
        crate::projects::Requester::Agent { run: None },
    )
    .unwrap();
    assert!(state.chats[0].project.as_ref().is_some_and(|project| project.pending));
    assert!(state.auto_decisions.is_empty());
}

#[test]
fn bots_within_limits_are_automatic_and_over_limits_ask() {
    let actor = Actor::LocalOperator;
    let bot = |facts| classify(&auto(), &Request::Bot(facts));
    assert_eq!(bot(bot_facts(&actor)), Verdict::Approve("bot-in-task-limits"));
    assert_eq!(
        bot(BotFacts {
            task_bots: BOTS_PER_TASK,
            ..bot_facts(&actor)
        }),
        Verdict::Human("bot or spawn limit")
    );
    assert_eq!(
        bot(BotFacts {
            node_bots: BOTS_PER_NODE,
            ..bot_facts(&actor)
        }),
        Verdict::Human("bot or spawn limit")
    );
    assert_eq!(
        bot(BotFacts {
            task_chats: 65,
            ..bot_facts(&actor)
        }),
        Verdict::Human("bot or spawn limit")
    );
    assert_eq!(
        bot(BotFacts {
            operator_tree: false,
            ..bot_facts(&actor)
        }),
        Verdict::Human("not an operator task")
    );
    assert_eq!(
        bot(BotFacts {
            observer: true,
            ..bot_facts(&actor)
        }),
        Verdict::Approve("observer-in-limits")
    );
    // Observers use their own allowance: a full team still gets one.
    assert_eq!(
        bot(BotFacts {
            observer: true,
            task_bots: BOTS_PER_TASK,
            task_chats: 65,
            ..bot_facts(&actor)
        }),
        Verdict::Approve("observer-in-limits")
    );
    assert_eq!(
        bot(BotFacts {
            observer: true,
            node_bots: BOTS_PER_NODE,
            ..bot_facts(&actor)
        }),
        Verdict::Human("bot or spawn limit")
    );
    assert_eq!(
        classify(
            &AutoApproval {
                bot: KindMode::Ask,
                ..auto()
            },
            &Request::Bot(bot_facts(&actor))
        ),
        Verdict::Human("bots set to ask")
    );
}

#[test]
fn strict_principals_always_ask_in_auto_mode() {
    let operator = Actor::LocalOperator;
    let user = Actor::DiscordDm {
        user_id: serde_json::from_value(serde_json::json!("42")).unwrap(),
        channel_id: serde_json::from_value(serde_json::json!("7")).unwrap(),
    };
    let policy = AutoApproval {
        strict: vec!["discord:42".into()],
        ..auto()
    };
    assert_eq!(
        port(&policy, egress(&user, "example.com", 443)),
        Verdict::Human("strict principal")
    );
    assert_eq!(
        port(&policy, egress(&operator, "example.com", 443)),
        Verdict::Approve("public-web")
    );
    assert_eq!(
        classify(&policy, &Request::Bot(bot_facts(&user))),
        Verdict::Human("strict principal")
    );
    let local = AutoApproval {
        strict: vec!["local".into()],
        ..auto()
    };
    assert_eq!(
        classify(&local, &Request::Bot(bot_facts(&operator))),
        Verdict::Human("strict principal")
    );
    assert_eq!(
        port(&auto(), egress(&Actor::Unknown, "example.com", 443)),
        Verdict::Human("strict principal"),
        "no trusted identity"
    );
}

/// A stand-in for a later model-based stage that approves everything it sees.
struct Yes;
impl Classifier for Yes {
    fn classify(&self, _: &AutoApproval, _: &Request<'_>) -> Verdict {
        Verdict::Approve("model")
    }
}
#[test]
fn a_later_stage_decides_only_borderline_requests_in_auto_mode() {
    let actor = Actor::LocalOperator;
    let request = Request::Port(egress(&actor, "example.com", 8443));
    assert_eq!(
        decide(&auto(), &request, Some(&Yes)),
        Verdict::Approve("model"),
        "borderline"
    );
    assert_eq!(
        decide(&auto(), &Request::Port(egress(&actor, "10.0.0.1", 443)), Some(&Yes)),
        Verdict::Human("unlisted private target")
    );
    assert_eq!(
        decide(&AutoApproval::default(), &request, Some(&Yes)),
        Verdict::Human("strict mode")
    );
    assert_eq!(
        classify(&auto(), &request),
        Verdict::Ask("unusual port"),
        "no later stage is installed"
    );
}

#[test]
fn decisions_are_recorded_with_the_policy_as_decider_and_rate_limited_notices() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);

    for index in 0..crate::intel::POLICY_NOTICES_PER_HOUR + 2 {
        record(
            &mut state,
            &auto(),
            "port",
            &format!("port-{index}"),
            &chat,
            format!("egress p{index}"),
            "public-web",
        );
    }
    let last = state.auto_decisions.last().unwrap();
    assert_eq!(
        (last.decider.as_str(), last.rule.as_str(), last.kind.as_str()),
        ("policy", "public-web", "port")
    );
    assert_eq!(rule(&state.auto_decisions, "port-0").as_deref(), Some("public-web"));
    assert_eq!(
        state.bots.notices.len(),
        crate::intel::POLICY_NOTICES_PER_HOUR,
        "notices are rate-limited"
    );
    assert!(
        state
            .bots
            .notices
            .iter()
            .all(|notice| notice.id.starts_with("policy-") && notice.text.contains("No action needed"))
    );
    // Agents keep their own notice budget.
    assert_eq!(
        crate::intel::pending_json_tree(&state, &chat).len(),
        crate::intel::POLICY_NOTICES_PER_HOUR
    );
    let mut quiet = State::default();
    let chat = crate::chat::create(&mut quiet);
    record(
        &mut quiet,
        &AutoApproval {
            notices: false,
            ..auto()
        },
        "bot",
        "bot-1",
        &chat,
        "persistent bot".into(),
        "bot-in-task-limits",
    );
    assert!(
        quiet.bots.notices.is_empty() && quiet.auto_decisions.len() == 1,
        "notices can be switched off"
    );
    let html = overview(&quiet);
    assert!(
        html.contains("Running now") && html.contains("bot-in-task-limits") && html.contains("persistent bot"),
        "{html}"
    );
    for index in 0..RECORDS + 5 {
        record(
            &mut quiet,
            &AutoApproval {
                notices: false,
                ..auto()
            },
            "bot",
            &format!("b{index}"),
            &chat,
            "x".into(),
            "r",
        );
    }
    assert_eq!(quiet.auto_decisions.len(), RECORDS);
}
