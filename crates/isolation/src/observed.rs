//! What the engine reports about containers, images and volumes: facts the
//! host observes, parsed into types at the backend boundary.
use crate::ContainerId;
use std::collections::BTreeMap;

/// One container as inspected. Fields the engine may leave empty are
/// `Option`s; callers decide what an absent value means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub id: ContainerId,
    /// The engine's name, with its leading `/`.
    pub name: String,
    /// The ID of the image it runs.
    pub image: String,
    /// The image reference it was created from.
    pub image_ref: String,
    pub labels: BTreeMap<String, String>,
    pub status: String,
    pub running: bool,
    pub exit_code: i64,
    pub user: String,
    pub entrypoint: Option<Vec<String>>,
    pub command: Option<Vec<String>>,
    pub network: String,
    pub read_only_root: bool,
    pub privileged: bool,
    pub pid_mode: String,
    pub restart: String,
    pub cap_drop: Option<Vec<String>>,
    pub security_opt: Option<Vec<String>>,
    pub memory: u64,
    pub nano_cpus: u64,
    pub pids_limit: Option<u64>,
    pub tmpfs: BTreeMap<String, String>,
    pub storage_opt: Option<BTreeMap<String, String>>,
    pub mounts: Vec<ObservedMount>,
    /// Container ports and whether each is published on the host; `None`
    /// when the engine reports no port table.
    pub ports: Option<BTreeMap<String, bool>>,
}
impl Observed {
    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(String::as_str)
    }
    /// The mount at `destination`, if exactly that path is mounted.
    pub fn mount(&self, destination: &str) -> Option<&ObservedMount> {
        self.mounts.iter().find(|mount| mount.destination == destination)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountKind {
    Bind,
    Volume,
    Tmpfs,
    Other(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedMount {
    pub kind: MountKind,
    /// The volume name, for a volume.
    pub name: Option<String>,
    pub driver: Option<String>,
    pub source: String,
    pub destination: String,
    pub writable: bool,
}
/// One container from a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub id: ContainerId,
    pub name: String,
    pub labels: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedVolume {
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub labels: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
}
/// One volume from a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedVolume {
    pub name: String,
    pub labels: BTreeMap<String, String>,
}
/// A point-in-time resource sample for one container, in the engine's units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    pub cpu: String,
    pub memory: String,
    pub pids: String,
}
/// One process inside a container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: String,
    pub user: String,
    pub command: String,
}
