use super::*;
use crate::accounts::{AccountKind, AccountName, Entry};
use control_agent::Form;
use std::{fs, os::unix::fs::DirBuilderExt};

#[test]
fn a_backend_declares_its_delivery_and_the_node_may_refuse_a_mounted_key() {
    let rule = &control_inject::ANTHROPIC;
    let cases = [
        (KeyDelivery::Proxied(rule), false, "delivery: proxied"),
        (KeyDelivery::Proxied(rule), true, "delivery: proxied"),
        (
            KeyDelivery::Mounted("no rule"),
            false,
            "delivery: mounted (no rule); warning: the key is readable in its container",
        ),
        (
            KeyDelivery::Mounted("no rule"),
            true,
            "delivery: refused (mounted keys are off by require_proxied_keys; no rule)",
        ),
    ];
    for (delivery, require, shown) in cases {
        assert_eq!(decide(delivery, require).describe(), shown);
    }
    assert!(matches!(decide(KeyDelivery::Proxied(rule), true), Decision::Proxied(r) if std::ptr::eq(r, rule)));
    // Every backend with API-key accounts has a rule; none mounts its key.
    for backend in [Backend::Codex, Backend::Claude, Backend::Grok, Backend::Antigravity] {
        assert!(
            matches!(backend.agent().key_delivery(), KeyDelivery::Proxied(_)),
            "{backend:?}"
        );
    }
}

#[test]
fn an_api_key_turn_gets_a_fresh_placeholder_and_a_sign_in_turn_its_login() {
    let name = AccountName::try_from("relay-grant").unwrap();
    let entry = |kind| Entry {
        backend: Backend::Claude,
        name: name.clone(),
        kind,
        cap: None,
        enabled: true,
        label: None,
    };
    let store = Store::default().add(entry(AccountKind::ApiKey)).unwrap();
    let account = crate::logins::account(&store, Backend::Claude, &name);
    // No key saved yet: the turn learns how to save one, as before.
    let missing = grant(&account, &store, "attempt-1").unwrap_err().to_string();
    assert!(
        missing.contains("control claude key --account relay-grant"),
        "{missing}"
    );
    let directory = crate::logins::directory(Backend::Claude, &name).unwrap();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .unwrap();
    crate::config::write_in(&directory, "anthropic-api-key", b"sk-ant-CANARY-GRANT").unwrap();
    let (granted, relay) = grant(&account, &store, "attempt-1").unwrap();
    assert_eq!(granted.form(), Form::Proxied);
    assert_eq!(granted.path(), std::path::Path::new(""), "nothing to mount");
    let placeholder = granted.relay().unwrap().placeholder().to_owned();
    assert!(
        placeholder.starts_with("ctl-") && placeholder.len() == 36,
        "{placeholder}"
    );
    let relay = relay.expect("a proxied key has a relay");
    assert_eq!(relay.cap.attempt().0, "attempt-1");
    assert_eq!(relay.cap.account().to_string(), "claude+relay-grant");
    assert!(!format!("{granted:?}{relay:?}").contains(&placeholder));
    assert!(!format!("{granted:?}{relay:?}").contains("CANARY"));
    // Each attempt gets its own.
    let (again, _) = grant(&account, &store, "attempt-2").unwrap();
    assert_ne!(again.relay().unwrap().placeholder(), placeholder);
    fs::remove_dir_all(directory).unwrap();
}
