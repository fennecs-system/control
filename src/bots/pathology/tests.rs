use super::*;

const LIMITS: Limits = Limits { churn: 3, repeat: 3, poll: 3, burn: 1_000 };

fn scan<'a>(commands: &[(&'a str, &'a str, &'a str, bool)], tokens: u64, score: u64) -> Scan<'a> {
    Scan { commands: commands.to_vec(), tokens, score }
}

#[test]
fn building_commands_are_writes_builds_and_edits_never_status_reads() {
    for command in ["cat > /workspace/bot.js <<'EOF'\nconsole.log(1)\nEOF", "npm install mineflayer", "cargo build --release", "echo hi | tee notes.md",
        "sed -i 's/a/b/' run.sh", "chmod +x run.sh", "git apply fix.patch", "printf 'x' >> log.txt", "env FOO=1 make all"] {
        assert!(builds(command), "{command}");
    }
    for command in ["ps aux | grep bot", "tail -n 50 /tmp/bot.log", "cat state.json", "curl -s localhost:3000/status", "./run.sh 2>&1", "node bot.js > /dev/null 2>&1", "ls"] {
        assert!(!builds(command), "{command}");
    }
}

#[test]
fn commands_differing_only_in_numbers_or_spacing_are_one_command() {
    assert_eq!(normal("goto  12 64 -3"), normal("goto 13 70 -3"));
    assert_ne!(normal("goto 12"), normal("mine 12"));
    assert!(normal(&"x".repeat(4000)).len() <= COMMAND);
}

#[test]
fn history_before_subscription_is_not_reported() {
    let commands = [("m1", "e1", "place 1", true), ("m1", "e2", "place 2", true), ("m1", "e3", "place 3", true)];
    let (snapshot, found) = fold(None, &scan(&commands, 50_000, 0), false, LIMITS);
    assert!(found.loops.is_empty() && found.burn.is_none() && found.churn.is_none());
    assert_eq!(snapshot.loops.len(), 1, "the current loop is remembered, not reported");
    assert_eq!(snapshot.tokens, 50_000);
    let (again, found) = fold(Some(&snapshot), &scan(&commands, 50_000, 0), false, LIMITS);
    assert!(found.loops.is_empty() && again == snapshot, "an unchanged team reports nothing");
}

#[test]
fn a_repetition_loop_is_reported_once_until_it_clears() {
    let (start, _) = fold(None, &scan(&[], 0, 0), false, LIMITS);
    let two = [("m1", "e1", "dig 10 64 3", true), ("m1", "e2", "dig 11 64 3", true)];
    let (snapshot, found) = fold(Some(&start), &scan(&two, 0, 0), false, LIMITS);
    assert!(found.loops.is_empty(), "below the threshold");
    let three = [two[0], two[1], ("m1", "e3", "dig 12 64 3", true), ("m2", "e4", "dig 12 64 3", true)];
    let (snapshot, found) = fold(Some(&snapshot), &scan(&three, 0, 0), false, LIMITS);
    assert_eq!(found.loops, [Loop { chat: "m1".into(), command: "dig 12 64 3".into(), failures: 3 }], "per member; another member's failure is not counted");
    let four = [three[0], three[1], three[2], three[3], ("m1", "e5", "dig 13 64 3", true)];
    let (snapshot, found) = fold(Some(&snapshot), &scan(&four, 0, 0), false, LIMITS);
    assert!(found.loops.is_empty(), "an ongoing loop is not repeated");
    // Successes do not count; once the failures age out of the window it clears.
    let cleared = [("m1", "e6", "dig 13 64 3", false)];
    let (snapshot, _) = fold(Some(&snapshot), &scan(&cleared, 0, 0), false, LIMITS);
    assert!(snapshot.loops.is_empty());
    let (_, found) = fold(Some(&snapshot), &scan(&three, 0, 0), false, LIMITS);
    assert_eq!(found.loops.len(), 1, "a new loop after clearing is a new fact");
}

#[test]
fn polling_is_reported_per_member_once() {
    let (start, _) = fold(None, &scan(&[], 0, 0), false, LIMITS);
    let reads = [("m1", "e1", "tail -n 20 /tmp/bot.log", false), ("m1", "e2", "ps aux", false), ("m1", "e3", "cat status.json", false), ("m2", "e4", "ls", false)];
    let (snapshot, found) = fold(Some(&start), &scan(&reads, 0, 0), false, LIMITS);
    assert_eq!(found.polling, [Polling { chat: "m1".into(), commands: 3 }]);
    let more = [reads[0], reads[1], reads[2], reads[3], ("m1", "e5", "tail /tmp/bot.log", false)];
    let (_, found) = fold(Some(&snapshot), &scan(&more, 0, 0), false, LIMITS);
    assert!(found.polling.is_empty());
}

#[test]
fn harness_churn_counts_building_commands_until_the_team_advances() {
    let (start, _) = fold(None, &scan(&[], 0, 0), false, LIMITS);
    let first = [("m1", "e1", "cat > lib.js <<EOF\nx\nEOF", false), ("m2", "e2", "npm install", false)];
    let (snapshot, found) = fold(Some(&start), &scan(&first, 0, 0), false, LIMITS);
    assert!(found.churn.is_none() && snapshot.churn == 2);
    // Evicted commands stay counted; a new one crosses the threshold.
    let later = [("m1", "e3", "sed -i 's/a/b/' lib.js", false)];
    let (snapshot, found) = fold(Some(&snapshot), &scan(&later, 0, 0), false, LIMITS);
    assert_eq!(found.churn, Some(Churn { commands: 3, members: vec!["m1".into()] }));
    let more = [later[0], ("m1", "e4", "tee x.js", false)];
    let (snapshot, found) = fold(Some(&snapshot), &scan(&more, 0, 0), false, LIMITS);
    assert!(found.churn.is_none(), "reported once per window");
    // A raised score is progress: the window restarts.
    let (snapshot, _) = fold(Some(&snapshot), &scan(&more, 0, 100), false, LIMITS);
    assert!(snapshot.churn == 0 && !snapshot.churned);
    // So is a claim advance.
    let (snapshot, found) = fold(Some(&snapshot), &scan(&[("m1", "e5", "make", false), ("m1", "e6", "make", false), ("m1", "e7", "make", false)], 0, 100), true, LIMITS);
    assert!(found.churn.is_none() && snapshot.churn == 0);
}

#[test]
fn cost_burn_is_tokens_since_the_last_advance() {
    let (start, _) = fold(None, &scan(&[], 10_000, 5), false, LIMITS);
    let (snapshot, found) = fold(Some(&start), &scan(&[], 10_900, 5), false, LIMITS);
    assert!(found.burn.is_none());
    let (snapshot, found) = fold(Some(&snapshot), &scan(&[], 11_000, 5), false, LIMITS);
    assert_eq!(found.burn, Some(Burn { tokens: 1_000 }));
    let (snapshot, found) = fold(Some(&snapshot), &scan(&[], 15_000, 5), false, LIMITS);
    assert!(found.burn.is_none() && snapshot.burned, "reported once");
    let (snapshot, _) = fold(Some(&snapshot), &scan(&[], 15_000, 9), false, LIMITS);
    assert!(!snapshot.burned && snapshot.tokens == 15_000, "a raised score resets the baseline");
    // Archived runs can lower the total; the baseline follows.
    let (snapshot, _) = fold(Some(&snapshot), &scan(&[], 2_000, 9), false, LIMITS);
    assert_eq!(snapshot.tokens, 2_000);
}

#[test]
fn a_score_rises_when_any_field_increases_over_the_previous_sample() {
    let sample = |unix: u64, value: f64| crate::scores::Sample { unix, trigger: crate::scores::Trigger::Schedule, values: [("blocks".to_owned(), value)].into() };
    let samples = [sample(10, 1.0), sample(20, 3.0), sample(30, 3.0), sample(40, 2.0)].into_iter().collect();
    assert_eq!(raised(&samples), 20);
    assert_eq!(raised(&std::collections::VecDeque::new()), 0);
}
