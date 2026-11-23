//! Host glue for `crate::accounts`: the backends' implicit accounts, the
//! node's accounts.json and the load a pool is balanced on.
use crate::accounts::{AccountKind, AccountName, Location, RelativePath, SecretRef, Store};
use crate::backend::Backend;

/// Where each backend's login is kept: the one table of these locations.
/// `crate::logins` resolves the implicit account's reference and checks it
/// before handing the login to a backend.
impl crate::accounts::Backend for Backend {
    fn id(self) -> &'static str {
        Backend::id(self)
    }
    fn implicit(self) -> (AccountKind, SecretRef) {
        let variable = |name: &'static str, join: Option<&'static str>| Location::Variable {
            name,
            join: join.map(fixed),
        };
        let locations = match self {
            Self::Codex => vec![
                variable("CONTROL_CODEX_AUTH_FILE", None),
                variable("CODEX_HOME", Some("auth.json")),
                Location::Home(fixed(".codex/auth.json")),
            ],
            Self::Claude => vec![
                variable("CONTROL_CLAUDE_AUTH_FILE", None),
                Location::Config(under(self, Some(".credentials.json"))),
            ],
            Self::Grok => vec![
                variable("CONTROL_GROK_AUTH_FILE", None),
                Location::Config(under(self, Some("auth.json"))),
            ],
            // A directory: the sign-in is chosen by which file it holds.
            Self::Antigravity => vec![Location::Config(under(self, None))],
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
/// A path in the backend's login directory, `<config>/ID`, which `control ID login` fills.
fn under(backend: Backend, name: Option<&str>) -> RelativePath {
    let directory = std::path::PathBuf::from(backend.id());
    RelativePath::try_from(name.map_or_else(|| directory.clone(), |name| directory.join(name)))
        .expect("a backend ID is one plain component")
}

/// The node's accounts.json; a missing file is the empty store.
pub(crate) fn read() -> std::io::Result<Store<Backend>> {
    crate::accounts::read(&crate::config::directory().map_err(std::io::Error::other)?)
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
