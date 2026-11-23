//! Host glue for `crate::accounts`: the backends' login locations, the
//! node's accounts.json and accounts-status.json, and the load and holds
//! admission applies.
use crate::accounts::{
    AccountKind, AccountName, Hold, KeyFile, KeyWriter, Location, RelativePath, SecretRef, Store, UnixSeconds,
};
use crate::backend::Backend;
use std::{io, path::PathBuf, time::SystemTime};

/// Where each backend's login is kept: the one table of these locations.
/// `crate::logins` resolves an account's reference and checks it before
/// handing the login to a backend.
impl crate::accounts::Backend for Backend {
    fn id(self) -> &'static str {
        Backend::id(self)
    }
    fn login_file(self) -> Option<&'static str> {
        match self {
            Self::Codex | Self::Grok => Some("auth.json"),
            Self::Claude => Some(".credentials.json"),
            // A directory: the sign-in is chosen by which file it holds.
            Self::Antigravity | Self::Local => None,
        }
    }
    /// Each CLI's documented API-key path; the backend crate decides how
    /// its CLI is handed the file (docs/LOCAL-MODELS.md).
    fn key_file(self) -> Option<KeyFile> {
        let host = |name| {
            Some(KeyFile {
                name,
                writer: KeyWriter::Host,
            })
        };
        match self {
            // `codex login --with-api-key` writes the key into auth.json.
            Self::Codex => Some(KeyFile {
                name: "auth.json",
                writer: KeyWriter::Cli,
            }),
            Self::Claude => host("anthropic-api-key"),
            Self::Grok => host("xai-api-key"),
            Self::Antigravity => host("gemini-api-key"),
            Self::Local => None,
        }
    }
    fn signs_in(self) -> bool {
        self != Self::Local
    }
    fn refreshes(self) -> bool {
        self == Self::Codex
    }
    fn implicit(self) -> (AccountKind, SecretRef) {
        let variable = |name: &'static str, join: Option<&'static str>| Location::Variable {
            name,
            join: join.map(fixed),
        };
        let file = self.login_file().unwrap_or_default();
        let locations = match self {
            Self::Codex => vec![
                variable("CONTROL_CODEX_AUTH_FILE", None),
                variable("CODEX_HOME", Some(file)),
                Location::Home(fixed(".codex/auth.json")),
            ],
            Self::Claude => vec![
                variable("CONTROL_CLAUDE_AUTH_FILE", None),
                Location::Config(under(self)),
            ],
            Self::Grok => vec![variable("CONTROL_GROK_AUTH_FILE", None), Location::Config(under(self))],
            Self::Antigravity => vec![Location::Config(under(self))],
            // An endpoint entry in model.json carries its own connection.
            Self::Local => Vec::new(),
        };
        let kind = match self {
            Self::Local => AccountKind::ApiKey,
            Self::Codex | Self::Claude | Self::Grok | Self::Antigravity => AccountKind::Subscription,
        };
        (kind, SecretRef(locations))
    }
}
fn fixed(path: &'static str) -> RelativePath {
    RelativePath::try_from(path).expect("a fixed relative path")
}
/// The login in the backend's own directory, `<config>/ID`, which `control ID login` fills.
fn under(backend: Backend) -> RelativePath {
    let directory = PathBuf::from(crate::accounts::Backend::id(backend));
    let path =
        crate::accounts::Backend::login_file(backend).map_or_else(|| directory.clone(), |file| directory.join(file));
    RelativePath::try_from(path).expect("a backend ID is one plain component")
}

/// The node's accounts.json; a missing file is the empty store.
pub(crate) fn read() -> io::Result<Store<Backend>> {
    crate::accounts::read(&crate::config::directory().map_err(io::Error::other)?)
}
/// Saves accounts.json privately; the server picks the change up (`refresh`).
pub(crate) fn write(store: &Store<Backend>) -> io::Result<()> {
    crate::config::write_private("accounts.json", &store.encode())
}
/// Reloads accounts.json when it changed on disk since it was read, so
/// `control accounts` changes apply without a restart. An invalid file keeps
/// the accounts already loaded and says why.
pub(crate) fn refresh(runtime: &mut crate::runtime::Runtime) {
    let modified = crate::config::directory()
        .ok()
        .and_then(|directory| std::fs::metadata(directory.join("accounts.json")).ok())
        .and_then(|meta| meta.modified().ok());
    if modified == runtime.accounts_read {
        return;
    }
    runtime.accounts_read = modified;
    match read() {
        Ok(store) => runtime.accounts = store,
        Err(error) => runtime.notice = format!("accounts.json was not reloaded: {error}"),
    }
}

pub(crate) fn now() -> UnixSeconds {
    UnixSeconds(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
}

/// Queued and running work captured on each account of `backend`: what a pool
/// balances. Finished runs hold nothing.
pub(crate) fn load(runs: &[crate::runtime::Run], backend: Backend) -> impl Fn(&AccountName) -> usize + '_ {
    move |name| {
        runs.iter()
            .filter(|run| {
                crate::application::runs::status::active(&run.status)
                    && run.target.backend == backend
                    && run.target.account == *name
            })
            .count()
    }
}
/// Whether the account is cooling down now: what a pool skips.
pub(crate) fn cooling(runtime: &crate::runtime::Runtime, backend: Backend) -> impl Fn(&AccountName) -> bool + '_ {
    let now = now();
    move |name| runtime.account_status.cooling(backend, name, now).is_some()
}

/// Why `run` may not start on its captured account now: its account runs
/// as many attempts as its cap allows, or is cooling down. An account no
/// longer in accounts.json starts as it was admitted.
pub(crate) fn hold(runtime: &crate::runtime::Runtime, run: &crate::runtime::Run) -> Option<Hold> {
    let target = &run.target;
    let account = runtime.accounts.get(target.backend, &target.account)?;
    let running = runtime
        .runs
        .iter()
        .filter(|other| {
            control_core::scheduling::RunView::occupies_slot(*other)
                && other.target.backend == target.backend
                && other.target.account == target.account
        })
        .count();
    crate::accounts::may_start(&account, &runtime.account_status, running, now()).err()
}
/// A held run's notice in its chat.
pub(crate) fn held(hold: &Hold, run: &crate::runtime::Run) -> String {
    let account = format!("{}+{}", run.target.backend.id(), run.target.account);
    match hold {
        Hold::Cap(cap) => format!("Queued · {account} is running its limit of {cap} at once"),
        Hold::Cooling(_) => format!(
            "Queued · {account} reported a usage limit; it waits until it is cleared on Manage → Models (no other account is used)"
        ),
    }
}

/// Marks the account cooling down after a usage limit; the caller saves the
/// state. The provider gives no retry time Control can read, so it holds
/// until an operator clears it.
pub(crate) fn cool(runtime: &mut crate::runtime::Runtime, backend: Backend, account: &AccountName) {
    runtime.account_status = runtime.account_status.cool(backend, account, now(), None);
}
/// Clears an account's cooldown; its held attempts may start. The caller saves.
pub(crate) fn clear(runtime: &mut crate::runtime::Runtime, backend: Backend, account: &AccountName) {
    runtime.account_status = runtime.account_status.clear(backend, account);
}

/// `control accounts …`, parsed once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    List,
    Add(crate::accounts::Entry<Backend>),
    Remove(Backend, AccountName),
    Default(Backend, AccountName),
}
pub(crate) const USAGE: &str = "Use control accounts list | add BACKEND NAME [--kind subscription|api-key|cloud] [--cap N] [--label TEXT] [--disabled] | remove BACKEND NAME | default BACKEND NAME";
impl TryFrom<&[String]> for Command {
    type Error = String;
    fn try_from(args: &[String]) -> Result<Self, String> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let backend = |id: &str| {
            Backend::named(id).ok_or_else(|| {
                let ids = Backend::ALL.map(Backend::id).join(", ");
                format!("Unknown backend {id:?}. Backends: {ids}.")
            })
        };
        let name = |text: &str| AccountName::try_from(text).map_err(|error| format!("Invalid account: {error}."));
        match words.as_slice() {
            [] | ["list"] => Ok(Self::List),
            ["remove", id, account] => Ok(Self::Remove(backend(id)?, name(account)?)),
            ["default", id, account] => Ok(Self::Default(backend(id)?, name(account)?)),
            ["add", id, account, options @ ..] => {
                let mut entry = crate::accounts::Entry {
                    backend: backend(id)?,
                    name: name(account)?,
                    kind: AccountKind::Subscription,
                    cap: None,
                    enabled: true,
                    label: None,
                };
                let mut rest = options;
                while let Some((option, tail)) = rest.split_first() {
                    rest = match (*option, tail) {
                        ("--disabled", tail) => {
                            entry.enabled = false;
                            tail
                        }
                        ("--kind", [kind, tail @ ..]) => {
                            entry.kind = AccountKind::try_from(*kind).map_err(str::to_owned)?;
                            tail
                        }
                        ("--cap", [cap, tail @ ..]) => {
                            entry.cap = Some(cap.parse().map_err(|_| "--cap is a number from 1 to 255".to_owned())?);
                            tail
                        }
                        ("--label", [label, tail @ ..]) => {
                            entry.label =
                                Some(crate::accounts::Label::try_from((*label).to_owned()).map_err(str::to_owned)?);
                            tail
                        }
                        _ => return Err(USAGE.into()),
                    };
                }
                Ok(Self::Add(entry))
            }
            _ => Err(USAGE.into()),
        }
    }
}
/// Runs `control accounts …`: reads accounts.json, applies the change and
/// saves it privately. A running Control picks the change up by itself.
pub(crate) fn command(args: &[String]) -> io::Result<()> {
    let command = Command::try_from(args).map_err(io::Error::other)?;
    let store = read()?;
    let changed = match &command {
        Command::List => {
            Backend::hosted()
                .flat_map(|backend| listing(backend, &store))
                .for_each(|line| println!("{line}"));
            return Ok(());
        }
        Command::Add(entry) => store.add(entry.clone()),
        Command::Remove(backend, name) => store.remove(*backend, name),
        Command::Default(backend, name) => store.set_default(*backend, name),
    }?;
    write(&changed)?;
    match command {
        Command::Add(entry) => println!(
            "Added {}+{} ({}). Set it up with {}.",
            entry.backend.id(),
            entry.name,
            entry.kind.as_str(),
            crate::logins::sign_in_command(&entry.clone().into())
        ),
        Command::Remove(backend, name) => println!(
            "Removed {}+{name}. Its login directory was left in place: {}",
            backend.id(),
            crate::logins::directory(backend, &name)?.display()
        ),
        Command::Default(backend, name) => println!(
            "{} now runs on {name} unless a selection names an account.",
            backend.id()
        ),
        Command::List => {}
    }
    Ok(())
}
/// One line per account of `backend` for `control accounts list`.
fn listing(backend: Backend, store: &Store<Backend>) -> Vec<String> {
    let default = store.default_name(backend);
    crate::logins::report(backend, store)
        .into_iter()
        .zip(store.of(backend))
        .map(|(line, account)| {
            let name = &account.id.name;
            let mut notes = Vec::new();
            if *name == default {
                notes.push("default".to_owned());
            }
            if let Some(cap) = account.effective_cap() {
                notes.push(format!("cap {cap}"));
            }
            if notes.is_empty() {
                line
            } else {
                format!("{line} [{}]", notes.join(", "))
            }
        })
        .collect()
}
