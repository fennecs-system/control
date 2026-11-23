//! Where each account's login lives on this node, located and checked by the
//! host and handed to a turn as a [`Granted`] value. Metadata only: no
//! credential is ever read. The implicit `default` account is at exactly the
//! path its backend used to locate itself; a named account's login is in
//! `<config>/accounts/BACKEND/NAME/`. Backends never learn which account
//! they were handed. An API-key account's key is read once, from standard
//! input by `control ID key`, and written only to its private file.
use crate::accounts::{AccountKind, AccountName, KeyWriter, Location, Owner, Roots, Store};
use crate::backend::Backend;
use control_agent::{ApiKey, Form, Granted, Write};

/// One account of one backend, as accounts.json defines it.
pub(crate) type Account = crate::accounts::Account<Backend>;
use control_isolation::User;
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn owner(meta: &fs::Metadata) -> User {
    User {
        uid: meta.uid(),
        gid: meta.gid(),
    }
}

/// The account's login directory, which `control ID login [--account NAME]`
/// creates and fills: `<config>/ID` for the implicit account, else
/// `<config>/accounts/ID/NAME`.
pub(crate) fn directory(backend: Backend, account: &AccountName) -> io::Result<PathBuf> {
    let config = crate::config::directory().map_err(fail)?;
    Ok(if account.is_implicit() {
        config.join(backend.id())
    } else {
        config.join(crate::accounts::directory(backend, account).as_path())
    })
}

/// Where the account's login is, from the one table of locations
/// (`crate::providers::accounts`), with the location that matched. Tests
/// never see the operator's home or `CODEX_HOME`: only `CONTROL_*` overrides
/// count, and the home is the test root.
fn resolve(account: &Account) -> io::Result<Option<(Location, PathBuf)>> {
    let config = crate::config::directory().map_err(fail)?;
    #[cfg(test)]
    let home = Some(crate::config::test_root());
    #[cfg(not(test))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let variable = |name: &str| {
        (!cfg!(test) || name.starts_with("CONTROL_"))
            .then(|| std::env::var_os(name))
            .flatten()
            .map(PathBuf::from)
    };
    let roots = Roots {
        config: &config,
        home: home.as_deref(),
        variable: &variable,
    };
    Ok(account
        .secret
        .resolve(&roots)
        .map(|(location, path)| (location.clone(), path)))
}

/// The account `name` of `backend` as `store` defines it. One no longer
/// defined runs as it was admitted, on the sign-in its directory holds.
pub(crate) fn account(store: &Store<Backend>, backend: Backend, name: &AccountName) -> Account {
    store.get(backend, name).unwrap_or_else(|| {
        crate::accounts::Entry {
            backend,
            name: name.clone(),
            kind: AccountKind::Subscription,
            cap: None,
            enabled: true,
            label: None,
        }
        .into()
    })
}

/// The account's login for a turn or text call, in the form its kind gives,
/// writable back only where [`write_policy`] says so.
pub(crate) fn locate(account: &Account) -> io::Result<Granted> {
    let located = || resolve(account)?.ok_or_else(|| fail("This backend has no login location"));
    let form = match account.kind {
        AccountKind::ApiKey => Form::ApiKey,
        AccountKind::Subscription | AccountKind::Cloud => Form::Session,
    };
    let granted = match (account.id.backend, account.kind) {
        (Backend::Codex, _) => codex_validate(&located()?.1, form).map_err(|error| {
            if account.id.name.is_implicit() {
                fail(error)
            } else {
                fail(&format!(
                    "{error} Set this account up with {}.",
                    sign_in_command(account)
                ))
            }
        }),
        (Backend::Claude | Backend::Grok, _) | (Backend::Antigravity, AccountKind::ApiKey) => {
            file(located()?, account, form)
        }
        (Backend::Antigravity, AccountKind::Subscription | AccountKind::Cloud) => antigravity(&located()?.1),
        (Backend::Local, _) => Err(fail("Local endpoints use no provider login")),
    }?;
    Ok(with_policy(granted, write_policy(account.kind, located()?.0.owner())))
}

/// Whether a turn may write an account's sign-in back, so a token its CLI
/// refreshes is kept: only a subscription kept in a folder Control owns. The
/// operator's own sign-in (`~/.codex`, or a file a variable names) and every
/// API key and cloud credential stay read-only.
pub(crate) fn write_policy(kind: AccountKind, owner: Owner) -> Write {
    match (kind, owner) {
        (AccountKind::Subscription, Owner::Control) => Write::Refreshable,
        (AccountKind::Subscription, Owner::Operator) | (AccountKind::ApiKey | AccountKind::Cloud, _) => Write::ReadOnly,
    }
}
/// The checked login, refreshable when the policy allows and it is a sign-in:
/// an Antigravity account finds a key or cloud credential in its folder too,
/// and those stay read-only whatever the account's kind.
fn with_policy(granted: Granted, write: Write) -> Granted {
    match (granted.form(), write) {
        (Form::Session, Write::Refreshable) => Granted::refreshable(granted.path().to_path_buf(), granted.owner()),
        (Form::Session, Write::ReadOnly)
        | (Form::ApiKey | Form::CloudDefault, Write::ReadOnly | Write::Refreshable) => granted,
    }
}

/// Credential metadata only; the error is the operator's next step.
pub(crate) fn ready(backend: Backend) -> Result<(), String> {
    match backend.agent().login() {
        None => Ok(()),
        Some(login) => locate(&crate::accounts::default_for(backend))
            .map(drop)
            .map_err(|error| login.hint.map_or_else(|| error.to_string(), str::to_owned)),
    }
}

/// How to sign the account in or save its key, for messages and the
/// providers page.
pub(crate) fn sign_in_command(account: &Account) -> String {
    let (id, name) = (account.id.backend.id(), &account.id.name);
    match account.kind {
        AccountKind::ApiKey => format!("control {id} key --account {name}"),
        AccountKind::Subscription | AccountKind::Cloud if !name.is_implicit() => {
            format!("control {id} login --account {name}")
        }
        AccountKind::Subscription | AccountKind::Cloud if account.id.backend == Backend::Codex => {
            "codex login (on the host)".to_owned()
        }
        AccountKind::Subscription | AccountKind::Cloud => format!("control {id} login"),
    }
}

/// A CLI's own sign-in file or a key file: an explicit absolute path, or the
/// file in the account's private login directory.
fn file(located: (Location, PathBuf), account: &Account, form: Form) -> io::Result<Granted> {
    let label = account.id.backend.label();
    let next = || match form {
        Form::ApiKey => format!("Run {} to save its key", sign_in_command(account)),
        Form::Session | Form::CloudDefault => {
            format!("Run {} to sign in inside the Nix container", sign_in_command(account))
        }
    };
    let path = match located {
        (Location::Variable { .. }, path) if !path.is_absolute() => {
            return Err(fail(&format!("{label} credential path must be absolute")));
        }
        (Location::Variable { .. }, path) => path,
        (Location::Config(_) | Location::Home(_), path) => {
            let directory = path.parent().ok_or_else(|| fail("Invalid login location"))?;
            let meta = fs::symlink_metadata(directory).map_err(|_| fail(&next()))?;
            if !meta.is_dir() || meta.mode() & 0o077 != 0 {
                return Err(fail(&format!(
                    "{label} login directory must be private and not a symlink"
                )));
            }
            path
        }
    };
    let meta = fs::symlink_metadata(&path).map_err(|_| fail(&next()))?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
        return Err(fail(&format!(
            "{label} credential must be a private regular file owned by a non-root user"
        )));
    }
    Ok(Granted::located(form, path, owner(&meta)))
}
/// The implicit Codex account's `auth.json`: its override, `CODEX_HOME`, else
/// `~/.codex`. Tests never resolve the operator's own.
#[cfg(test)]
pub(crate) fn codex_path() -> Result<PathBuf, &'static str> {
    resolve(&crate::accounts::default_for(Backend::Codex))
        .ok()
        .flatten()
        .map(|(_, path)| path)
        .ok_or("Cannot locate Codex auth.json. Sign in with codex login on the host.")
}
/// Inspect metadata only. Only Codex inside the worker consumes credential contents.
pub(crate) fn codex_validate(path: &Path, form: Form) -> Result<Granted, &'static str> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "Codex auth.json is missing. Run codex login on the host with file credential storage.")?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.uid() == 0 {
        return Err(
            "Codex auth.json must be a private regular file owned by a non-root user (mode 0600). Symlink credentials are not accepted.",
        );
    }
    let path = fs::canonicalize(path).map_err(|_| "Cannot resolve Codex auth.json.")?;
    control_codex::mount_path(&path)?;
    Ok(Granted::located(form, path, owner(&metadata)))
}

fn private_owned(meta: &fs::Metadata) -> bool {
    meta.mode() & 0o077 == 0 && meta.uid() != 0
}
/// How Antigravity signs in, chosen by which private file the operator
/// created: an API key wins over Application Default Credentials, which win
/// over the Google sign-in `control antigravity login` saved.
pub(crate) fn antigravity(directory: &Path) -> io::Result<Granted> {
    let meta =
        fs::symlink_metadata(directory).map_err(|_| fail("Run control antigravity login, or save a Gemini API key"))?;
    if !meta.is_dir() || !private_owned(&meta) {
        return Err(fail(
            "Antigravity login directory must be private, owned by a non-root user and not a symlink",
        ));
    }
    let file = |name: &str| {
        let path = directory.join(name);
        fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file() && private_owned(m))
            .map(|meta| (path, owner(&meta)))
    };
    [
        ("gemini-api-key", Form::ApiKey),
        ("adc.json", Form::CloudDefault),
        (control_antigravity::SAVED, Form::Session),
    ]
    .into_iter()
    .find_map(|(name, form)| file(name).map(|(path, owner)| Granted::located(form, path, owner)))
    .ok_or_else(|| {
        fail("No Antigravity login: run control antigravity login, or save a private gemini-api-key or adc.json")
    })
}

/// `--account NAME` taken out of a backend subcommand's arguments; the
/// backend itself never sees it.
pub(crate) fn account_argument(args: &[String]) -> io::Result<(AccountName, Vec<String>)> {
    match args.iter().position(|arg| arg == "--account") {
        None => Ok((AccountName::implicit(), args.to_vec())),
        Some(at) => {
            let name = args.get(at + 1).ok_or_else(|| fail("--account needs a name"))?;
            let name = AccountName::try_from(name.as_str()).map_err(|error| fail(&error.to_string()))?;
            let rest = args[..at].iter().chain(&args[at + 2..]).cloned().collect();
            Ok((name, rest))
        }
    }
}

/// One line per account of `backend`: its kind and whether its login is
/// present and private, and its cooldown when `cooling` lists one. Metadata only.
pub(crate) fn report(
    backend: Backend,
    store: &crate::accounts::Store<Backend>,
    cooling: &[crate::providers::accounts::Note],
) -> Vec<String> {
    store
        .of(backend)
        .map(|account| {
            let name = &account.id.name;
            let state = match (locate(&account), account.kind) {
                (Ok(_), AccountKind::ApiKey) => "api-key: present (private file, non-root owner)".to_owned(),
                (Ok(_), AccountKind::Subscription | AccountKind::Cloud) => {
                    "signed in (private file, non-root owner)".to_owned()
                }
                (Err(error), _) => format!("not ready: {error}"),
            };
            let enabled = if account.enabled { "" } else { ", disabled" };
            let cooling = cooling
                .iter()
                .find(|note| note.backend == backend && note.account == *name)
                .map(|note| format!(" · {}", note.note))
                .unwrap_or_default();
            // The one command that recovers this account, whatever its state.
            format!(
                "{}+{name} ({}{enabled}): {state}{cooling} · sign in again with {}",
                backend.id(),
                account.kind.as_str(),
                sign_in_command(&account)
            )
        })
        .collect()
}

/// Runs a backend's operator subcommand (`login`, `check`, `key`) for one account:
/// the backend gets that account's directory and login and never its name.
/// A named account must be in accounts.json first (`control accounts add`).
pub(crate) fn command(
    backend: Backend,
    args: &[String],
    run: fn(control_agent::Command<'_>) -> io::Result<()>,
) -> io::Result<()> {
    let (name, args) = account_argument(args)?;
    let store = crate::providers::accounts::read()?;
    let id = backend.id();
    let account = store.get(backend, &name).ok_or_else(|| {
        fail(&format!(
            "{id} has no account {name}. Add it first: control accounts add {id} {name}"
        ))
    })?;
    let action = args.first().map(String::as_str);
    // A login kept in the operator's own home (Codex's `~/.codex`) is the
    // operator's to sign in there; Control fills only its own directories.
    if action == Some("login") && matches!(resolve(&account)?, Some((Location::Home(_), _))) {
        return Err(fail(&format!(
            "The {} {name} account is your own sign-in on this machine: sign in with {}. To add another, run control accounts add {id} NAME, then control {id} login --account NAME.",
            backend.label(),
            sign_in_command(&account),
        )));
    }
    if action == Some("check") {
        let cooling = crate::providers::accounts::live();
        report(backend, &store, cooling.as_deref().unwrap_or_default())
            .into_iter()
            .chain(cooling.err())
            .for_each(|line| println!("{line}"));
    }
    let directory = directory(backend, &name)?;
    let backend_command = |key| control_agent::Command {
        args: &args,
        node: &crate::agent_host::NODE,
        isolation: crate::engine::isolation(),
        image: backend.image(),
        directory: directory.clone(),
        login: locate(&account),
        key,
    };
    if action != Some("key") {
        return run(backend_command(None));
    }
    if args.len() != 1 {
        return Err(fail(&format!(
            "Use control {id} key --account NAME, with the key on standard input"
        )));
    }
    let key = key_for(&account)?;
    let file = crate::accounts::Backend::key_file(backend).ok_or_else(|| fail(&format!("{id} takes no API key")))?;
    match file.writer {
        KeyWriter::Host => {
            private_directory(&directory)?;
            crate::config::write_in(&directory, file.name, key.expose())?;
        }
        KeyWriter::Cli => run(backend_command(Some(&key)))?,
    }
    locate(&account)?;
    println!("key saved");
    Ok(())
}

/// The key for an API-key account, read once from standard input: never
/// from a terminal, where it would echo, and never from an argument.
fn key_for(account: &Account) -> io::Result<ApiKey> {
    use std::io::IsTerminal;
    let (id, name) = (account.id.backend.id(), &account.id.name);
    match account.kind {
        AccountKind::ApiKey => {}
        AccountKind::Subscription | AccountKind::Cloud => {
            return Err(fail(&format!(
                "{id}+{name} is a {} account. Add an API-key account first: control accounts add {id} NAME --kind api-key",
                account.kind.as_str()
            )));
        }
    }
    let input = io::stdin();
    if input.is_terminal() {
        return Err(fail(&format!(
            "Pipe the key in on standard input, e.g. pbpaste | control {id} key --account {name}. It is never read from a terminal or an argument."
        )));
    }
    ApiKey::read(input.lock()).map_err(|error| fail(&format!("No key saved: {error}.")))
}

/// Creates the account's login directory privately, or checks the one there.
fn private_directory(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(directory)?;
    let meta = fs::symlink_metadata(directory)?;
    if !meta.is_dir() || !private_owned(&meta) {
        return Err(fail(
            "The account's login directory must be private, owned by a non-root user and not a symlink",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
