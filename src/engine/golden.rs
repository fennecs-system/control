//! Pinned engine argument vectors. Each launch site's tests compare what it
//! builds with the exact vector it produced before the isolation interface, so
//! a flag that changes, appears or disappears fails a test. Golden text uses
//! `{name}` placeholders for values a test cannot fix (paths, ids, uids).
//!
//! Every site now builds a [`control_isolation::RunSpec`] or `ExecSpec` and gets
//! the backend's one order and spelling of options, while the sites used to
//! differ in both, so most compare with [`pin_equivalent`] against the vector
//! they produced before; those whose bytes did not change keep [`pin`].
use std::collections::{BTreeMap, BTreeSet};

/// The engine arguments the Docker backend runs `spec` with.
pub(crate) fn run(spec: &control_isolation::RunSpec) -> Vec<String> {
    control_isolation::Docker::run_arguments(spec).unwrap()
}
/// The golden vector with each `{key}` replaced by its value.
pub(crate) fn fill(golden: &[&str], values: &[(&str, &str)]) -> Vec<String> {
    golden
        .iter()
        .map(|word| {
            values.iter().fold((*word).to_owned(), |word, (key, value)| {
                word.replace(&format!("{{{key}}}"), value)
            })
        })
        .collect()
}
/// Byte-for-byte equality.
#[track_caller]
pub(crate) fn pin(actual: &[String], golden: &[String]) {
    assert_eq!(actual, golden, "engine arguments changed:\n{}", literal(actual));
}
/// Equality as the engine CLI parses the vector: the same subcommand and image
/// or container and the same program arguments, byte for byte; the same
/// options, whatever their order or `--flag=value` / `--flag value` spelling.
/// Repeated options compare as sets except `--env`, whose order decides
/// duplicates. Tmpfs option lists compare as sets. Unknown flags fail, and
/// `--restart=no`, the engine default, equals its absence.
#[track_caller]
pub(crate) fn pin_equivalent(actual: &[String], golden: &[String]) {
    assert_eq!(
        parse(actual),
        parse(golden),
        "engine arguments changed:\n{}",
        literal(actual)
    );
}
fn literal(actual: &[String]) -> String {
    actual
        .iter()
        .map(|word| format!("{word:?},"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, PartialEq)]
struct Parsed {
    subcommand: String,
    switches: BTreeSet<String>,
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
    rest: Vec<String>,
}
const SWITCHES: [&str; 6] = ["rm", "interactive", "tty", "read-only", "init", "detach"];
const LISTS: [&str; 8] = [
    "label",
    "tmpfs",
    "mount",
    "env",
    "security-opt",
    "log-opt",
    "storage-opt",
    "cap-drop",
];
const SCALARS: [&str; 12] = [
    "name",
    "user",
    "workdir",
    "network",
    "pids-limit",
    "memory",
    "memory-swap",
    "cpus",
    "log-driver",
    "entrypoint",
    "restart",
    "pull",
];

fn parse(words: &[String]) -> Parsed {
    let (subcommand, mut words) = words.split_first().expect("empty engine arguments");
    let mut parsed = Parsed {
        subcommand: subcommand.clone(),
        switches: BTreeSet::new(),
        scalars: BTreeMap::new(),
        lists: BTreeMap::new(),
        rest: Vec::new(),
    };
    let mut switches = BTreeMap::new();
    while let Some((word, tail)) = words.split_first() {
        words = tail;
        let flag = match word.as_str() {
            "-i" => "--interactive",
            "-t" => "--tty",
            word if word.starts_with("--") => word,
            _ => {
                parsed.rest = std::iter::once(word.clone()).chain(words.iter().cloned()).collect();
                break;
            }
        };
        let (name, inline) = match flag[2..].split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (&flag[2..], None),
        };
        if SWITCHES.contains(&name) {
            switches.insert(name.to_owned(), inline.as_deref() != Some("false"));
            continue;
        }
        // Docker's default restart policy, stated by `create` and implied elsewhere.
        if flag == "--restart=no" {
            continue;
        }
        assert!(
            LISTS.contains(&name) || SCALARS.contains(&name),
            "unknown engine flag {flag}"
        );
        let value = match inline {
            Some(value) => value,
            None => {
                let (value, tail) = words.split_first().unwrap_or_else(|| panic!("{flag} has no value"));
                words = tail;
                value.clone()
            }
        };
        if SCALARS.contains(&name) {
            parsed.scalars.insert(name.to_owned(), value);
        } else {
            let value = if name == "tmpfs" { tmpfs(&value) } else { value };
            parsed.lists.entry(name.to_owned()).or_default().push(value);
        }
    }
    parsed.switches = switches
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name)
        .collect();
    for (name, values) in &mut parsed.lists {
        if name != "env" {
            values.sort();
        }
    }
    parsed
}
fn tmpfs(value: &str) -> String {
    let (path, options) = value.split_once(':').unwrap_or((value, ""));
    let mut options: Vec<&str> = options.split(',').collect();
    options.sort();
    format!("{path}:{}", options.join(","))
}

#[test]
fn equivalence_ignores_spelling_and_order_but_not_values() {
    let words = |text: &str| text.split(' ').map(str::to_owned).collect::<Vec<_>>();
    let golden = words(
        "run --rm -i --name n --label b=2 --label a=1 --memory=2g --tmpfs /tmp:rw,nosuid,size=1m --read-only --read-only=false --env A=1 --env B=2 img sh -c x",
    );
    pin_equivalent(
        &words(
            "run --interactive --rm --name=n --label a=1 --label=b=2 --memory 2g --tmpfs /tmp:nosuid,rw,size=1m --env A=1 --env B=2 img sh -c x",
        ),
        &golden,
    );
    for changed in [
        "run --rm --interactive --name n --label b=2 --label a=1 --memory=4g --tmpfs /tmp:rw,nosuid,size=1m --env A=1 --env B=2 img sh -c x",
        "run --rm --interactive --name n --label b=2 --label a=1 --memory=2g --tmpfs /tmp:rw,nosuid,size=1m --read-only --env A=1 --env B=2 img sh -c x",
        "run --rm --interactive --name n --label b=2 --label a=1 --memory=2g --tmpfs /tmp:rw,nosuid,size=1m --env B=2 --env A=1 img sh -c x",
        "run --rm --interactive --name n --label b=2 --label a=1 --memory=2g --tmpfs /tmp:rw,nosuid,size=1m --env A=1 --env B=2 img sh -c y",
        "run --rm --name n --label b=2 --label a=1 --memory=2g --tmpfs /tmp:rw,nosuid,size=1m --env A=1 --env B=2 img sh -c x",
    ] {
        assert_ne!(parse(&words(changed)), parse(&golden), "{changed}");
    }
}
