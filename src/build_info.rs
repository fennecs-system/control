//! Immutable build identification shared by CLI and health probes. No runtime
//! Git queries, configuration reads, credentials or application state.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub version: String,
    pub revision: Option<String>,
    pub dirty: bool,
    pub arch: String,
    pub os: String,
}

pub fn current() -> BuildInfo {
    let revision = env!("CONTROL_BUILD_REVISION");
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        revision: (!revision.is_empty()).then(|| revision.to_owned()),
        dirty: env!("CONTROL_BUILD_DIRTY") == "true",
        arch: std::env::consts::ARCH.into(),
        os: std::env::consts::OS.into(),
    }
}
