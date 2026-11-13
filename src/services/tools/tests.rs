
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl ServiceOps for Recorder {
    fn service(&self, attempt: &AttemptRef<'_>, call: ServiceCall) -> io::Result<Value> {
        let id = |id: Option<String>| id.unwrap_or_else(|| "-".into());
        let call = match call {
            ServiceCall::Start(_) => "start".to_owned(),
            ServiceCall::Status(service) => format!("status {}", id(service)),
            ServiceCall::Attach(service) => format!("attach {}", id(service)),
            ServiceCall::Stop(service) => format!("stop {}", id(service)),
            ServiceCall::Exec(_) => "exec".to_owned(),
            ServiceCall::ExecStatus(_) => "exec_status".to_owned(),
        };
        self.0.lock().unwrap().push(format!(
            "{}/{}/{} {call}",
            attempt.run, attempt.generation, attempt.nonce
        ));
        Ok(json!({"recorded":true}))
    }
}

#[test]
fn service_tools_decode_typed_calls_and_keep_lenient_service_ids() {
    let host = Recorder::default();
    let attempt = AttemptRef {
        run: "run",
        generation: "gen",
        nonce: "n1",
    };
    let call = |name: &str, args: Value| run(&host, &attempt, name, &args);
    call("control_service_start", json!({"command":"sleep infinity"})).unwrap();
    call("control_service_status", json!({})).unwrap();
    call(
        "control_service_status",
        json!({"service_id":7,"ignored":true}),
    )
    .unwrap();
    call("control_service_attach", json!({"service_id":"svc-a"})).unwrap();
    call(
        "control_service_stop",
        json!({"service_id":"svc-a","extra":1}),
    )
    .unwrap();
    call(
        "control_service_exec",
        json!({"service_id":"svc-a","command":"true"}),
    )
    .unwrap();
    call(
        "control_service_exec_status",
        json!({"service_id":"svc-a","execution_id":"e1"}),
    )
    .unwrap();
    assert_eq!(
        *host.0.lock().unwrap(),
        [
            "run/gen/n1 start",
            "run/gen/n1 status -",
            "run/gen/n1 status -",
            "run/gen/n1 attach svc-a",
            "run/gen/n1 stop svc-a",
            "run/gen/n1 exec",
            "run/gen/n1 exec_status",
        ]
    );
    let error = |name: &str, args: Value| call(name, args).unwrap_err().to_string();
    assert!(
        error("control_service_start", json!({"command":"true","extra":1}))
            .contains("unknown field")
    );
    assert_eq!(
        error("control_service_exec", json!({"command":"true"})),
        "Expected service_id, command and optional timeout_seconds"
    );
    assert_eq!(
        error("control_service_exec_status", json!({"service_id":"svc-a"})),
        "Expected service_id and execution_id"
    );
    assert_eq!(
        error("control_service_restart", json!({})),
        "Unknown service tool"
    );
    assert_eq!(host.0.lock().unwrap().len(), 7);
}
