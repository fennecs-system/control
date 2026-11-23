//! Where each account's login lives on this node, located and checked by the
//! host and handed to a turn as a [`Granted`] value. Metadata only: no
//! credential is ever read. The implicit `default` account is at exactly the
//! path its backend used to locate itself; a named account's login is in
//! `<config>/accounts/BACKEND/NAME/`. Backends never learn which account
//! they were handed.
use crate::accounts::{AccountName, Location, Roots};
use crate::backend::Backend;
use control_agent::{Form, Granted};
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
fn resolve(backend: Backend, account: &AccountName) -> io::Result<Option<(Location, PathBuf)>> {
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
    let secret = crate::accounts::secret(backend, account);
    let roots = Roots {
        config: &config,
        home: home.as_deref(),
        variable: &variable,
    };
    Ok(secret.resolve(&roots).map(|(location, path)| (location.clone(), path)))
}

/// The account's login for a turn or text call.
pub(crate) fn locate(backend: Backend, account: &AccountName) -> io::Result<Granted> {
    let located = || resolve(backend, account)?.ok_or_else(|| fail("This backend has no login location"));
    match backend {
        Backend::Codex => codex_validate(&located()?.1).map_err(|error| {
            if account.is_implicit() {
                fail(error)
            } else {
                fail(&format!(
                    "{error} Sign this account in with {}.",
                    login_command(backend, account)
                ))
            }
        }),
        Backend::Claude | Backend::Grok => file(located()?, backend, account),
        Backend::Antigravity => antigravity(&located()?.1),
        Backend::Local => Err(fail("Local endpoints use no provider login")),
    }
}

/// Credential metadata only; the error is the operator's next step.
pub(crate) fn ready(backend: Backend) -> Result<(), String> {
    match backend.agent().login() {
        None => Ok(()),
        Some(login) => locate(backend, &AccountName::implicit())
            .map(drop)
            .map_err(|error| login.hint.map_or_else(|| error.to_string(), str::to_owned)),
    }
}

/// How to sign the account in, for messages and the providers page.
pub(crate) fn login_command(backend: Backend, account: &AccountName) -> String {
    if !account.is_implicit() {
        format!("control {} login --account {account}", backend.id())
    } else if backend == Backend::Codex {
        "codex login (on the host)".to_owned()
    } else {
        format!("control {} login", backend.id())
    }
}

/// A CLI's own sign-in file: an explicit absolute path, or the file in the
/// account's private login directory.
fn file(located: (Location, PathBuf), backend: Backend, account: &AccountName) -> io::Result<Granted> {
    let label = backend.label();
    let path = match located {
        (Location::Variable { .. }, path) if !path.is_absolute() => {
            return Err(fail(&format!("{label} credential path must be absolute")));
        }
        (Location::Variable { .. }, path) => path,
        (Location::Config(_) | Location::Home(_), path) => {
            let directory = path.parent().ok_or_else(|| fail("Invalid login location"))?;
            let meta = fs::symlink_metadata(directory)
                .map_err(|_| fail(&format!("Run {} to sign in", login_command(backend, account))))?;
            if !meta.is_dir() || meta.mode() & 0o077 != 0 {
                return Err(fail(&format!(
                    "{label} login directory must be private and not a symlink"
                )));
            }
            path
        }
    };
    let meta = fs::symlink_metadata(&path).map_err(|_| {
        fail(&format!(
            "Run {} to sign in inside the Nix container",
            login_command(backend, account)
        ))
    })?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
        return Err(fail(&format!(
            "{label} credential must be a private regular file owned by a non-root user"
        )));
    }
    Ok(Granted::located(Form::Session, path, owner(&meta)))
}
/// The implicit Codex account's `auth.json`: its override, `CODEX_HOME`, else
/// `~/.codex`. Tests never resolve the operator's own.
#[cfg(test)]
pub(crate) fn codex_path() -> Result<PathBuf, &'static str> {
    resolve(Backend::Codex, &AccountName::implicit())
        .ok()
        .flatten()
        .map(|(_, path)| path)
        .ok_or("Cannot locate Codex auth.json. Sign in with codex login on the host.")
}
/// Inspect metadata only. Only Codex inside the worker consumes credential contents.
pub(crate) fn codex_validate(path: &Path) -> Result<Granted, &'static str> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "Codex auth.json is missing. Run codex login on the host with file credential storage.")?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.uid() == 0 {
        return Err(
            "Codex auth.json must be a private regular file owned by a non-root user (mode 0600). Symlink credentials are not accepted.",
        );
    }
    let path = fs::canonicalize(path).map_err(|_| "Cannot resolve Codex auth.json.")?;
    control_codex::mount_path(&path)?;
    Ok(Granted::located(Form::Session, path, owner(&metadata)))
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
/// present and private. Metadata only.
pub(crate) fn report(backend: Backend, store: &crate::accounts::Store<Backend>) -> Vec<String> {
    store
        .of(backend)
        .map(|account| {
            let name = &account.id.name;
            let state = match locate(backend, name) {
                Ok(_) => "signed in (private file, non-root owner)".to_owned(),
                Err(error) => format!("not ready: {error}"),
            };
            let enabled = if account.enabled { "" } else { ", disabled" };
            format!("{}+{name} ({}{enabled}): {state}", backend.id(), account.kind.as_str())
        })
        .collect()
}

/// Runs a backend's operator subcommand (`login`, `check`) for one account:
/// the backend gets that account's directory and login and never its name.
/// A named account must be in accounts.json first (`control accounts add`).
pub(crate) fn command(
    backend: Backend,
    args: &[String],
    run: fn(control_agent::Command<'_>) -> io::Result<()>,
) -> io::Result<()> {
    let (account, args) = account_argument(args)?;
    let store = crate::providers::accounts::read()?;
    if store.get(backend, &account).is_none() {
        return Err(fail(&format!(
            "{} has no account {account}. Add it first: control accounts add {} {account}",
            backend.id(),
            backend.id()
        )));
    }
    // A login kept in the operator's own home (Codex's `~/.codex`) is the
    // operator's to sign in there; Control fills only its own directories.
    if args.first().is_some_and(|arg| arg == "login")
        && matches!(resolve(backend, &account)?, Some((Location::Home(_), _)))
    {
        return Err(fail(&format!(
            "The {} {account} account is your own sign-in on this machine: sign in with {}. To add another, run control accounts add {} NAME, then control {} login --account NAME.",
            backend.label(),
            login_command(backend, &account),
            backend.id(),
            backend.id()
        )));
    }
    if args.first().is_some_and(|arg| arg == "check") {
        report(backend, &store).iter().for_each(|line| println!("{line}"));
    }
    run(control_agent::Command {
        args: &args,
        node: &crate::agent_host::NODE,
        isolation: crate::engine::isolation(),
        image: backend.image(),
        directory: directory(backend, &account)?,
        login: locate(backend, &account),
    })
}

#[cfg(test)]
mod tests;
