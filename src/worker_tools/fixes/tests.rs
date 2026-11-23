use super::*;

/// Source files whose error strings reach a model through each tool family.
/// Turn-handoff messages must carry their own fix; the other families may
/// rely on their default fix, which dispatch appends.
const TURN: &[(&str, &str)] = &[
    ("turn_context/tools.rs", include_str!("../../turn_context/tools.rs")),
    (
        "turn_mailbox.rs",
        include_str!("../../../crates/context/src/turn_mailbox.rs"),
    ),
    (
        "working_state.rs",
        include_str!("../../../crates/context/src/working_state.rs"),
    ),
];
const FAMILIES: &[(&str, &str, &str)] = &[
    ("control_service_exec", "services.rs", include_str!("../../services.rs")),
    (
        "control_service_exec",
        "services/commands.rs",
        include_str!("../../services/commands.rs"),
    ),
    (
        "control_service_copy",
        "services/copy.rs",
        include_str!("../../services/copy.rs"),
    ),
    (
        "control_service_start",
        "services/page.rs",
        include_str!("../../services/page.rs"),
    ),
    (
        "control_service_status",
        "services/tools.rs",
        include_str!("../../services/tools.rs"),
    ),
    (
        "control_service_exec",
        "services/workspace.rs",
        include_str!("../../services/workspace.rs"),
    ),
    ("control_room_post", "rooms.rs", include_str!("../../rooms.rs")),
    (
        "control_room_read",
        "rooms/address.rs",
        include_str!("../../rooms/address.rs"),
    ),
    (
        "control_room_claim",
        "rooms/claims.rs",
        include_str!("../../rooms/claims.rs"),
    ),
    (
        "control_room_notifications",
        "rooms/notifications.rs",
        include_str!("../../rooms/notifications.rs"),
    ),
    ("control_watch_start", "watches.rs", include_str!("../../watches.rs")),
    (
        "control_watch_events",
        "watches/delivery.rs",
        include_str!("../../watches/delivery.rs"),
    ),
    (
        "control_watch_events",
        "watches/ops.rs",
        include_str!("../../watches/ops.rs"),
    ),
    (
        "control_watch_start",
        "watches/policy.rs",
        include_str!("../../watches/policy.rs"),
    ),
    (
        "control_watch_status",
        "watches/tools.rs",
        include_str!("../../watches/tools.rs"),
    ),
    (
        "control_agent_message",
        "agent_message.rs",
        include_str!("../../agent_message.rs"),
    ),
    (
        "control_agent_message",
        "application/messages.rs",
        include_str!("../../application/messages.rs"),
    ),
];

/// String literals passed to an error constructor (`err(`, `fail(`, `Err(`,
/// `Error::other(`, `ok_or(`), optionally through `format!(`, before any
/// inline test module.
fn rejections(source: &str) -> Vec<String> {
    let source = source.split("#[cfg(test)]\nmod tests {").next().unwrap();
    let mut found = Vec::new();
    for marker in ["err(", "fail(", "Err(", "Error::other(", "ok_or("] {
        for (at, _) in source.match_indices(marker) {
            // `Err(` also ends identifiers such as `map_err(`; skip those.
            if marker == "err(" && source[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let mut rest = source[at + marker.len()..].trim_start();
            if let Some(after) = rest.strip_prefix("format!(") {
                rest = after.trim_start();
            }
            let Some(body) = rest.strip_prefix('"') else { continue };
            let mut literal = String::new();
            let mut chars = body.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            literal.push(escaped);
                        }
                    }
                    '"' => break,
                    c => literal.push(c),
                }
            }
            found.push(literal);
        }
    }
    found
}

#[test]
fn every_turn_handoff_rejection_names_a_callable_fix() {
    let mut checked = 0;
    let mut missing = Vec::new();
    for (file, source) in TURN {
        for literal in rejections(source) {
            let expanded = literal.replace("{OPERATOR_FIX}", OPERATOR);
            if !names_fix(&expanded) {
                missing.push(format!("{file}: {literal:?}"));
            }
            checked += 1;
        }
    }
    assert!(
        missing.is_empty(),
        "rejections without a callable fix:\n{}",
        missing.join("\n")
    );
    eprintln!("turn rejections checked: {checked}");
    assert!(checked >= 30, "the scanner stopped finding rejections ({checked})");
}

#[test]
fn every_service_room_watch_and_message_rejection_reaches_the_model_with_a_fix() {
    let mut own = 0;
    let mut total = 0;
    for (tool, file, source) in FAMILIES {
        assert!(covered(tool), "{tool} has no default fix");
        for literal in rejections(source) {
            own += usize::from(names_fix(&literal));
            total += 1;
            let shown = actionable(tool, &literal);
            assert!(names_fix(&shown), "{file}: {literal:?} -> {shown:?}");
            assert!(
                shown.starts_with(literal.trim_end()),
                "the host's own message is kept first"
            );
        }
    }
    eprintln!("service/room/watch/message rejections checked: {total}, {own} with their own fix");
    assert!(total >= 100, "the scanner stopped finding rejections ({total})");
    assert!(own > 0);
}

#[test]
fn messages_with_their_own_fix_are_unchanged_and_others_get_the_family_default() {
    let own =
        "Service s is busy. Call control_service_exec_status {\"service_id\":\"s\",\"execution_id\":\"e\"} and retry.";
    assert_eq!(actionable("control_service_exec", own), own);
    let operator = "Not permitted; ask the operator via control_design_ask";
    assert_eq!(actionable("control_room_post", operator), operator);
    let bare = actionable("control_watch_stop", "Unknown watch");
    assert!(
        bare.starts_with("Unknown watch. Next: call control_watch_status {}"),
        "{bare}"
    );
    // Naming a tool without its arguments is not a concrete call.
    assert!(!names_fix("Poll control_service_exec_status until it finishes"));
    assert!(names_fix(
        "then retry control_turn_handoff {\"disposition\":{\"kind\":\"done\"}}"
    ));
    // Tools outside the contract are left alone.
    assert!(!covered("control_web_search"));
    assert_eq!(actionable("control_web_search", "x"), "x");
}
