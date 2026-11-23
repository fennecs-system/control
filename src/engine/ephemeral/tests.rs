use super::*;
use std::cell::Cell;

fn spec(input: &[u8], deadline: Duration, stderr_bytes: Option<usize>) -> Spec {
    Spec { run: RunSpec::default(), input: input.to_vec(), deadline, stdout_bytes: 8, stderr_bytes, unavailable: "Could not start the test tool" }
}
fn shell(script: &str) -> impl FnOnce(Streams) -> io::Result<Child> {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", script]);
    started(command)
}
fn started(mut command: std::process::Command) -> impl FnOnce(Streams) -> io::Result<Child> {
    move |streams| streams.apply(&mut command).spawn()
}

#[test]
fn output_readers_keep_the_head_and_the_tail() {
    let input = vec![b'x'; 300_000];
    assert_eq!(head(&input[..], 1000).map(|(kept, truncated)| (kept.len(), truncated)).unwrap(), (1000, true));
    assert_eq!(head(&b"run 0\nok"[..], 65536).unwrap(), (b"run 0\nok".to_vec(), false));
    let mut text = vec![b'a'; 200_000];
    text.extend_from_slice(b"END");
    let kept = tail(&text[..], 1000).unwrap();
    assert!(kept.len() >= 1000 && kept.len() <= 2000 && kept.ends_with(b"END"));
}

#[test]
fn feeds_stdin_and_keeps_bounded_output_and_diagnostics() {
    let removed = Cell::new(false);
    let finished = run_with(shell("cat; echo diagnostics >&2; exit 3"), spec(b"model text", Duration::from_secs(10), Some(64)), || removed.set(true)).unwrap();
    assert_eq!(finished.status.and_then(|status| status.code()), Some(3));
    assert!(finished.delivered);
    assert_eq!((finished.stdout.as_slice(), finished.stdout_truncated), (&b"model te"[..], true));
    assert_eq!(finished.stderr, b"diagnostics\n");
    assert!(!removed.get(), "a finished container is not force-removed");
}

#[test]
fn discards_diagnostics_when_not_kept() {
    let finished = run_with(shell("cat >/dev/null; echo hidden >&2; printf ok"), spec(b"x", Duration::from_secs(10), None), || ()).unwrap();
    assert!(finished.status.is_some_and(|status| status.success()));
    assert_eq!((finished.stdout.as_slice(), finished.stderr.as_slice()), (&b"ok"[..], &b""[..]));
}

#[test]
fn removes_and_kills_a_container_past_its_deadline() {
    let removed = Cell::new(false);
    let finished = run_with(shell("exec sleep 30"), spec(b"", Duration::from_millis(200), Some(64)), || removed.set(true)).unwrap();
    assert!(finished.status.is_none());
    assert!(removed.get());
    assert!(finished.elapsed < Duration::from_secs(10), "{:?}", finished.elapsed);
}

#[test]
fn a_spawn_failure_names_the_tool() {
    let error = run_with(started(std::process::Command::new("/nonexistent/control-engine")), spec(b"", Duration::from_secs(1), None), || ()).unwrap_err();
    assert_eq!(error.to_string(), "Could not start the test tool");
}
