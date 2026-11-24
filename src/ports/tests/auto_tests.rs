//! Port requests under the node's automatic approval policy. Service checks
//! and DNS pinning are injected; the transition is the operator's `approve`.
use super::*;
use crate::runtime::RunStatus;
use control::permissions::{AutoApproval, Mode};

fn auto() -> AutoApproval {
    AutoApproval {
        mode: Mode::Auto,
        ..Default::default()
    }
}
/// Files a pending request and returns its ID.
fn file(state: &mut State, run: &str, nonce: &str, input: Value) -> String {
    let input: Input = serde_json::from_value(input).unwrap();
    let value = admit(
        state,
        run,
        "attempt-a",
        nonce,
        input,
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    value["port_id"].as_str().unwrap().to_owned()
}
fn egress(state: &mut State, run: &str, nonce: &str, host: &str, port: u16) -> String {
    file(
        state,
        run,
        nonce,
        json!({"service_id":"svc-a","name":nonce,"direction":"egress","service_port":31000 + state.ports.len() as u16,"target":{"host":host,"port":port}}),
    )
}
/// Runs the policy with the service check passing and `host` pinned to `ip`.
fn decide(shared: &Store, id: &str, policy: &AutoApproval, ip: &str) -> Option<Value> {
    let ip: IpAddr = ip.parse().unwrap();
    auto_approve(
        shared,
        id,
        policy,
        |_| Ok(()),
        |_, _| Ok(()),
        |endpoint, loopback| validate_destination(endpoint, &[SocketAddr::new(ip, endpoint.port.get())], loopback),
    )
}
fn status(shared: &Store, id: &str) -> Status {
    shared
        .guard()
        .unwrap()
        .ports
        .iter()
        .find(|p| p.id.as_str() == id)
        .unwrap()
        .status
}

#[test]
fn public_https_is_approved_and_recorded_with_the_policy_as_decider() {
    let (mut state, run, chat) = tests::fixture();
    let id = egress(&mut state, &run, "registry", "registry.npmjs.org", 443);
    let shared = crate::store::Store::new(state);
    let value = decide(&shared, &id, &auto(), "104.16.1.34").expect("approved");
    assert_eq!(
        (
            value["status"].as_str(),
            value["decided_by"].as_str(),
            value["rule"].as_str()
        ),
        (Some("approved"), Some("policy"), Some("public-web"))
    );
    assert!(value["next"].is_null(), "nothing to wait for");
    let state = shared.guard().unwrap();
    let port = &state.ports[0];
    assert_eq!(port.status, Status::Approved);
    assert_eq!(
        port.approval.as_ref().unwrap().pinned,
        Some("104.16.1.34:443".parse().unwrap())
    );
    assert!(!port.approval.as_ref().unwrap().allow_loopback);
    let record = state.auto_decisions.last().unwrap();
    assert_eq!(
        (
            record.kind.as_str(),
            record.id.as_str(),
            record.chat.as_str(),
            record.decider.as_str()
        ),
        ("port", id.as_str(), chat.as_str(), "policy")
    );
    assert!(record.what.contains("registry.npmjs.org:443"));
    assert!(
        state
            .bots
            .notices
            .iter()
            .any(|notice| notice.id == format!("policy-{id}"))
    );
    assert!(
        overview(&state.ports, &state.auto_decisions)
            .iter()
            .map(Open::render)
            .collect::<String>()
            .contains("auto · public-web")
    );
    drop(state);
    // The tool result carried the decision: no decision turn is queued.
    let mut state = shared.guard().unwrap();
    state.runtime.runs[0].status = RunStatus::Completed;
    let runs = state.runtime.runs.len();
    decisions::advance(&mut state, |_| Ok(())).unwrap();
    assert_eq!(state.runtime.runs.len(), runs);
    // Revocation is the operator's usual one.
    let chosen = vec![0];
    stop_all(&mut state, &chosen, |_| Ok(())).unwrap();
    assert_eq!(state.ports[0].status, Status::Stopping);
}

#[test]
fn private_unlisted_loopback_and_privately_resolving_targets_stay_pending() {
    let (mut state, run, _) = tests::fixture();
    let private = egress(&mut state, &run, "lan", "10.0.0.5", 443);
    let loopback = egress(&mut state, &run, "loop", "127.0.0.1", 25565);
    let sneaky = egress(&mut state, &run, "sneaky", "rebind.example.com", 443);
    let shared = crate::store::Store::new(state);
    assert!(decide(&shared, &private, &auto(), "10.0.0.5").is_none());
    assert!(decide(&shared, &loopback, &auto(), "127.0.0.1").is_none());
    assert!(
        decide(&shared, &sneaky, &auto(), "192.168.1.9").is_none(),
        "pinning still refuses a private answer"
    );
    for id in [&private, &loopback, &sneaky] {
        assert_eq!(status(&shared, id), Status::Pending);
    }
    assert!(shared.guard().unwrap().auto_decisions.is_empty());
}

#[test]
fn a_listed_standing_loopback_target_is_approved_with_the_loopback_grant() {
    let (mut state, run, _) = tests::fixture();
    let id = egress(&mut state, &run, "testbed", "127.0.0.1", 25565);
    let shared = crate::store::Store::new(state);
    let policy = AutoApproval {
        standing_targets: vec!["127.0.0.1:25565".into()],
        ..auto()
    };
    assert_eq!(
        decide(&shared, &id, &policy, "127.0.0.1").unwrap()["rule"],
        "standing-target"
    );
    let state = shared.guard().unwrap();
    assert!(state.ports[0].approval.as_ref().unwrap().allow_loopback);
}

#[test]
fn strict_mode_and_an_exhausted_task_quota_stay_pending() {
    let (mut state, run, chat) = tests::fixture();
    let id = egress(&mut state, &run, "first", "example.com", 443);
    let shared = crate::store::Store::new(state);
    assert!(
        decide(&shared, &id, &AutoApproval::default(), "93.184.216.34").is_none(),
        "strict by default"
    );
    // A spawned chat's services count against the same task quota.
    {
        let mut state = shared.guard().unwrap();
        let child = crate::chat::create(&mut state);
        state.chats.last_mut().unwrap().parent_chat = Some(chat.clone());
        let template = state.ports[0].clone();
        for index in 0..crate::auto_approval::TASK_PORTS {
            let mut port = template.clone();
            port.id = ResourceId::try_from(format!("port-held-{index}")).unwrap();
            port.chat = child.clone();
            port.status = Status::Approved;
            state.ports.push(port);
        }
    }
    assert!(
        decide(&shared, &id, &auto(), "93.184.216.34").is_none(),
        "over the task quota"
    );
    assert_eq!(status(&shared, &id), Status::Pending);
}

#[test]
fn high_ingress_is_approved_and_privileged_ingress_asks() {
    let (mut state, run, _) = tests::fixture();
    let high = file(
        &mut state,
        &run,
        "high",
        json!({"service_id":"svc-a","name":"console","direction":"ingress","service_port":25565}),
    );
    let low = file(
        &mut state,
        &run,
        "low",
        json!({"service_id":"svc-a","name":"web","direction":"ingress","service_port":80}),
    );
    let shared = crate::store::Store::new(state);
    assert_eq!(
        decide(&shared, &high, &auto(), "127.0.0.1").unwrap()["rule"],
        "ingress-high-port"
    );
    assert!(decide(&shared, &low, &auto(), "127.0.0.1").is_none());
    assert_eq!(status(&shared, &low), Status::Pending);
}

#[test]
fn a_failed_check_or_save_leaves_the_request_with_the_operator() {
    let (mut state, run, _) = tests::fixture();
    let id = egress(&mut state, &run, "web", "example.com", 443);
    let shared = crate::store::Store::new(state);
    let pin = |endpoint: &Endpoint, loopback| {
        validate_destination(endpoint, &["93.184.216.34:443".parse().unwrap()], loopback)
    };
    assert!(
        auto_approve(
            &shared,
            &id,
            &auto(),
            |_| Ok(()),
            |_, _| Err(io::Error::other("old relay image")),
            pin
        )
        .is_none()
    );
    assert!(
        auto_approve(
            &shared,
            &id,
            &auto(),
            |_| Err(io::Error::other("disk full")),
            |_, _| Ok(()),
            pin
        )
        .is_none()
    );
    let state = shared.guard().unwrap();
    assert_eq!(state.ports[0].status, Status::Pending);
    assert!(state.ports[0].approval.is_none() && state.auto_decisions.is_empty() && state.bots.notices.is_empty());
}
