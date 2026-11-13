//! Personal node configuration is independent of the checkout and actor roles.
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub fn directory() -> Result<PathBuf, &'static str> {
    resolve(
        std::env::var_os("CONTROL_CONFIG_DIR"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Validate the opened file, never a separate path lookup. The bounded read also
/// rejects growth after metadata inspection. No contents are included in errors.
pub fn read_regular(path: &Path, limit: usize, private: bool) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.len() > limit as u64
        || (private && meta.permissions().mode() & 0o077 != 0)
    {
        return Err(io::Error::other(
            "Expected a bounded regular file with appropriate permissions",
        ));
    }
    // The descriptor's size has already passed the caller's bound. Reserve once
    // instead of repeatedly growing and copying a large state-file buffer.
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("File exceeds size limit"));
    }
    Ok(bytes)
}

pub fn state_directory() -> Result<PathBuf, &'static str> {
    let path = std::env::var_os("CONTROL_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|p| PathBuf::from(p).join("control")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/control")))
        .ok_or("Set HOME or CONTROL_STATE_DIR to locate this node's state")?;
    if !path.is_absolute() {
        return Err("Control state paths must be absolute");
    }
    Ok(path)
}

pub fn state(name: &str) -> Result<PathBuf, &'static str> {
    if Path::new(name)
        .components()
        .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        return Err("Invalid node state path");
    }
    Ok(state_directory()?.join(name))
}

pub fn node_id() -> Result<String, &'static str> {
    use std::os::unix::fs::PermissionsExt;
    let path = state("node-id")?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| "Node identity unavailable")?;
    if !metadata.is_file() || metadata.len() > 64 || metadata.permissions().mode() & 0o077 != 0 {
        return Err("Node identity must be a private regular file");
    }
    let bytes =
        fs::read(path).map_err(|_| "Node identity unavailable; initialize the state directory")?;
    let id = std::str::from_utf8(&bytes)
        .map_err(|_| "Invalid node identity")?
        .trim();
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid node identity");
    }
    Ok(id.into())
}

/// Loopback address the server binds: `CONTROL_ADDR`, default `127.0.0.1:8080`.
pub fn server_addr() -> String {
    std::env::var("CONTROL_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into())
}

/// Client origin for the local server. Uses `localhost`, where the operator cookie is scoped.
pub fn server_url() -> String {
    url_for(&server_addr())
}
fn url_for(addr: &str) -> String {
    let port = addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(8080);
    format!("http://localhost:{port}")
}

/// Model run time limit: `CONTROL_WORKER_TIMEOUT_SECS` or `worker-timeout-secs` in the
/// node configuration directory; default 600 s, clamped to 60 s – 24 h.
pub fn worker_deadline() -> std::time::Duration {
    configured_worker_deadline().unwrap_or(std::time::Duration::from_secs(600))
}
/// Explicit operator policy, captured at attempt admission. An absent policy
/// does not impose a wall-clock limit on an approved persistent agent.
pub fn configured_worker_deadline() -> Option<std::time::Duration> {
    let configured = std::env::var("CONTROL_WORKER_TIMEOUT_SECS")
        .ok()
        .or_else(|| fs::read_to_string(directory().ok()?.join("worker-timeout-secs")).ok());
    configured.map(|value| std::time::Duration::from_secs(deadline_secs(Some(&value))))
}
fn deadline_secs(configured: Option<&str>) -> u64 {
    configured
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(600, |s| s.clamp(60, 86400))
}

pub fn source_root() -> io::Result<PathBuf> {
    source_root_in(
        std::env::var_os("CONTROL_SOURCE_ROOT"),
        &directory().map_err(io::Error::other)?,
    )
}

fn source_root_in(explicit: Option<OsString>, directory: &Path) -> io::Result<PathBuf> {
    let path = match explicit {
        Some(path) => PathBuf::from(path),
        None => match fs::read_to_string(directory.join("source-root")) {
            Ok(path) => PathBuf::from(path.trim()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::other(
                    "Control source is not configured. From your Control checkout, run control config init --source-root \"$(pwd -P)\", or set CONTROL_SOURCE_ROOT to its absolute path.",
                ));
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "Could not read source-root in the node configuration: {error}"
                )));
            }
        },
    };
    validate_source_root(&path)
}

fn validate_source_root(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::other(
            "Control source root must be absolute. Set CONTROL_SOURCE_ROOT or source-root to your Control checkout.",
        ));
    }
    let path = fs::canonicalize(path).map_err(|e| io::Error::other(format!("Control source checkout is unavailable: {e}. Check CONTROL_SOURCE_ROOT or source-root in the node configuration.")))?;
    for name in ["Cargo.toml", "src/main.rs", "web/Trunk.toml", "flake.nix"] {
        if !fs::symlink_metadata(path.join(name)).is_ok_and(|m| m.is_file()) {
            return Err(io::Error::other(format!(
                "Control source must be a Control checkout (missing regular {name}). Set CONTROL_SOURCE_ROOT or source-root to the checkout root."
            )));
        }
    }
    Ok(path)
}

/// Setup explicitly authorizes its checkout for improvement; runtime never infers
/// source access from the current directory. An existing choice is never replaced.
pub fn initialize_source_root(path: &Path) -> io::Result<()> {
    initialize_source_root_in(&directory().map_err(io::Error::other)?, path)
}

fn initialize_source_root_in(directory: &Path, source: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let target = directory.join("source-root");
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_file() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::other(
                "Existing source-root must be a regular file, not a symlink; existing configuration was preserved",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let source = validate_source_root(source)?;
    let source = source
        .to_str()
        .filter(|s| !s.contains(['\n', '\r']))
        .ok_or_else(|| io::Error::other("Control source path must be UTF-8 without line breaks"))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)?;
    writeln!(file, "{source}")?;
    file.sync_all()?;
    fs::File::open(directory)?.sync_all()
}

pub fn write_private(name: &str, bytes: &[u8]) -> io::Result<()> {
    write_in(&directory().map_err(io::Error::other)?, name, bytes)
}

pub fn initialize(template: &[u8]) -> io::Result<()> {
    initialize_in(&directory().map_err(io::Error::other)?, template)
}
fn initialize_in(directory: &Path, template: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "Configuration directory must be private (mode 0700) and not a symlink",
        ));
    }
    let path = directory.join("discord-access.json");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(template)?;
            file.sync_all()?;
            fs::File::open(directory)?.sync_all()?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::other(
                    "Existing Discord policy must be a private regular file",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

pub fn write_in(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if Path::new(name).file_name() != Some(std::ffi::OsStr::new(name)) {
        return Err(io::Error::other("Invalid configuration filename"));
    }
    let temporary = directory.join(format!("{name}.{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join(name))?;
        fs::File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn resolve(
    explicit: Option<OsString>,
    xdg: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, &'static str> {
    let path = explicit
        .map(PathBuf::from)
        .or_else(|| xdg.map(|p| PathBuf::from(p).join("control")))
        .or_else(|| home.map(|p| PathBuf::from(p).join(".config/control")))
        .ok_or("Set HOME or CONTROL_CONFIG_DIR to locate this node's configuration")?;
    if !path.is_absolute() {
        return Err("Control configuration paths must be absolute");
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
