//! A scripted [`Isolation`] for tests that drive whole attempts without an
//! engine or a model. Every call is recorded as a typed [`Call`], so a test can
//! assert what would have been launched. The test's script answers each call:
//! [`Answer::Model`] lets a small in-memory [`Model`] of what was created,
//! started, killed and removed answer it; [`Answer::Run`] serves a program call
//! (`spawn`, `exec`, `wait`, `logs`) with a local process; [`Answer::Refuse`]
//! fails it. Specs are rendered as [`Docker`] would render them first, so a
//! spec the engine would refuse is refused here too.
//!
//! The model is plain data with pure transitions; the only effects are the
//! local processes and the standard inputs of `wait` processes, which are
//! closed when the container they wait on stops.
use crate::{
    ContainerId, Docker, ExecSpec, ImageId, Isolation, Listed, ListedVolume, Network, Observed, ObservedMount,
    ObservedVolume, Process, RunSpec, Select, Size, Source, States, Stats, Streams, Target,
    docker::{disk_size, tmpfs},
};
use std::{
    collections::{BTreeMap, HashMap},
    io,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Mutex, MutexGuard},
};

/// One request, as the caller made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Ready,
    DiskQuota,
    ImageLabels(ImageId),
    Spawn(RunSpec),
    RunDetached(RunSpec),
    Create(RunSpec),
    Start(ContainerId),
    Kill(ContainerId),
    /// The ID or name the caller gave.
    Remove(String),
    Inspect(Vec<String>),
    List(States, Select),
    Exec(ExecSpec),
    Wait(ContainerId),
    Logs(ContainerId, u32),
    Stats(Vec<ContainerId>),
    Top(ContainerId),
    VolumeCreate(String, Vec<(String, String)>),
    VolumeInspect(String),
    Volumes(Vec<(String, String)>),
    VolumeRemove(String),
}
impl Call {
    /// The container spec of a call that launches one.
    pub fn spec(&self) -> Option<&RunSpec> {
        match self {
            Self::Spawn(spec) | Self::RunDetached(spec) | Self::Create(spec) => Some(spec),
            _ => None,
        }
    }
}

/// How the script answers one call.
#[derive(Debug)]
pub enum Answer {
    /// What the model says. A program call the model answers runs `true`,
    /// except `wait`, which runs until its container stops.
    Model,
    /// This local process serves a program call.
    Run(Command),
    /// The engine refuses the call with this reason.
    Refuse(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Life {
    Created,
    Running,
    Exited(i64),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub spec: RunSpec,
    pub life: Life,
}
/// What the engine holds: containers by ID and volumes with their labels.
/// Containers an attached `spawn` starts are removed when they exit, so they
/// are never in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub containers: BTreeMap<ContainerId, Container>,
    pub volumes: BTreeMap<String, BTreeMap<String, String>>,
    created: u64,
}
fn missing(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("No such {what}"))
}
fn labelled(labels: &BTreeMap<String, String>, wanted: &[(String, String)]) -> bool {
    wanted.iter().all(|(name, value)| labels.get(name) == Some(value))
}
fn bytes(size: Size) -> u64 {
    match size {
        Size::Bytes(bytes) => bytes,
        Size::Mib(mib) => mib << 20,
        Size::Gib(gib) => gib << 30,
    }
}
impl Model {
    /// A new container with an engine-shaped ID; a taken name is a conflict.
    pub fn create(mut self, spec: &RunSpec, life: Life) -> io::Result<(Self, ContainerId)> {
        if self.find(Target::Name(&spec.name)).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Conflict. The container name \"/{}\" is already in use", spec.name),
            ));
        }
        self.created += 1;
        let id = ContainerId::from(format!("{:064x}", self.created));
        let container = Container {
            spec: spec.clone(),
            life,
        };
        self.containers.insert(id.clone(), container);
        Ok((self, id))
    }
    fn with(mut self, id: &ContainerId, change: impl FnOnce(Life) -> io::Result<Life>) -> io::Result<Self> {
        let container = self.containers.get_mut(id).ok_or_else(|| missing("container"))?;
        container.life = change(container.life)?;
        Ok(self)
    }
    pub fn start(self, id: &ContainerId) -> io::Result<Self> {
        self.with(id, |_| Ok(Life::Running))
    }
    /// A killed program exits with the engine's SIGKILL status.
    pub fn kill(self, id: &ContainerId) -> io::Result<Self> {
        self.with(id, |life| match life {
            Life::Running => Ok(Life::Exited(137)),
            _ => Err(io::Error::other("Container is not running")),
        })
    }
    pub fn remove(mut self, target: Target<'_>) -> io::Result<Self> {
        let id = self
            .find(target)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| missing("container"))?;
        self.containers.remove(&id);
        Ok(self)
    }
    pub fn find(&self, target: Target<'_>) -> Option<(&ContainerId, &Container)> {
        self.containers.iter().find(|(id, container)| match target {
            Target::Id(wanted) => *id == wanted,
            Target::Name(name) => container.spec.name == name || id.as_str() == name,
        })
    }
    pub fn running(&self, id: &ContainerId) -> bool {
        self.containers.get(id).is_some_and(|c| c.life == Life::Running)
    }
    pub fn inspect(&self, targets: &[Target<'_>]) -> io::Result<Vec<Observed>> {
        targets
            .iter()
            .map(|target| {
                self.find(*target)
                    .map(|(id, container)| observed(id, container))
                    .ok_or_else(|| missing("container"))
            })
            .collect()
    }
    pub fn list(&self, states: States, select: &Select) -> Vec<Listed> {
        self.containers
            .iter()
            .filter(|(_, c)| states == States::All || c.life == Life::Running)
            .filter(|(id, c)| match select {
                Select::Id(wanted) => *id == wanted,
                Select::Name(name) => c.spec.name == *name,
                Select::Labels(wanted) => labelled(&c.spec.labels.iter().cloned().collect(), wanted),
                Select::Any => true,
            })
            .map(|(id, c)| Listed {
                id: id.clone(),
                name: c.spec.name.clone(),
                labels: c.spec.labels.iter().cloned().collect(),
            })
            .collect()
    }
    /// An existing volume is left unchanged.
    pub fn volume_create(mut self, name: &str, labels: &[(String, String)]) -> Self {
        self.volumes
            .entry(name.to_owned())
            .or_insert_with(|| labels.iter().cloned().collect());
        self
    }
    /// A volume any container still mounts is refused.
    pub fn volume_remove(mut self, name: &str) -> io::Result<Self> {
        let mounted = self.containers.values().any(|c| {
            c.spec
                .mounts
                .iter()
                .any(|mount| mount.source == Source::Volume(name.to_owned()))
        });
        if mounted {
            return Err(io::Error::other(format!("volume is in use: {name}")));
        }
        self.volumes.remove(name).map(|_| self).ok_or_else(|| missing("volume"))
    }
}
/// What the engine would report for a container made from its spec.
pub fn observed(id: &ContainerId, container: &Container) -> Observed {
    let spec = &container.spec;
    let (status, exit_code) = match container.life {
        Life::Created => ("created", 0),
        Life::Running => ("running", 0),
        Life::Exited(code) => ("exited", code),
    };
    let secrets = spec
        .secrets
        .iter()
        .map(|s| (Source::Bind(s.source.clone()), &s.target, s.access));
    let mounts = spec
        .mounts
        .iter()
        .map(|m| (m.source.clone(), &m.target, m.access))
        .chain(secrets)
        .map(|(source, target, access)| {
            let (kind, name, source) = match source {
                Source::Bind(path) => (crate::MountKind::Bind, None, path.display().to_string()),
                Source::Volume(name) => (
                    crate::MountKind::Volume,
                    Some(name.clone()),
                    format!("/var/lib/docker/volumes/{name}/_data"),
                ),
            };
            ObservedMount {
                driver: name.as_ref().map(|_| "local".to_owned()),
                kind,
                name,
                source,
                destination: target.clone(),
                writable: access == crate::Access::ReadWrite,
            }
        })
        .collect();
    let security = ["no-new-privileges".to_owned()]
        .into_iter()
        .chain(spec.seccomp.as_ref().map(|p| format!("seccomp={}", p.display())))
        .chain((spec.proc == crate::Proc::Unmasked).then(|| "systempaths=unconfined".to_owned()))
        .collect();
    Observed {
        id: id.clone(),
        name: format!("/{}", spec.name),
        image: spec.image.as_str().to_owned(),
        image_ref: spec.image.as_str().to_owned(),
        labels: spec.labels.iter().cloned().collect(),
        status: status.to_owned(),
        running: container.life == Life::Running,
        exit_code,
        user: spec.user.map(|u| format!("{}:{}", u.uid, u.gid)).unwrap_or_default(),
        entrypoint: spec.entrypoint.clone().map(|e| vec![e]),
        command: (!spec.argv.is_empty()).then(|| spec.argv.clone()),
        network: match spec.network {
            Network::None => "none",
            Network::Engine => "bridge",
        }
        .to_owned(),
        read_only_root: spec.root == crate::Access::ReadOnly,
        privileged: false,
        pid_mode: String::new(),
        restart: "no".to_owned(),
        cap_drop: Some(vec!["ALL".to_owned()]),
        security_opt: Some(security),
        memory: spec.limits.memory.map_or(0, bytes),
        nano_cpus: spec.limits.cpus.map_or(0, |cpus| cpus * 1_000_000_000),
        pids_limit: spec.limits.pids,
        tmpfs: spec
            .tmpfs
            .iter()
            .filter_map(|fs| tmpfs(fs).split_once(':').map(|(t, o)| (t.to_owned(), o.to_owned())))
            .collect(),
        storage_opt: spec
            .disk
            .map(|disk| BTreeMap::from([("size".to_owned(), disk_size(disk))])),
        mounts,
        ports: None,
    }
}

/// The scripted backend. `script` sees each call after it is recorded.
pub struct Scripted<F> {
    script: F,
    images: HashMap<ImageId, HashMap<String, String>>,
    ready: bool,
    calls: Mutex<Vec<Call>>,
    model: Mutex<Model>,
    /// The standard input of each `wait` process the model serves, closed
    /// (ending the process) when its container stops.
    waiters: Mutex<Vec<(ContainerId, ChildStdin)>>,
    /// Each attached `spawn`'s container name and the process serving it: the
    /// engine runs that container, by name, while the process lives.
    attached: Mutex<Vec<(String, u32)>>,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl<F: Fn(&Call) -> Answer + Sync> Scripted<F> {
    pub fn new(script: F) -> Self {
        Self {
            script,
            images: HashMap::new(),
            ready: true,
            calls: Mutex::default(),
            model: Mutex::default(),
            waiters: Mutex::default(),
            attached: Mutex::default(),
        }
    }
    /// Whether the process serving the attached container `name` still runs.
    fn attached_running(&self, name: &str) -> bool {
        let pid = lock(&self.attached)
            .iter()
            .rev()
            .find(|(attached, _)| attached == name)
            .map(|(_, pid)| *pid);
        pid.is_some_and(|pid| {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }
    /// A local image and its labels.
    pub fn image(mut self, image: &str, labels: &[(&str, &str)]) -> Self {
        let labels = labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned()));
        self.images.insert(image.into(), labels.collect());
        self
    }
    /// An engine that does not answer.
    pub fn unready(self) -> Self {
        Self { ready: false, ..self }
    }
    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }
    pub fn model(&self) -> Model {
        lock(&self.model).clone()
    }
    /// Records `call` and asks the script: `None` means the model answers.
    fn ask(&self, call: Call) -> io::Result<Option<Command>> {
        lock(&self.calls).push(call.clone());
        match (self.script)(&call) {
            Answer::Model => Ok(None),
            Answer::Run(command) => Ok(Some(command)),
            Answer::Refuse(reason) => Err(io::Error::other(reason)),
        }
    }
    /// A call that runs no program.
    fn ask_model(&self, call: Call) -> io::Result<()> {
        match self.ask(call)? {
            None => Ok(()),
            Some(_) => Err(io::Error::other("A program was scripted for a call that runs none")),
        }
    }
    fn transition<T>(&self, change: impl FnOnce(Model) -> io::Result<(Model, T)>) -> io::Result<T> {
        let mut model = lock(&self.model);
        let (next, value) = change(model.clone())?;
        *model = next;
        Ok(value)
    }
    /// Ends the `wait` processes of containers that no longer run.
    fn release(&self) {
        let model = lock(&self.model);
        lock(&self.waiters).retain(|(id, _)| model.running(id));
    }
    fn launch(&self, call: Call, spec: &RunSpec, life: Life) -> io::Result<ContainerId> {
        self.ask_model(call)?;
        self.transition(|model| model.create(spec, life))
    }
}
impl<F: Fn(&Call) -> Answer + Sync> Isolation for Scripted<F> {
    fn ready(&self) -> bool {
        self.ask_model(Call::Ready).is_ok() && self.ready
    }
    fn disk_quota(&self) -> io::Result<bool> {
        self.ask_model(Call::DiskQuota).map(|()| false)
    }
    fn image_labels(&self, image: &ImageId) -> io::Result<HashMap<String, String>> {
        self.ask_model(Call::ImageLabels(image.clone()))?;
        self.images.get(image).cloned().ok_or_else(|| missing("image"))
    }
    fn spawn(&self, spec: &RunSpec, streams: Streams) -> io::Result<Child> {
        Docker::run_arguments(spec)?;
        let mut command = self
            .ask(Call::Spawn(spec.clone()))?
            .unwrap_or_else(|| Command::new("true"));
        let child = streams.apply(&mut command).spawn()?;
        lock(&self.attached).push((spec.name.clone(), child.id()));
        Ok(child)
    }
    fn run_detached(&self, spec: &RunSpec) -> io::Result<ContainerId> {
        Docker::detached_arguments(spec)?;
        self.launch(Call::RunDetached(spec.clone()), spec, Life::Running)
    }
    fn create(&self, spec: &RunSpec) -> io::Result<ContainerId> {
        Docker::create_arguments(spec)?;
        self.launch(Call::Create(spec.clone()), spec, Life::Created)
    }
    fn start(&self, id: &ContainerId) -> io::Result<()> {
        self.ask_model(Call::Start(id.clone()))?;
        self.transition(|model| Ok((model.start(id)?, ())))
    }
    fn kill(&self, id: &ContainerId) -> io::Result<()> {
        self.ask_model(Call::Kill(id.clone()))?;
        self.transition(|model| Ok((model.kill(id)?, ())))?;
        self.release();
        Ok(())
    }
    fn remove(&self, target: Target<'_>) -> io::Result<()> {
        let named = match target {
            Target::Id(id) => id.as_str().to_owned(),
            Target::Name(name) => name.to_owned(),
        };
        self.ask_model(Call::Remove(named))?;
        self.transition(|model| Ok((model.remove(target)?, ())))?;
        self.release();
        Ok(())
    }
    fn inspect(&self, targets: &[Target<'_>]) -> io::Result<Vec<Observed>> {
        let named = targets.iter().map(|target| match target {
            Target::Id(id) => id.as_str().to_owned(),
            Target::Name(name) => (*name).to_owned(),
        });
        self.ask_model(Call::Inspect(named.collect()))?;
        lock(&self.model).inspect(targets)
    }
    fn list(&self, states: States, select: &Select) -> io::Result<Vec<Listed>> {
        self.ask_model(Call::List(states, select.clone()))?;
        Ok(lock(&self.model).list(states, select))
    }
    /// The engine refuses an exec into a container that is not running.
    fn exec(&self, spec: &ExecSpec, streams: Streams) -> io::Result<Child> {
        let scripted = self.ask(Call::Exec(spec.clone()))?;
        if !lock(&self.model).running(&spec.container) && !self.attached_running(spec.container.as_str()) {
            return Err(io::Error::other("Container is not running"));
        }
        let mut command = scripted.unwrap_or_else(|| Command::new("true"));
        streams.apply(&mut command).spawn()
    }
    fn wait(&self, id: &ContainerId) -> io::Result<Child> {
        let quiet = |mut command: Command| {
            command.stdout(Stdio::null()).stderr(Stdio::null());
            command
        };
        if let Some(command) = self.ask(Call::Wait(id.clone()))? {
            return quiet(command).stdin(Stdio::null()).spawn();
        }
        if !lock(&self.model).running(id) {
            return quiet(Command::new("true")).stdin(Stdio::null()).spawn();
        }
        let mut child = quiet(Command::new("cat")).stdin(Stdio::piped()).spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| io::Error::other("No wait input"))?;
        lock(&self.waiters).push((id.clone(), stdin));
        Ok(child)
    }
    fn logs(&self, id: &ContainerId, tail: u32) -> io::Result<Child> {
        let mut command = self
            .ask(Call::Logs(id.clone(), tail))?
            .unwrap_or_else(|| Command::new("true"));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }
    fn stats(&self, ids: &[&ContainerId]) -> io::Result<HashMap<String, Stats>> {
        self.ask_model(Call::Stats(ids.iter().map(|id| (*id).clone()).collect()))?;
        let model = lock(&self.model);
        let sample = || Stats {
            cpu: "0.00%".into(),
            memory: "0B / 0B".into(),
            pids: "1".into(),
        };
        Ok(ids
            .iter()
            .filter(|id| model.running(id))
            .map(|id| (id.as_str().to_owned(), sample()))
            .collect())
    }
    fn top(&self, id: &ContainerId) -> io::Result<Vec<Process>> {
        self.ask_model(Call::Top(id.clone()))?;
        match lock(&self.model).running(id) {
            true => Ok(Vec::new()),
            false => Err(io::Error::other("Container is not running")),
        }
    }
    fn volume_create(&self, name: &str, labels: &[(&str, &str)]) -> io::Result<()> {
        let labels: Vec<_> = labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        self.ask_model(Call::VolumeCreate(name.to_owned(), labels.clone()))?;
        self.transition(|model| Ok((model.volume_create(name, &labels), ())))
    }
    fn volume_inspect(&self, name: &str) -> io::Result<Vec<ObservedVolume>> {
        self.ask_model(Call::VolumeInspect(name.to_owned()))?;
        let model = lock(&self.model);
        let labels = model.volumes.get(name).ok_or_else(|| missing("volume"))?;
        Ok(vec![ObservedVolume {
            name: name.to_owned(),
            driver: "local".into(),
            scope: "local".into(),
            labels: labels.clone(),
            options: BTreeMap::new(),
        }])
    }
    fn volumes(&self, labels: &[(&str, &str)]) -> io::Result<Vec<ListedVolume>> {
        let wanted: Vec<_> = labels.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        self.ask_model(Call::Volumes(wanted.clone()))?;
        Ok(lock(&self.model)
            .volumes
            .iter()
            .filter(|(_, labels)| labelled(labels, &wanted))
            .map(|(name, labels)| ListedVolume {
                name: name.clone(),
                labels: labels.clone(),
            })
            .collect())
    }
    fn volume_remove(&self, name: &str) -> io::Result<()> {
        self.ask_model(Call::VolumeRemove(name.to_owned()))?;
        self.transition(|model| Ok((model.volume_remove(name)?, ())))
    }
}

#[cfg(test)]
mod tests;
