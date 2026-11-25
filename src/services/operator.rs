//! The operator's per-chat view of persistent services: which live services a
//! chat (or its whole team) owns, and the same stop the services page makes.
//! Served by the session MCP endpoint (`control.services.list`/`.stop`), which
//! only the local operator reaches; worker attempts use their own service tools.
use super::*;
use control_journal::snapshot::Part;

/// The chat and, with `team`, every chat spawned from it, for the named
/// chat's actor. The named chat must exist.
fn chats(sessions: &[Part<crate::chat::Session>], id: &str, team: bool) -> io::Result<identity::Unchecked<Vec<String>>> {
    let session = sessions
        .iter()
        .find(|session| session.id == id)
        .ok_or_else(|| fail("Chat not found."))?;
    Ok(identity::Unchecked::new(
        session.actor.clone(),
        if team {
            Lineage::of(sessions)
                .subtree([id])
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            vec![id.to_owned()]
        },
    ))
}
fn row(service: &Service) -> Value {
    json!({"service_id":service.resource.id,"chat":service.resource.owner.chat.as_str(),"phase":service.phase_name(),"command":service.label(),"running_commands":service.running_commands()})
}
/// Live services owned by `id` (or its team), oldest first, shown once the
/// chat's actor is authorized (after the lock: a Discord actor's policy is
/// read from disk).
pub(crate) fn list(
    services: &[Service],
    sessions: &[Part<crate::chat::Session>],
    id: &str,
    team: bool,
) -> io::Result<identity::Unchecked<Value>> {
    Ok(chats(sessions, id, team)?.map(|chats| {
        json!({"services":services.iter()
            .filter(|service| service.live() && chats.iter().any(|chat| chat == service.resource.owner.chat.as_str()))
            .map(row).collect::<Vec<_>>()})
    }))
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
    save: impl Fn(&mut State) -> io::Result<()>,
) -> io::Result<Value> {
    let chats = chats(&state.chats, id, team)?
        .check(identity::authorize)
        .map_err(fail)?;
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
