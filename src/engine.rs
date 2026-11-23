//! The root's entry to the container engine. Every container operation goes
//! through the [`isolation`] backend as typed data (`control_isolation`); no
//! production code here builds engine arguments.
pub(crate) mod ephemeral;
use control_isolation::{Docker, Isolation};

/// The node's isolation backend. Entry points take it from here and hand it
/// down, so tests can supply another.
pub(crate) fn isolation() -> &'static dyn Isolation {
    &Docker
}

/// Raw engine requests for the ignored live tests' own fixtures (pausing,
/// inspecting or cleaning up containers), which no production path performs.
#[cfg(test)]
pub(crate) fn query(arguments: &[&str]) -> std::io::Result<String> {
    control_isolation::bounded_output(Docker::cli().args(arguments), std::time::Duration::from_secs(5), false)
}
#[cfg(test)]
pub(crate) fn cli() -> std::process::Command {
    Docker::cli()
}
#[cfg(test)]
pub(crate) fn scrubbed() -> std::process::Command {
    Docker::scrubbed()
}

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
pub(crate) use control_isolation::golden;
#[cfg(test)]
mod tests;
