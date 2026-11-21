//! `control_room_plan`: a read-only planning view of one room's claims for
//! whoever steers the team. Claims form a functional graph (each blocked claim
//! names at most one claim it needs), so cycles, the longest dependency chain
//! (critical path) and the parallel frontier are linear walks. Bottlenecks use
//! the advisor's walk (`claims::graph`). Nothing is assigned or changed.
use super::claims::{self, Claim, Status, graph::{Fast, Waits}};
use crate::State;
use serde_json::{Value, json};

/// Entries per list; a room holds at most 128 claims.
const LIST: usize = 16;
const BOTTLENECKS: usize = 8;

pub(crate) fn definition() -> Value {
    json!({"name":"control_room_plan","description":"Planning view of the team room's claims, for whoever steers or assigns work: open claims, dependency cycles, the critical path (longest blocked_on chain, producer first), bottlenecks (claims with the most transitive dependents), ready claims that can run in parallel now (working, resumable because their dependency is done, unclaimed inputs, abandoned), and members idle or only blocked who could take them. Use it to plan and assign instead of reconstructing the dependency graph from control_room_read. Read-only; omit room for the team room.","annotations":{"readOnlyHint":true,"openWorldHint":false},
        "inputSchema":{"type":"object","properties":{"room":{"type":"string"}},"additionalProperties":false}})
}

/// Depth of a node's dependency chain, or `None` when it leads into a cycle.
#[derive(Clone, Copy)]
enum Mark {
    Walking(usize),
    Depth(Option<u32>),
}

pub(crate) fn plan(state: &State, room: &str) -> Value {
    let claims = state.collective.claims.iter().filter(|claim| claim.room == room && claim.status != Status::Released).collect::<Vec<&Claim>>();
    let by_key = claims.iter().map(|claim| (claim.key.as_str(), *claim)).collect::<Fast<&str, &Claim>>();
    let open = claims.iter().copied().filter(|claim| matches!(claim.status, Status::Working | Status::Blocked)).collect::<Vec<_>>();
    // Needed keys nobody holds (never claimed, or released) are work too.
    let mut unclaimed = open.iter().filter_map(|claim| claim.blocked_on.as_deref()).filter(|key| !by_key.contains_key(key)).collect::<Vec<_>>();
    unclaimed.sort_unstable();
    unclaimed.dedup();
    // The edge a node still waits on: none once its dependency is done.
    let next = |key: &str| -> Option<&str> {
        let on = by_key.get(key)?.blocked_on.as_deref().filter(|_| by_key[key].status == Status::Blocked)?;
        by_key.get(on).is_none_or(|claim| claim.status != Status::Done).then_some(on)
    };
    let nodes = open.iter().map(|claim| claim.key.as_str()).chain(unclaimed.iter().copied()).collect::<Vec<_>>();
    let mut marks: Fast<&str, Mark> = Fast::default();
    let mut cycles: Vec<Vec<&str>> = Vec::new();
    for &start in &nodes {
        let mut path: Vec<&str> = Vec::new();
        let mut at = Some(start);
        // Follow the single outgoing edge until a known depth, the chain's
        // end, or a node already on this path (a cycle).
        let mut below = loop {
            let Some(key) = at else { break Some(0) };
            match marks.get(key).copied() {
                Some(Mark::Depth(depth)) => break depth,
                Some(Mark::Walking(index)) => {
                    let cycle = path[index..].to_vec();
                    for key in &cycle {
                        marks.insert(key, Mark::Depth(None));
                    }
                    path.truncate(index);
                    cycles.push(cycle);
                    break None;
                }
                None => {
                    marks.insert(key, Mark::Walking(path.len()));
                    path.push(key);
                    at = next(key);
                }
            }
        };
        for key in path.into_iter().rev() {
            below = below.map(|depth| depth + 1);
            marks.insert(key, Mark::Depth(below));
        }
    }
    let depth = |key: &str| match marks.get(key) { Some(Mark::Depth(depth)) => *depth, _ => None };
    // Deepest chain, producer first.
    let deepest = nodes.iter().copied().filter_map(|key| Some((depth(key)?, key))).max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(a.1)));
    let mut critical = Vec::new();
    let mut at = deepest.map(|(_, key)| key);
    while let Some(key) = at {
        critical.push(key);
        at = next(key);
    }
    critical.reverse();
    // Bottlenecks: unfinished producers by transitive dependents.
    let waits = Waits::of(claims.iter().copied());
    let mut dependents: Fast<&str, usize> = Fast::default();
    let mut necks = waits.keys().filter(|(_, key)| by_key.get(key).is_none_or(|claim| claim.status != Status::Done)).map(|(room, key)| {
        let held = by_key.get(key);
        let reach = waits.reach(room, key, held.map(|claim| claim.owner.as_str()));
        dependents.insert(key, reach.dependents.len());
        (key, held, reach)
    }).collect::<Vec<_>>();
    necks.sort_by(|a, b| b.2.members.len().cmp(&a.2.members.len()).then(b.2.dependents.len().cmp(&a.2.dependents.len())).then(a.0.cmp(b.0)));
    let bottlenecks = necks.iter().take(BOTTLENECKS).map(|(key, held, reach)| {
        let mut neck = json!({"key":key,"owner":held.map(|claim| &claim.owner),"status":held.map_or("unclaimed", |claim| status(claim)),
            "dependents":reach.dependents.len(),"members":reach.members.iter().take(LIST).collect::<Vec<_>>()});
        if reach.cycle { neck["cycle"] = json!(true); }
        neck
    }).collect::<Vec<_>>();
    // The parallel frontier: nodes with no unfinished dependency.
    let mut ready = open.iter().filter(|claim| next(&claim.key).is_none())
        .map(|claim| (claim.key.as_str(), Some(claim.owner.as_str()), if claim.abandoned { "abandoned" } else if claim.status == Status::Blocked { "resumable" } else { "working" }))
        .chain(unclaimed.iter().map(|key| (*key, None, "unclaimed")))
        .collect::<Vec<_>>();
    let unblocks = |key: &str| dependents.get(key).copied().unwrap_or(0);
    ready.sort_by_key(|(key, ..)| (!critical.contains(key), std::cmp::Reverse(unblocks(key)), *key));
    let ready = ready.iter().take(LIST).map(|(key, owner, why)| {
        let mut entry = json!({"key":key,"why":why,"unblocks":unblocks(key)});
        if let Some(owner) = owner { entry["owner"] = json!(owner); }
        if critical.contains(key) { entry["critical"] = json!(true); }
        entry
    }).collect::<Vec<_>>();
    // Members with no live working claim: idle, or only blocked.
    let active = claims::active_owners(state);
    let mut members = state.chats.iter().zip(crate::callsigns::all(&state.chats))
        .filter(|(chat, _)| chat.room.as_deref() == Some(room) || open.iter().any(|claim| claim.owner == chat.id))
        // An advisor answers questions; it is not a member to assign work to.
        .filter(|(chat, _)| !state.bots.advises(&chat.id))
        .filter(|(chat, _)| !open.iter().any(|claim| claim.owner == chat.id && claim.status == Status::Working && !claim.abandoned))
        .map(|(chat, callsign)| {
            let waiting = open.iter().filter(|claim| claim.owner == chat.id && claim.status == Status::Blocked).filter_map(|claim| claim.blocked_on.as_deref()).collect::<Vec<_>>();
            (waiting.is_empty(), active.contains(chat.id.as_str()), chat, callsign, waiting)
        })
        .collect::<Vec<_>>();
    // Free members first, then those without an active turn.
    members.sort_by_key(|(free, busy, ..)| (!free, *busy));
    let available = members.iter().take(LIST).map(|(_, busy, chat, callsign, waiting)| {
        let mut member = json!({"chat":chat.id,"title":crate::callsigns::label(chat, *callsign),"idle":!busy});
        if !waiting.is_empty() { member["blocked_on"] = json!(waiting); }
        member
    }).collect::<Vec<_>>();
    let open_list = open.iter().take(LIST * 4).map(|claim| {
        let mut entry = json!({"key":claim.key,"owner":claim.owner,"status":status(claim)});
        if let Some(on) = claim.blocked_on.as_deref() { entry["blocked_on"] = json!(on); }
        entry
    }).collect::<Vec<_>>();
    json!({"room":room,"open":open_list,"done":claims.len() - open.len(),"unclaimed":unclaimed.iter().take(LIST).collect::<Vec<_>>(),
        "cycles":cycles,"critical_path":critical,"bottlenecks":bottlenecks,"ready":ready,"available":available})
}
fn status(claim: &Claim) -> &'static str {
    match claim.status {
        Status::Done => "done",
        Status::Released => "released",
        _ if claim.abandoned => "abandoned",
        Status::Working => "working",
        Status::Blocked => "blocked",
    }
}

#[cfg(test)]
mod tests;
