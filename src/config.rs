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
    let port = addr.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()).unwrap_or(8080);
    format!("http://localhost:{port}")
}

/// Model run time limit: `CONTROL_WORKER_TIMEOUT_SECS` or `worker-timeout-secs` in the
/// node configuration directory; default 600 s, clamped to 60 s – 24 h.
pub fn worker_deadline() -> std::time::Duration {
    let configured = std::env::var("CONTROL_WORKER_TIMEOUT_SECS").ok()
        .or_else(|| fs::read_to_string(directory().ok()?.join("worker-timeout-secs")).ok());
    std::time::Duration::from_secs(deadline_secs(configured.as_deref()))
}
fn deadline_secs(configured: Option<&str>) -> u64 {
    configured.and_then(|s| s.trim().parse::<u64>().ok()).map_or(600, |s| s.clamp(60, 86400))
}

pub fn source_root() -> io::Result<PathBuf> {
    let path = std::env::var_os("CONTROL_SOURCE_ROOT").map(PathBuf::from)
        .or_else(|| fs::read_to_string(directory().ok()?.join("source-root")).ok().map(|p| PathBuf::from(p.trim())))
        .ok_or_else(|| io::Error::other("Set CONTROL_SOURCE_ROOT or source-root in the node configuration for source improvements"))?;
    if !path.is_absolute() {
        return Err(io::Error::other("Control source root must be absolute"));
    }
    fs::canonicalize(path)
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
mod tests {
    use super::*;
    #[test]
    fn worker_deadline_defaults_and_clamps() {
        assert_eq!(deadline_secs(None), 600);
        assert_eq!(deadline_secs(Some(" 1800\n")), 1800);
        assert_eq!(deadline_secs(Some("5")), 60);
        assert_eq!(deadline_secs(Some("soon")), 600);
    }
    #[test]
    fn client_url_uses_localhost_and_the_bound_port() {
        assert_eq!(url_for("127.0.0.1:9090"), "http://localhost:9090");
        assert_eq!(url_for("[::1]:8181"), "http://localhost:8181");
        assert_eq!(url_for("bad"), "http://localhost:8080");
    }
    #[test]
    fn initialization_preserves_existing_configuration_and_rejects_symlinks() {
        let directory = std::env::temp_dir().join(format!("control-init-{}", std::process::id()));
        initialize_in(&directory, b"original fixture").unwrap();
        initialize_in(&directory, b"replacement must not be written").unwrap();
        assert_eq!(
            fs::read(directory.join("discord-access.json")).unwrap(),
            b"original fixture"
        );
        let link = directory.with_extension("link");
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        assert!(initialize_in(&link, b"must not write").is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn node_configuration_has_explicit_precedence_and_no_checkout_fallback() {
        let path = |value: &str| Some(OsString::from(value));
        assert_eq!(
            resolve(path("/node"), path("/xdg"), path("/home")),
            Ok(PathBuf::from("/node"))
        );
        assert_eq!(
            resolve(None, path("/xdg"), path("/home")),
            Ok(PathBuf::from("/xdg/control"))
        );
        assert_eq!(
            resolve(None, None, path("/home")),
            Ok(PathBuf::from("/home/.config/control"))
        );
        assert!(resolve(None, None, None).is_err());
        assert!(resolve(path("relative"), None, path("/home")).is_err());
        assert!(resolve(None, path("relative"), path("/home")).is_err());
    }
}
