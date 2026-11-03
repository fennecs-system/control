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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_snapshot_reads_only_accepted_message_ids() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "runtime": {"runs": [{"id": "r", "result": {"journal": 3}, "attempts": [{"activity": []}]}]},
            "discord_seen": ["1", "2", "1"],
        })).unwrap();
        assert_eq!(snapshot.discord_seen.len(), 2);
    }
}
