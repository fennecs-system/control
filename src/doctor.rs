//! Read-only local prerequisites: never reads credential contents or calls a model.
use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};

fn private(path: &Path, directory: bool) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        m.permissions().mode() & 0o077 == 0 && if directory { m.is_dir() } else { m.is_file() }
    })
}

pub fn run() -> io::Result<()> {
    let config = crate::config::directory().map_err(io::Error::other)?;
    let state = crate::config::state_directory().map_err(io::Error::other)?;
    let mut failures = 0;
    let mut check = |label: &str, passed: bool, fix: &str| {
        if passed {
            println!("ok  {label}");
        } else {
            println!("fix {label}: {fix}");
            failures += 1;
        }
    };
    check(
        "configuration",
        private(&config, true),
        "run control config init; directory mode must be 0700",
    );
    check(
        "state",
        private(&state, true),
        "run control config init; state directory mode must be 0700",
    );
    check(
        "node identity",
        crate::config::node_id().is_ok(),
        "run control config init or inspect the node-id file",
    );
    check(
        "embedded UI",
        crate::web_assets::ready(),
        "use a release binary built with the Rust/WASM assets",
    );
    let engine = crate::runtime::engine_ready();
    check(
        "Linux container engine",
        engine,
        "start a Linux Docker-compatible engine",
    );
    let image = crate::runtime::worker_image();
    check(
        "pinned worker image",
        image.is_some(),
        "set worker-image-id in the configuration directory to the Nix image digest",
    );
    if let (true, Some(image)) = (engine, image) {
        check(
            "worker image installed",
            crate::runtime::docker_query(&["image", "inspect", &image, "--format", "{{.Id}}"])
                .is_ok(),
            "load the Nix-built worker image into the engine",
        );
    }
    match crate::worker::credentials_ready() {
        Ok(()) => check("Codex credential metadata", true, ""),
        Err(reason) => check("Codex credential metadata", false, reason),
    }
    if config.join("discord.env").exists() || config.join("discord-access.json").exists() {
        // A placeholder policy is valid; a missing token leaves the optional bridge off.
        check(
            "Discord policy",
            crate::identity::Policy::load().is_ok(),
            "configure private discord-access.json with numeric IDs and valid roles",
        );
        if config.join("discord.env").exists() {
            check(
                "Discord credential metadata",
                private(&config.join("discord.env"), false),
                "discord.env must be a private regular file (mode 0600)",
            );
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(io::Error::other(
            "Resolve the reported prerequisites; no model was called",
        ))
    }
}
