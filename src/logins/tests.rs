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
    let missing = locate(&super::account(&Store::default(), Backend::Grok, &second))
        .unwrap_err()
        .to_string();
    assert!(missing.contains("control grok login --account second"), "{missing}");
    let account = super::directory(Backend::Grok, &second).unwrap();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&account)
        .unwrap();
    private_file(&account.join("auth.json"), "").unwrap();
    let granted = locate(&super::account(&Store::default(), Backend::Grok, &second)).unwrap();
    assert_eq!(
        (granted.form(), granted.path()),
        (Form::Session, account.join("auth.json").as_path())
    );
    // A shared account directory is refused, as the default one is.
    fs::set_permissions(&account, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(locate(&super::account(&Store::default(), Backend::Grok, &second)).is_err());
    fs::remove_dir_all(account).unwrap();
}

#[test]
fn an_api_key_account_is_located_by_its_kind_and_reported_from_metadata() {
    let keyed = AccountName::try_from("keyed").unwrap();
    let store = Store::default()
        .add(crate::accounts::Entry {
            backend: Backend::Claude,
            name: keyed.clone(),
            kind: AccountKind::ApiKey,
            cap: None,
            enabled: true,
            label: None,
        })
        .unwrap();
    let account = super::account(&store, Backend::Claude, &keyed);
    assert_eq!(sign_in_command(&account), "control claude key --account keyed");
    let missing = locate(&account).unwrap_err().to_string();
    assert!(missing.contains("control claude key --account keyed"), "{missing}");
    let directory = super::directory(Backend::Claude, &keyed).unwrap();
    private_directory(&directory).unwrap();
    // Written as `control claude key` writes it; its contents are never read here.
    crate::config::write_in(&directory, "anthropic-api-key", b"sk-ant-CANARY").unwrap();
    let mode = fs::metadata(directory.join("anthropic-api-key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let granted = locate(&account).unwrap();
    assert_eq!(
        (granted.form(), granted.path()),
        (Form::ApiKey, directory.join("anthropic-api-key").as_path())
    );
    let lines = report(Backend::Claude, &store, &[]);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("claude+keyed (api-key): api-key: present")),
        "{lines:?}"
    );
    assert!(lines.iter().all(|line| !line.contains("CANARY")));
    // A named Antigravity key is its key file, not a pick among files.
    let gemini = Store::default()
        .add(crate::accounts::Entry {
            backend: Backend::Antigravity,
            name: keyed.clone(),
            kind: AccountKind::ApiKey,
            cap: None,
            enabled: true,
            label: None,
        })
        .unwrap();
    let account = super::account(&gemini, Backend::Antigravity, &keyed);
    let directory = super::directory(Backend::Antigravity, &keyed).unwrap();
    private_directory(&directory).unwrap();
    crate::config::write_in(&directory, "gemini-api-key", b"k").unwrap();
    let granted = locate(&account).unwrap();
    assert_eq!(
        (granted.form(), granted.path()),
        (Form::ApiKey, directory.join("gemini-api-key").as_path())
    );
    fs::remove_dir_all(super::directory(Backend::Claude, &keyed).unwrap()).unwrap();
    fs::remove_dir_all(directory).unwrap();
}

/// Only a subscription in a folder Control owns may be written back; the
/// operator's own sign-in and every key or cloud credential stay read-only,
/// wherever they are.
#[test]
fn only_a_subscription_control_keeps_is_refreshable() {
    use crate::accounts::Owner;
    for (kind, owner, write) in [
        (AccountKind::Subscription, Owner::Control, Write::Refreshable),
        (AccountKind::Subscription, Owner::Operator, Write::ReadOnly),
        (AccountKind::ApiKey, Owner::Control, Write::ReadOnly),
        (AccountKind::ApiKey, Owner::Operator, Write::ReadOnly),
        (AccountKind::Cloud, Owner::Control, Write::ReadOnly),
        (AccountKind::Cloud, Owner::Operator, Write::ReadOnly),
    ] {
        assert_eq!(write_policy(kind, owner), write, "{kind:?} {owner:?}");
    }
    // The table: Codex's default is the operator's own; every other default
    // and every named account is in Control's folder.
    let owners = |backend: Backend| -> Vec<Owner> {
        crate::accounts::default_for(backend)
            .secret
            .0
            .iter()
            .map(crate::accounts::Location::owner)
            .collect()
    };
    assert_eq!(owners(Backend::Codex), [Owner::Operator; 3]);
    assert_eq!(owners(Backend::Claude), [Owner::Operator, Owner::Control]);
    assert_eq!(owners(Backend::Antigravity), [Owner::Control]);
    let second = AccountName::try_from("second").unwrap();
    let named = super::account(&Store::default(), Backend::Codex, &second);
    assert_eq!(
        named
            .secret
            .0
            .iter()
            .map(crate::accounts::Location::owner)
            .collect::<Vec<_>>(),
        [Owner::Control]
    );
}

#[test]
fn a_located_login_carries_the_policy_and_a_key_is_never_writable() {
    let name = AccountName::try_from("policy").unwrap();
    let subscription = super::account(&Store::default(), Backend::Grok, &name);
    let directory = super::directory(Backend::Grok, &name).unwrap();
    private_directory(&directory).unwrap();
    private_file(&directory.join("auth.json"), "").unwrap();
    assert_eq!(locate(&subscription).unwrap().write(), Write::Refreshable);
    let keyed = Store::default()
        .add(crate::accounts::Entry {
            backend: Backend::Grok,
            name: name.clone(),
            kind: AccountKind::ApiKey,
            cap: None,
            enabled: true,
            label: None,
        })
        .unwrap();
    crate::config::write_in(&directory, "xai-api-key", b"k").unwrap();
    let key = locate(&super::account(&keyed, Backend::Grok, &name)).unwrap();
    assert_eq!((key.form(), key.write()), (Form::ApiKey, Write::ReadOnly));
    fs::remove_dir_all(directory).unwrap();
}

/// A sign-in failure names the one command that recovers that account.
#[test]
fn a_sign_in_failure_names_the_command_for_its_account() {
    let entry = |name: &str, kind| crate::accounts::Entry {
        backend: Backend::Claude,
        name: AccountName::try_from(name).unwrap(),
        kind,
        cap: None,
        enabled: true,
        label: None,
    };
    let store = Store::default()
        .add(entry("work", AccountKind::Subscription))
        .and_then(|store| store.add(entry("metered", AccountKind::ApiKey)))
        .unwrap();
    let failure = |backend: Backend, account: &str| {
        let account = super::account(&store, backend, &AccountName::try_from(account).unwrap());
        control_agent::Failure::Provider {
            agent: backend.agent().name(),
            cause: control_agent::Cause::AuthRejected,
        }
        .message(&control_agent::SignIn::from(sign_in_command(&account)))
    };
    for (backend, account, command) in [
        (Backend::Claude, "work", "control claude login --account work"),
        (Backend::Claude, "metered", "control claude key --account metered"),
        (Backend::Claude, "default", "control claude login"),
        (Backend::Codex, "default", "codex login (on the host)"),
    ] {
        let message = failure(backend, account);
        assert!(
            message.contains(&format!("Sign in again with {command} and submit a new run.")),
            "{message}"
        );
    }
    // Each account's line in `control accounts list` and `check` names it too.
    let lines = report(Backend::Claude, &store, &[]);
    assert!(
        lines
            .iter()
            .any(|line| line.ends_with("sign in again with control claude login --account work")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.ends_with("sign in again with control claude key --account metered"))
    );
}
