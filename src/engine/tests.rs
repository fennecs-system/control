use control_isolation::{Docker, ExecSpec, Input, User};

/// The exec form `engine::exec` and `engine::exec_input` produced, as an `ExecSpec`.
fn exec(input: Input, workdir: Option<&str>, env: &[(&str, &str)]) -> Vec<String> {
    Docker::exec_arguments(&ExecSpec {
        container: "c1".into(),
        user: Some(User { uid: 7, gid: 8 }),
        workdir: workdir.map(str::to_owned),
        env: env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        input,
        argv: vec!["tar".into(), "-x".into()],
    })
}

#[test]
fn only_input_exec_forwards_stdin() {
    let plain = exec(Input::Closed, Some("/workspace"), &[("A", "b")]);
    let input = exec(Input::Attached, Some("/workspace"), &[("A", "b")]);
    assert!(!plain.contains(&"--interactive".to_owned()));
    let expected = [
        "--user",
        "7:8",
        "--env",
        "A=b",
        "--workdir",
        "/workspace",
        "c1",
        "tar",
        "-x",
    ];
    assert!(plain.ends_with(&expected.map(str::to_owned)));
    assert!(input.ends_with(&expected.map(str::to_owned)));
    assert_eq!(
        input.iter().position(|a| a == "--interactive"),
        input.iter().position(|a| a == "exec").map(|i| i + 1)
    );
}

#[test]
fn exec_vectors_are_pinned() {
    use super::golden::{fill, pin};
    pin(
        &exec(Input::Closed, Some("/workspace"), &[("A", "b")]),
        &fill(EXEC, &[]),
    );
    pin(&exec(Input::Attached, None, &[]), &fill(EXEC_INPUT, &[]));
}
const EXEC: &[&str] = &[
    "exec",
    "--user",
    "7:8",
    "--env",
    "A=b",
    "--workdir",
    "/workspace",
    "c1",
    "tar",
    "-x",
];
const EXEC_INPUT: &[&str] = &["exec", "--interactive", "--user", "7:8", "c1", "tar", "-x"];
