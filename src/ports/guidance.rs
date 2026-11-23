//! What an agent should do with a port request: how to reach an approved
//! egress (service containers have no DNS or direct network, only the
//! loopback relay), how to wait for a pending decision instead of polling, and
//! a hint when a command fails to reach a host one of its requests names.
//! Text only; it grants and changes nothing.
use super::*;
use crate::runtime::Run;

impl Port {
    /// How a process in the service reaches this route once approved.
    pub(super) fn usage(&self) -> Option<String> {
        if self.direction != Direction::Egress {
            return None;
        }
        let local = format!("127.0.0.1:{}", self.service_port);
        Some(match &self.target {
            Some(target) => {
                let (host, port) = (&target.host, target.port);
                let url = match port.get() {
                    443 => format!("https://{host}/…"),
                    80 => format!("http://{host}/…"),
                    _ => format!("https://{host}:{port}/…"),
                };
                format!(
                    "Use the real host name: {} For example curl -fsSLO {url}. Non-HTTP clients connect to {local} instead; it relays to {host}:{port}.",
                    super::proxy::usage()
                )
            }
            None => format!("Connect to {local} inside the service; the relay forwards to the peer ingress."),
        })
    }
    /// The call that waits for this request's decision instead of polling.
    pub(super) fn wait(&self) -> Option<String> {
        (self.status == Status::Pending).then(|| format!(
            "Pending operator approval. Do not poll or retry: hand off with control_turn_handoff {{\"disposition\":{{\"kind\":\"wait\",\"topics\":[\"port:{}\"]}}}}; Control starts this chat's next turn with the decision.",
            self.id.as_str()))
    }
    fn decided(&self) -> &'static str {
        match (self.status, self.decision.as_ref().map(|decision| decision.allowed)) {
            (Status::Approved, _) => "approved",
            (_, Some(false)) => "denied",
            _ => "stopped",
        }
    }
}

/// The chat requested `port` or owns its service, as the same actor.
fn related(runs: &[Run], port: &Port, chat: &str, actor: &Actor) -> bool {
    &port.actor == actor
        && (port.chat == chat
            || runs
                .iter()
                .any(|run| run.id == port.origin_run && run.chat_id.as_deref() == Some(chat)))
}

/// Resolves a `port:<id>` wait for `chat`. `Ok` when the request is still
/// pending: its decision queues the chat's next turn, so no subscription is
/// needed. `Err((decided, message))` otherwise; `decided` means the decision
/// is already known and the agent should act on it rather than wait.
/// What resolving a port wait reads: the requests, the chats that own them
/// and the runs that asked.
#[derive(Clone, Copy)]
pub(crate) struct Requests<'a> {
    ports: &'a [Port],
    chats: &'a [crate::chat::Session],
    runs: &'a [Run],
}
impl<'a> Requests<'a> {
    pub(crate) fn of(state: &'a State) -> Self {
        Self {
            ports: &state.ports,
            chats: &state.chats,
            runs: &state.runtime.runs,
        }
    }
}
pub(crate) fn wait_topic(requests: Requests<'_>, chat: &str, id: &str) -> Result<(), (bool, String)> {
    let actor = requests
        .chats
        .iter()
        .find(|session| session.id == chat)
        .map(|session| session.actor.clone());
    let Some(port) = requests.ports.iter().find(|port| {
        port.id.as_str() == id
            && actor
                .as_ref()
                .is_some_and(|actor| related(requests.runs, port, chat, actor))
    }) else {
        return Err((
            false,
            format!("No port request {id} of this chat; control_port_status lists them"),
        ));
    };
    match port.status {
        Status::Pending => Ok(()),
        _ => Err((
            true,
            format!(
                "Port request {id} was already {}; act on it instead of waiting{}",
                port.decided(),
                port.usage()
                    .filter(|_| port.status == Status::Approved)
                    .map(|usage| format!(". {usage}"))
                    .unwrap_or_default()
            ),
        )),
    }
}

/// Text of a command that could not reach a host: name resolution or
/// connection failures as curl, wget, getaddrinfo and common runtimes report them.
const UNREACHABLE: [&str; 10] = [
    "could not resolve host",
    "name or service not known",
    "temporary failure in name resolution",
    "nodename nor servname",
    "no address associated with hostname",
    "unknownhostexception",
    "getaddrinfo",
    "connection refused",
    "network is unreachable",
    "failed to connect to",
];

/// A one-line hint when a command's `output` (with its `command` text) failed
/// to reach a host named by one of the chat's pending or approved egress
/// requests: wait for the decision, or use the approved relay. Only hosts of
/// the run's own requests match; nothing is hard-coded.
pub(crate) fn egress_hint(runs: &[Run], ports: &[Port], run_id: &str, command: &str, output: &str) -> Option<String> {
    let lower = output.to_ascii_lowercase();
    if !UNREACHABLE.iter().any(|signature| lower.contains(signature)) {
        return None;
    }
    let run = runs.iter().find(|run| run.id == run_id)?;
    let chat = run.chat_id.as_deref()?;
    let command = command.to_ascii_lowercase();
    let port = ports
        .iter()
        .rev()
        .filter(|port| port.direction == Direction::Egress && matches!(port.status, Status::Pending | Status::Approved))
        .filter(|port| related(runs, port, chat, &run.actor))
        .find(|port| {
            port.target.as_ref().is_some_and(|target| {
                let host = target.host.to_ascii_lowercase();
                lower.contains(&host) || command.contains(&host)
            })
        })?;
    let target = port.target.as_ref()?;
    let (host, id) = (format!("{}:{}", target.host, target.port), port.id.as_str());
    let local = format!("127.0.0.1:{}", port.service_port);
    Some(match port.status {
        Status::Pending => format!(
            "Egress to {host} is pending operator approval (port request {id}): hand off with control_turn_handoff {{\"disposition\":{{\"kind\":\"wait\",\"topics\":[\"port:{id}\"]}}}} and wait on it rather than retrying."
        ),
        _ if command.contains(&local) => format!(
            "The egress relay for {host} (port {id}) at {local} did not connect: check control_port_status {{\"port_id\":\"{id}\"}} for ready and diagnostic before retrying."
        ),
        _ => format!(
            "Egress to {host} is approved as port {id}, but this process did not use its proxy. {}",
            port.usage()?
        ),
    })
}

#[cfg(test)]
mod tests;
