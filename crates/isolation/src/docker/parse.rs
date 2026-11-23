//! Docker's output as typed values. Pure: text in, values out, so every
//! format is tested without an engine.
use crate::{ContainerId, Listed, ListedVolume, MountKind, Observed, ObservedMount, ObservedVolume, Process, Stats};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    io,
};

fn invalid(what: &str) -> io::Error {
    io::Error::other(format!("Invalid container engine {what}"))
}
/// An engine container ID: 64 hexadecimal digits.
pub(super) fn container_id(text: &str) -> io::Result<ContainerId> {
    let id = text.trim();
    (id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| ContainerId::from(id)).ok_or_else(|| invalid("container ID"))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    id: String,
    name: String,
    image: String,
    config: Config,
    state: State,
    host_config: HostConfig,
    #[serde(default)]
    mounts: Vec<MountRecord>,
    network_settings: Option<NetworkSettings>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Config {
    image: String,
    labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    user: String,
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct State {
    status: String,
    running: bool,
    exit_code: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HostConfig {
    #[serde(default)]
    network_mode: String,
    #[serde(default)]
    readonly_rootfs: bool,
    #[serde(default)]
    privileged: bool,
    #[serde(default)]
    pid_mode: String,
    restart_policy: Option<RestartPolicy>,
    cap_drop: Option<Vec<String>>,
    security_opt: Option<Vec<String>>,
    #[serde(default)]
    memory: u64,
    #[serde(default)]
    nano_cpus: u64,
    pids_limit: Option<u64>,
    tmpfs: Option<BTreeMap<String, String>>,
    storage_opt: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RestartPolicy {
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MountRecord {
    #[serde(rename = "Type")]
    kind: String,
    name: Option<String>,
    driver: Option<String>,
    #[serde(default)]
    source: String,
    destination: String,
    #[serde(rename = "RW")]
    rw: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct NetworkSettings {
    ports: Option<BTreeMap<String, Value>>,
}

/// `docker inspect` output: one record per container.
pub(super) fn inspection(text: &str) -> io::Result<Vec<Observed>> {
    let records: Vec<Container> = serde_json::from_str(text.trim()).map_err(|_| invalid("inspection"))?;
    records.into_iter().map(observed).collect()
}
fn observed(record: Container) -> io::Result<Observed> {
    let Container { id, name, image, config, state, host_config: host, mounts, network_settings } = record;
    Ok(Observed {
        id: container_id(&id)?,
        name,
        image,
        image_ref: config.image,
        labels: config.labels.unwrap_or_default(),
        status: state.status,
        running: state.running,
        exit_code: state.exit_code,
        user: config.user,
        entrypoint: config.entrypoint,
        command: config.cmd,
        network: host.network_mode,
        read_only_root: host.readonly_rootfs,
        privileged: host.privileged,
        pid_mode: host.pid_mode,
        restart: host.restart_policy.map(|policy| policy.name).unwrap_or_default(),
        cap_drop: host.cap_drop,
        security_opt: host.security_opt,
        memory: host.memory,
        nano_cpus: host.nano_cpus,
        pids_limit: host.pids_limit,
        tmpfs: host.tmpfs.unwrap_or_default(),
        storage_opt: host.storage_opt,
        mounts: mounts.into_iter().map(mount).collect(),
        ports: network_settings.and_then(|settings| settings.ports).map(|ports| ports.into_iter().map(|(port, bindings)| (port, !bindings.is_null())).collect()),
    })
}
fn mount(record: MountRecord) -> ObservedMount {
    let kind = match record.kind.as_str() {
        "bind" => MountKind::Bind,
        "volume" => MountKind::Volume,
        "tmpfs" => MountKind::Tmpfs,
        other => MountKind::Other(other.to_owned()),
    };
    ObservedMount { kind, name: record.name, driver: record.driver, source: record.source, destination: record.destination, writable: record.rw }
}

/// A `{{.Labels}}` field: `key=value` pairs joined by commas.
fn labels(text: &str) -> BTreeMap<String, String> {
    text.split(',').filter_map(|pair| pair.split_once('=')).map(|(key, value)| (key.to_owned(), value.to_owned())).collect()
}
/// `ps` lines of ID, names and labels, tab-separated.
pub(super) fn listing(text: &str) -> io::Result<Vec<Listed>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let (id, name, labeled) = (fields.next().unwrap_or(""), fields.next().ok_or_else(|| invalid("listing"))?, fields.next().unwrap_or(""));
            Ok(Listed { id: container_id(id)?, name: name.to_owned(), labels: labels(labeled) })
        })
        .collect()
}
/// `volume ls` lines of name and labels, tab-separated.
pub(super) fn volume_listing(text: &str) -> io::Result<Vec<ListedVolume>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (name, labeled) = line.split_once('\t').unwrap_or((line, ""));
            Ok(ListedVolume { name: name.to_owned(), labels: labels(labeled) })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Volume {
    name: String,
    driver: String,
    scope: String,
    labels: Option<BTreeMap<String, String>>,
    options: Option<BTreeMap<String, String>>,
}
/// `volume inspect` output.
pub(super) fn volumes(text: &str) -> io::Result<Vec<ObservedVolume>> {
    let records: Vec<Volume> = serde_json::from_str(text.trim()).map_err(|_| invalid("volume inspection"))?;
    Ok(records
        .into_iter()
        .map(|volume| ObservedVolume { name: volume.name, driver: volume.driver, scope: volume.scope, labels: volume.labels.unwrap_or_default(), options: volume.options.unwrap_or_default() })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Image {
    config: Option<ImageConfig>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ImageConfig {
    labels: Option<HashMap<String, String>>,
}
/// `image inspect` output for one image: its labels.
pub(super) fn image_labels(text: &str) -> io::Result<HashMap<String, String>> {
    let images: Vec<Image> = serde_json::from_str(text.trim()).map_err(|_| invalid("image inspection"))?;
    match <[Image; 1]>::try_from(images) {
        Ok([image]) => Ok(image.config.and_then(|config| config.labels).unwrap_or_default()),
        Err(_) => Err(invalid("image inspection")),
    }
}

/// `stats` lines of ID, CPU, memory and process count, tab-separated.
pub(super) fn stats(text: &str) -> HashMap<String, Stats> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.trim();
            let (cpu, memory, pids) = (fields.next()?, fields.next()?, fields.next()?);
            Some((id.to_owned(), Stats { cpu: cpu.trim().into(), memory: memory.trim().into(), pids: pids.trim().into() }))
        })
        .collect()
}
/// `top -o pid,user,args` output after its header line.
pub(super) fn top(text: &str) -> Vec<Process> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let (user, command) = rest.trim_start().split_once(char::is_whitespace).unwrap_or((rest.trim(), ""));
            Some(Process { pid: pid.into(), user: user.into(), command: command.trim().into() })
        })
        .collect()
}
/// Drivers Docker documents size quotas for; overlay2 only over xfs. `info`
/// does not report xfs's required pquota mount option, so creation can still
/// refuse. Docker Desktop's containerd snapshotter (`overlayfs`) has none.
pub fn quota_capable(info: &str) -> bool {
    let (driver, status) = info.trim().split_once(' ').unwrap_or((info.trim(), ""));
    match driver {
        "btrfs" | "zfs" | "devicemapper" | "windowsfilter" => true,
        "overlay2" => serde_json::from_str::<Vec<[String; 2]>>(status).is_ok_and(|status| status.iter().any(|[key, value]| key == "Backing Filesystem" && value == "xfs")),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
