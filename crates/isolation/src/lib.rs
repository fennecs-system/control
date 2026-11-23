//! Containers as data and one interface. A [`RunSpec`] says what runs and
//! under which limits; an [`Isolation`] backend carries it out. Callers never
//! build engine argument vectors, so another backend can replace [`Docker`]
//! and a test can stand in for both.
//!
//! This layer reports what the host observes (a child's exit status); what the
//! program inside claims (its output stream, result files) is the caller's.
//! Every container drops all capabilities and gains no new privileges: those
//! are not options.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    process::{Child, Stdio},
};

mod docker;
/// Pinned engine argument vectors for launch-site tests.
#[cfg(any(test, feature = "testing"))]
pub mod golden;
mod observed;
#[cfg(any(test, feature = "testing"))]
pub mod scripted;
pub use docker::{Docker, bounded_output};
pub use observed::*;

/// A pinned image reference, as the engine names it (`sha256:…`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageId(String);
impl ImageId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl From<&str> for ImageId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}
impl From<String> for ImageId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// A container as the engine identifies it. Opaque here; a backend checks the
/// form of the IDs it reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContainerId(String);
impl ContainerId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl From<String> for ContainerId {
    fn from(id: String) -> Self {
        Self(id)
    }
}
impl From<&str> for ContainerId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}
/// A container named by its engine ID or by the name Control gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Id(&'a ContainerId),
    Name(&'a str),
}
/// Which containers a listing includes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum States {
    Running,
    All,
}
/// Which containers a listing selects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Select {
    Id(ContainerId),
    /// Exactly this name.
    Name(String),
    /// Every one of these labels.
    Labels(Vec<(String, String)>),
    Any,
}

/// Numeric owner of the container's processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    #[default]
    ReadOnly,
    ReadWrite,
}
/// A memory or filesystem size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Size {
    Bytes(u64),
    Mib(u64),
    Gib(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// A host path: absolute UTF-8 without commas or newlines.
    Bind(PathBuf),
    /// A named engine volume.
    Volume(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub source: Source,
    pub target: String,
    pub access: Access,
}
/// Credential material handed to the program, kept apart from ordinary mounts
/// so every place a secret enters a container is visible here. Today the
/// material is a host file; `name` says which secret it is, independent of
/// where it is stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMount {
    pub name: String,
    pub source: PathBuf,
    pub target: String,
    /// `ReadWrite` lets the program write it back (a CLI refreshing its token).
    pub access: Access,
}
/// Whether a tmpfs may hold executables; `Engine` keeps the engine's default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exec {
    #[default]
    Engine,
    Allow,
    Deny,
}
/// An in-memory filesystem; always `rw,nosuid`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tmpfs {
    pub target: String,
    pub size: Size,
    pub owner: Option<User>,
    pub exec: Exec,
    pub nodev: bool,
    /// Permission bits of the mount point.
    pub mode: Option<u32>,
}
impl Tmpfs {
    pub fn new(target: &str, size: Size) -> Self {
        Self {
            target: target.into(),
            size,
            owner: None,
            exec: Exec::Engine,
            nodev: false,
            mode: None,
        }
    }
    pub fn owned(self, owner: User) -> Self {
        Self {
            owner: Some(owner),
            ..self
        }
    }
}
/// Resource bounds; `None` leaves the engine default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub pids: Option<u64>,
    pub memory: Option<Size>,
    /// Memory plus swap; equal to `memory` forbids swapping.
    pub memory_swap: Option<Size>,
    pub cpus: Option<u64>,
}
/// Whether `/proc` keeps the engine's masked paths. `Unmasked` lets an inner
/// sandbox mount its own proc; no capability is granted with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Proc {
    #[default]
    Masked,
    Unmasked,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Network {
    #[default]
    Engine,
    None,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Logs {
    #[default]
    Engine,
    None,
    /// Kept by the engine, rotated at `max_size` across `max_files` files.
    Local {
        max_size: Size,
        max_files: u32,
    },
}
/// What the program's standard input is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    #[default]
    Closed,
    /// The caller's standard input.
    Attached,
    /// The caller's terminal, for an interactive sign-in.
    Terminal,
}

/// What runs as the container's first process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pid1 {
    #[default]
    Program,
    /// A minimal init that reaps orphans and forwards signals to the program.
    Init,
}

/// One container: what runs, as whom, with what, under which limits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    pub name: String,
    pub image: ImageId,
    /// Replaces the image's entrypoint; `argv` follows it.
    pub entrypoint: Option<String>,
    pub argv: Vec<String>,
    pub user: Option<User>,
    pub workdir: Option<String>,
    pub env: Vec<(String, String)>,
    pub labels: Vec<(String, String)>,
    pub mounts: Vec<Mount>,
    pub secrets: Vec<SecretMount>,
    pub tmpfs: Vec<Tmpfs>,
    /// `ReadOnly` makes the image's root filesystem immutable.
    pub root: Access,
    pub limits: Limits,
    pub seccomp: Option<PathBuf>,
    pub proc: Proc,
    pub network: Network,
    pub logs: Logs,
    pub input: Input,
    pub pid1: Pid1,
    /// A cap on the container's writable layer, where the engine supports one.
    pub disk: Option<Size>,
}
impl RunSpec {
    pub fn env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }
    pub fn label(mut self, name: &str, value: &str) -> Self {
        self.labels.push((name.into(), value.into()));
        self
    }
    pub fn bind(mut self, source: impl Into<PathBuf>, target: &str, access: Access) -> Self {
        self.mounts.push(Mount {
            source: Source::Bind(source.into()),
            target: target.into(),
            access,
        });
        self
    }
    pub fn secret(mut self, name: &str, source: impl Into<PathBuf>, target: &str, access: Access) -> Self {
        self.secrets.push(SecretMount {
            name: name.into(),
            source: source.into(),
            target: target.into(),
            access,
        });
        self
    }
    pub fn tmpfs(mut self, tmpfs: Tmpfs) -> Self {
        self.tmpfs.push(tmpfs);
        self
    }
    pub fn args<S: AsRef<str>>(mut self, words: impl IntoIterator<Item = S>) -> Self {
        self.argv.extend(words.into_iter().map(|word| word.as_ref().to_owned()));
        self
    }
}

/// Where one of a child's standard streams goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Null,
    Piped,
    Inherit,
}
impl Stream {
    pub fn stdio(self) -> Stdio {
        match self {
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
            Self::Inherit => Stdio::inherit(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Streams {
    pub stdin: Stream,
    pub stdout: Stream,
    pub stderr: Stream,
}
impl Streams {
    pub const fn new(stdin: Stream, stdout: Stream, stderr: Stream) -> Self {
        Self { stdin, stdout, stderr }
    }
    /// The terminal's own streams, for an interactive sign-in.
    pub const INHERIT: Self = Self::new(Stream::Inherit, Stream::Inherit, Stream::Inherit);
    pub fn apply(self, command: &mut std::process::Command) -> &mut std::process::Command {
        command
            .stdin(self.stdin.stdio())
            .stdout(self.stdout.stdio())
            .stderr(self.stderr.stdio())
    }
}

/// A program run inside a running container.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecSpec {
    pub container: ContainerId,
    pub user: Option<User>,
    pub workdir: Option<String>,
    pub env: Vec<(String, String)>,
    /// `Attached` gives the program the caller's standard input.
    pub input: Input,
    pub argv: Vec<String>,
}

/// The container operations Control performs. Implementations carry out a
/// [`RunSpec`] faithfully or fail; they never weaken it. Operations that run a
/// program (`spawn`, `exec`, `wait`, `logs`) return its process; the rest
/// return what the engine reports, as data.
pub trait Isolation: Sync {
    /// Whether the engine answers and runs Linux containers.
    fn ready(&self) -> bool;
    /// Whether the engine can enforce [`RunSpec::disk`].
    fn disk_quota(&self) -> io::Result<bool>;
    /// The labels of a local image; fails when the image is not present.
    fn image_labels(&self, image: &ImageId) -> io::Result<HashMap<String, String>>;
    /// Starts `spec` attached to this process; the container is removed when
    /// it exits. The child is the engine client, whose exit status is the
    /// program's.
    fn spawn(&self, spec: &RunSpec, streams: Streams) -> io::Result<Child>;
    /// Starts `spec` in the background and returns its ID.
    fn run_detached(&self, spec: &RunSpec) -> io::Result<ContainerId>;
    /// Creates `spec` without starting it; failures keep the engine's reason.
    fn create(&self, spec: &RunSpec) -> io::Result<ContainerId>;
    fn start(&self, id: &ContainerId) -> io::Result<()>;
    fn kill(&self, id: &ContainerId) -> io::Result<()>;
    /// Removes a container, stopping it first if it runs.
    fn remove(&self, target: Target<'_>) -> io::Result<()>;
    /// Inspects each target; fails if any is absent.
    fn inspect(&self, targets: &[Target<'_>]) -> io::Result<Vec<Observed>>;
    fn list(&self, states: States, select: &Select) -> io::Result<Vec<Listed>>;
    fn exec(&self, spec: &ExecSpec, streams: Streams) -> io::Result<Child>;
    /// A process that exits when the container does.
    fn wait(&self, id: &ContainerId) -> io::Result<Child>;
    /// A process printing the last `tail` lines of the container's output on
    /// its standard output.
    fn logs(&self, id: &ContainerId, tail: u32) -> io::Result<Child>;
    /// One sample per container, keyed by ID; the engine measures for about a second.
    fn stats(&self, ids: &[&ContainerId]) -> io::Result<HashMap<String, Stats>>;
    fn top(&self, id: &ContainerId) -> io::Result<Vec<Process>>;
    /// Creates a local volume with `labels`; an existing one is left unchanged.
    fn volume_create(&self, name: &str, labels: &[(&str, &str)]) -> io::Result<()>;
    fn volume_inspect(&self, name: &str) -> io::Result<Vec<ObservedVolume>>;
    fn volumes(&self, labels: &[(&str, &str)]) -> io::Result<Vec<ListedVolume>>;
    /// Removes a volume; the engine refuses one still mounted.
    fn volume_remove(&self, name: &str) -> io::Result<()>;
}

#[cfg(test)]
mod tests;
