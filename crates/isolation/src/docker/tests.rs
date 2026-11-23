use super::*;
use crate::{Limits, User};

fn spec() -> RunSpec {
    RunSpec {
        name: "n".into(),
        image: "i".into(),
        root: Access::ReadWrite,
        ..RunSpec::default()
    }
}

#[test]
fn each_lifetime_has_its_own_verb() {
    assert_eq!(
        Docker::detached_arguments(&RunSpec {
            pid1: Pid1::Init,
            ..spec()
        })
        .unwrap()[..4],
        ["run", "--detach", "--pull=never", "--init"]
    );
    let created = Docker::create_arguments(&RunSpec {
        disk: Some(Size::Mib(512)),
        limits: Limits {
            memory: Some(Size::Bytes(1024)),
            ..Limits::default()
        },
        ..spec()
    })
    .unwrap();
    assert_eq!(created[..3], ["create", "--pull=never", "--restart=no"]);
    assert!(created.windows(2).any(|pair| pair == ["--storage-opt", "size=512M"]));
    assert!(created.contains(&"--memory=1024".to_owned()) && !created.contains(&"--rm".to_owned()));
}

#[test]
fn exec_renders_user_environment_and_input() {
    let exec = ExecSpec {
        container: "c1".into(),
        user: Some(User { uid: 7, gid: 8 }),
        workdir: Some("/workspace".into()),
        env: vec![("A".into(), "b".into())],
        input: Input::Attached,
        argv: vec!["tar".into(), "-x".into()],
    };
    assert_eq!(
        Docker::exec_arguments(&exec),
        [
            "exec",
            "--interactive",
            "--user",
            "7:8",
            "--env",
            "A=b",
            "--workdir",
            "/workspace",
            "c1",
            "tar",
            "-x"
        ]
    );
    assert_eq!(
        Docker::exec_arguments(&ExecSpec {
            container: "c1".into(),
            argv: vec!["true".into()],
            ..ExecSpec::default()
        }),
        ["exec", "c1", "true"]
    );
}

#[test]
fn listings_filter_by_state_and_selection() {
    let format = ["--format", LISTING];
    assert_eq!(
        list_arguments(States::Running, &Select::Id("abc".into())),
        [&["ps", "--no-trunc", "--filter", "id=abc"][..], &format].concat()
    );
    assert_eq!(
        list_arguments(States::All, &Select::Name("control-x".into())),
        [
            &["ps", "--all", "--no-trunc", "--filter", "name=^/control-x$"][..],
            &format
        ]
        .concat()
    );
    assert_eq!(
        list_arguments(
            States::All,
            &Select::Labels(vec![("a".into(), "1".into()), ("b".into(), "2".into())])
        ),
        [
            &[
                "ps",
                "--all",
                "--no-trunc",
                "--filter",
                "label=a=1",
                "--filter",
                "label=b=2"
            ][..],
            &format
        ]
        .concat()
    );
    assert_eq!(
        list_arguments(States::All, &Select::Any),
        [&["ps", "--all", "--no-trunc"][..], &format].concat()
    );
}

#[test]
fn engine_requests_drain_pipes_and_reject_partial_results() {
    let output = bounded_output(Command::new("head").args(["-c", "131072", "/dev/zero"]), REQUEST, false).unwrap();
    assert_eq!(output.len(), 131072);
    assert!(
        bounded_output(
            Command::new("head").args(["-c", "1048577", "/dev/zero"]),
            REQUEST,
            false
        )
        .is_err()
    );
}
#[test]
fn engine_request_deadline_does_not_wait_for_process_exit() {
    let started = Instant::now();
    assert!(bounded_output(Command::new("sleep").arg("5"), Duration::from_millis(30), false).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}
#[test]
fn engine_diagnostics_are_opt_in_and_bounded() {
    let missing = "/control-qa-missing-storage-probe";
    let error = bounded_output(Command::new("ls").arg(missing), Duration::from_secs(2), true).unwrap_err();
    assert!(error.to_string().contains(missing));
    let redacted = bounded_output(Command::new("ls").arg(missing), Duration::from_secs(2), false).unwrap_err();
    assert_eq!(redacted.to_string(), "Container engine request failed");
    let error = bounded_output(
        Command::new("sh").args(["-c", "head -c 65536 /dev/zero >&2; exit 1"]),
        Duration::from_secs(2),
        true,
    )
    .unwrap_err();
    assert!(error.to_string().len() <= 4096 + "Container engine request failed: ".len());
}
