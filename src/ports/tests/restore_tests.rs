
use super::*;
#[test]
fn stale_pending_and_foreign_node_resources_are_retired_before_any_listener_effect() {
    let (mut state, run, _) = super::tests::fixture();
    let input: Input = serde_json::from_value(
        json!({"service_id":"svc-a","name":"pending","direction":"ingress","service_port":3000}),
    )
    .unwrap();
    admit(
        &mut state,
        &run,
        "attempt-a",
        "pending",
        input,
        "node-a".to_owned().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    let p = state.ports[0].clone();
    let node = "node-a".to_owned().try_into().unwrap();
    assert_eq!(intent(&state, &p, &node, now()), Intent::Wait);
    assert_eq!(
        intent(&state, &p, &"node-b".to_owned().try_into().unwrap(), now()),
        Intent::Stop
    );
    assert_eq!(
        intent(
            &state,
            &p,
            &node,
            state.services[0].resource.lease.expires()
        ),
        Intent::Stop
    );
    state.services[0].resource.phase = Phase::Stopped(control_core::resources::StopReason::Exited);
    assert_eq!(intent(&state, &p, &node, now()), Intent::Stop);
}
