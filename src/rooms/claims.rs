//! Advisory coordination claims: at most one owner per key in a task room.
//! A claim is an agent's assertion, never host-verified completion or
//! authority, and it prevents duplicate work only when agents check it.
//! Revisions come from one room-wide sequence, so they never repeat after a
//! release and double as wait cursors. Each acquisition starts a new
//! incarnation; waits name it, so reusing a key never delivers an old outcome.
use crate::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

const MAX_KEY: usize = 48;
const MAX_NOTE: usize = 500;
/// Live and released claims retained per room; released ones are evicted first.
const MAX_PER_ROOM: usize = 128;
const MAX_RELATED: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    Blocked,
    Done,
    /// Kept so waiters observe the release and revisions stay monotonic.
    Released,
}
/// How a claim incarnation resolved for the agents waiting on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The owner reported it done; evidence is the owner's, not the host's.
    Done,
    /// The owner or the operator released it unfinished.
    Released,
    /// Host observation: the owner has no active turn and is not a persistent bot.
    Abandoned,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub room: String,
    pub key: String,
    /// Owning chat.
    pub owner: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub revision: u64,
    /// Revision of the acquisition that started this incarnation.
    #[serde(default)]
    pub incarnation: u64,
    /// Host-observed loss of the owner, cleared by the owner's next update.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abandoned: bool,
}
impl Claim {
    fn outcome(&self) -> Option<Outcome> {
        match self.status {
            Status::Done => Some(Outcome::Done),
            Status::Released => Some(Outcome::Released),
            _ if self.abandoned => Some(Outcome::Abandoned),
            _ => None,
        }
    }
}

pub(crate) fn valid_key(key: &str) -> bool {
    (1..=MAX_KEY).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// Wait-topic name for one claim incarnation. Keys contain no '/' or '#'.
pub(crate) fn topic_name(room: &str, key: &str, incarnation: u64) -> String {
    format!("{room}/{key}#{incarnation}")
}
pub(crate) fn parse_topic(name: &str) -> Option<(&str, &str, u64)> {
    let (name, incarnation) = name.rsplit_once('#')?;
    let (room, key) = name.rsplit_once('/')?;
    (crate::rooms::valid(room) && valid_key(key)).then_some(())?;
    Some((room, key, incarnation.parse().ok()?))
}
/// How the named incarnation has resolved, if it has. A key acquired again
/// means the waited incarnation was released.
pub(crate) fn resolution(state: &State, room: &str, key: &str, incarnation: u64) -> Option<(Outcome, u64, String)> {
    let claim = find(state, room, key)?;
    let outcome = if claim.incarnation == incarnation { claim.outcome()? } else { Outcome::Released };
    Some((outcome, claim.revision, claim.owner.clone()))
}
/// Canonical handoff wait topic `claim:<room>/<key>#<incarnation>`. A wait
/// needs a live, unresolved claim: sleeping on a key nobody has committed to
/// produce, or on one that already resolved, would never wake usefully.
pub(crate) fn wait_topic(state: &State, chat: &str, name: &str) -> Result<String, String> {
    let (name, named) = match name.rsplit_once('#') {
        Some((name, incarnation)) => (name, Some(incarnation.parse::<u64>().map_err(|_| "Claim incarnations are integers.")?)),
        None => (name, None),
    };
    let (room, key) = name.rsplit_once('/').filter(|(_, key)| valid_key(key))
        .ok_or("Name a claim wait as claim:<room>/<key>, optionally with #<incarnation>.")?;
    let room = crate::rooms::resolve(state, chat, room)?;
    let Some(claim) = find(state, &room, key).filter(|c| c.status != Status::Released) else {
        return Err(format!("No live claim {key}: nobody has committed to produce it. Claim and produce it yourself, ask the producer to claim it, or ask the operator."));
    };
    let incarnation = named.unwrap_or(claim.incarnation);
    match resolution(state, &room, key, incarnation) {
        Some((outcome, revision, _)) => Err(format!("Claim {key} incarnation {incarnation} already resolved as {} (revision {revision}); act on it instead of waiting.", outcome_name(outcome))),
        None => Ok(format!("claim:{}", topic_name(&room, key, incarnation))),
    }
}
pub(crate) fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Done => "done",
        Outcome::Released => "released",
        Outcome::Abandoned => "abandoned",
    }
}
pub(crate) fn find<'a>(state: &'a State, room: &str, key: &str) -> Option<&'a Claim> {
    state.collective.claims.iter().find(|c| c.room == room && c.key == key)
}
/// Chats that can still act on their claims: an active or queued turn, or a
/// persistent bot. Computed once per pass.
fn active_owners(state: &State) -> HashSet<&str> {
    let chats: HashSet<&str> = state.chats.iter().map(|chat| chat.id.as_str()).collect();
    state.runtime.runs.iter()
        .filter(|run| matches!(run.status.as_str(), "queued" | "running" | "waiting"))
        .filter_map(|run| run.chat_id.as_deref())
        .chain(state.collective.claims.iter().map(|c| c.owner.as_str()).filter(|owner| crate::bots::continuation_enabled(state, owner)))
        .filter(|chat| chats.contains(chat))
        .collect()
}
fn owner_active(state: &State, owner: &str) -> bool {
    active_owners(state).contains(owner)
}
pub(crate) fn listed(state: &State, room: &str) -> Vec<Value> {
    let active = active_owners(state);
    state.collective.claims.iter().filter(|c| c.room == room && c.status != Status::Released)
        .map(|c| json!({"key":c.key,"owner":c.owner,"status":c.status,"blocked_on":c.blocked_on,
            "note":c.note,"revision":c.revision,"incarnation":c.incarnation,
            "abandoned":c.abandoned,"owner_idle":!active.contains(c.owner.as_str())}))
        .collect()
}

/// Lowercase words of at least three characters, without filler.
fn words(text: &str) -> HashSet<String> {
    const FILLER: [&str; 12] = ["the", "and", "for", "with", "own", "owns", "from", "into", "via", "then", "this", "that"];
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| word.len() >= 3 && !FILLER.contains(&word.as_str()))
        .collect()
}
/// Other live claims in the room that look like the same effort. Advisory and
/// bounded: exact keys decide ownership; this only lets the agent join an
/// existing effort or release its duplicate.
fn related(state: &State, room: &str, key: &str, note: &str) -> Vec<Value> {
    let (keys, all) = (words(key), words(&format!("{key} {note}")));
    let mut scored: Vec<(usize, &Claim)> = state.collective.claims.iter()
        .filter(|c| c.room == room && c.key != key && c.status != Status::Released)
        .filter_map(|c| {
            let (their_keys, theirs) = (words(&c.key), words(&format!("{} {}", c.key, c.note)));
            let shared_keys = keys.intersection(&their_keys).count();
            let shared = all.intersection(&theirs).count();
            (shared_keys >= 2 || shared >= 3).then_some((shared_keys * 2 + shared, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.revision.cmp(&a.1.revision)));
    scored.into_iter().take(MAX_RELATED)
        .map(|(_, c)| json!({"key":c.key,"owner":c.owner,"status":c.status,"note":c.note.chars().take(160).collect::<String>()}))
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    /// Resolved by the room broker before the claim operation.
    #[serde(default, rename = "room")]
    _room: Option<serde::de::IgnoredAny>,
    key: String,
    action: Action,
    #[serde(default)]
    status: Option<Status>,
    #[serde(default)]
    blocked_on: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    revision: Option<u64>,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    Acquire,
    Update,
    Release,
}

pub(crate) fn definition() -> Value {
    json!({"name":"control_room_claim","description":"Advisory ownership of one deliverable or investigation in this task's thread, so peers do not duplicate it. acquire is atomic: it succeeds if the key is free or released, otherwise returns the current owner without changing it. Either way it lists related live claims that look like the same effort: join or wait on one of those, or release your duplicate. update and release need the current revision and only work for the owner. status working, blocked (set blocked_on to the claim key you need) or done; done is your claim backed by evidence, not host verification. control_room_read marks abandoned=true when the owner is lost (its turn failed, was cancelled or interrupted, or its bot was stopped) and owner_idle=true when it has no active turn; only the operator releases another owner's claim. A bot waits for a dependency with wait topic claim:<room>/<key>: it wakes once, when that claim incarnation resolves as done, released or abandoned, not on progress updates. Claims grant no authority.","annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false},
        "inputSchema":{"type":"object","properties":{
            "room":{"type":"string"},
            "key":{"type":"string","minLength":1,"maxLength":MAX_KEY,"pattern":"^[A-Za-z0-9._-]+$"},
            "action":{"type":"string","enum":["acquire","update","release"]},
            "status":{"type":"string","enum":["working","blocked","done"]},
            "blocked_on":{"type":"string","maxLength":MAX_KEY},
            "note":{"type":"string","maxLength":MAX_NOTE},
            "revision":{"type":"integer","minimum":1}},
        "required":["key","action"],"additionalProperties":false}})
}

/// A claim change waiters may need: only resolutions are offered to them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub room: String,
    pub key: String,
    pub incarnation: u64,
    pub revision: u64,
    pub outcome: Outcome,
    /// Who caused it; that chat is not woken by its own resolution.
    pub by: Option<String>,
}

/// Applies one claim operation for `chat`. Returns a resolution, if any, so
/// the caller can notify waiters inside its checkpoint.
pub(crate) fn call(state: &mut State, chat: &str, room: &str, args: &Value) -> Result<(Value, Option<Resolved>), &'static str> {
    let call: Call = serde_json::from_value(args.clone()).map_err(|_| "Expected key, action and optional status, blocked_on, note and revision.")?;
    if !valid_key(&call.key) {
        return Err("Claim keys are 1–48 letters, digits, '.', '_' or '-'.");
    }
    let note = call.note.as_deref().map(str::trim).unwrap_or_default();
    if note.chars().count() > MAX_NOTE {
        return Err("Claim notes are at most 500 characters.");
    }
    let status = match call.status {
        Some(Status::Released) => return Err("Use action=release to release a claim."),
        Some(status) => status,
        None if call.action == Action::Acquire => Status::Working,
        None => find(state, room, &call.key).map_or(Status::Working, |c| c.status),
    };
    let blocked_on = match (status, call.blocked_on.as_deref().map(str::trim)) {
        (Status::Blocked, Some(key)) if valid_key(key) && key != call.key => Some(key.to_owned()),
        (Status::Blocked, _) => return Err("A blocked claim names the different claim key it needs in blocked_on."),
        (_, Some(_)) => return Err("blocked_on is only for status=blocked."),
        (_, None) => None,
    };
    let index = state.collective.claims.iter().position(|c| c.room == room && c.key == call.key);
    let current = index.map(|i| state.collective.claims[i].clone());
    let related = if call.action == Action::Acquire { related(state, room, &call.key, note) } else { Vec::new() };
    let unchanged = |claim: &Claim, reason: &str| json!({"applied":false,"reason":reason,"claim":claim,"related":related});
    match call.action {
        Action::Acquire => {
            if let Some(claim) = current.as_ref().filter(|c| c.status != Status::Released) {
                let reason = if claim.owner == chat { "You already own this claim; use update with its revision." } else { "Another participant owns this claim." };
                return Ok((unchanged(claim, reason), None));
            }
        }
        Action::Update | Action::Release => {
            let claim = current.as_ref().filter(|c| c.status != Status::Released).ok_or("No live claim with that key. Acquire it first.")?;
            if claim.owner != chat {
                return Ok((unchanged(claim, "Only the owner can change this claim."), None));
            }
            if call.revision != Some(claim.revision) {
                return Ok((unchanged(claim, "Stale revision; read the claim and retry with its current revision."), None));
            }
        }
    }
    if index.is_none() {
        evict(state, room)?;
    }
    state.collective.claim_seq += 1;
    let revision = state.collective.claim_seq;
    let release = call.action == Action::Release;
    let claim = Claim {
        room: room.to_owned(),
        key: call.key,
        owner: chat.to_owned(),
        status: if release { Status::Released } else { status },
        blocked_on: if release { None } else { blocked_on },
        note: if release { String::new() } else { note.to_owned() },
        revision,
        incarnation: match &current {
            Some(previous) if call.action != Action::Acquire => previous.incarnation,
            _ => revision,
        },
        abandoned: false,
    };
    // Resolution is a transition: re-reporting done is progress, not a new outcome.
    let resolved = claim.outcome().filter(|_| current.as_ref().is_none_or(|c| c.incarnation != claim.incarnation || c.outcome().is_none_or(|o| o == Outcome::Abandoned)))
        .map(|outcome| Resolved { room: room.to_owned(), key: claim.key.clone(), incarnation: claim.incarnation, revision, outcome, by: Some(chat.to_owned()) });
    let value = json!({"applied":true,"claim":claim,"related":related});
    match index {
        Some(i) => state.collective.claims[i] = claim,
        None => state.collective.claims.push(claim),
    }
    Ok((value, resolved))
}
fn evict(state: &mut State, room: &str) -> Result<(), &'static str> {
    if state.collective.claims.iter().filter(|c| c.room == room).count() < MAX_PER_ROOM {
        return Ok(());
    }
    let released = state.collective.claims.iter().position(|c| c.room == room && c.status == Status::Released)
        .ok_or("This room holds the maximum number of live claims; release finished ones first.")?;
    state.collective.claims.remove(released);
    Ok(())
}

/// Owners that host facts say will not act again without new input: the chat
/// is gone, or its latest turn failed, was cancelled, interrupted or blocked.
/// An owner idle after a completed turn is not lost, and a persistent bot's
/// failed turn is retried, so neither wakes dependents.
fn lost_owners(state: &State) -> HashSet<&str> {
    let mut latest: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for run in &state.runtime.runs {
        if let Some(chat) = run.chat_id.as_deref() {
            latest.insert(chat, run.status.as_str());
        }
    }
    let chats: HashSet<&str> = state.chats.iter().map(|chat| chat.id.as_str()).collect();
    state.collective.claims.iter().map(|c| c.owner.as_str())
        .filter(|owner| !chats.contains(owner)
            || (!crate::bots::continuation_enabled(state, owner)
                && latest.get(owner).is_some_and(|status| matches!(*status, "failed" | "cancelled" | "interrupted" | "blocked"))))
        .collect()
}
fn lost(state: &State) -> Vec<usize> {
    let lost = lost_owners(state);
    state.collective.claims.iter().enumerate()
        .filter(|(_, c)| matches!(c.status, Status::Working | Status::Blocked) && !c.abandoned && lost.contains(c.owner.as_str()))
        .map(|(index, _)| index)
        .collect()
}
pub(crate) fn abandonment_due(state: &State) -> bool {
    !lost(state).is_empty()
}
fn abandon(state: &mut State, indices: Vec<usize>) -> Vec<Resolved> {
    indices.into_iter().map(|index| {
        state.collective.claim_seq += 1;
        let revision = state.collective.claim_seq;
        let claim = &mut state.collective.claims[index];
        (claim.abandoned, claim.revision) = (true, revision);
        Resolved { room: claim.room.clone(), key: claim.key.clone(), incarnation: claim.incarnation, revision, outcome: Outcome::Abandoned, by: None }
    }).collect()
}
/// Host observation: live claims whose owner is lost become abandoned once,
/// so dependents wake instead of sleeping on a producer that will not report.
pub(crate) fn observe_abandoned(state: &mut State) -> Vec<Resolved> {
    let indices = lost(state);
    abandon(state, indices)
}
/// An explicit stop (task cancellation or `/bot off`) abandons the chat's live
/// claims immediately; its final running turn may still report afterwards.
pub(crate) fn abandon_owned(state: &mut State, owner: &str) -> Vec<Resolved> {
    let indices = state.collective.claims.iter().enumerate()
        .filter(|(_, c)| c.owner == owner && matches!(c.status, Status::Working | Status::Blocked) && !c.abandoned)
        .map(|(index, _)| index)
        .collect();
    abandon(state, indices)
}

/// Operator reclaim of a claim whose owner can no longer act. The revision
/// must match what the operator saw, so a revived owner's update is not lost.
pub(crate) fn operator_release(state: &mut State, room: &str, key: &str, revision: u64) -> Result<Resolved, &'static str> {
    let index = state.collective.claims.iter()
        .position(|c| c.room == room && c.key == key && c.status != Status::Released)
        .ok_or("That claim is no longer live.")?;
    let claim = &state.collective.claims[index];
    if claim.revision != revision {
        return Err("That claim changed since it was shown; review it again.");
    }
    if owner_active(state, &claim.owner) {
        return Err("Its owner can still act on this claim.");
    }
    state.collective.claim_seq += 1;
    let revision = state.collective.claim_seq;
    let claim = &mut state.collective.claims[index];
    (claim.status, claim.blocked_on, claim.revision) = (Status::Released, None, revision);
    claim.note.clear();
    Ok(Resolved { room: room.to_owned(), key: key.to_owned(), incarnation: claim.incarnation, revision, outcome: Outcome::Released, by: None })
}

/// `POST /room-claim-release`: the operator's explicit reclaim.
pub(crate) fn operator_action(state: &mut State, f: &std::collections::HashMap<String, String>) {
    let field = |name: &str| f.get(name).map(|value| value.trim()).unwrap_or_default();
    let (room, key) = (field("channel"), field("key"));
    let result = field("revision").parse::<u64>().map_err(|_| "Missing claim revision.")
        .and_then(|revision| if crate::rooms::valid(room) && valid_key(key) { Ok(revision) } else { Err("Unknown claim.") })
        .and_then(|revision| operator_release(state, room, key, revision));
    state.collective.notice = match result {
        Ok(resolved) => {
            crate::bots::claim_resolved(state, &resolved);
            String::new()
        }
        Err(message) => message.into(),
    };
}
pub(crate) fn render(state: &State, room: &str) -> String {
    let claims = listed(state, room);
    let mut html = String::from("<h2>Claims</h2>");
    if !state.collective.notice.is_empty() {
        html.push_str(&format!("<p class='muted' role='status'>{}</p>", crate::escape(&state.collective.notice)));
    }
    if claims.is_empty() {
        html.push_str("<p class='muted'>No live claims.</p>");
        return html;
    }
    html.push_str("<ul class='room-claims'>");
    for claim in &claims {
        let text = |name: &str| crate::escape(claim[name].as_str().unwrap_or_default());
        let owner = state.chats.iter().find(|chat| Some(chat.id.as_str()) == claim["owner"].as_str())
            .map_or_else(|| text("owner"), |chat| crate::escape(&chat.title));
        let blocked = claim["blocked_on"].as_str().map_or(String::new(), |key| format!(" on {}", crate::escape(key)));
        html.push_str(&format!("<li><strong>{}</strong> <small>{}{blocked} · {owner} · r{}</small>", text("key"), text("status"), claim["revision"]));
        if !claim["note"].as_str().unwrap_or_default().is_empty() {
            html.push_str(&format!("<p class='preserve muted'>{}</p>", text("note")));
        }
        if claim["abandoned"] == true || claim["owner_idle"] == true {
            html.push_str(&format!("<form method='post' action='/room-claim-release'><input type='hidden' name='channel' value='{}'><input type='hidden' name='key' value='{}'><input type='hidden' name='revision' value='{}'><button>Release claim</button></form>",
                crate::escape(room), text("key"), claim["revision"]));
        }
        html.push_str("</li>");
    }
    html.push_str("</ul>");
    html
}

#[cfg(test)]
mod tests;
