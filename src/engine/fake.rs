//! A scripted [`Isolation`] for tests of ownership decisions: inspections,
//! listings, kills and removals become [`Call`]s answered by the test's
//! closure. Other operations are not scripted and fail.
use control_isolation::*;
use std::{collections::HashMap, io, process::Child};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Call {
    Kill(ContainerId),
    Remove(String),
    Inspect(Vec<String>),
    List(States, Select),
}
#[derive(Debug, Default)]
pub(crate) enum Reply {
    #[default]
    Done,
    Observed(Vec<Observed>),
    Listed(Vec<Listed>),
}
pub(crate) struct Fake<F>(pub F);
fn unscripted() -> io::Error {
    io::Error::other("not scripted")
}
fn name(target: Target<'_>) -> String {
    match target {
        Target::Id(id) => id.as_str().to_owned(),
        Target::Name(name) => name.to_owned(),
    }
}
impl<F: Fn(Call) -> io::Result<Reply> + Sync> Isolation for Fake<F> {
    fn ready(&self) -> bool {
        false
    }
    fn disk_quota(&self) -> io::Result<bool> {
        Err(unscripted())
    }
    fn image_labels(&self, _: &ImageId) -> io::Result<HashMap<String, String>> {
        Err(unscripted())
    }
    fn spawn(&self, _: &RunSpec, _: Streams) -> io::Result<Child> {
        Err(unscripted())
    }
    fn run_detached(&self, _: &RunSpec) -> io::Result<ContainerId> {
        Err(unscripted())
    }
    fn create(&self, _: &RunSpec) -> io::Result<ContainerId> {
        Err(unscripted())
    }
    fn start(&self, _: &ContainerId) -> io::Result<()> {
        Err(unscripted())
    }
    fn kill(&self, id: &ContainerId) -> io::Result<()> {
        (self.0)(Call::Kill(id.clone())).map(drop)
    }
    fn remove(&self, target: Target<'_>) -> io::Result<()> {
        (self.0)(Call::Remove(name(target))).map(drop)
    }
    fn inspect(&self, targets: &[Target<'_>]) -> io::Result<Vec<Observed>> {
        match (self.0)(Call::Inspect(targets.iter().copied().map(name).collect()))? {
            Reply::Observed(observed) => Ok(observed),
            _ => Err(unscripted()),
        }
    }
    fn list(&self, states: States, select: &Select) -> io::Result<Vec<Listed>> {
        match (self.0)(Call::List(states, select.clone()))? {
            Reply::Listed(listed) => Ok(listed),
            _ => Err(unscripted()),
        }
    }
    fn exec(&self, _: &ExecSpec, _: Streams) -> io::Result<Child> {
        Err(unscripted())
    }
    fn wait(&self, _: &ContainerId) -> io::Result<Child> {
        Err(unscripted())
    }
    fn logs(&self, _: &ContainerId, _: u32) -> io::Result<Child> {
        Err(unscripted())
    }
    fn stats(&self, _: &[&ContainerId]) -> io::Result<HashMap<String, Stats>> {
        Err(unscripted())
    }
    fn top(&self, _: &ContainerId) -> io::Result<Vec<Process>> {
        Err(unscripted())
    }
    fn volume_create(&self, _: &str, _: &[(&str, &str)]) -> io::Result<()> {
        Err(unscripted())
    }
    fn volume_inspect(&self, _: &str) -> io::Result<Vec<ObservedVolume>> {
        Err(unscripted())
    }
    fn volumes(&self, _: &[(&str, &str)]) -> io::Result<Vec<ListedVolume>> {
        Err(unscripted())
    }
    fn volume_remove(&self, _: &str) -> io::Result<()> {
        Err(unscripted())
    }
}
