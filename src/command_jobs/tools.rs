//! Workspace job tool schemas and input decoding. Attempt fencing, jobs and
//! checkpoints stay behind `JobOps`.
use super::Input;
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub enum JobCall {
    Start(Input),
    Status(String),
    Cancel(String),
}
pub trait JobOps {
    fn job(&self, attempt: &AttemptRef<'_>, call: JobCall) -> io::Result<Value>;
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"name":"control_workspace_start","description":"Start a credential-free offline workspace command and return a job handle immediately. Output streams into the chat. Poll status until complete; cancel before editing concurrently. Only one command writer per attempt. worker uses the captured worker image; build uses the operator-declared build environment. No host shell, network, credentials or image mutation. Jobs are cancelled when the attempt ends; await completion before returning your answer.","inputSchema":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":16000},"timeout_seconds":{"type":"integer","minimum":1,"maximum":1800,"default":600},"environment":{"type":"string","enum":["worker","build"],"default":"worker"}},"required":["command"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}}),
            json!({"name":"control_workspace_status","description":"Read the bounded live output, status and exit code of a workspace job in this attempt. Status does not wait for completion.","inputSchema":{"type":"object","properties":{"job_id":{"type":"string"}},"required":["job_id"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
            json!({"name":"control_workspace_cancel","description":"Cancel an exact workspace job in this attempt. The job remains stopping until its verified container has exited.","inputSchema":{"type":"object","properties":{"job_id":{"type":"string"}},"required":["job_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":false}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        matches!(name, "control_workspace_start" | "control_workspace_status" | "control_workspace_cancel")
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        run(cx.host, &cx.attempt, name, args)
    }
}
impl JobOps for LocalHost<'_> {
    fn job(&self, attempt: &AttemptRef<'_>, call: JobCall) -> io::Result<Value> {
        super::execute(self.0, attempt, call)
    }
}
/// Decodes a call and runs it on whatever provides `JobOps`.
fn run(ops: &dyn JobOps, attempt: &AttemptRef<'_>, name: &str, args: &Value) -> io::Result<Value> {
    ops.job(attempt, decode(name, args)?)
}
fn decode(name: &str, args: &Value) -> io::Result<JobCall> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Handle {
        job_id: String,
    }
    let handle = || {
        serde_json::from_value::<Handle>(args.clone()).map(|handle| handle.job_id).map_err(|_| io::Error::other("Expected job_id"))
    };
    match name {
        "control_workspace_start" => serde_json::from_value(args.clone())
            .map(JobCall::Start)
            .map_err(|_| io::Error::other("Expected command, timeout_seconds and declared environment")),
        "control_workspace_status" => handle().map(JobCall::Status),
        "control_workspace_cancel" => handle().map(JobCall::Cancel),
        _ => Err(io::Error::other("Unknown workspace tool")),
    }
}
