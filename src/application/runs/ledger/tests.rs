use super::*;
use crate::identity::{Action, Actor};

#[test]
fn a_shared_hash_costs_a_scan_never_an_answer() {
    let run = |id: &str| {
        crate::application::runs::Submission {
            objective: id.into(),
            kind: control_context::agent_policy::RunKind::Task,
            source: "test".into(),
            actor: Actor::LocalOperator,
            action: Action::Task,
        }
        .into_run(id, 1, Default::default())
    };
    let mut runs: Runs = ["a", "b"].into_iter().map(run).collect();
    // Pretend "b" and "c" share "a"'s hash: their lookups land on "a".
    runs.index.insert(key("b"), 0);
    runs.push(run("c"));
    runs.index.insert(key("c"), 0);
    assert_eq!(["a", "b", "c"].map(|id| runs.position(id)), [Some(0), Some(1), Some(2)]);
    assert_eq!(key("run-1"), key("run-1"));
    assert_ne!(key("run-1"), key("run-2"));
}

#[test]
fn the_indexed_prefix_and_the_scanned_tail_agree_through_rollbacks() {
    let run = |index: usize| {
        crate::application::runs::Submission {
            objective: "work".into(),
            kind: control_context::agent_policy::RunKind::Task,
            source: "test".into(),
            actor: Actor::LocalOperator,
            action: Action::Task,
        }
        .into_run(format!("run-{index}"), 1, Default::default())
    };
    let mut runs = Runs::default();
    let expect = |runs: &Runs, len: usize| {
        for index in 0..40 {
            let found = runs.position(&format!("run-{index}"));
            assert_eq!(found, (index < len).then_some(index), "run-{index} with {len} runs");
        }
        assert!(runs.indexed <= runs.len() && runs.len() - runs.indexed <= TAIL);
        assert_eq!(runs.index.len(), runs.indexed);
    };
    for index in 0..30 {
        runs.push(run(index));
        expect(&runs, index + 1);
    }
    runs.truncate(26);
    expect(&runs, 26);
    runs.truncate(5);
    expect(&runs, 5);
    runs.pop();
    expect(&runs, 4);
    runs.extend((4..20).map(run));
    expect(&runs, 20);
    while runs.pop().is_some() {}
    expect(&runs, 0);
}
