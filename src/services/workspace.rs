//! Service task files outlive disposable containers. Names and ownership come
//! from the captured chat owner, never from agent-supplied paths or volume names.
use super::{Service, Storage, fail};
use control_core::resources::Owner;
use control_isolation::{Isolation, MountKind, ObservedMount, ObservedVolume};
use std::{collections::HashMap, io};

pub(super) struct Workspace<'a> {
    owner: &'a Owner,
    name: String,
}
impl<'a> Workspace<'a> {
    pub(super) fn of(service: &'a Service) -> Option<Self> {
        matches!(service.storage, Storage::Workspace { .. }).then(|| {
            let owner = &service.resource.owner;
            let hash = ring::digest::digest(
                &ring::digest::SHA256,
                format!("{}\0{}", owner.node.as_str(), owner.chat.as_str()).as_bytes(),
            );
            let digest = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>();
            Self {
                owner,
                name: format!("control-service-workspace-{digest}"),
            }
        })
    }
    pub(super) fn name(&self) -> &str {
        &self.name
    }
    fn labels(&self) -> [(&'static str, &str); 3] {
        [
            ("dev.control.purpose", "service-workspace"),
            ("dev.control.node", self.owner.node.as_str()),
            ("dev.control.chat", self.owner.chat.as_str()),
        ]
    }
    pub(super) fn ensure(&self, isolation: &dyn Isolation) -> io::Result<()> {
        // Creating an existing name is idempotent; Docker does not change its
        // labels/options. Inspect it before mounting, including after retries.
        isolation.volume_create(self.name(), &self.labels())?;
        let volumes = isolation.volume_inspect(self.name())?;
        if volumes.len() != 1 || !self.matches(&volumes[0]) {
            return Err(fail(
                "Service workspace does not match its durable chat ownership and local storage policy",
            ));
        }
        Ok(())
    }
    fn matches(&self, volume: &ObservedVolume) -> bool {
        volume.name == self.name
            && volume.driver == "local"
            && volume.scope == "local"
            && volume.options.is_empty()
            && self
                .labels()
                .iter()
                .all(|(key, value)| volume.labels.get(*key).is_some_and(|observed| observed == value))
    }
}

pub(super) fn valid_mounts(service: &Service, mounts: &[ObservedMount], tmpfs: &HashMap<String, String>) -> bool {
    let workspace = Workspace::of(service);
    let mut found = false;
    for mount in mounts {
        if mount.kind == MountKind::Tmpfs && tmpfs.contains_key(&mount.destination) {
            continue;
        }
        let Some(workspace) = &workspace else {
            return false;
        };
        if found
            || mount.kind != MountKind::Volume
            || mount.name.as_deref() != Some(workspace.name())
            || mount.driver.as_deref() != Some("local")
            || mount.destination != "/workspace"
            || !mount.writable
        {
            return false;
        }
        found = true;
    }
    found == workspace.is_some()
}

#[cfg(test)]
mod tests;

/// Removes this node's service workspace volumes for chats not in `keep`, or
/// only `chat`'s when given. Ownership comes from the volume's Control labels;
/// the engine refuses a volume still mounted by any container.
pub(super) fn remove_unused(
    isolation: &dyn Isolation,
    node: &str,
    keep: &std::collections::HashSet<String>,
    chat: Option<&str>,
) -> (usize, Vec<String>) {
    let listed = match isolation.volumes(&[("dev.control.purpose", "service-workspace"), ("dev.control.node", node)]) {
        Ok(listed) => listed,
        Err(error) => return (0, vec![format!("Could not list service workspaces: {error}")]),
    };
    let (mut removed, mut failures) = (0, Vec::new());
    for volume in &listed {
        let (name, owner) = (
            volume.name.as_str(),
            volume.labels.get("dev.control.chat").map_or("", String::as_str),
        );
        if !name.starts_with("control-service-workspace-")
            || keep.contains(owner)
            || chat.is_some_and(|chat| chat != owner)
        {
            continue;
        }
        match isolation.volume_remove(name) {
            Ok(_) => removed += 1,
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    (removed, failures)
}
