//! Service task files outlive disposable containers. Names and ownership come
//! from the captured chat owner, never from agent-supplied paths or volume names.
use super::{Service, Storage, fail};
use control_core::resources::Owner;
use serde::Deserialize;
use serde_json::Value;
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
            let digest = hash
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
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
    pub(super) fn ensure(&self) -> io::Result<()> {
        let mut args = vec!["volume", "create", "--driver", "local"];
        let labels = self.labels().map(|(key, value)| format!("{key}={value}"));
        for label in &labels {
            args.extend(["--label", label]);
        }
        args.push(self.name());
        // Creating an existing name is idempotent; Docker does not change its
        // labels/options. Inspect it before mounting, including after retries.
        crate::engine::query(&args)?;
        let text = crate::engine::query(&["volume", "inspect", self.name()])?;
        let volumes: Vec<Inspection> = serde_json::from_str(&text).map_err(fail)?;
        if volumes.len() != 1 || !self.matches(&volumes[0]) {
            return Err(fail(
                "Service workspace does not match its durable chat ownership and local storage policy",
            ));
        }
        Ok(())
    }
    fn matches(&self, volume: &Inspection) -> bool {
        volume.name == self.name
            && volume.driver == "local"
            && volume.scope == "local"
            && volume.options.as_ref().is_none_or(HashMap::is_empty)
            && self.labels().iter().all(|(key, value)| {
                volume
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(*key))
                    .is_some_and(|observed| observed == value)
            })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Inspection {
    name: String,
    driver: String,
    scope: String,
    labels: Option<HashMap<String, String>>,
    options: Option<HashMap<String, String>>,
}

pub(super) fn valid_mounts(
    service: &Service,
    mounts: &[Value],
    tmpfs: &HashMap<String, String>,
) -> bool {
    let workspace = Workspace::of(service);
    let mut found = false;
    for mount in mounts {
        if mount["Type"] == "tmpfs"
            && tmpfs.contains_key(mount["Destination"].as_str().unwrap_or(""))
        {
            continue;
        }
        let Some(workspace) = &workspace else {
            return false;
        };
        if found
            || mount["Type"] != "volume"
            || mount["Name"] != workspace.name()
            || mount["Driver"] != "local"
            || mount["Destination"] != "/workspace"
            || mount["RW"] != true
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
pub(super) fn remove_unused(node: &str, keep: &std::collections::HashSet<String>, chat: Option<&str>) -> (usize, Vec<String>) {
    let node_label = format!("label=dev.control.node={node}");
    let listed = match crate::engine::query(&["volume", "ls", "--filter", "label=dev.control.purpose=service-workspace", "--filter", &node_label, "--format", "{{.Name}}\t{{.Label \"dev.control.chat\"}}"]) {
        Ok(text) => text,
        Err(error) => return (0, vec![format!("Could not list service workspaces: {error}")]),
    };
    let (mut removed, mut failures) = (0, Vec::new());
    for line in listed.lines() {
        let Some((name, owner)) = line.split_once('\t') else { continue };
        if !name.starts_with("control-service-workspace-") || keep.contains(owner) || chat.is_some_and(|chat| chat != owner) {
            continue;
        }
        match crate::engine::query(&["volume", "rm", name]) {
            Ok(_) => removed += 1,
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    (removed, failures)
}
