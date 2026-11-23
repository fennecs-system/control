//! The credential canary of `control ID check`: a fake credential mounted
//! exactly as a turn mounts the real one, in the turn's own container with a
//! stand-in program, and read from inside as the CLI's uid and as another.
//! No model, no network, no real credential. What is asserted is data
//! ([`assertions`]); [`run`] is the thin layer that starts the container and
//! execs each reader.
use control_isolation::{ExecSpec, Input, Isolation, Network, RunSpec, Stream, Streams, Target, User};
use std::io;

/// A uid no CLI runs as: what any other process in the container would be.
pub const OTHER: User = User { uid: 65534, gid: 65534 };

/// Who reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// The uid the vendor CLI runs as.
    Cli(User),
    /// Any other uid, started in the container by the engine.
    Other(User),
}
impl Reader {
    fn user(self) -> User {
        match self {
            Self::Cli(user) | Self::Other(user) => user,
        }
    }
    fn name(self) -> String {
        match self {
            Self::Cli(user) => format!("CLI uid {}", user.uid),
            Self::Other(user) => format!("other uid {}", user.uid),
        }
    }
}
/// What a turn needs the reader's attempt to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Succeeds,
    Fails,
}
/// One line of the canary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assertion {
    pub reader: Reader,
    /// What is attempted, for the line.
    pub attempt: String,
    pub argv: Vec<String>,
    pub expect: Expect,
}
fn read(reader: Reader, path: &str, expect: Expect) -> Assertion {
    Assertion {
        reader,
        attempt: format!("read {path}"),
        argv: ["sh", "-c", "cat -- \"$1\" >/dev/null 2>&1", "sh", path]
            .map(str::to_owned)
            .to_vec(),
        expect,
    }
}
/// What must hold for a credential mounted at `target` whose contents are
/// `canary`. The CLI's own read comes first and last: it proves the
/// container ran throughout, so a refusal in between is the kernel's.
pub fn assertions(cli: User, target: &str, canary: &str) -> Vec<Assertion> {
    let (own, other) = (Reader::Cli(cli), Reader::Other(OTHER));
    vec![
        read(own, target, Expect::Succeeds),
        Assertion {
            reader: own,
            attempt: "find the credential in PID 1's environment".into(),
            argv: [
                "sh",
                "-c",
                "case \"$(tr '\\0' '\\n' </proc/1/environ)\" in *\"$1\"*) exit 0;; esac; exit 1",
                "sh",
                canary,
            ]
            .map(str::to_owned)
            .to_vec(),
            expect: Expect::Fails,
        },
        read(other, target, Expect::Fails),
        read(other, &format!("/proc/1/root{target}"), Expect::Fails),
        read(other, "/proc/1/environ", Expect::Fails),
        read(own, target, Expect::Succeeds),
    ]
}
/// One line per assertion, and whether every one held.
pub fn report(results: &[(Assertion, bool)]) -> (Vec<String>, bool) {
    let lines = results
        .iter()
        .map(|(assertion, succeeded)| {
            let held = *succeeded == (assertion.expect == Expect::Succeeds);
            let outcome = if *succeeded { "allowed" } else { "refused" };
            format!(
                "{} canary: {} may {}: {outcome}",
                if held { "PASS" } else { "FAIL" },
                assertion.reader.name(),
                assertion.attempt
            )
        })
        .collect();
    (
        lines,
        results.iter().all(|(a, ok)| *ok == (a.expect == Expect::Succeeds)),
    )
}
/// The turn's container with a program that only waits, and no network:
/// the mounts, user, home and environment stay the turn's.
pub fn stand_in(turn: RunSpec) -> RunSpec {
    RunSpec {
        entrypoint: Some("sleep".into()),
        argv: vec!["300".into()],
        network: Network::None,
        input: Input::Closed,
        ..turn
    }
}
/// Starts the stand-in of `turn`, runs every assertion inside it and removes
/// it. A reader whose exec cannot start counts as refused.
pub fn run(isolation: &dyn Isolation, turn: RunSpec, target: &str, canary: &str) -> io::Result<Vec<(Assertion, bool)>> {
    let cli = turn
        .user
        .ok_or_else(|| io::Error::other("the turn container names no user"))?;
    let id = isolation.run_detached(&stand_in(turn))?;
    let quiet = Streams::new(Stream::Null, Stream::Null, Stream::Null);
    let results = assertions(cli, target, canary)
        .into_iter()
        .map(|assertion| {
            let exec = ExecSpec {
                container: id.clone(),
                user: Some(assertion.reader.user()),
                argv: assertion.argv.clone(),
                ..ExecSpec::default()
            };
            let succeeded = isolation
                .exec(&exec, quiet)
                .and_then(|mut child| child.wait())
                .is_ok_and(|status| status.success());
            (assertion, succeeded)
        })
        .collect();
    isolation.remove(Target::Id(&id))?;
    Ok(results)
}

#[cfg(test)]
mod tests;
