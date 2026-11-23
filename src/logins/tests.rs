use super::*;
use control_agent::cli::private_file;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

fn directory(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("control-antigravity-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&root).unwrap();
    root
}
/// The form and file of the login found in `root`.
fn login_in(root: &Path) -> io::Result<(Form, PathBuf)> {
    antigravity(root).map(|granted| (granted.form(), granted.path().to_path_buf()))
}

#[test]
fn the_login_is_chosen_by_which_private_file_exists() {
    let root = directory("login");
    assert!(login_in(&root).is_err());
    // A login directory without a saved token is not a sign-in yet.
    let token = root.join(control_antigravity::SAVED);
    fs::create_dir_all(token.parent().unwrap()).unwrap();
    assert!(login_in(&root).is_err());
    private_file(&token, "").unwrap();
    assert_eq!(login_in(&root).unwrap(), (Form::Session, token.clone()));
    private_file(&root.join("adc.json"), "{}").unwrap();
    assert_eq!(login_in(&root).unwrap(), (Form::CloudDefault, root.join("adc.json")));
    // An API key wins, but only as a private file.
    fs::write(root.join("gemini-api-key"), "k").unwrap();
    fs::set_permissions(root.join("gemini-api-key"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(login_in(&root).unwrap(), (Form::CloudDefault, root.join("adc.json")));
    fs::set_permissions(root.join("gemini-api-key"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(login_in(&root).unwrap(), (Form::ApiKey, root.join("gemini-api-key")));
    // A shared login directory is refused outright.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(login_in(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}
