
use super::*;
#[test]
fn bounded_reads_reject_links_public_private_files_and_special_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = std::env::temp_dir().join(format!(
        "control-private-read-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let file = root.join("fixture");
    fs::write(&file, b"fixture").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_regular(&file, 7, true).unwrap(), b"fixture");
    assert!(read_regular(&file, 6, true).is_err());
    symlink(&file, root.join("link")).unwrap();
    assert!(read_regular(&root.join("link"), 7, true).is_err());
    assert!(read_regular(&root, 1024, true).is_err());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_regular(&file, 7, true).is_err());
    assert_eq!(read_regular(&file, 7, false).unwrap(), b"fixture");
    use std::os::unix::ffi::OsStrExt;
    let fifo = std::ffi::CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(read_regular(&root.join("fifo"), 7, true).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn source_setup_preserves_choice_and_requires_explicit_checkout() {
    let root = std::env::temp_dir().join(format!("control-source-config-{}", std::process::id()));
    let config = root.join("config");
    fs::create_dir_all(&config).unwrap();
    let source = root.join("checkout");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("web")).unwrap();
    for name in ["Cargo.toml", "src/main.rs", "web/Trunk.toml", "flake.nix"] {
        fs::write(source.join(name), "fixture").unwrap();
    }
    assert!(
        source_root_in(None, &config)
            .unwrap_err()
            .to_string()
            .contains("config init --source-root")
    );
    assert!(initialize_source_root_in(&config, &root).is_err());
    initialize_source_root_in(&config, &source).unwrap();
    assert_eq!(
        source_root_in(None, &config).unwrap(),
        fs::canonicalize(&source).unwrap()
    );
    // Re-running setup from another checkout cannot switch source authority.
    initialize_source_root_in(&config, &root.join("missing")).unwrap();
    assert_eq!(
        source_root_in(None, &config).unwrap(),
        fs::canonicalize(&source).unwrap()
    );
    assert!(source_root_in(Some(OsString::from("relative")), &config).is_err());
    assert!(
        source_root_in(Some(root.join("missing").into_os_string()), &config)
            .unwrap_err()
            .to_string()
            .contains("unavailable")
    );
    fs::remove_file(config.join("source-root")).unwrap();
    std::os::unix::fs::symlink(source.join("Cargo.toml"), config.join("source-root")).unwrap();
    assert!(initialize_source_root_in(&config, &source).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn worker_deadline_defaults_and_clamps() {
    assert_eq!(deadline_secs(None), 600);
    assert_eq!(deadline_secs(Some(" 1800\n")), 1800);
    assert_eq!(deadline_secs(Some("5")), 60);
    assert_eq!(deadline_secs(Some("soon")), 600);
}
#[test]
fn bot_turn_deadline_defaults_clamps_and_can_be_unlimited() {
    assert_eq!(bot_turn_secs(None), Some(1200));
    assert_eq!(bot_turn_secs(Some(" 1800\n")), Some(1800));
    assert_eq!(bot_turn_secs(Some("5")), Some(60));
    assert_eq!(bot_turn_secs(Some("999999")), Some(86400));
    assert_eq!(bot_turn_secs(Some("soon")), Some(1200));
    assert_eq!(bot_turn_secs(Some("0\n")), None);
    assert_eq!(bot_turn_secs(Some("none")), None);
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

#[test]
fn state_directory_has_explicit_precedence() {
    let path = |value: &str| Some(OsString::from(value));
    assert_eq!(resolve_state(path("/node"), path("/xdg"), path("/home")), Ok(PathBuf::from("/node")));
    assert_eq!(resolve_state(None, path("/xdg"), path("/home")), Ok(PathBuf::from("/xdg/control")));
    assert_eq!(resolve_state(None, None, path("/home")), Ok(PathBuf::from("/home/.local/state/control")));
    assert!(resolve_state(None, None, None).is_err());
    assert!(resolve_state(path("relative"), None, None).is_err());
}

#[test]
fn test_builds_default_node_directories_to_the_test_root() {
    let operator = [PathBuf::from("/home/me/.config/control"), PathBuf::from("/home/me/.local/state/control")];
    assert_eq!(isolated(None, "state", &operator), Ok(test_root().join("state")));
    assert_eq!(isolated(None, "config", &operator), Ok(test_root().join("config")));
    assert!(test_root().starts_with(std::env::temp_dir()));
    assert_eq!(isolated(Some("/scratch/state".into()), "state", &operator), Ok(PathBuf::from("/scratch/state")));
    assert!(isolated(Some("relative".into()), "state", &operator).is_err());
    // This process's environment: unset variables resolve under the test root.
    if std::env::var_os("CONTROL_STATE_DIR").is_none() {
        assert_eq!(state_directory(), Ok(test_root().join("state")));
        assert_eq!(state("state.json"), Ok(test_root().join("state/state.json")));
    }
    if std::env::var_os("CONTROL_CONFIG_DIR").is_none() {
        assert_eq!(directory(), Ok(test_root().join("config")));
    }
}

#[test]
#[should_panic(expected = "refused the operator's node directory")]
fn test_builds_refuse_an_explicit_operator_state_directory() {
    let operator = [PathBuf::from("/home/me/.local/state/control")];
    let _ = isolated(Some("/home/me/.local/state/control".into()), "state", &operator);
}

#[test]
#[should_panic(expected = "refused the operator's node directory")]
fn test_builds_refuse_paths_inside_the_operator_config_directory() {
    let operator = [PathBuf::from("/home/me/.config/control")];
    let _ = isolated(Some("/home/me/.config/control/nested".into()), "config", &operator);
}

/// Guard: no library node path helper resolves inside the operator's real
/// directories. The binary's helpers have the same guard in `worker/tests.rs`.
#[test]
fn no_path_helper_resolves_operator_node_directories() {
    let operator = operator_directories();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        assert!(operator.contains(&home.join(".config/control")));
        assert!(operator.contains(&home.join(".local/state/control")));
    }
    let helpers = [
        ("config::directory", directory()),
        ("config::state_directory", state_directory()),
        ("config::state", state("state.json")),
        ("outbox::path", crate::outbox::path()),
    ];
    for (name, path) in helpers {
        let path = path.unwrap();
        assert!(!operator.iter().any(|real| path.starts_with(real)), "{name} resolved {}", path.display());
    }
}
