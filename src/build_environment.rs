//! Readiness of an immutable, operator-installed offline build environment.
//! Dependency metadata is checked outside State; no model or container is run.
use std::{collections::BTreeMap, io, path::Path};

const REBUILD: &str = "rebuild the declared build image from the current lockfiles (nix run .#setup -- --build-environment)";

pub(crate) fn image() -> io::Result<Option<String>> {
    let value = match std::env::var("CONTROL_BUILD_IMAGE") {
        Ok(value) => Some(value),
        Err(_) => {
            let path = crate::config::directory()
                .map_err(io::Error::other)?
                .join("build-image-id");
            match crate::config::read_regular(&path, 256, true) {
                Ok(bytes) => Some(String::from_utf8(bytes).map_err(io::Error::other)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        }
    };
    value
        .map(|value| {
            let value = value.trim().to_owned();
            if !crate::runtime::pinned_image(&value) {
                return Err(io::Error::other(
                    "Build environment must be pinned by digest",
                ));
            }
            Ok(value)
        })
        .transpose()
}

pub(crate) fn check(image: &str, workspace: &Path) -> io::Result<()> {
    if !crate::runtime::pinned_image(image) {
        return Err(io::Error::other(
            "Build environment must be pinned by digest",
        ));
    }
    let metadata = crate::engine::query(&[
        "image",
        "inspect",
        image,
        "--format",
        "{{index .Config.Labels \"dev.control.build-locks\"}}",
    ])?;
    let locks = parse(&metadata)?;
    matches(&locks, |path| {
        match crate::artifacts::safe_read(workspace, path, 2 * 1024 * 1024) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    })
}

fn parse(text: &str) -> io::Result<BTreeMap<String, String>> {
    let locks: BTreeMap<String, String> = serde_json::from_str(text.trim()).map_err(|_| {
        io::Error::other(format!(
            "Build environment lacks dependency metadata; {REBUILD}"
        ))
    })?;
    if locks.is_empty()
        || locks.len() > 64
        || locks.iter().any(|(path, hash)| {
            path.len() > 1024
                || Path::new(path)
                    .file_name()
                    .is_none_or(|name| name != "Cargo.lock")
                || !Path::new(path)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                || hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
    {
        return Err(io::Error::other(
            "Invalid build environment dependency metadata",
        ));
    }
    Ok(locks)
}

fn matches(
    locks: &BTreeMap<String, String>,
    mut read: impl FnMut(&str) -> io::Result<Option<Vec<u8>>>,
) -> io::Result<()> {
    for (path, expected) in locks {
        // An environment can include dependencies for several workspaces. A
        // task need not contain every one; present lockfiles must match exactly.
        let Some(bytes) = read(path)? else {
            continue;
        };
        let actual = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if &actual != expected {
            return Err(io::Error::other(format!(
                "Build environment is stale for {path}; {REBUILD}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
