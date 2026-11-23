//! A login the host located, checked and handed to one turn or text call.
//! Backends never locate credentials and never read them, and never decide
//! whether a login may be written: the host decides that ([`Write`]); a
//! backend only says where in its container the granted login goes and how
//! its CLI keeps a sign-in it refreshes ([`Keeps`]).
use control_isolation::{Access, RunSpec, User};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// How the login signs the CLI in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The CLI's own saved sign-in.
    Session,
    /// A provider API key in a private file.
    ApiKey,
    /// Google Cloud Application Default Credentials.
    CloudDefault,
}

/// Whether a turn may write its login back. Decided by the host from who
/// owns the login's location and what kind of login it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    /// Mounted read-only: the operator's own sign-in (`~/.codex`), every API
    /// key and every cloud credential.
    ReadOnly,
    /// A subscription sign-in in a folder Control owns: the CLI may replace
    /// it when it refreshes its token, so the refreshed token is kept.
    Refreshable,
}

/// How a CLI writes back a sign-in it refreshed, which decides what a
/// [`Write::Refreshable`] login mounts. A read-only login is always the file alone.
#[derive(Clone, Copy, Debug)]
pub enum Keeps {
    /// The CLI rewrites the file in place (truncate and write), which a bind
    /// mount of the file itself carries to the host.
    InPlace,
    /// The CLI replaces the file by renaming a temporary file over it and
    /// locks beside it, which a mounted file cannot take: the login's folder
    /// is mounted instead, and the CLI is pointed at it.
    Folder(&'static Folder),
}
/// Where a CLI that replaces its sign-in by rename is given its own folder.
#[derive(Debug)]
pub struct Folder {
    /// The folder's place in the container, beneath the CLI's private home.
    pub target: &'static str,
    /// The sign-in's file name, the same in the container and on the host.
    pub file: &'static str,
    /// The variable that moves only the sign-in to the folder, leaving the
    /// rest of the CLI's state on its in-memory home.
    pub variable: &'static str,
    pub names: Names,
    /// What may stay in the folder after a turn: the sign-in, its lock and
    /// the CLI's temporary copies while it writes. Everything else is
    /// removed ([`sweep`]).
    pub keep: &'static [Keep],
}
impl Folder {
    /// Where the sign-in file is in the container.
    pub fn credential(&self) -> String {
        format!("{}/{}", self.target, self.file)
    }
}
/// What [`Folder::variable`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Names {
    /// The folder (`CLAUDE_SECURESTORAGE_CONFIG_DIR`).
    Folder,
    /// The sign-in file in it (`GROK_AUTH_PATH`).
    File,
}
/// A name a login folder keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    Name(&'static str),
    /// A name with this prefix and suffix and something between them: a
    /// temporary copy or a lock the CLI names per process.
    Between(&'static str, &'static str),
}
impl Keep {
    fn keeps(self, name: &str) -> bool {
        match self {
            Self::Name(kept) => name == kept,
            Self::Between(prefix, suffix) => {
                name.len() > prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix)
            }
        }
    }
}
/// The entries of a login folder that `keep` does not name: what a sweep removes.
pub fn sweep_plan<'a>(entries: impl IntoIterator<Item = &'a str>, keep: &[Keep]) -> Vec<&'a str> {
    entries
        .into_iter()
        .filter(|name| !keep.iter().any(|kept| kept.keeps(name)))
        .collect()
}
/// Removes from `folder` everything [`sweep_plan`] names: sessions, caches
/// and settings a CLI (or an earlier login) left beside the sign-in. A
/// directory is removed whole; a symlink is removed, never followed.
pub fn sweep(folder: &Path, keep: &[Keep]) -> io::Result<()> {
    let names = fs::read_dir(folder)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    sweep_plan(names.iter().map(String::as_str), keep)
        .into_iter()
        .map(|name| folder.join(name))
        .try_for_each(|path| match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
            // Already gone: a sweep after another turn on the account removed it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        })
}

/// Metadata of the account's login file: never its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    form: Form,
    path: PathBuf,
    owner: User,
    write: Write,
}
impl Granted {
    /// The host's checked login, read-only: a private regular file owned by `owner`.
    pub fn located(form: Form, path: PathBuf, owner: User) -> Self {
        Self {
            form,
            path,
            owner,
            write: Write::ReadOnly,
        }
    }
    /// A subscription sign-in the CLI may write back. Only a sign-in can be
    /// refreshable, so no key or cloud credential is ever mounted writable.
    pub fn refreshable(path: PathBuf, owner: User) -> Self {
        Self {
            write: Write::Refreshable,
            ..Self::located(Form::Session, path, owner)
        }
    }
    pub fn form(&self) -> Form {
        self.form
    }
    pub fn write(&self) -> Write {
        self.write
    }
    /// The file's owner, whom the container runs as.
    pub fn owner(&self) -> User {
        self.owner
    }
    /// The same login with a stand-in file, for an offline probe that must
    /// mount no account.
    pub fn canary(&self, path: PathBuf) -> Self {
        Self { path, ..self.clone() }
    }
    /// `spec` with the login mounted read-only at `target` as secret `name`.
    pub fn mount(&self, spec: RunSpec, name: &str, target: &str) -> io::Result<RunSpec> {
        Ok(spec.secret(name, crate::cli::checked(&self.path)?, target, Access::ReadOnly))
    }
    /// `spec` with a sign-in mounted as secret `name`: read-only at `target`
    /// unless the host made it refreshable; then writable, as the file
    /// itself or as its folder, the way the CLI `keeps` it.
    pub fn mount_session(&self, spec: RunSpec, name: &str, target: &str, keeps: Keeps) -> io::Result<RunSpec> {
        match (self.write, keeps) {
            (Write::ReadOnly, _) => self.mount(spec, name, target),
            (Write::Refreshable, Keeps::InPlace) => {
                Ok(spec.secret(name, crate::cli::checked(&self.path)?, target, Access::ReadWrite))
            }
            (Write::Refreshable, Keeps::Folder(folder)) => {
                let source = self.folder(keeps).ok_or_else(|| {
                    io::Error::other("The sign-in file is not named as the CLI keeps it in its folder")
                })?;
                let value = match folder.names {
                    Names::Folder => folder.target.to_owned(),
                    Names::File => folder.credential(),
                };
                Ok(spec
                    .secret(name, crate::cli::checked(source)?, folder.target, Access::ReadWrite)
                    .env(folder.variable, &value))
            }
        }
    }
    /// Where the sign-in is in the container once [`Self::mount_session`]
    /// mounted it with `target` and `keeps`.
    pub fn session_target(&self, target: &str, keeps: Keeps) -> String {
        match (self.write, keeps) {
            (Write::Refreshable, Keeps::Folder(folder)) => folder.credential(),
            (Write::Refreshable, Keeps::InPlace) | (Write::ReadOnly, _) => target.to_owned(),
        }
    }
    /// The host folder a refreshable sign-in kept in a folder is mounted
    /// from, which a sweep tidies after the turn; `None` for every other login.
    pub fn folder(&self, keeps: Keeps) -> Option<&Path> {
        match (self.write, keeps) {
            (Write::Refreshable, Keeps::Folder(folder))
                if self.path.file_name().is_some_and(|name| name == folder.file) =>
            {
                self.path.parent()
            }
            (Write::Refreshable, Keeps::Folder(_) | Keeps::InPlace) | (Write::ReadOnly, _) => None,
        }
    }
    /// For engine paths that must be checked apart from the mount.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests;
