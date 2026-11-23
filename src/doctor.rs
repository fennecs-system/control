//! Read-only local prerequisites: never reads credential contents or calls a model.
use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};

fn private(path: &Path, directory: bool) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|m| m.permissions().mode() & 0o077 == 0 && if directory { m.is_dir() } else { m.is_file() })
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
    // Read the way offline readers see it: snapshot plus patch log, no lock,
    // nothing repaired. Errors name no contents.
    let kib = |name: &str| fs::metadata(state.join(name)).map_or(0, |m| m.len().div_ceil(1024));
    let checkpoint = crate::config::read_state::<crate::State>(isize::MAX as usize);
    check(
        &format!(
            "state checkpoint (state.json {} KiB, {} {} KiB)",
            kib("state.json"),
            crate::config::STATE_LOG,
            kib(crate::config::STATE_LOG)
        ),
        checkpoint.is_ok(),
        &format!(
            "state.json and state.log must parse ({:?}); stop Control, keep both files and inspect them with jq (docs/HISTORY.md)",
            checkpoint.as_ref().err().map_or(io::ErrorKind::Other, io::Error::kind)
        ),
    );
    check(
        "execution limits",
        crate::scheduler::load_limits().is_ok(),
        "execution.json must be private valid JSON; max_parallel_runs must be 1–32",
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
    let isolation = crate::engine::isolation();
    let engine = isolation.ready();
    check(
        "Linux container engine",
        engine,
        "start a Linux Docker-compatible engine",
    );
    match crate::build_environment::image() {
        Ok(Some(image)) if engine => {
            let readiness = crate::config::source_root()
                .and_then(|source| crate::build_environment::check(isolation, &image, &source));
            match readiness {
                Ok(()) => check("declared build environment", true, ""),
                Err(error) => check("declared build environment", false, &error.to_string()),
            }
        }
        Ok(_) => {}
        Err(error) => check("declared build environment", false, &error.to_string()),
    }
    match crate::solver::image() {
        Ok(Some(image)) if engine => match crate::solver::installed(isolation, &image) {
            Ok(solvers) => check(&format!("solver image ({solvers})"), true, ""),
            Err(error) => check("solver image", false, &error.to_string()),
        },
        Ok(_) => {}
        Err(error) => check("solver image", false, &error.to_string()),
    }
    let mut runtime = crate::runtime::Runtime::default();
    crate::providers::load_settings(&mut runtime)?;
    // Every backend with a pinned image is checked; the selected one must be complete.
    for backend in crate::backend::Backend::hosted() {
        let agent = backend.agent();
        let image = backend.image();
        if image.is_none() && backend != runtime.backend {
            continue;
        }
        let name = backend.label();
        check(&format!("{name}: pinned image"), image.is_some(), agent.image_hint());
        if let (true, Some(image)) = (engine, &image) {
            let installed = isolation.image_labels(&image.as_str().into()).is_ok();
            check(
                &format!("{name}: image installed"),
                installed,
                "load the Nix-built image into the engine",
            );
            if installed {
                // Offline isolation probe with an empty canary credential: no account is mounted and no model is called.
                let probe = agent.probe(&crate::agent_host::NODE, isolation, image);
                match probe {
                    Ok(()) => check(&format!("{name}: offline sandbox probe"), true, ""),
                    Err(reason) => check(&format!("{name}: offline sandbox probe"), false, &reason),
                }
            }
        }
        let credentials = crate::logins::ready(backend);
        match credentials {
            Ok(()) => check(&format!("{name}: credential metadata"), true, ""),
            Err(reason) => check(&format!("{name}: credential metadata"), false, &reason),
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
