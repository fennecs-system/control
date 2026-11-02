//! Port tool schemas and input decoding. Domain rules, authorization and
//! checkpoints stay behind `PortOps`.
use super::Input;
use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools};
use control_core::resources::ResourceId;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io;

pub enum PortCall {
    Request(Input),
    Status(Option<ResourceId>),
    Stop(Option<ResourceId>),
}
pub trait PortOps {
    fn port(&self, attempt: &AttemptRef<'_>, call: PortCall) -> io::Result<Value>;
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"name":"control_port_request","description":"Request operator approval for a named TCP port on an owned or explicitly granted running service. ingress exposes non-HTTP/non-TLS raw service loopback on host 127.0.0.1; HTTP artifacts use control_preview_start. egress exposes a service loopback relay to one approved pinned target, including TLS. Containers remain network=none. To connect two accessible services, request ingress on the listening service, then egress on the connecting service with peer_port_id set to that ingress port_id; the client dials its own 127.0.0.1:service_port. Different owner chats require captured service grants, not just a shared actor. Egress requires exactly one of target or peer_port_id. No traffic until approved; a peer route also needs its ingress approved and ready. Poll control_port_status before connecting. Updated worker images keep one listener alive for up to eight concurrent sessions per port. Start new services from rebuilt images to upgrade older single-session relays. Port lifetime cannot exceed the service lifetime.","inputSchema":{"type":"object","properties":{"service_id":{"type":"string"},"name":{"type":"string","maxLength":48},"direction":{"type":"string","enum":["ingress","egress"]},"service_port":{"type":"integer","minimum":1,"maximum":65535},"target":{"type":"object","properties":{"host":{"type":"string"},"port":{"type":"integer","minimum":1,"maximum":65535}},"required":["host","port"],"additionalProperties":false},"peer_port_id":{"type":"string","description":"Ingress port_id of another owned or explicitly granted service; egress only."}},"required":["service_id","name","direction","service_port"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
            json!({"name":"control_port_status","description":"Read owned port approvals, readiness, pinned endpoints, assigned loopback addresses and bounded diagnostics. An ingress local_address is reachable only from the host; connect another owned service with an egress peer_port_id route. Omit port_id for active and ten recent terminal records.","inputSchema":{"type":"object","properties":{"port_id":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
            json!({"name":"control_port_stop","description":"Persist revocation of an owned port and close its listener/connections. Stopping is intent; inspect until stopped. Never stops the owning service.","inputSchema":{"type":"object","properties":{"port_id":{"type":"string"}},"required":["port_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        matches!(name, "control_port_request" | "control_port_status" | "control_port_stop")
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        cx.host.ports().port(&cx.attempt, decode(name, args)?)
    }
}
fn decode(name: &str, args: &Value) -> io::Result<PortCall> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Id {
        port_id: Option<ResourceId>,
    }
    let id = || serde_json::from_value::<Id>(args.clone()).map(|id| id.port_id).map_err(io::Error::other);
    match name {
        "control_port_request" => serde_json::from_value(args.clone()).map(PortCall::Request).map_err(io::Error::other),
        "control_port_status" => id().map(PortCall::Status),
        "control_port_stop" => id().map(PortCall::Stop),
        _ => Err(io::Error::other("Unknown port tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::Host;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
    }
    impl Host for Recorder {
        fn jobs(&self) -> &dyn crate::command_jobs::JobOps {
            unreachable!("port tools must not reach job operations")
        }
        fn services(&self) -> &dyn crate::services::ServiceOps {
            unreachable!("port tools must not reach service operations")
        }
        fn ports(&self) -> &dyn PortOps {
            self
        }
        fn bots(&self) -> &dyn crate::bots::BotOps {
            unreachable!("port tools must not reach bot operations")
        }
    }
    impl PortOps for Recorder {
        fn port(&self, attempt: &AttemptRef<'_>, call: PortCall) -> io::Result<Value> {
            let id = |id: Option<ResourceId>| id.map_or("-".to_owned(), |id| id.as_str().to_owned());
            let call = match call {
                PortCall::Request(input) => format!("request {}", input.service_id.as_str()),
                PortCall::Status(port) => format!("status {}", id(port)),
                PortCall::Stop(port) => format!("stop {}", id(port)),
            };
            self.calls.lock().unwrap().push(format!("{}/{}/{} {call}", attempt.run, attempt.generation, attempt.nonce));
            Ok(json!({"recorded":true}))
        }
    }

    #[test]
    fn port_tools_decode_typed_calls_and_reach_only_the_host_operations() {
        let host = Recorder::default();
        let cx = AttemptCall { attempt: AttemptRef { run: "run", generation: "gen", nonce: "n1" }, host: &host };
        let request = json!({"service_id":"svc-a","name":"echo","direction":"ingress","service_port":31000});
        assert_eq!(Tools.call(&cx, "control_port_request", &request).unwrap(), json!({"recorded":true}));
        Tools.call(&cx, "control_port_status", &json!({})).unwrap();
        Tools.call(&cx, "control_port_status", &json!({"port_id":"port-1"})).unwrap();
        Tools.call(&cx, "control_port_stop", &json!({"port_id":"port-1"})).unwrap();
        assert_eq!(*host.calls.lock().unwrap(), [
            "run/gen/n1 request svc-a",
            "run/gen/n1 status -",
            "run/gen/n1 status port-1",
            "run/gen/n1 stop port-1",
        ]);
        assert!(Tools.call(&cx, "control_port_status", &json!({"port_id":"port-1","extra":1})).is_err());
        assert!(Tools.call(&cx, "control_port_request", &json!({"service_id":"svc-a"})).is_err());
        assert!(Tools.call(&cx, "control_port_open", &json!({})).is_err());
        assert_eq!(host.calls.lock().unwrap().len(), 4);
    }
}
