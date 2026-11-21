
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl ServiceOps for Recorder {
    fn service(&self, attempt: &AttemptRef<'_>, call: ServiceCall) -> io::Result<Value> {
        let id = |id: Option<String>| id.unwrap_or_else(|| "-".into());
        let call = match call {
            ServiceCall::Start(_) => "start".to_owned(),
            ServiceCall::Status(service, node) => format!("status {}{}", id(service), if node { " node" } else { "" }),
            ServiceCall::Attach(service, node) => format!("attach {}{}", id(service), if node { " node" } else { "" }),
            ServiceCall::Stop(service) => format!("stop {}", id(service)),
            ServiceCall::Exec(_) => "exec".to_owned(),
            ServiceCall::ExecStatus(_) => "exec_status".to_owned(),
            ServiceCall::Copy(_) => "copy".to_owned(),
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
    call("control_service_attach", json!({"service_id":"svc-a","detail":"node"})).unwrap();
    call("control_service_status", json!({"detail":"everything"})).unwrap();
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
            "run/gen/n1 attach svc-a node",
            "run/gen/n1 status -",
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
    // A wrong exec shape names the field (or the command's own bound) and
    // shows one valid call, which is itself the fix.
    let example = r#"Example: control_service_exec {"service_id":"svc-<id>","command":"<shell command>"}"#;
    for (args, names) in [
        (json!({"command":"true"}), "missing field `service_id`"),
        (json!({"service_id":"svc-a","cmd":"true"}), "unknown field `cmd`"),
        (json!({"service_id":"svc-a","command":["ls"]}), "invalid type: sequence"),
        (json!({"service_id":"svc-a","command":"x".repeat(16_001)}), "1–16000 bytes"),
    ] {
        let shown = error("control_service_exec", args);
        assert!(shown.contains(names) && shown.ends_with(example), "{shown}");
        assert_eq!(crate::worker_tools::fixes::actionable("control_service_exec", &shown), shown);
    }
    assert_eq!(
        error("control_service_exec_status", json!({"service_id":"svc-a"})),
        "Expected service_id, execution_id and optional wait_seconds"
    );
    assert_eq!(
        error("control_service_restart", json!({})),
        "Unknown service tool"
    );
    assert_eq!(host.0.lock().unwrap().len(), 9);
}

#[test]
fn exec_description_reserves_repetition_for_routines() {
    let definitions = Tools.definitions();
    let exec = definitions.iter().find(|d| d["name"] == "control_service_exec").unwrap()["description"].as_str().unwrap();
    assert!(exec.starts_with("Run a one-off command inside"));
    for phrase in ["setup, inspection, or installing and editing a routine", "For anything repeated or polled, write a routine (control_watch_start) instead", "Uncertain commands are never replayed", "Results return inline when the command finishes within wait_seconds", "Call control_service_exec_status only when the result says running and gives an execution_id"] {
        assert!(exec.contains(phrase), "{phrase}");
    }
}
