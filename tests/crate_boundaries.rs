//! Dependency direction among workspace crates: shared formats live in leaf
//! crates that clients and the server both use; only the binary composes the
//! clients. See docs/CRATE-BOUNDARIES.md.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Each client-side crate, agent backend crate (and the root) with the only
/// crates that may depend on it. Only the root links a backend, so no backend
/// depends on another, on a client or on the root.
const UPPER: [(&str, &[&str]); 9] = [
    ("control", &[]),
    ("control-client", &["control", "control-tui", "control-discord"]),
    ("control-tui", &["control"]),
    ("control-discord", &["control"]),
    ("control-codex", &["control"]),
    ("control-claude", &["control"]),
    ("control-grok", &["control"]),
    ("control-antigravity", &["control"]),
    (
        "control-agent",
        &[
            "control",
            "control-codex",
            "control-claude",
            "control-grok",
            "control-antigravity",
        ],
    ),
];

/// `package -> dependency` edges that break the direction, in any dependency kind.
fn violations(graph: &[(String, Vec<String>)]) -> Vec<(&str, &str)> {
    graph
        .iter()
        .flat_map(|(package, dependencies)| {
            dependencies
                .iter()
                .map(move |dependency| (package.as_str(), dependency.as_str()))
        })
        .filter(|(package, dependency)| package != dependency)
        .filter(|(package, dependency)| {
            UPPER
                .iter()
                .any(|(upper, allowed)| upper == dependency && !allowed.contains(package))
        })
        .collect()
}

/// `cargo metadata` for the root workspace, without resolving dependencies.
fn metadata() -> Value {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON")
}

/// Workspace packages and the workspace crates each one names.
fn workspace() -> Vec<(String, Vec<String>)> {
    let metadata = metadata();
    let names = |value: &Value| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let members = names(&metadata["packages"]);
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|package| {
            let dependencies = names(&package["dependencies"])
                .into_iter()
                .filter(|name| members.contains(name))
                .collect();
            (package["name"].as_str().unwrap_or_default().to_owned(), dependencies)
        })
        .collect()
}

#[test]
fn leaf_crates_and_clients_do_not_depend_upwards() {
    let graph = workspace();
    [
        "control",
        "control-client",
        "control-delivery",
        "control-diff",
        "control-url-names",
        "control-ingress",
        "control-isolation",
        "control-agent",
        "control-codex",
        "control-claude",
        "control-grok",
        "control-antigravity",
    ]
    .iter()
    .for_each(|name| {
        assert!(
            graph.iter().any(|(package, _)| package == name),
            "{name} is not a workspace member"
        )
    });
    assert_eq!(violations(&graph), Vec::<(&str, &str)>::new());
}

#[test]
fn the_rule_rejects_a_leaf_depending_on_a_client() {
    let graph = [
        ("control-delivery".to_owned(), vec!["control-client".to_owned()]),
        (
            "control-client".to_owned(),
            vec!["control".to_owned(), "control-delivery".to_owned()],
        ),
        (
            "control".to_owned(),
            vec!["control".to_owned(), "control-client".to_owned()],
        ),
    ];
    assert_eq!(
        violations(&graph),
        [("control-delivery", "control-client"), ("control-client", "control")]
    );
}

#[test]
fn every_workspace_package_is_on_edition_2024() {
    let metadata = metadata();
    let other_editions = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|package| package["edition"] != "2024")
        .map(|package| format!("{} ({})", package["name"], package["edition"]))
        .collect::<Vec<_>>();
    assert_eq!(other_editions, Vec::<String>::new());
}

/// Standalone manifests are their own workspaces and cannot inherit the edition.
#[test]
fn standalone_manifests_are_on_edition_2024() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tools = std::fs::read_dir(root.join("tools")).expect("tools directory is readable");
    let other_editions = tools
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("Cargo.toml")))
        .chain([root.join("web/Cargo.toml")])
        .filter(|manifest| manifest.is_file())
        .filter(|manifest| {
            let text = std::fs::read_to_string(manifest).expect("manifest is readable");
            !text.lines().any(|line| line.trim() == r#"edition = "2024""#)
        })
        .collect::<Vec<_>>();
    assert_eq!(other_editions, Vec::<PathBuf>::new());
}

#[test]
fn the_rule_rejects_a_backend_depending_on_another_backend_or_the_root() {
    let graph = [
        (
            "control-grok".to_owned(),
            vec!["control-agent".to_owned(), "control-claude".to_owned()],
        ),
        ("control-codex".to_owned(), vec!["control".to_owned()]),
        ("control-agent".to_owned(), vec!["control-client".to_owned()]),
        (
            "control".to_owned(),
            vec!["control-agent".to_owned(), "control-grok".to_owned()],
        ),
    ];
    assert_eq!(
        violations(&graph),
        [
            ("control-grok", "control-claude"),
            ("control-codex", "control"),
            ("control-agent", "control-client")
        ]
    );
}
