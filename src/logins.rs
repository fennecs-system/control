//! Where each backend's login lives on this node, located and checked by the
//! host and handed to a turn as a [`Granted`] value. Metadata only: no
//! credential is ever read. There is one implicit account per backend, at
//! exactly the path its backend used to locate itself.
use crate::backend::Backend;
use control_agent::{Form, Granted};
use control_isolation::User;
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn owner(meta: &fs::Metadata) -> User {
    User {
        uid: meta.uid(),
        gid: meta.gid(),
    }
}

/// The backend's login directory under the configuration directory, which
/// `control ID login` creates.
pub(crate) fn directory(backend: Backend) -> io::Result<PathBuf> {
    Ok(crate::config::directory().map_err(fail)?.join(backend.id()))
}

/// The backend's login for a turn or text call.
pub(crate) fn locate(backend: Backend) -> io::Result<Granted> {
    match backend {
        Backend::Codex => codex().map_err(fail),
        Backend::Claude => file(
            backend,
            "CONTROL_CLAUDE_AUTH_FILE",
            ".credentials.json",
            "Claude",
            "claude",
        ),
        Backend::Grok => file(backend, "CONTROL_GROK_AUTH_FILE", "auth.json", "Grok", "grok"),
        Backend::Antigravity => antigravity(&directory(backend)?),
        Backend::Local => Err(fail("Local endpoints use no provider login")),
    }
}

/// Credential metadata only; the error is the operator's next step.
pub(crate) fn ready(backend: Backend) -> Result<(), String> {
    match backend.agent().login() {
        None => Ok(()),
        Some(login) => locate(backend)
            .map(drop)
            .map_err(|error| login.hint.map_or_else(|| error.to_string(), str::to_owned)),
    }
}

/// A CLI's own sign-in file: an explicit absolute path, or the file in the
/// backend's private login directory.
fn file(backend: Backend, variable: &str, name: &str, label: &str, command: &str) -> io::Result<Granted> {
    let path = if let Some(path) = std::env::var_os(variable) {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(fail(&format!("{label} credential path must be absolute")));
        }
        path
    } else {
        let directory = directory(backend)?;
        let meta = fs::symlink_metadata(&directory)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 {
            return Err(fail(&format!(
                "{label} login directory must be private and not a symlink"
            )));
        }
        directory.join(name)
    };
    let meta = fs::symlink_metadata(&path).map_err(|_| {
        fail(&format!(
            "Run control {command} login to sign in inside the Nix container"
        ))
    })?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
        return Err(fail(&format!(
            "{label} credential must be a private regular file owned by a non-root user"
        )));
    }
    Ok(Granted::located(Form::Session, path, owner(&meta)))
}

/// Codex's `auth.json`. Tests resolve only an explicit credential path or one
/// under the test node directory, never the operator's `CODEX_HOME`/`~/.codex`.
#[cfg(test)]
pub(crate) fn codex_path() -> Result<PathBuf, &'static str> {
    Ok(std::env::var_os("CONTROL_CODEX_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::test_root().join("codex/auth.json")))
}
#[cfg(not(test))]
pub(crate) fn codex_path() -> Result<PathBuf, &'static str> {
    std::env::var_os("CONTROL_CODEX_AUTH_FILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CODEX_HOME").map(|p| PathBuf::from(p).join("auth.json")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex/auth.json")))
        .ok_or("Cannot locate Codex auth.json. Sign in with codex login on the host.")
}
fn codex() -> Result<Granted, &'static str> {
    codex_validate(&codex_path()?)
}
/// Inspect metadata only. Only Codex inside the worker consumes credential contents.
pub(crate) fn codex_validate(path: &Path) -> Result<Granted, &'static str> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "Codex auth.json is missing. Run codex login on the host with file credential storage.")?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.uid() == 0 {
        return Err(
            "Codex auth.json must be a private regular file owned by a non-root user (mode 0600). Symlink credentials are not accepted.",
        );
    }
    let path = fs::canonicalize(path).map_err(|_| "Cannot resolve Codex auth.json.")?;
    control_codex::mount_path(&path)?;
    Ok(Granted::located(Form::Session, path, owner(&metadata)))
}

fn private_owned(meta: &fs::Metadata) -> bool {
    meta.mode() & 0o077 == 0 && meta.uid() != 0
}
/// How Antigravity signs in, chosen by which private file the operator
/// created: an API key wins over Application Default Credentials, which win
/// over the Google sign-in `control antigravity login` saved.
pub(crate) fn antigravity(directory: &Path) -> io::Result<Granted> {
    let meta =
        fs::symlink_metadata(directory).map_err(|_| fail("Run control antigravity login, or save a Gemini API key"))?;
    if !meta.is_dir() || !private_owned(&meta) {
        return Err(fail(
            "Antigravity login directory must be private, owned by a non-root user and not a symlink",
        ));
    }
    let file = |name: &str| {
        let path = directory.join(name);
        fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file() && private_owned(m))
            .map(|meta| (path, owner(&meta)))
    };
    [
        ("gemini-api-key", Form::ApiKey),
        ("adc.json", Form::CloudDefault),
        (control_antigravity::SAVED, Form::Session),
    ]
    .into_iter()
    .find_map(|(name, form)| file(name).map(|(path, owner)| Granted::located(form, path, owner)))
    .ok_or_else(|| {
        fail("No Antigravity login: run control antigravity login, or save a private gemini-api-key or adc.json")
    })
}

/// Runs a backend's operator subcommand with its account directory and login.
pub(crate) fn command(
    backend: Backend,
    args: &[String],
    run: fn(control_agent::Command<'_>) -> io::Result<()>,
) -> io::Result<()> {
    run(control_agent::Command {
        args,
        node: &crate::agent_host::NODE,
        isolation: crate::engine::isolation(),
        image: backend.image(),
        directory: directory(backend)?,
        login: locate(backend),
    })
}

#[cfg(test)]
mod tests;
