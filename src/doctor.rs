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
        "run ./target/release/control config init; directory mode must be 0700",
    );
    check(
        "state",
        private(&state, true),
        "run ./target/release/control config init; state directory mode must be 0700",
    );
    check(
        "execution limits",
        crate::scheduler::load_limits().is_ok(),
        "execution.json must be private valid JSON; max_parallel_runs must be 1–8",
    );
    check(
        "node identity",
        crate::config::node_id().is_ok(),
        "run ./target/release/control config init or inspect the node-id file",
    );
    check(
        "embedded UI",
        crate::web_assets::ready(),
        "use a release binary built with the Rust/WASM assets",
    );
    let engine = crate::engine::ready();
    check(
        "Linux container engine",
        engine,
        "start a Linux Docker-compatible engine",
    );
    let mut runtime = crate::runtime::Runtime::default();
    crate::providers::load_settings(&mut runtime)?;
    // Every backend with a pinned image is checked; the selected one must be complete.
    for backend in [crate::backend::Backend::Codex, crate::backend::Backend::Claude] {
        let image = backend.image();
        if image.is_none() && backend != runtime.backend {
            continue;
        }
        let name = backend.label();
        check(
            &format!("{name}: pinned image"),
            image.is_some(),
            match backend {
                crate::backend::Backend::Codex => {
                    "set worker-image-id in the configuration directory to the Nix image digest"
                }
                crate::backend::Backend::Claude => {
                    "set claude-image-id in the configuration directory to the Nix image digest"
                }
            },
        );
        if let (true, Some(image)) = (engine, &image) {
            let installed = crate::engine::query(&["image", "inspect", image, "--format", "{{.Id}}"]).is_ok();
            check(&format!("{name}: image installed"), installed, "load the Nix-built image into the engine");
            if installed {
                // Offline isolation probe with an empty canary credential: no account is mounted and no model is called.
                let probe = match backend {
                    crate::backend::Backend::Codex => crate::worker::offline_probe(image),
                    crate::backend::Backend::Claude => crate::config::node_id()
                        .map_err(io::Error::other)
                        .and_then(|node| crate::claude::probe(image, &node, &crate::config::state("claude-check").map_err(io::Error::other)?))
                        .map_err(|_| "the offline Claude tool-isolation probe failed; check the pinned image and container sandbox support"),
                };
                match probe {
                    Ok(()) => check(&format!("{name}: offline sandbox probe"), true, ""),
                    Err(reason) => check(&format!("{name}: offline sandbox probe"), false, reason),
                }
            }
        }
        let credentials = match backend {
            crate::backend::Backend::Codex => crate::worker::credentials_ready(),
            crate::backend::Backend::Claude => crate::claude::credentials_ready()
                .map_err(|_| "run ./target/release/control claude login; its private login stays outside the checkout"),
        };
        match credentials {
            Ok(()) => check(&format!("{name}: credential metadata"), true, ""),
            Err(reason) => check(&format!("{name}: credential metadata"), false, reason),
        }
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
