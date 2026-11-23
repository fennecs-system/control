//! A login the host located, checked and handed to one turn or text call.
//! Backends never locate credentials and never read them: they only decide
//! where in their container the granted file is mounted.
use control_isolation::{Access, RunSpec, User};
use std::{
    io,
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

/// Metadata of the account's login file: never its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    form: Form,
    path: PathBuf,
    owner: User,
}
impl Granted {
    /// The host's checked login: a private regular file owned by `owner`.
    pub fn located(form: Form, path: PathBuf, owner: User) -> Self {
        Self { form, path, owner }
    }
    pub fn form(&self) -> Form {
        self.form
    }
    /// The file's owner, whom the container runs as.
    pub fn owner(&self) -> User {
        self.owner
    }
    /// The same owner with a stand-in file, for an offline probe that must
    /// mount no account.
    pub fn canary(&self, path: PathBuf) -> Self {
        Self {
            form: self.form,
            path,
            owner: self.owner,
        }
    }
    /// `spec` with the login mounted read-only at `target` as secret `name`.
    pub fn mount(&self, spec: RunSpec, name: &str, target: &str) -> io::Result<RunSpec> {
        Ok(spec.secret(name, crate::cli::checked(&self.path)?, target, Access::ReadOnly))
    }
    /// For engine paths that must be checked apart from the mount.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
