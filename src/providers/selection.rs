//! Selection interprets configuration; it does not call models or change authority.
//!
//! One grammar, parsed once by [`parse`]: `backend[+account]:model[@node[:domain]]`.
//! `codex:MODEL` runs on the backend's default account, `codex+NAME:MODEL` on a
//! named one, `codex+*:MODEL` on any enabled one (picked at admission), and
//! `@node` is reserved for running on another node, refused until federation
//! routes work. A bare `MODEL` is looked up across the catalogue. The first `@`
//! always starts the node address: model IDs cannot contain `@`
//! (`usage::valid_model`), so a second `@` is refused rather than guessed at.
//! Model IDs may contain `:`; only the first `:` ends the backend.
use super::{Model, Target};
use crate::{
    accounts::{AccountChoice, AccountName, NameError, Refusal, Unusable},
    backend::Backend,
    runtime::Runtime,
};
use std::{borrow::Cow, fmt};

pub(crate) fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "maxLength": 128,
        "description": concat!(
            "Optional configured model ID, e.g. claude:opus or codex:MODEL_ID; codex+NAME:MODEL_ID runs on a named ",
            "account and codex+*:MODEL_ID on any enabled account. Omit to inherit the parent's captured model and ",
            "account. Unknown/unavailable models never silently fall back to another provider. local:NAME selects a ",
            "private endpoint model declared tool-capable; text-only endpoints are refused for agent turns."
        )
    })
}

/// A parsed selection. Every form of the grammar, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selector {
    pub(crate) backend: BackendChoice,
    pub(crate) model: ModelId,
    pub(crate) node: NodeChoice,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BackendChoice {
    /// A bare model ID: whichever backend's catalogue holds it.
    Any,
    Named {
        backend: Backend,
        account: AccountChoice,
    },
}
/// A model ID as `usage::valid_model` accepts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelId(String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeChoice {
    Local,
    /// `@name` or `@name:domain`, Matrix style. Parsed, never run yet.
    Remote {
        name: NodeName,
        domain: Option<Domain>,
    },
}
/// `[a-z0-9-]{1,63}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeName(String);
/// Dot-separated `[a-z0-9-]` labels, at most 253 characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Domain(String);
impl TryFrom<&str> for ModelId {
    type Error = SelectionError;
    fn try_from(text: &str) -> Result<Self, SelectionError> {
        match text {
            "" => Err(SelectionError::EmptyModel),
            text if crate::usage::valid_model(text) => Ok(Self(text.into())),
            _ => Err(SelectionError::InvalidModel),
        }
    }
}
fn label(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.len()) && text.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}
impl TryFrom<&str> for NodeName {
    type Error = SelectionError;
    fn try_from(text: &str) -> Result<Self, SelectionError> {
        label(text, 63)
            .then(|| Self(text.into()))
            .ok_or(SelectionError::InvalidNode)
    }
}
impl TryFrom<&str> for Domain {
    type Error = SelectionError;
    fn try_from(text: &str) -> Result<Self, SelectionError> {
        (text.len() <= 253 && text.split('.').all(|part| label(part, 63)))
            .then(|| Self(text.into()))
            .ok_or(SelectionError::InvalidNode)
    }
}

/// The one parser every selection goes through.
pub(crate) fn parse(text: &str) -> Result<Selector, SelectionError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(SelectionError::Empty);
    }
    if text.chars().any(char::is_whitespace) {
        return Err(SelectionError::Whitespace);
    }
    let (left, node) = match text.split_once('@') {
        None => (text, NodeChoice::Local),
        Some((_, address)) if address.contains('@') => return Err(SelectionError::TwoNodes),
        Some((left, address)) => (left, node(address)?),
    };
    let (backend, model) = match left.split_once(':') {
        Some((head, model)) => (named(head)?, model),
        None if left.contains('+') => return Err(SelectionError::MissingModel),
        None => (BackendChoice::Any, left),
    };
    Ok(Selector {
        backend,
        model: ModelId::try_from(model)?,
        node,
    })
}
fn named(head: &str) -> Result<BackendChoice, SelectionError> {
    let (backend, account) = match head.split_once('+') {
        None => (head, AccountChoice::Default),
        Some((_, account)) if account.contains('+') => return Err(SelectionError::TwoAccounts),
        Some((backend, crate::accounts::POOL)) => (backend, AccountChoice::Pool),
        Some((backend, account)) => (
            backend,
            AccountChoice::Named(AccountName::try_from(account).map_err(SelectionError::Account)?),
        ),
    };
    let backend = match backend {
        "" => Err(SelectionError::MissingBackend),
        id => Backend::named(id).ok_or_else(|| SelectionError::UnknownBackend(id.chars().take(32).collect())),
    }?;
    Ok(BackendChoice::Named { backend, account })
}
fn node(address: &str) -> Result<NodeChoice, SelectionError> {
    let (name, domain) = match address.split_once(':') {
        Some((name, domain)) => (name, Some(domain)),
        None => (address, None),
    };
    if name.is_empty() || domain == Some("") {
        return Err(SelectionError::EmptyNode);
    }
    Ok(NodeChoice::Remote {
        name: NodeName::try_from(name)?,
        domain: domain.map(Domain::try_from).transpose()?,
    })
}
impl fmt::Display for NodeChoice {
    /// The `@node[:domain]` suffix; nothing for this node.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => Ok(()),
            Self::Remote { name, domain: None } => write!(f, "@{}", name.0),
            Self::Remote {
                name,
                domain: Some(domain),
            } => write!(f, "@{}:{}", name.0, domain.0),
        }
    }
}
impl fmt::Display for Selector {
    /// The canonical text, which parses back to the same selector.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.backend {
            BackendChoice::Any => {}
            BackendChoice::Named { backend, account } => {
                f.write_str(backend.id())?;
                match account {
                    AccountChoice::Default => {}
                    AccountChoice::Named(name) => write!(f, "+{name}")?,
                    AccountChoice::Pool => write!(f, "+{}", crate::accounts::POOL)?,
                }
                f.write_str(":")?;
            }
        }
        write!(f, "{}{}", self.model.0, self.node)
    }
}

/// Why a selection was refused. [`fmt::Display`] is the only place its
/// operator-facing text is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectionError {
    Empty,
    Whitespace,
    /// `+NAME:MODEL` or `:MODEL`.
    MissingBackend,
    UnknownBackend(String),
    /// `backend+NAME` without `:MODEL`.
    MissingModel,
    EmptyModel,
    InvalidModel,
    Account(NameError),
    TwoAccounts,
    TwoNodes,
    EmptyNode,
    InvalidNode,
    /// A node address names another node; federation does not route work yet.
    Remote(NodeChoice),
    NotConfigured,
    TextOnly,
    Unusable(Refusal<Backend>),
}
impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let backends = || Backend::ALL.map(Backend::id).join(", ");
        match self {
            Self::Empty => f.write_str("Name a model, e.g. BACKEND:MODEL."),
            Self::Whitespace => f.write_str("A model selection contains no spaces."),
            Self::MissingBackend => f.write_str("Name the backend before +ACCOUNT or :MODEL, e.g. codex+NAME:MODEL."),
            Self::UnknownBackend(id) => write!(f, "Unknown backend {id:?}. Backends: {}.", backends()),
            Self::MissingModel => f.write_str("Add :MODEL after the account, e.g. codex+NAME:MODEL."),
            Self::EmptyModel => f.write_str("The model ID after : is empty."),
            Self::InvalidModel => write!(
                f,
                "Use a configured model ID (letters, digits and . _ - : /), optionally prefixed with BACKEND: or BACKEND+ACCOUNT: (backends: {}).",
                backends()
            ),
            Self::Account(error) => write!(f, "Invalid account: {error}."),
            Self::TwoAccounts => f.write_str("Name one account: BACKEND+ACCOUNT:MODEL."),
            Self::TwoNodes => {
                f.write_str("Only one @ is allowed: it starts the node address, and model IDs cannot contain @.")
            }
            Self::EmptyNode => f.write_str("Name the node after @, as @NODE or @NODE:DOMAIN."),
            Self::InvalidNode => f.write_str("Node names use a-z, 0-9 and -; a domain is such names joined by dots."),
            Self::Remote(node) => write!(
                f,
                "Running on another node ({node}) is not available yet; omit the @ address to run on this node."
            ),
            Self::NotConfigured => f.write_str("Model is not configured or is ambiguous. Use /model to list choices."),
            Self::TextOnly => f.write_str(crate::backend::LOCAL_TEXT_ONLY),
            Self::Unusable(refusal) => {
                let backend = refusal.backend.id();
                let existing = refusal
                    .existing
                    .iter()
                    .map(AccountName::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                match &refusal.why {
                    Unusable::Missing(name) => write!(f, "{backend} has no account {name}.")?,
                    Unusable::Disabled(name) => write!(f, "Account {backend}+{name} is disabled.")?,
                    Unusable::Mixed(mixed) => {
                        let kinds = mixed
                            .0
                            .iter()
                            .map(|kind| kind.as_str())
                            .collect::<Vec<_>>()
                            .join(" and ");
                        write!(
                            f,
                            "{backend}+* would mix {kinds} accounts, which bill differently, so it is refused. Name one account, e.g. {backend}+NAME:MODEL."
                        )?
                    }
                }
                write!(f, " Accounts of {backend}: {existing}.")
            }
        }
    }
}
impl From<SelectionError> for Cow<'static, str> {
    fn from(error: SelectionError) -> Self {
        error.to_string().into()
    }
}

/// The catalogue entry a selection names, keeping its account choice: a named
/// account must exist and be enabled now; a pool stays a pool until admission.
/// `preferred` breaks a tie between backends serving the same model ID.
pub(crate) fn choose(runtime: &Runtime, preferred: Backend, text: &str) -> Result<Model, SelectionError> {
    let selector = parse(text)?;
    if selector.node != NodeChoice::Local {
        return Err(SelectionError::Remote(selector.node));
    }
    let (wanted, account) = match selector.backend {
        BackendChoice::Any => (None, AccountChoice::Default),
        BackendChoice::Named { backend, account } => (Some(backend), account),
    };
    let models = super::choices(runtime);
    let matching: Vec<_> = models
        .iter()
        .filter(|m| m.model == selector.model.0 && wanted.is_none_or(|b| m.backend == b))
        .collect();
    let selected = matching
        .iter()
        .find(|m| m.backend == wanted.unwrap_or(preferred))
        .copied()
        .or_else(|| (matching.len() == 1).then(|| matching[0]))
        .cloned()
        .ok_or(SelectionError::NotConfigured)?;
    // Capability, not a listing, decides whether an agent turn can run.
    if !super::agent_capable(runtime, &selected) {
        return Err(SelectionError::TextOnly);
    }
    runtime
        .accounts
        .choose(selected.backend, &account, |_| 0, |_| false)
        .map_err(SelectionError::Unusable)?;
    Ok(Model { account, ..selected })
}

/// The concrete target a choice runs on, captured now: its account resolved
/// once (a pool to its least-loaded enabled account), so later changes of
/// default never re-point the work.
pub(crate) fn admit(runtime: &Runtime, model: &Model) -> Result<Target, SelectionError> {
    let account = runtime
        .accounts
        .choose(
            model.backend,
            &model.account,
            super::accounts::load(&runtime.runs, model.backend),
            super::accounts::cooling(runtime, model.backend),
        )
        .map_err(SelectionError::Unusable)?;
    Ok(Target {
        backend: model.backend,
        account: account.id.name,
        model: Some(model.model.clone()),
    })
}

/// Omission inherits the parent's captured target, account included, even
/// after configuration changes; an explicit selection is chosen and admitted.
pub(crate) fn resolve(runtime: &Runtime, inherited: &Target, selector: Option<&str>) -> Result<Target, SelectionError> {
    match selector {
        None => Ok(Target {
            model: Some(inherited.model_or_default()),
            ..inherited.clone()
        }),
        Some(text) => admit(runtime, &choose(runtime, inherited.backend, text)?),
    }
}

/// One sentence teaching the forms with this node's backends and a model it has.
fn forms(runtime: &Runtime) -> String {
    // One example model per backend that has one, the node default first.
    let examples = std::iter::once(Model::captured(runtime.backend, runtime.configured_model()))
        .chain(super::choices(runtime))
        .filter(|m| super::agent_capable(runtime, m))
        .fold(Vec::<Model>::new(), |mut kept, model| {
            if !kept.iter().any(|k| k.backend == model.backend) {
                kept.push(model);
            }
            kept
        });
    let lines: Vec<String> = examples
        .iter()
        .flat_map(|example| {
            let (backend, model) = (example.backend.id(), &example.model);
            let named: Vec<String> = runtime
                .accounts
                .of(example.backend)
                .filter(|account| account.enabled && !account.id.name.is_implicit())
                .map(|account| format!("{backend}+{}:{model}", account.id.name))
                .collect();
            let pool = (!named.is_empty()).then(|| format!("{backend}+*:{model} (any enabled account)"));
            std::iter::once(format!("{backend}:{model}")).chain(named).chain(pool)
        })
        .collect();
    let add = examples
        .iter()
        .find(|m| m.backend != Backend::Local)
        .filter(|m| runtime.accounts.of(m.backend).count() == 1)
        .map(|m| {
            format!(
                " Another account: control accounts add {0} NAME, then {0}+NAME:{1}.",
                m.backend.id(),
                m.model
            )
        })
        .unwrap_or_default();
    format!("Forms: {}.{add}", lines.join(" · "))
}

pub(crate) fn command(
    session: &mut crate::chat::Session,
    runtime: &Runtime,
    option: &str,
) -> Result<String, Cow<'static, str>> {
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    let option = option.trim();
    let current = session
        .model
        .clone()
        .unwrap_or_else(|| Model::captured(runtime.backend, runtime.configured_model()));
    if option.is_empty() {
        // Only selectable models are listed; text-only local models are named once,
        // apart, with the way to enable them.
        let (agents, text): (Vec<_>, Vec<_>) = super::choices(runtime)
            .into_iter()
            .partition(|m| super::agent_capable(runtime, m));
        let names = |models: &[Model]| models.iter().map(Model::to_string).collect::<Vec<_>>().join(", ");
        let text = if text.is_empty() {
            String::new()
        } else {
            format!(
                " Text-only (control_model_generate and summaries; edit in Manage → Models and enable Serves tool calls to run turns): {}.",
                names(&text)
            )
        };
        return Ok(format!(
            "Selected {current}. Configured: {}. Use /model BACKEND:MODEL or /model default. Changes apply to future turns; active attempts keep their captured model. Listing does not establish account access. {}{text}",
            names(&agents),
            forms(runtime)
        ));
    }
    let selected = if option == "default" {
        Model::captured(runtime.backend, runtime.configured_model())
    } else {
        choose(runtime, current.backend, option).map_err(|error| format!("{error} {}", forms(runtime)))?
    };
    let reply = format!("Future turns use {selected}. Active attempts are unchanged.");
    session.model = Some(selected);
    Ok(reply)
}

#[cfg(test)]
mod tests;
