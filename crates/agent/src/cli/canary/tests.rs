use super::*;
use control_isolation::scripted::{Answer, Call, Scripted};
use std::process::Command;

const CLI: User = User { uid: 501, gid: 20 };
const TARGET: &str = "/home/agent/.claude/.credentials.json";

fn turn() -> RunSpec {
    RunSpec {
        name: "control-claude-probe-1-canary".into(),
        image: "sha256:img".into(),
        user: Some(CLI),
        argv: vec!["claude".into(), "--print".into()],
        input: Input::Attached,
        ..RunSpec::default()
    }
    .secret(
        "claude/credentials",
        "/c/canary",
        TARGET,
        control_isolation::Access::ReadOnly,
    )
}
/// An engine where each exec exits as `allowed` says for its user and argv.
fn engine(allowed: fn(&ExecSpec) -> bool) -> Scripted<impl Fn(&Call) -> Answer + Sync> {
    Scripted::new(move |call: &Call| match call {
        Call::Exec(exec) => Answer::Run(Command::new(if allowed(exec) { "true" } else { "false" })),
        _ => Answer::Model,
    })
}
fn reads(exec: &ExecSpec) -> bool {
    exec.argv[2].starts_with("cat")
}
fn writes(exec: &ExecSpec) -> bool {
    exec.argv[2].starts_with(':')
}

#[test]
fn the_canary_runs_the_turn_container_with_a_stand_in_and_removes_it() {
    let engine = engine(|exec| exec.user == Some(CLI) && reads(exec));
    let results = run(&engine, turn(), TARGET, "control-canary-1", Write::ReadOnly).unwrap();
    let (lines, held) = report(&results);
    assert!(held, "{lines:#?}");
    assert_eq!(lines.len(), 8);
    assert!(lines.iter().all(|line| line.starts_with("PASS canary: ")));
    let calls = engine.calls();
    let Some(Call::RunDetached(spec)) = calls.first() else {
        panic!("{calls:?}")
    };
    // The turn's mounts and user, with a program that only waits, offline.
    assert_eq!(spec.secrets, turn().secrets);
    assert_eq!(spec.user, Some(CLI));
    assert_eq!(spec.entrypoint.as_deref(), Some("sleep"));
    assert_eq!(spec.argv, ["300"]);
    assert_eq!((spec.network, spec.input), (Network::None, Input::Closed));
    let readers: Vec<_> = calls
        .iter()
        .filter_map(|call| match call {
            Call::Exec(exec) => exec.user,
            _ => None,
        })
        .collect();
    assert_eq!(readers, [CLI, CLI, OTHER, OTHER, OTHER, CLI, OTHER, CLI]);
    assert!(matches!(calls.last(), Some(Call::Remove(_))));
    assert!(engine.model().containers.is_empty());
}

#[test]
fn another_uid_reaching_the_credential_fails_the_canary() {
    // Every uid may read, as under an open home on Docker Desktop and OrbStack.
    let engine = engine(reads);
    let (lines, held) = report(&run(&engine, turn(), TARGET, "c", Write::ReadOnly).unwrap());
    assert!(!held);
    assert_eq!(
        lines[2],
        format!("FAIL canary: other uid 65534 may read {TARGET}: allowed")
    );
    assert_eq!(
        lines[3],
        format!("FAIL canary: other uid 65534 may read /proc/1/root{TARGET}: allowed")
    );
}

#[test]
fn a_credential_in_the_environment_or_a_stopped_container_fails_the_canary() {
    let in_environment = engine(|exec| exec.user == Some(CLI));
    let (lines, held) = report(&run(&in_environment, turn(), TARGET, "c", Write::ReadOnly).unwrap());
    assert!(!held);
    assert_eq!(
        lines[1],
        "FAIL canary: CLI uid 501 may find the credential in PID 1's environment: allowed"
    );
    // Nothing can be read: the stand-in is not running, so no refusal counts.
    let stopped = engine(|_| false);
    let (lines, held) = report(&run(&stopped, turn(), TARGET, "c", Write::ReadOnly).unwrap());
    assert!(!held);
    assert!(lines[0].starts_with("FAIL canary: CLI uid 501 may read"));
}

#[test]
fn a_turn_without_a_user_is_refused_before_anything_starts() {
    let engine = engine(|_| true);
    let turn = RunSpec { user: None, ..turn() };
    assert!(run(&engine, turn, TARGET, "c", Write::ReadOnly).is_err());
    assert!(engine.calls().is_empty());
}

#[test]
fn a_refreshable_login_must_be_writable_by_the_cli_alone_and_a_read_only_one_by_nobody() {
    // The CLI may read and write; no other uid may do either.
    let refreshable = engine(|exec| exec.user == Some(CLI) && (reads(exec) || writes(exec)));
    let (lines, held) = report(&run(&refreshable, turn(), TARGET, "c", Write::Refreshable).unwrap());
    assert!(held, "{lines:#?}");
    assert_eq!(
        lines[5],
        format!("PASS canary: CLI uid 501 may write {TARGET}: allowed")
    );
    assert_eq!(
        lines[6],
        format!("PASS canary: other uid 65534 may write {TARGET}: refused")
    );
    // The same mount checked as read-only fails: the CLI could write it.
    let (lines, held) = report(&run(&refreshable, turn(), TARGET, "c", Write::ReadOnly).unwrap());
    assert!(!held);
    assert_eq!(
        lines[5],
        format!("FAIL canary: CLI uid 501 may write {TARGET}: allowed")
    );
    // A refreshable login the CLI cannot write fails as well.
    let read_only = engine(|exec| exec.user == Some(CLI) && reads(exec));
    let (lines, held) = report(&run(&read_only, turn(), TARGET, "c", Write::Refreshable).unwrap());
    assert!(!held);
    assert_eq!(
        lines[5],
        format!("FAIL canary: CLI uid 501 may write {TARGET}: refused")
    );
}
