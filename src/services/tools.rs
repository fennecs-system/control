//! Service tool schemas and input decoding. Ownership, the resource reducer,
//! container checks and checkpoints stay behind `ServiceOps`.
use super::{Input, commands};
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools};
use serde_json::{Value, json};
use std::io;

pub enum ServiceCall {
    Start(Input),
    Status(Option<String>),
    Attach(Option<String>),
    Stop(Option<String>),
    Exec(commands::Input),
    ExecStatus(commands::Handle),
}
pub trait ServiceOps {
    fn service(&self, attempt: &AttemptRef<'_>, call: ServiceCall) -> io::Result<Value>;
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        let mut tools = vec![
            json!({"name":"control_service_start","description":"Start a chat-owned offline tool container that survives model turns and Control restart. lease_seconds=-1 (default) retains it until explicitly stopped; a positive duration limits its lifetime. Private disk-backed /workspace for task files and worktrees and /home/agent for tool configuration and caches, with no default disk cap. RAM-backed /tmp is disposable scratch (128 MiB), erased on container restart. Optional storage_limit_mb caps the private writable layer only and requires Docker quota support; mounted folders are excluded. No attempt files, credentials, network or host mounts. Use a foreground command. The immutable worker image needs control-service-runner. Returns a durable handle immediately; inspect status before assuming startup succeeded. Workspace and home files last until container removal.","inputSchema":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":16000},"lease_seconds":{"type":"integer","oneOf":[{"const":-1},{"minimum":1,"maximum":3600}],"default":-1},"storage_limit_mb":{"type":"integer","minimum":1,"maximum":u32::MAX,"description":"Optional private writable-layer quota in MiB; mounts are excluded. Requires a quota-capable Docker storage driver. Omit for no Control disk cap; unsupported quotas fail creation."}},"required":["command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
        ];
        for (name, description, readonly) in [
            (
                "control_service_attach",
                "Attach this turn to one of this chat's services; returns status and bounded logs. Attachment grants no host filesystem access and does not extend the lease.",
                true,
            ),
            (
                "control_service_status",
                "Read this chat's service status and bounded logs. Omit service_id to list active services and ten recent completed handles.",
                true,
            ),
            (
                "control_service_stop",
                "Request stop of an exact chat-owned service. Stopping is not confirmation of removal; inspect status until Stopped. Does not cancel the model turn.",
                false,
            ),
        ] {
            tools.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":{"service_id":{"type":"string"}},"required":if name=="control_service_status"{json!([])}else{json!(["service_id"])},"additionalProperties":false},"annotations":{"readOnlyHint":readonly,"openWorldHint":false}}));
        }
        tools.extend(commands::definitions());
        tools
    }
    fn known(&self, name: &str) -> bool {
        matches!(
            name,
            "control_service_start"
                | "control_service_attach"
                | "control_service_status"
                | "control_service_stop"
                | "control_service_exec"
                | "control_service_exec_status"
        )
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        cx.host.services().service(&cx.attempt, decode(name, args)?)
    }
}
/// Status, attach and stop read `service_id` only when it is a string; other
/// fields are ignored, as before typed decoding.
fn decode(name: &str, args: &Value) -> io::Result<ServiceCall> {
    let id = || args.get("service_id").and_then(Value::as_str).map(str::to_owned);
    match name {
        "control_service_start" => serde_json::from_value(args.clone()).map(ServiceCall::Start).map_err(io::Error::other),
        "control_service_status" => Ok(ServiceCall::Status(id())),
        "control_service_attach" => Ok(ServiceCall::Attach(id())),
        "control_service_stop" => Ok(ServiceCall::Stop(id())),
        "control_service_exec" => serde_json::from_value(args.clone())
            .map(ServiceCall::Exec)
            .map_err(|_| io::Error::other("Expected service_id, command and optional timeout_seconds")),
        "control_service_exec_status" => serde_json::from_value(args.clone())
            .map(ServiceCall::ExecStatus)
            .map_err(|_| io::Error::other("Expected service_id and execution_id")),
        _ => Err(io::Error::other("Unknown service tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::Host;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);
    impl Host for Recorder {
        fn jobs(&self) -> &dyn crate::command_jobs::JobOps {
            unreachable!("service tools must not reach job operations")
        }
        fn services(&self) -> &dyn ServiceOps {
            self
        }
        fn ports(&self) -> &dyn crate::ports::PortOps {
            unreachable!("service tools must not reach port operations")
        }
    }
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
            self.0.lock().unwrap().push(format!("{}/{}/{} {call}", attempt.run, attempt.generation, attempt.nonce));
            Ok(json!({"recorded":true}))
        }
    }

    #[test]
    fn service_tools_decode_typed_calls_and_keep_lenient_service_ids() {
        let host = Recorder::default();
        let cx = AttemptCall { attempt: AttemptRef { run: "run", generation: "gen", nonce: "n1" }, host: &host };
        let call = |name: &str, args: Value| Tools.call(&cx, name, &args);
        call("control_service_start", json!({"command":"sleep infinity"})).unwrap();
        call("control_service_status", json!({})).unwrap();
        call("control_service_status", json!({"service_id":7,"ignored":true})).unwrap();
        call("control_service_attach", json!({"service_id":"svc-a"})).unwrap();
        call("control_service_stop", json!({"service_id":"svc-a","extra":1})).unwrap();
        call("control_service_exec", json!({"service_id":"svc-a","command":"true"})).unwrap();
        call("control_service_exec_status", json!({"service_id":"svc-a","execution_id":"e1"})).unwrap();
        assert_eq!(*host.0.lock().unwrap(), [
            "run/gen/n1 start",
            "run/gen/n1 status -",
            "run/gen/n1 status -",
            "run/gen/n1 attach svc-a",
            "run/gen/n1 stop svc-a",
            "run/gen/n1 exec",
            "run/gen/n1 exec_status",
        ]);
        let error = |name: &str, args: Value| call(name, args).unwrap_err().to_string();
        assert!(error("control_service_start", json!({"command":"true","extra":1})).contains("unknown field"));
        assert_eq!(error("control_service_exec", json!({"command":"true"})), "Expected service_id, command and optional timeout_seconds");
        assert_eq!(error("control_service_exec_status", json!({"service_id":"svc-a"})), "Expected service_id and execution_id");
        assert_eq!(error("control_service_restart", json!({})), "Unknown service tool");
        assert_eq!(host.0.lock().unwrap().len(), 7);
    }
}
