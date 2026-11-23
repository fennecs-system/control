//! Named accounts of a backend: pure types and one loader.
//!
//! An account is one sign-in or key of a backend (`codex+second:MODEL`). Every
//! backend has an implicit account named `default`: its credential where it is
//! kept today, so a node without `<config>/accounts.json` behaves exactly as
//! before. Nothing here reads a credential; a [`SecretRef`] only says where the
//! host finds one. The module is generic over the backend type and names no
//! application state, so it can move to a leaf crate unchanged.
//! Examples and the file format are in `accounts/tests.rs` and docs/LOCAL-MODELS.md.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fmt, io,
    num::NonZeroU8,
    path::{Component, Path, PathBuf},
};

/// What accounts need from a backend identifier.
pub trait Backend: Copy + Eq + fmt::Debug + Serialize + DeserializeOwned {
    /// The ID the operator types (`codex`).
    fn id(self) -> &'static str;
    /// The implicit `default` account's kind and credential location: where
    /// the backend keeps its sign-in today, overrides included.
    fn implicit(self) -> (AccountKind, SecretRef);
}

/// An account's name: `[a-z0-9-]{1,32}`. `default` is reserved for the
/// implicit account, so accounts.json cannot define it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountName(String);
const IMPLICIT: &str = "default";
/// The account-choice text that selects the pool.
pub const POOL: &str = "*";
impl AccountName {
    pub const MAX: usize = 32;
    /// The implicit account every backend has.
    pub fn implicit() -> Self {
        Self(IMPLICIT.into())
    }
    pub fn is_implicit(&self) -> bool {
        self.0 == IMPLICIT
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Default for AccountName {
    fn default() -> Self {
        Self::implicit()
    }
}
/// Why text is not an account name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    /// The first character outside `a-z`, `0-9` and `-`.
    Character(char),
}
impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("an account name is empty"),
            Self::TooLong => write!(f, "account names are at most {} characters", AccountName::MAX),
            Self::Character(c) => write!(f, "account names use a-z, 0-9 and - only (found {c:?})"),
        }
    }
}
impl TryFrom<&str> for AccountName {
    type Error = NameError;
    fn try_from(text: &str) -> Result<Self, NameError> {
        if text.is_empty() {
            return Err(NameError::Empty);
        }
        if text.len() > Self::MAX {
            return Err(NameError::TooLong);
        }
        match text.chars().find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '-')) {
            Some(c) => Err(NameError::Character(c)),
            None => Ok(Self(text.into())),
        }
    }
}
impl TryFrom<String> for AccountName {
    type Error = NameError;
    fn try_from(text: String) -> Result<Self, NameError> {
        Self::try_from(text.as_str())
    }
}
impl From<AccountName> for String {
    fn from(name: AccountName) -> Self {
        name.0
    }
}
impl fmt::Display for AccountName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which account of a backend a selection names. Stored as an optional
/// string: absent is `Default`, `*` is `Pool`, anything else a name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AccountChoice {
    /// The backend's default account (accounts.json `defaults`, else `default`),
    /// decided when work is admitted.
    #[default]
    Default,
    Named(AccountName),
    /// Any enabled account of the backend, picked once at admission.
    Pool,
}
impl AccountChoice {
    pub fn is_default(&self) -> bool {
        *self == Self::Default
    }
    /// The `+ACCOUNT` suffix an operator would type, or none for the default
    /// and the implicit account, so existing selections look unchanged.
    pub fn suffix(&self) -> Option<&str> {
        match self {
            Self::Default => None,
            Self::Named(name) if name.is_implicit() => None,
            Self::Named(name) => Some(name.as_str()),
            Self::Pool => Some(POOL),
        }
    }
}
impl From<AccountName> for AccountChoice {
    fn from(name: AccountName) -> Self {
        Self::Named(name)
    }
}
impl Serialize for AccountChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => serializer.serialize_none(),
            Self::Named(name) => serializer.serialize_str(name.as_str()),
            Self::Pool => serializer.serialize_str(POOL),
        }
    }
}
impl<'de> Deserialize<'de> for AccountChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<String>::deserialize(deserializer)? {
            None => Self::Default,
            Some(text) if text == POOL => Self::Pool,
            Some(text) => Self::Named(AccountName::try_from(text).map_err(serde::de::Error::custom)?),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// A vendor sign-in (Codex, Claude, Grok or Google subscription).
    Subscription,
    ApiKey,
    /// Cloud provider credentials such as Google Application Default Credentials.
    Cloud,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountId<B> {
    pub backend: B,
    pub name: AccountName,
}

/// Operator-facing text naming an account, at most [`Label::MAX`] characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Label(String);
impl Label {
    pub const MAX: usize = 64;
}
impl TryFrom<String> for Label {
    type Error = &'static str;
    fn try_from(text: String) -> Result<Self, &'static str> {
        if text.trim().is_empty() || text.chars().count() > Self::MAX || text.chars().any(char::is_control) {
            return Err("an account label is 1–64 printable characters");
        }
        Ok(Self(text))
    }
}
impl From<Label> for String {
    fn from(label: Label) -> Self {
        label.0
    }
}

/// A relative path of plain components, resolved under a root the host owns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct RelativePath(PathBuf);
impl TryFrom<PathBuf> for RelativePath {
    type Error = &'static str;
    fn try_from(path: PathBuf) -> Result<Self, &'static str> {
        let plain = path.components().all(|c| matches!(c, Component::Normal(_)));
        match path.components().next() {
            Some(_) if plain => Ok(Self(path)),
            _ => Err("a credential path under the configuration directory must be relative, without . or .."),
        }
    }
}
impl TryFrom<&str> for RelativePath {
    type Error = &'static str;
    fn try_from(path: &str) -> Result<Self, &'static str> {
        Self::try_from(PathBuf::from(path))
    }
}
impl From<RelativePath> for PathBuf {
    fn from(path: RelativePath) -> Self {
        path.0
    }
}
/// An absolute path without `..` components.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct AbsolutePath(PathBuf);
impl TryFrom<PathBuf> for AbsolutePath {
    type Error = &'static str;
    fn try_from(path: PathBuf) -> Result<Self, &'static str> {
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return Err("a credential path must be absolute, without ..");
        }
        Ok(Self(path))
    }
}
impl From<AbsolutePath> for PathBuf {
    fn from(path: AbsolutePath) -> Self {
        path.0
    }
}

/// One place a credential may be. Only the host turns it into a path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    /// The path an environment variable names, joined with `join` when set
    /// (`CONTROL_CLAUDE_AUTH_FILE`; `CODEX_HOME` + `auth.json`).
    Variable {
        name: &'static str,
        join: Option<RelativePath>,
    },
    /// Under the operator's home directory (Codex's own `~/.codex`).
    Home(RelativePath),
    /// Under the node configuration directory.
    Config(RelativePath),
    Path(AbsolutePath),
}
/// Where the host finds an account's credential: the first location whose
/// root is available (a set variable, a known home). Empty when the backend
/// needs none. A reference only, never contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecretRef(pub Vec<Location>);
/// The roots a [`SecretRef`] is resolved against, supplied by the host so the
/// resolution itself is pure (and tests can supply their own).
pub struct Roots<'a> {
    pub config: &'a Path,
    pub home: Option<&'a Path>,
    /// An environment variable's value, when the host lets it count.
    pub variable: &'a dyn Fn(&str) -> Option<PathBuf>,
}
impl SecretRef {
    /// The first location whose root is available, with the path it names.
    /// Nothing is checked on disk.
    pub fn resolve(&self, roots: &Roots<'_>) -> Option<(&Location, PathBuf)> {
        self.0.iter().find_map(|location| {
            let path = match location {
                Location::Variable { name, join } => {
                    let value = (roots.variable)(name)?;
                    join.as_ref().map_or_else(|| value.clone(), |join| value.join(&join.0))
                }
                Location::Home(path) => roots.home?.join(&path.0),
                Location::Config(path) => roots.config.join(&path.0),
                Location::Path(path) => path.0.clone(),
            };
            Some((location, path))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account<B> {
    pub id: AccountId<B>,
    pub kind: AccountKind,
    pub secret: SecretRef,
    /// The most attempts it may run at once; none: only the node's limits.
    pub cap: Option<NonZeroU8>,
    pub enabled: bool,
    pub label: Option<Label>,
}

/// The implicit `default` account: today's credential location, always enabled.
pub fn default_for<B: Backend>(backend: B) -> Account<B> {
    let (kind, secret) = backend.implicit();
    Account {
        id: AccountId {
            backend,
            name: AccountName::implicit(),
        },
        kind,
        secret,
        cap: None,
        enabled: true,
        label: None,
    }
}

/// `<config>/accounts.json`, validated. Empty when the file is missing: every
/// backend then has only its implicit account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store<B> {
    defaults: Vec<(B, AccountName)>,
    accounts: Vec<Account<B>>,
}
impl<B> Default for Store<B> {
    fn default() -> Self {
        Self {
            defaults: Vec::new(),
            accounts: Vec::new(),
        }
    }
}

/// Why accounts.json was refused; nothing in it is used then.
#[derive(Debug)]
pub enum LoadError<B> {
    Syntax(serde_json::Error),
    /// An account named `default`: that is the implicit account.
    Reserved(B),
    Duplicate(AccountId<B>),
    /// A `defaults` entry naming an account the file does not define as enabled,
    /// so the default account is always usable.
    Default(B, AccountName),
    /// A backend named twice in `defaults`.
    DefaultTwice(B),
}
impl<B: Backend> fmt::Display for LoadError<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(f, "accounts.json is invalid: {error}"),
            Self::Reserved(backend) => write!(
                f,
                "accounts.json defines a {} account named default; that name is the implicit account",
                backend.id()
            ),
            Self::Duplicate(id) => write!(f, "accounts.json defines {}+{} twice", id.backend.id(), id.name),
            Self::Default(backend, name) => write!(
                f,
                "accounts.json defaults {} to {name}, which it does not define as an enabled account",
                backend.id()
            ),
            Self::DefaultTwice(backend) => write!(f, "accounts.json names a default for {} twice", backend.id()),
        }
    }
}
impl<B: Backend> From<LoadError<B>> for io::Error {
    fn from(error: LoadError<B>) -> Self {
        io::Error::other(error.to_string())
    }
}

/// Why an account choice cannot run; `existing` lists the backend's accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal<B> {
    pub backend: B,
    pub name: AccountName,
    pub why: Unusable,
    pub existing: Vec<AccountName>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unusable {
    Missing,
    Disabled,
}

impl<B: Backend> Store<B> {
    /// Pure: file bytes to a validated store.
    pub fn parse(bytes: &[u8]) -> Result<Self, LoadError<B>> {
        let file: File<B> = serde_json::from_slice(bytes).map_err(LoadError::Syntax)?;
        let accounts: Vec<Account<B>> = file.accounts.into_iter().map(Account::from).collect();
        accounts.iter().enumerate().try_for_each(|(at, account)| {
            if account.id.name.is_implicit() {
                return Err(LoadError::Reserved(account.id.backend));
            }
            if accounts[..at].iter().any(|other| other.id == account.id) {
                return Err(LoadError::Duplicate(account.id.clone()));
            }
            Ok(())
        })?;
        let store = Self {
            defaults: file.defaults,
            accounts,
        };
        store
            .defaults
            .iter()
            .enumerate()
            .try_for_each(|(at, (backend, name))| {
                if store.defaults[..at].iter().any(|(other, _)| other == backend) {
                    return Err(LoadError::DefaultTwice(*backend));
                }
                match store.get(*backend, name).filter(|account| account.enabled) {
                    None => Err(LoadError::Default(*backend, name.clone())),
                    Some(_) => Ok(()),
                }
            })?;
        Ok(store)
    }
    /// The account an unqualified selection of `backend` uses.
    pub fn default_name(&self, backend: B) -> AccountName {
        self.defaults
            .iter()
            .find(|(b, _)| *b == backend)
            .map_or_else(AccountName::implicit, |(_, name)| name.clone())
    }
    /// Every account of `backend`, the implicit one first.
    pub fn of(&self, backend: B) -> impl Iterator<Item = Account<B>> + '_ {
        std::iter::once(default_for(backend))
            .chain(self.accounts.iter().filter(move |a| a.id.backend == backend).cloned())
    }
    pub fn get(&self, backend: B, name: &AccountName) -> Option<Account<B>> {
        self.of(backend).find(|a| a.id.name == *name)
    }
    /// The account a choice runs on. A pool takes the enabled account with the
    /// least `load`, the first on a tie; the implicit account is always enabled,
    /// so a pool always has one.
    pub fn choose(
        &self,
        backend: B,
        choice: &AccountChoice,
        load: impl Fn(&AccountName) -> usize,
    ) -> Result<Account<B>, Refusal<B>> {
        let named = |name: AccountName| match self.get(backend, &name) {
            Some(account) if account.enabled => Ok(account),
            found => Err(Refusal {
                backend,
                why: if found.is_some() {
                    Unusable::Disabled
                } else {
                    Unusable::Missing
                },
                name,
                existing: self.of(backend).map(|a| a.id.name).collect(),
            }),
        };
        match choice {
            // Never refused: `parse` admits only enabled defaults.
            AccountChoice::Default => named(self.default_name(backend)),
            AccountChoice::Named(name) => named(name.clone()),
            AccountChoice::Pool => Ok(self
                .of(backend)
                .filter(|a| a.enabled)
                .map(|a| (load(&a.id.name), a))
                .reduce(|best, next| if next.0 < best.0 { next } else { best })
                .map_or_else(|| default_for(backend), |(_, account)| account)),
        }
    }
}

/// The thin reader: `<directory>/accounts.json`, or the empty store when it
/// does not exist. Nothing is written; an operator adds accounts by hand.
pub fn read<B: Backend>(directory: &Path) -> io::Result<Store<B>> {
    match std::fs::read(directory.join("accounts.json")) {
        Ok(bytes) => Ok(Store::parse(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Store::default()),
        Err(error) => Err(error),
    }
}

/// accounts.json as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, bound = "B: Backend")]
struct File<B> {
    #[serde(default, deserialize_with = "pairs")]
    defaults: Vec<(B, AccountName)>,
    #[serde(default)]
    accounts: Vec<Entry<B>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, bound = "B: Backend")]
struct Entry<B> {
    backend: B,
    name: AccountName,
    kind: AccountKind,
    secret: Secret,
    #[serde(default)]
    cap: Option<NonZeroU8>,
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    label: Option<Label>,
}
fn enabled() -> bool {
    true
}
/// Where a named account's credential is: under the configuration directory,
/// or an absolute path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Secret {
    Config(RelativePath),
    Path(AbsolutePath),
}
impl<B> From<Entry<B>> for Account<B> {
    fn from(entry: Entry<B>) -> Self {
        Self {
            id: AccountId {
                backend: entry.backend,
                name: entry.name,
            },
            kind: entry.kind,
            secret: SecretRef(vec![match entry.secret {
                Secret::Config(path) => Location::Config(path),
                Secret::Path(path) => Location::Path(path),
            }]),
            cap: entry.cap,
            enabled: entry.enabled,
            label: entry.label,
        }
    }
}
/// A JSON object keyed by backend, kept in file order without needing the
/// backend type to hash or order.
fn pairs<'de, D: serde::Deserializer<'de>, B: Deserialize<'de>>(
    deserializer: D,
) -> Result<Vec<(B, AccountName)>, D::Error> {
    struct Pairs<B>(std::marker::PhantomData<B>);
    impl<'de, B: Deserialize<'de>> serde::de::Visitor<'de> for Pairs<B> {
        type Value = Vec<(B, AccountName)>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object from backend to account name")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            std::iter::from_fn(|| map.next_entry().transpose()).collect()
        }
    }
    deserializer.deserialize_map(Pairs(std::marker::PhantomData))
}

#[cfg(test)]
mod tests;
