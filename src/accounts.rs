//! Named accounts of a backend: pure types and one loader.
//!
//! An account is one sign-in or key of a backend (`codex+second:MODEL`). Every
//! backend has an implicit account named `default`: its credential where it is
//! kept today, so a node without `<config>/accounts.json` behaves exactly as
//! before. Nothing here reads a credential; a [`SecretRef`] only says where the
//! host finds one. The module is generic over the backend type and names no
//! application state, so it can move to a leaf crate unchanged.
//! Examples and the file format are in `accounts/tests.rs` and docs/LOCAL-MODELS.md.
//! A named account's login lives in `<config>/accounts/BACKEND/NAME/`, holding
//! the same file the backend's default location holds.
use serde::{Deserialize, Serialize, de::DeserializeOwned, ser::SerializeMap};
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
    /// The file a subscription's login directory holds, `None` when the
    /// directory itself is the login (Antigravity picks a file inside it).
    fn login_file(self) -> Option<&'static str>;
    /// Where an API-key account keeps its key; `None` when the backend takes
    /// no key.
    fn key_file(self) -> Option<KeyFile>;
    /// Whether the backend signs in at all; local endpoints do not, so they
    /// have no named accounts.
    fn signs_in(self) -> bool;
}

/// The file an API-key account's directory holds, and who writes it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyFile {
    pub name: &'static str,
    pub writer: KeyWriter,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyWriter {
    /// Control saves the key as given, privately.
    Host,
    /// The CLI's own API-key login turns the key into its sign-in file
    /// (Codex's `auth.json`), run once in its pinned image; the key is this
    /// JSON field of that file, which is where the host's key relay reads it.
    Cli { field: &'static str },
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
impl AccountKind {
    pub const ALL: [Self; 3] = [Self::Subscription, Self::ApiKey, Self::Cloud];
    /// The kind as the operator types and reads it. accounts.json keeps
    /// `api_key`, which `--kind` also accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subscription => "subscription",
            Self::ApiKey => "api-key",
            Self::Cloud => "cloud",
        }
    }
}
impl TryFrom<&str> for AccountKind {
    type Error = &'static str;
    fn try_from(text: &str) -> Result<Self, &'static str> {
        let text = text.replace('_', "-");
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or("an account kind is subscription, api-key or cloud")
    }
}
/// The kinds a pool would mix, in the order its accounts list them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixedKinds(pub Vec<AccountKind>);
/// Whether accounts of these kinds may form one pool: only when they are all
/// one kind. Kinds bill differently (a subscription's allowance, a key's
/// metered spend), so a pool never switches between them silently.
pub fn one_kind(kinds: impl IntoIterator<Item = AccountKind>) -> Result<(), MixedKinds> {
    let distinct = kinds.into_iter().fold(Vec::new(), |mut seen, kind| {
        if !seen.contains(&kind) {
            seen.push(kind);
        }
        seen
    });
    match distinct.as_slice() {
        [] | [_] => Ok(()),
        _ => Err(MixedKinds(distinct)),
    }
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

impl Label {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A relative path of plain components, resolved under a root the host owns.
#[derive(Clone, Debug, PartialEq, Eq)]
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
impl RelativePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// One place a credential may be. Only the host turns it into a path.
#[derive(Clone, Debug, PartialEq, Eq)]
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
}
/// Who a credential location belongs to, which decides whether a turn may
/// write the sign-in there back (see `crate::logins`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The operator's own: their home (`~/.codex`) or a file a variable names.
    /// Never written by a container.
    Operator,
    /// Control's configuration folder, which Control's own logins fill.
    Control,
}
impl Location {
    pub fn owner(&self) -> Owner {
        match self {
            Self::Variable { .. } | Self::Home(_) => Owner::Operator,
            Self::Config(_) => Owner::Control,
        }
    }
}
/// Where the host finds an account's credential: the first location whose
/// root is available (a set variable, a known home). Empty when the backend
/// needs none. A reference only, never contents.
#[derive(Clone, Debug, PartialEq, Eq)]
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
            };
            Some((location, path))
        })
    }
}

/// A named account's login directory, relative to the configuration
/// directory: `accounts/BACKEND/NAME`.
pub fn directory<B: Backend>(backend: B, name: &AccountName) -> RelativePath {
    RelativePath(["accounts", backend.id(), name.as_str()].iter().collect())
}
/// Where an account's login is: today's location for the implicit account,
/// else the file its kind keeps in the account's own directory.
pub fn secret<B: Backend>(backend: B, name: &AccountName, kind: AccountKind) -> SecretRef {
    if name.is_implicit() {
        return backend.implicit().1;
    }
    let directory = directory(backend, name).0;
    let file = match kind {
        AccountKind::ApiKey => backend.key_file().map(|key| key.name),
        AccountKind::Subscription | AccountKind::Cloud => backend.login_file(),
    };
    let path = file.map_or_else(|| directory.clone(), |file| directory.join(file));
    SecretRef(vec![Location::Config(RelativePath(path))])
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

/// What an operator gives to add an account; its login location follows
/// from its backend and name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound = "B: Backend")]
pub struct Entry<B> {
    pub backend: B,
    pub name: AccountName,
    pub kind: AccountKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<NonZeroU8>,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Label>,
}
fn enabled() -> bool {
    true
}
fn is_enabled(enabled: &bool) -> bool {
    *enabled
}
impl<B: Backend> From<Entry<B>> for Account<B> {
    fn from(entry: Entry<B>) -> Self {
        Self {
            secret: secret(entry.backend, &entry.name, entry.kind),
            id: AccountId {
                backend: entry.backend,
                name: entry.name,
            },
            kind: entry.kind,
            cap: entry.cap,
            enabled: entry.enabled,
            label: entry.label,
        }
    }
}
impl<B: Backend> From<&Account<B>> for Entry<B> {
    fn from(account: &Account<B>) -> Self {
        Self {
            backend: account.id.backend,
            name: account.id.name.clone(),
            kind: account.kind,
            cap: account.cap,
            enabled: account.enabled,
            label: account.label.clone(),
        }
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

/// Why accounts.json, or a change to it, was refused; nothing in it is used then.
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
    /// An account of a backend that signs in to nothing (a local endpoint).
    NoLogin(B),
    /// An API-key account of a backend that takes no key.
    NoKey(B),
    /// A change naming an account that is not defined.
    Missing(AccountId<B>),
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
            Self::NoLogin(backend) => write!(f, "{} signs in to nothing, so it has no accounts", backend.id()),
            Self::NoKey(backend) => write!(f, "{} takes no API key", backend.id()),
            Self::Missing(id) => write!(f, "{} has no account {}", id.backend.id(), id.name),
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
    pub why: Unusable,
    pub existing: Vec<AccountName>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    Missing(AccountName),
    Disabled(AccountName),
    /// A pool whose enabled accounts are of more than one kind.
    Mixed(MixedKinds),
}

impl<B: Backend> Store<B> {
    /// Pure: file bytes to a validated store.
    pub fn parse(bytes: &[u8]) -> Result<Self, LoadError<B>> {
        let file: File<B> = serde_json::from_slice(bytes).map_err(LoadError::Syntax)?;
        Self::validated(file.defaults, file.accounts.into_iter().map(Account::from).collect())
    }
    /// The file's bytes, which [`Store::parse`] reads back to this store.
    pub fn encode(&self) -> Vec<u8> {
        let file = Saved {
            defaults: Defaults(&self.defaults),
            accounts: self.accounts.iter().map(Entry::from).collect(),
        };
        let mut bytes = serde_json::to_vec_pretty(&file).expect("accounts encode as JSON");
        bytes.push(b'\n');
        bytes
    }
    fn validated(defaults: Vec<(B, AccountName)>, accounts: Vec<Account<B>>) -> Result<Self, LoadError<B>> {
        accounts.iter().enumerate().try_for_each(|(at, account)| {
            if account.id.name.is_implicit() {
                return Err(LoadError::Reserved(account.id.backend));
            }
            if !account.id.backend.signs_in() {
                return Err(LoadError::NoLogin(account.id.backend));
            }
            if account.kind == AccountKind::ApiKey && account.id.backend.key_file().is_none() {
                return Err(LoadError::NoKey(account.id.backend));
            }
            if accounts[..at].iter().any(|other| other.id == account.id) {
                return Err(LoadError::Duplicate(account.id.clone()));
            }
            Ok(())
        })?;
        let store = Self { defaults, accounts };
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
    /// The store with `entry` added.
    pub fn add(&self, entry: Entry<B>) -> Result<Self, LoadError<B>> {
        let accounts = self.accounts.iter().cloned().chain([entry.into()]).collect();
        Self::validated(self.defaults.clone(), accounts)
    }
    /// The store without the account, and without a default naming it.
    pub fn remove(&self, backend: B, name: &AccountName) -> Result<Self, LoadError<B>> {
        let id = AccountId {
            backend,
            name: name.clone(),
        };
        if !self.accounts.iter().any(|account| account.id == id) {
            return Err(LoadError::Missing(id));
        }
        Self::validated(
            self.defaults
                .iter()
                .filter(|(b, n)| !(*b == backend && n == name))
                .cloned()
                .collect(),
            self.accounts
                .iter()
                .filter(|account| account.id != id)
                .cloned()
                .collect(),
        )
    }
    /// The store with `name` as `backend`'s default; `default` restores the
    /// implicit account.
    pub fn set_default(&self, backend: B, name: &AccountName) -> Result<Self, LoadError<B>> {
        let others = self.defaults.iter().filter(|(b, _)| *b != backend).cloned();
        let defaults = if name.is_implicit() {
            others.collect()
        } else {
            others.chain([(backend, name.clone())]).collect()
        };
        Self::validated(defaults, self.accounts.clone())
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
    /// The account a choice runs on. A pool takes the enabled account that is
    /// not cooling down with the least `load`, the first on a tie; when every
    /// one is cooling it still takes one, which admission then holds. The
    /// implicit account is always enabled, so a pool always has one.
    pub fn choose(
        &self,
        backend: B,
        choice: &AccountChoice,
        load: impl Fn(&AccountName) -> usize,
        cooling: impl Fn(&AccountName) -> bool,
    ) -> Result<Account<B>, Refusal<B>> {
        let refused = |why| Refusal {
            backend,
            why,
            existing: self.of(backend).map(|a| a.id.name).collect(),
        };
        let named = |name: AccountName| match self.get(backend, &name) {
            Some(account) if account.enabled => Ok(account),
            Some(_) => Err(refused(Unusable::Disabled(name))),
            None => Err(refused(Unusable::Missing(name))),
        };
        let enabled = || self.of(backend).filter(|a| a.enabled);
        match choice {
            // Never refused: `parse` admits only enabled defaults.
            AccountChoice::Default => named(self.default_name(backend)),
            AccountChoice::Named(name) => named(name.clone()),
            AccountChoice::Pool => {
                one_kind(enabled().map(|a| a.kind)).map_err(|mixed| refused(Unusable::Mixed(mixed)))?;
                Ok(enabled()
                    .map(|a| ((cooling(&a.id.name), load(&a.id.name)), a))
                    .reduce(|best, next| if next.0 < best.0 { next } else { best })
                    .map_or_else(|| default_for(backend), |(_, account)| account))
            }
        }
    }
}

/// Seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UnixSeconds(pub u64);

/// An account that reported a usage limit: new attempts on it are held until
/// `until` passes (when the provider said when) or an operator clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound = "B: Backend")]
pub struct Cooling<B> {
    pub backend: B,
    pub name: AccountName,
    pub since: UnixSeconds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<UnixSeconds>,
}
/// Which accounts are cooling down, saved with the node state. There is no
/// automatic failover: a held attempt waits for its own account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound = "B: Backend")]
pub struct Status<B> {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cooling: Vec<Cooling<B>>,
}
impl<B> Default for Status<B> {
    fn default() -> Self {
        Self { cooling: Vec::new() }
    }
}
impl<B: Backend> Status<B> {
    pub fn is_empty(&self) -> bool {
        self.cooling.is_empty()
    }
    /// The cooldown holding `name` of `backend` at `now`, if any.
    pub fn cooling(&self, backend: B, name: &AccountName, now: UnixSeconds) -> Option<&Cooling<B>> {
        self.cooling
            .iter()
            .find(|c| c.backend == backend && c.name == *name && c.until.is_none_or(|until| now < until))
    }
    /// The status with the account cooling from `now`; a repeated limit
    /// keeps the first `since`.
    pub fn cool(&self, backend: B, name: &AccountName, now: UnixSeconds, until: Option<UnixSeconds>) -> Self {
        let since = self.cooling(backend, name, now).map_or(now, |c| c.since);
        Self {
            cooling: self
                .without(backend, name)
                .chain([Cooling {
                    backend,
                    name: name.clone(),
                    since,
                    until,
                }])
                .collect(),
        }
    }
    /// The status with the account's cooldown cleared.
    pub fn clear(&self, backend: B, name: &AccountName) -> Self {
        Self {
            cooling: self.without(backend, name).collect(),
        }
    }
    fn without<'a>(&'a self, backend: B, name: &'a AccountName) -> impl Iterator<Item = Cooling<B>> + 'a {
        self.cooling
            .iter()
            .filter(move |c| !(c.backend == backend && c.name == *name))
            .cloned()
    }
}

/// Why admission holds an attempt on its account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The account already runs `cap` attempts.
    Cap(NonZeroU8),
    /// The account reported a usage limit; until when, if the provider said.
    Cooling(Option<UnixSeconds>),
}
/// Whether an attempt on `account` may start now, given the attempts it
/// already runs. Pure, beside `bots::may_start`.
pub fn may_start<B: Backend>(
    account: &Account<B>,
    status: &Status<B>,
    running: usize,
    now: UnixSeconds,
) -> Result<(), Hold> {
    if let Some(cooling) = status.cooling(account.id.backend, &account.id.name, now) {
        return Err(Hold::Cooling(cooling.until));
    }
    match account.cap {
        Some(cap) if running >= usize::from(cap.get()) => Err(Hold::Cap(cap)),
        _ => Ok(()),
    }
}

/// The thin reader: `<directory>/accounts.json`, or the empty store when it
/// does not exist.
pub fn read<B: Backend>(directory: &Path) -> io::Result<Store<B>> {
    match std::fs::read(directory.join("accounts.json")) {
        Ok(bytes) => Ok(Store::parse(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Store::default()),
        Err(error) => Err(error),
    }
}

/// accounts.json as read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, bound = "B: Backend")]
struct File<B> {
    #[serde(default, deserialize_with = "pairs")]
    defaults: Vec<(B, AccountName)>,
    #[serde(default)]
    accounts: Vec<Entry<B>>,
}
/// accounts.json as written.
#[derive(Serialize)]
#[serde(bound = "B: Backend")]
struct Saved<'a, B> {
    #[serde(skip_serializing_if = "Defaults::is_empty")]
    defaults: Defaults<'a, B>,
    accounts: Vec<Entry<B>>,
}
/// The defaults as a JSON object keyed by backend.
struct Defaults<'a, B>(&'a [(B, AccountName)]);
impl<B> Defaults<'_, B> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl<B: Backend> Serialize for Defaults<'_, B> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        self.0
            .iter()
            .try_for_each(|(backend, name)| map.serialize_entry(backend, name))?;
        map.end()
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
