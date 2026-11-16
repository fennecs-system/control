//! Every refusal a model receives from the turn, service, room, watch and
//! agent-message tools names a callable fix: a concrete next call with its
//! parameters, or asking the operator. A message that carries its own fix is
//! returned unchanged; otherwise the tool family's default is appended, so no
//! rejection leaves an agent without a next step. See the adjacent tests.

/// The fix for a refusal no tool call can repair.
pub(crate) const OPERATOR: &str = "ask the operator via control_design_ask";

/// Whether `message` already names a callable fix.
pub(crate) fn names_fix(message: &str) -> bool {
    message.contains(OPERATOR) || concrete_call(message)
}
/// A `control_*` tool name directly followed by its JSON arguments.
fn concrete_call(message: &str) -> bool {
    message.match_indices("control_").any(|(at, prefix)| {
        let rest = &message[at..];
        let name = rest
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
            .unwrap_or(rest.len());
        name > prefix.len() && rest[name..].trim_start_matches(' ').starts_with('{')
    })
}
/// The families this contract covers, by tool name.
pub(crate) fn covered(tool: &str) -> bool {
    default_fix(tool).is_some()
}
fn default_fix(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "control_turn_handoff" | "control_turn_context" => {
            "Next: call control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}}, which needs no receipts or notes; if that is refused too, ask the operator via control_design_ask."
        }
        "control_agent_message" => {
            "Next: call control_room_read {} to confirm participant chat IDs, then retry control_agent_message {\"chat_id\":\"<participant>\",\"text\":\"<message>\"}; if it is refused again, ask the operator via control_design_ask."
        }
        t if t.starts_with("control_service_") => {
            "Next: call control_service_status {} to confirm service IDs, states and node capacity, then retry with a listed service_id; if access is refused, ask the operator via control_design_ask."
        }
        t if t.starts_with("control_room_") => {
            "Next: call control_room_read {} to confirm the room, participants and claims, then retry with the values it lists; if it is refused again, ask the operator via control_design_ask."
        }
        t if t.starts_with("control_watch_") => {
            "Next: call control_watch_status {} to list your watches and their state, then retry with a listed watch_id; if it is refused again, ask the operator via control_design_ask."
        }
        _ => return None,
    })
}
/// `message` with a callable fix for `tool`'s family.
pub(crate) fn actionable(tool: &str, message: &str) -> String {
    match default_fix(tool) {
        Some(fix) if !names_fix(message) => {
            let message = message.trim_end();
            let separator = if message.ends_with(['.', '!', '?']) { " " } else { ". " };
            format!("{message}{separator}{fix}")
        }
        _ => message.to_owned(),
    }
}

#[cfg(test)]
mod tests;
