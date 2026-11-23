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

#[test]
fn the_account_argument_is_taken_out_before_the_backend_sees_its_arguments() {
    let args = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let (account, rest) = account_argument(&args(&["login", "--account", "second"])).unwrap();
    assert_eq!((account.as_str(), rest), ("second", args(&["login"])));
    let (account, rest) = account_argument(&args(&["--account", "work", "check"])).unwrap();
    assert_eq!((account.as_str(), rest), ("work", args(&["check"])));
    let (account, rest) = account_argument(&args(&["login"])).unwrap();
    assert!(account.is_implicit() && rest == args(&["login"]));
    assert!(account_argument(&args(&["login", "--account"])).is_err());
    assert!(account_argument(&args(&["login", "--account", "Bad"])).is_err());
}

#[test]
fn a_named_account_is_located_in_its_own_private_directory() {
    let second = AccountName::try_from("second").unwrap();
    let config = crate::config::directory().unwrap();
    assert_eq!(
        super::directory(Backend::Grok, &second).unwrap(),
        config.join("accounts/grok/second")
    );
    assert_eq!(
        super::directory(Backend::Grok, &AccountName::implicit()).unwrap(),
        config.join("grok")
    );
    let missing = locate(Backend::Grok, &second).unwrap_err().to_string();
    assert!(missing.contains("control grok login --account second"), "{missing}");
    let account = super::directory(Backend::Grok, &second).unwrap();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&account)
        .unwrap();
    private_file(&account.join("auth.json"), "").unwrap();
    let granted = locate(Backend::Grok, &second).unwrap();
    assert_eq!(
        (granted.form(), granted.path()),
        (Form::Session, account.join("auth.json").as_path())
    );
    // A shared account directory is refused, as the default one is.
    fs::set_permissions(&account, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(locate(Backend::Grok, &second).is_err());
    fs::remove_dir_all(account).unwrap();
}
