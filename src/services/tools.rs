//! Service tool schemas and input decoding. Ownership, the resource reducer,
//! container checks and checkpoints stay behind `ServiceOps`.
use super::{Input, commands};
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost};
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
            json!({"name":"control_service_start","description":"Start a chat-owned offline tool container that survives model turns and Control restart. Status reports its platform (e.g. linux/aarch64): download binaries built for that architecture. lease_seconds=-1 (default) disables lease expiry; a positive duration limits its lifetime. Process exit still triggers container cleanup. Keep ALL task code and durable data in /workspace: it is a mounted disk volume shared by this chat's new services, retained after container removal and automatically reopened by replacement services. Avoid conflicting writes between same-chat services. /home/agent and other writable-layer files are lost on removal; /tmp is disposable RAM scratch (128 MiB), erased on restart. Optional shape selects small (2 GiB, 1 CPU), medium (4 GiB, 2 CPUs), or large (8 GiB, 2 CPUs), independent of image capabilities. Omit shape for the image default. Node service budget: 16 GiB and 8 CPUs; stopping and fenced services retain reservations until cleanup. No automatic resizing: stop and replace to change shape, reusing this chat's workspace. Leave memory headroom inside the container for application overhead. Optional storage_limit_mb caps only the private writable layer, NOT /workspace, and requires Docker quota support. No attempt files, credentials, network or host mounts. Use a foreground command. Returns a durable handle immediately; inspect status before assuming startup succeeded. Older services keep their captured layout: check status.workspace for retained storage. A retained volume is not a backup against disk loss or explicit volume deletion.","inputSchema":{"type":"object","properties":{"shape":{"type":"string","enum":["small","medium","large"],"description":"Optional compute preset. Omit to retain image default limits. Use control_service_status without an ID to see presets and remaining node capacity."},"command":{"type":"string","minLength":1,"maxLength":16000},"lease_seconds":{"type":"integer","oneOf":[{"const":-1},{"minimum":1,"maximum":3600}],"default":-1},"storage_limit_mb":{"type":"integer","minimum":1,"maximum":u32::MAX,"description":"Optional private writable-layer quota in MiB; /workspace and other mounts are excluded. Requires a quota-capable Docker storage driver. Omit for no Control disk cap; unsupported quotas fail creation."}},"required":["command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
        ];
        for (name, description, readonly) in [
            (
                "control_service_attach",
                "Attach this turn to one of this chat's services; returns status and bounded logs. Attachment grants no host filesystem access and does not extend the lease.",
                true,
            ),
            (
                "control_service_status",
                "Read this chat's service status and bounded logs. Omit service_id to list active services, ten recent completed handles, available compute shapes and the node service budget.",
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
        run(cx.host, &cx.attempt, name, args)
    }
}
impl ServiceOps for LocalHost<'_> {
    fn service(&self, attempt: &AttemptRef<'_>, call: ServiceCall) -> io::Result<Value> {
        super::execute(self.0, attempt, call)
    }
}
/// Decodes a call and runs it on whatever provides `ServiceOps`.
fn run(ops: &dyn ServiceOps, attempt: &AttemptRef<'_>, name: &str, args: &Value) -> io::Result<Value> {
    ops.service(attempt, decode(name, args)?)
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
mod tests;
