
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
