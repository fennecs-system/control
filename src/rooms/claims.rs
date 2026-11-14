//! Advisory coordination claims: at most one owner per key in a task room.
//! A claim is an agent's assertion, never host-verified completion or
//! authority, and it prevents duplicate work only when agents check it.
//! Revisions come from one room-wide sequence, so they never repeat after a
//! release and double as wait cursors.
use crate::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_KEY: usize = 48;
const MAX_NOTE: usize = 500;
/// Live and released claims retained per room; released ones are evicted first.
const MAX_PER_ROOM: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    Blocked,
    Done,
    /// Kept so waiters observe the release and revisions stay monotonic.
    Released,
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
}

pub(crate) fn valid_key(key: &str) -> bool {
    (1..=MAX_KEY).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
/// Wait-topic name for a claim. Keys contain no '/', so the last one splits.
pub(crate) fn topic_name(room: &str, key: &str) -> String {
    format!("{room}/{key}")
}
pub(crate) fn parse_topic(name: &str) -> Option<(&str, &str)> {
    name.rsplit_once('/').filter(|(room, key)| crate::rooms::valid(room) && valid_key(key))
}
/// Canonical handoff wait topic `claim:<room>/<key>@<revision>`, where the
/// revision is the one the agent last saw. Without one the current revision
/// is used, and waiting for a claim that is already done or released is
/// refused rather than sleeping until a change that may never come.
pub(crate) fn wait_topic(state: &State, chat: &str, name: &str) -> Result<String, String> {
    let (name, seen) = match name.rsplit_once('@') {
        Some((name, seen)) => (name, Some(seen.parse::<u64>().map_err(|_| "Claim wait revisions are integers.")?)),
        None => (name, None),
    };
    let (room, key) = name.rsplit_once('/').filter(|(_, key)| valid_key(key))
        .ok_or("Name a claim wait as claim:<room>/<key> or claim:<room>/<key>@<revision>.")?;
    let room = crate::rooms::resolve(state, chat, room)?;
    let claim = find(state, &room, key);
    let current = claim.map_or(0, |c| c.revision);
    match (seen, claim) {
        (Some(seen), _) if seen < current => Err(format!("Claim {key} changed since revision {seen} (now {current}); read it before waiting.")),
        (None, Some(claim)) if matches!(claim.status, Status::Done | Status::Released) => {
            let status = if claim.status == Status::Done { "done" } else { "released" };
            Err(format!("Claim {key} is already {status} at revision {current}; read it before waiting."))
        }
        _ => Ok(format!("claim:{}@{current}", topic_name(&room, key))),
    }
}
pub(crate) fn find<'a>(state: &'a State, room: &str, key: &str) -> Option<&'a Claim> {
    state.collective.claims.iter().find(|c| c.room == room && c.key == key)
}
/// Whether anyone can still act on the owner's claim: an active or queued
/// turn, or a persistent bot. Operators may release claims that fail this.
fn owner_active(state: &State, owner: &str) -> bool {
    state.chats.iter().any(|chat| chat.id == owner)
        && (crate::bots::continuation_enabled(state, owner)
            || state.runtime.runs.iter().any(|run| {
                run.chat_id.as_deref() == Some(owner)
                    && matches!(run.status.as_str(), "queued" | "running" | "waiting")
            }))
}
pub(crate) fn listed(state: &State, room: &str) -> Vec<Value> {
    state.collective.claims.iter().filter(|c| c.room == room && c.status != Status::Released)
        .map(|c| json!({"key":c.key,"owner":c.owner,"status":c.status,"blocked_on":c.blocked_on,
            "note":c.note,"revision":c.revision,"abandoned":!owner_active(state, &c.owner)}))
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
    json!({"name":"control_room_claim","description":"Advisory ownership of one deliverable or investigation in this task's thread, so peers do not duplicate it. acquire is atomic: it succeeds if the key is free or released, otherwise returns the current owner without changing it. update and release need the current revision and only work for the owner. status working, blocked (set blocked_on to the claim key you need) or done; done is your claim backed by evidence, not host verification. Claims appear in control_room_read with abandoned=true when their owner can no longer act; only the operator reclaims those. A bot can wait for a claim to change with wait topic claim:<room>/<key>@<revision>. Claims grant no authority.","annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false},
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

/// Applies one claim operation for `chat`. Returns the change, if any, so the
/// caller can notify waiters after its checkpoint decision.
pub(crate) fn call(state: &mut State, chat: &str, room: &str, args: &Value) -> Result<(Value, Option<u64>), &'static str> {
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
    let unchanged = |claim: &Claim, applied: bool, reason: &str| json!({"applied":applied,"reason":reason,"claim":claim});
    match call.action {
        Action::Acquire => {
            if let Some(claim) = current.as_ref().filter(|c| c.status != Status::Released) {
                let reason = if claim.owner == chat { "You already own this claim; use update with its revision." } else { "Another participant owns this claim." };
                return Ok((unchanged(claim, false, reason), None));
            }
        }
        Action::Update | Action::Release => {
            let claim = current.as_ref().filter(|c| c.status != Status::Released).ok_or("No live claim with that key. Acquire it first.")?;
            if claim.owner != chat {
                return Ok((unchanged(claim, false, "Only the owner can change this claim."), None));
            }
            if call.revision != Some(claim.revision) {
                return Ok((unchanged(claim, false, "Stale revision; read the claim and retry with its current revision."), None));
            }
        }
    }
    if index.is_none() {
        evict(state, room)?;
    }
    state.collective.claim_seq += 1;
    let revision = state.collective.claim_seq;
    let claim = Claim {
        room: room.to_owned(),
        key: call.key,
        owner: chat.to_owned(),
        status: if call.action == Action::Release { Status::Released } else { status },
        blocked_on: if call.action == Action::Release { None } else { blocked_on },
        note: if call.action == Action::Release { String::new() } else { note.to_owned() },
        revision,
    };
    let value = json!({"applied":true,"claim":claim});
    match state.collective.claims.iter().position(|c| c.room == room && c.key == claim.key) {
        Some(i) => state.collective.claims[i] = claim,
        None => state.collective.claims.push(claim),
    }
    Ok((value, Some(revision)))
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

/// Operator reclaim of a claim whose owner can no longer act. The revision
/// must match what the operator saw, so a revived owner's update is not lost.
pub(crate) fn operator_release(state: &mut State, room: &str, key: &str, revision: u64) -> Result<u64, &'static str> {
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
    Ok(revision)
}

/// `POST /room-claim-release`: the operator's explicit reclaim.
pub(crate) fn operator_action(state: &mut State, f: &std::collections::HashMap<String, String>) {
    let field = |name: &str| f.get(name).map(|value| value.trim()).unwrap_or_default();
    let (room, key) = (field("channel"), field("key"));
    let result = field("revision").parse::<u64>().map_err(|_| "Missing claim revision.")
        .and_then(|revision| if crate::rooms::valid(room) && valid_key(key) { Ok(revision) } else { Err("Unknown claim.") })
        .and_then(|revision| operator_release(state, room, key, revision));
    state.collective.notice = match result {
        Ok(revision) => {
            crate::bots::claim_changed(state, room, key, revision, None);
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
        if claim["abandoned"] == true {
            html.push_str(&format!("<form method='post' action='/room-claim-release'><input type='hidden' name='channel' value='{}'><input type='hidden' name='key' value='{}'><input type='hidden' name='revision' value='{}'><button>Release abandoned claim</button></form>",
                crate::escape(room), text("key"), claim["revision"]));
        }
        html.push_str("</li>");
    }
    html.push_str("</ul>");
    html
}

#[cfg(test)]
mod tests;
