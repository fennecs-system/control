
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
}
impl PortOps for Recorder {
    fn port(&self, attempt: &AttemptRef<'_>, call: PortCall) -> io::Result<Value> {
        let id = |id: Option<ResourceId>| id.map_or("-".to_owned(), |id| id.as_str().to_owned());
        let call = match call {
            PortCall::Request(input) => format!("request {}", input.service_id.as_str()),
            PortCall::Status(port) => format!("status {}", id(port)),
            PortCall::Stop(Selection::Port(port)) => format!("stop {}", port.as_str()),
            PortCall::Stop(Selection::Matching { service, direction }) => {
                format!("stop {} {direction:?}", id(service))
            }
        };
        self.calls.lock().unwrap().push(format!(
            "{}/{}/{} {call}",
            attempt.run, attempt.generation, attempt.nonce
        ));
        Ok(json!({"recorded":true}))
    }
}

#[test]
fn port_tools_decode_typed_calls_and_reach_only_the_host_operations() {
    let host = Recorder::default();
    let attempt = AttemptRef {
        run: "run",
        generation: "gen",
        nonce: "n1",
    };
    let request =
        json!({"service_id":"svc-a","name":"echo","direction":"ingress","service_port":31000});
    assert_eq!(
        run(&host, &attempt, "control_port_request", &request).unwrap(),
        json!({"recorded":true})
    );
    run(&host, &attempt, "control_port_status", &json!({})).unwrap();
    run(
        &host,
        &attempt,
        "control_port_status",
        &json!({"port_id":"port-1"}),
    )
    .unwrap();
    run(
        &host,
        &attempt,
        "control_port_stop",
        &json!({"port_id":"port-1"}),
    )
    .unwrap();
    run(
        &host,
        &attempt,
        "control_port_stop",
        &json!({"direction":"egress"}),
    )
    .unwrap();
    run(&host, &attempt, "control_port_stop", &json!({"all":true})).unwrap();
    assert_eq!(
        *host.calls.lock().unwrap(),
        [
            "run/gen/n1 request svc-a",
            "run/gen/n1 status -",
            "run/gen/n1 status port-1",
            "run/gen/n1 stop port-1",
            "run/gen/n1 stop - Some(Egress)",
            "run/gen/n1 stop - None",
        ]
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_port_status",
            &json!({"port_id":"port-1","extra":1})
        )
        .is_err()
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_port_request",
            &json!({"service_id":"svc-a"})
        )
        .is_err()
    );
    assert!(run(&host, &attempt, "control_port_open", &json!({})).is_err());
    for bad in [
        json!({}),
        json!({"all":false}),
        json!({"all":true,"direction":"egress"}),
        json!({"port_id":"port-1","all":true}),
    ] {
        assert!(
            run(&host, &attempt, "control_port_stop", &bad).is_err(),
            "{bad}"
        );
    }
    assert_eq!(host.calls.lock().unwrap().len(), 6);
}
