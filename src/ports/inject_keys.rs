//! The relay's [`SecretSource`] for today's file-based API-key accounts: the
//! key is read from where `control ID key` saved it, through the same host-side
//! location table and checks as every other login (`crate::logins::locate`),
//! once per request, and cleared when the edge's call returns.
use super::inject_edge::SecretSource;
use crate::accounts::{AccountId, KeyWriter, Store};
use crate::backend::Backend;
use std::{fmt, io};
use zeroize::Zeroizing;

/// An API-key account the relay injects for; `backend+name` in the audit log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyAccount(pub(crate) AccountId<Backend>);
impl fmt::Display for KeyAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+{}", self.0.backend.id(), self.0.name)
    }
}

/// The node's accounts, as accounts.json defined them when the attempt started.
pub(crate) struct AccountKeys(pub(crate) Store<Backend>);
impl SecretSource<KeyAccount> for AccountKeys {
    fn with_secret<R>(&self, account: &KeyAccount, f: impl FnOnce(&[u8]) -> R) -> io::Result<R> {
        let none = || io::Error::other("This account has no key the relay can inject");
        let id = &account.0;
        let account = crate::logins::account(&self.0, id.backend, &id.name);
        let file = crate::accounts::Backend::key_file(id.backend).ok_or_else(none)?;
        let granted = crate::logins::locate(&account)?;
        if granted.form() != control_agent::Form::ApiKey {
            return Err(none());
        }
        // A key file is small; a CLI's sign-in file holding one is too.
        let bytes = Zeroizing::new(crate::config::read_regular(granted.path(), 64 * 1024, true)?);
        match file.writer {
            KeyWriter::Host => {
                let key = bytes
                    .strip_suffix(b"\n")
                    .map_or(&bytes[..], |line| line.strip_suffix(b"\r").unwrap_or(line));
                Ok(f(key))
            }
            KeyWriter::Cli { field } => {
                let mut saved: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_slice(&bytes).map_err(|_| none())?;
                let key = match saved.remove(field) {
                    Some(serde_json::Value::String(key)) => Zeroizing::new(key),
                    _ => return Err(none()),
                };
                Ok(f(key.as_bytes()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountKind, AccountName, Entry};
    use std::{fs, os::unix::fs::DirBuilderExt};

    #[test]
    fn a_saved_key_is_lent_for_one_call_and_other_accounts_have_none() {
        let name = AccountName::try_from("relay-key").unwrap();
        let entry = |backend, kind| Entry {
            backend,
            name: name.clone(),
            kind,
            cap: None,
            enabled: true,
            label: None,
        };
        let store = Store::default()
            .add(entry(Backend::Claude, AccountKind::ApiKey))
            .unwrap()
            .add(entry(Backend::Grok, AccountKind::Subscription))
            .unwrap()
            .add(entry(Backend::Codex, AccountKind::ApiKey))
            .unwrap();
        let directory = crate::logins::directory(Backend::Claude, &name).unwrap();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .unwrap();
        crate::config::write_in(&directory, "anthropic-api-key", b"sk-ant-CANARY-KEY\n").unwrap();
        let keys = AccountKeys(store);
        let account = |backend| {
            KeyAccount(AccountId {
                backend,
                name: name.clone(),
            })
        };
        let lent = keys.with_secret(&account(Backend::Claude), <[u8]>::to_vec).unwrap();
        assert_eq!(lent, b"sk-ant-CANARY-KEY");
        assert_eq!(account(Backend::Claude).to_string(), "claude+relay-key");
        // Codex's own `auth.json`, as `codex login --with-api-key` writes it.
        let codex = crate::logins::directory(Backend::Codex, &name).unwrap();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&codex)
            .unwrap();
        let saved = br#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-proj-CANARY-CODEX"}"#;
        crate::config::write_in(&codex, "auth.json", saved).unwrap();
        let lent = keys.with_secret(&account(Backend::Codex), <[u8]>::to_vec).unwrap();
        assert_eq!(lent, b"sk-proj-CANARY-CODEX");
        crate::config::write_in(&codex, "auth.json", br#"{"tokens":{}}"#).unwrap();
        for backend in [Backend::Grok, Backend::Codex, Backend::Antigravity] {
            let error = keys.with_secret(&account(backend), |_| ()).unwrap_err().to_string();
            assert!(!error.contains("CANARY"), "{error}");
        }
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(codex).unwrap();
    }
}
