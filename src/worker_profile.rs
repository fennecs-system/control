//! Captured capability of an operator-pinned image, never a model Docker option.
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Lite,
    Full,
}

impl Profile {
    pub fn is_lite(&self) -> bool {
        *self == Self::Lite
    }

    /// Only credential-free tool containers may use Full's private writable layer.
    pub fn readonly_root(self) -> bool {
        self == Self::Lite
    }

    pub fn service_memory(self) -> u64 {
        match self {
            Self::Lite => 512 * 1024 * 1024,
            Self::Full => 2 * 1024 * 1024 * 1024,
        }
    }

    pub fn service_pids(self) -> u64 {
        match self {
            Self::Lite => 64,
            Self::Full => 128,
        }
    }

    pub fn service_workspace_mb(self) -> u64 {
        match self {
            Self::Lite => 64,
            Self::Full => 512,
        }
    }

    pub fn inspect(image: &str) -> io::Result<Self> {
        if !crate::runtime::pinned_image(image) {
            return Err(io::Error::other("Worker image must be pinned by digest"));
        }
        let marker = crate::engine::query(&[
            "image",
            "inspect",
            "--format",
            "{{index .Config.Labels \"dev.control.worker-profile\"}}",
            image,
        ])?;
        Self::from_marker(marker.trim())
    }

    fn from_marker(marker: &str) -> io::Result<Self> {
        match marker {
            "" | "<no value>" | "lite" => Ok(Self::Lite),
            "full-v1" => Ok(Self::Full),
            _ => Err(io::Error::other(
                "Unsupported worker profile; rebuild the pinned image",
            )),
        }
    }
}

#[cfg(test)]
mod tests;
