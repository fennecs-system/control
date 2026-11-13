//! The one piece of Control's state the bridge still reads: which inbound DMs
//! Control has already accepted. Deliveries come from the outbox
//! (`crate::outbox`), never from runs in state. Unknown fields are skipped.
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Snapshot {
    pub discord_seen: HashSet<String>,
}

#[cfg(test)]
mod tests;
