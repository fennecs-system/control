//! The operator's per-chat view of persistent services: which live services a
//! chat (or its whole team) owns, and the same stop the services page makes.
//! Served by the session MCP endpoint (`control.services.list`/`.stop`), which
//! only the local operator reaches; worker attempts use their own service tools.
use super::*;

/// The chat and, with `team`, every chat spawned from it. The named chat must
/// exist and be visible to its actor.
fn chats(state: &State, id: &str, team: bool) -> io::Result<Vec<String>> {
    let session = state
        .chats
        .iter()
        .find(|session| session.id == id)
        .ok_or_else(|| fail("Chat not found."))?;
    identity::authorize(&session.actor, Action::Task).map_err(fail)?;
    Ok(if team {
        crate::chat::Lineage::of(&state.chats)
            .subtree([id])
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        vec![id.to_owned()]
    })
}
fn row(service: &Service) -> Value {
    json!({"service_id":service.resource.id,"chat":service.resource.owner.chat.as_str(),"phase":service.phase_name(),"command":service.label(),"running_commands":service.running_commands()})
}
/// Live services owned by `id` (or its team), oldest first.
pub(crate) fn list(state: &State, id: &str, team: bool) -> io::Result<Value> {
    let chats = chats(state, id, team)?;
    Ok(json!({"services":state.services.iter()
        .filter(|service| service.live() && chats.iter().any(|chat| chat == service.resource.owner.chat.as_str()))
        .map(row).collect::<Vec<_>>()}))
}
/// Requests a stop of every live service owned by `id` (or its team), or only
/// `service` when named. Each stop is the services page's own transition and
/// persists before the supervisor stops the container; ports bound to a
/// service lose authorization with it. Already-stopping services are no-ops.
pub(crate) fn stop(
    state: &mut State,
    id: &str,
    team: bool,
    service: Option<&str>,
    save: impl Fn(&State) -> io::Result<()>,
) -> io::Result<Value> {
    let chats = chats(state, id, team)?;
    let chosen = state
        .services
        .iter()
        .filter(|s| s.live() && chats.iter().any(|chat| chat == s.resource.owner.chat.as_str()))
        .filter(|s| service.is_none_or(|wanted| s.resource.id.as_str() == wanted))
        .map(|s| s.resource.id.as_str().to_owned())
        .collect::<Vec<_>>();
    if chosen.is_empty() && service.is_some() {
        return Err(fail("No live service with that ID in this chat."));
    }
    let time = now();
    for id in &chosen {
        page::stop(state, id, time, &save)?;
    }
    Ok(json!({"stopped":chosen}))
}

#[cfg(test)]
mod tests;
