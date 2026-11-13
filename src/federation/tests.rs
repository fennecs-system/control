
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
const TIME: u64 = 1_793_145_600;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "control-federation-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn bundle() -> Bundle {
    let library = Library::default();
    let roots = vec!["engineering".into()];
    let resolved = library.resolve(&roots, &[]).unwrap();
    Bundle {
        roots,
        definitions: resolved
            .lenses
            .iter()
            .map(|v| library.get(&v.id).unwrap().clone())
            .collect(),
        documents: vec![],
    }
}
fn sign(store: &Store, mut e: Envelope) -> Envelope {
    e.resource_id = hash(&e.payload);
    e.signature = hex(store.key.sign(&e.preimage()).as_ref());
    e
}
fn trust(receiver: &mut Store, sender: &Store) {
    receiver
        .trust(&sender.id(), "Sender", vec!["engineering".into()], false)
        .unwrap();
}
#[test]
fn dropping_store_releases_ownership_even_with_an_inherited_descriptor() {
    let dir = Temp::new();
    let store = Store::open(&dir.0).unwrap();
    let inherited = store._lock.0.try_clone().unwrap();
    drop(store);
    let reopened = Store::open(&dir.0).unwrap();
    drop(inherited);
    assert!(
        Store::open(&dir.0).is_err(),
        "The new owner must retain exclusivity"
    );
    drop(reopened);
    assert!(Store::open(&dir.0).is_ok());
}
#[test]
fn two_nodes_persist_identity_quarantine_and_idempotent_receipts() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    assert_ne!(sender.id(), receiver.id());
    let id = sender.id();
    let e = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    assert!(receiver.import(e.clone(), TIME).is_err());
    trust(&mut receiver, &sender);
    assert!(receiver.import(e.clone(), TIME).unwrap());
    assert!(!receiver.import(e.clone(), TIME).unwrap());
    assert_eq!(receiver.ledger.imports.len(), 1);
    assert!(!b.0.join("state.json").exists());
    drop(receiver);
    let mut receiver = Store::open(&b.0).unwrap();
    assert!(!receiver.import(e, TIME).unwrap());
    drop(sender);
    let mut sender = Store::open(&a.0).unwrap();
    assert_eq!(sender.id(), id);
    let next = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    assert_eq!(next.sequence, 2);
}
#[test]
fn signed_headers_digest_and_recipient_cannot_be_substituted() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    trust(&mut receiver, &sender);
    let e = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    let mut cases = Vec::new();
    let mut t = e.clone();
    t.payload.push(' ');
    cases.push(t);
    let mut t = e.clone();
    t.expires_at += 1;
    cases.push(t);
    let mut t = e.clone();
    t.sequence += 1;
    cases.push(t);
    let mut t = e.clone();
    t.recipient = sender.id();
    cases.push(t);
    let mut t = e.clone();
    t.sender = receiver.id();
    cases.push(t);
    let mut t = e.clone();
    t.kind = "job".into();
    cases.push(t);
    let mut t = e.clone();
    t.protocol = "control-federation/9".into();
    cases.push(t);
    let mut t = e.clone();
    t.signature = "00".repeat(64);
    cases.push(t);
    let mut t = e.clone();
    t.resource_id = "sha256:00".into();
    cases.push(t);
    for t in cases {
        assert!(receiver.import(t, TIME).is_err());
    }
    assert!(receiver.ledger.imports.is_empty());
    assert!(receiver.import(e, TIME).unwrap());
}
#[test]
fn valid_signatures_do_not_bypass_expiry_scope_or_revocation() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    trust(&mut receiver, &sender);
    let e = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    let mut t = e.clone();
    t.expires_at = TIME;
    assert!(receiver.import(sign(&sender, t), TIME).is_err());
    let mut t = e.clone();
    t.issued_at = TIME + 301;
    assert!(receiver.import(sign(&sender, t), TIME).is_err());
    let mut t = e.clone();
    t.expires_at = TIME + WEEK + 1;
    assert!(receiver.import(sign(&sender, t), TIME).is_err());
    receiver
        .trust(&sender.id(), "Sender", vec!["research".into()], false)
        .unwrap();
    assert!(receiver.import(e.clone(), TIME).is_err());
    trust(&mut receiver, &sender);
    assert!(receiver.import(e.clone(), TIME).unwrap());
    receiver.revoke(&sender.id()).unwrap();
    assert!(receiver.import(e.clone(), TIME).is_err());
    trust(&mut receiver, &sender);
    assert!(!receiver.import(e.clone(), TIME).unwrap());
    let mut t = e;
    t.payload.push(' ');
    assert!(receiver.import(sign(&sender, t), TIME).is_err());
    assert_eq!(receiver.ledger.imports.len(), 1);
    assert!(
        receiver
            .trust(&receiver.id(), "Self", vec!["engineering".into()], false)
            .is_err()
    );
}
#[test]
fn bundles_are_closed_unique_and_documents_need_separate_scope() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    trust(&mut receiver, &sender);
    let mut duplicate = bundle();
    duplicate.definitions.push(duplicate.definitions[0].clone());
    assert!(duplicate.validate().is_err());
    let mut missing = bundle();
    missing.definitions.remove(0);
    assert!(missing.validate().is_err());
    let mut extra = bundle();
    extra
        .definitions
        .push(Library::default().get("research").unwrap().clone());
    assert!(extra.validate().is_err());
    let mut cycle = bundle();
    let id = cycle.definitions[0].id.clone();
    cycle.definitions[0].includes.push(id);
    assert!(cycle.validate().is_err());
    let mut unknown = bundle();
    unknown.definitions[0].tools.push("shell.run".into());
    assert!(unknown.validate().is_err());
    let mut docs = bundle();
    docs.definitions[0].documents.push("fact".into());
    docs.documents.push(DocumentInput {
        id: "fact".into(),
        title: "Fact".into(),
        revision: 1,
        body: "Untrusted text".into(),
    });
    docs.validate().unwrap();
    let e = sender.export(docs, &receiver.id(), TIME).unwrap();
    assert!(receiver.import(e.clone(), TIME).is_err());
    receiver
        .trust(&sender.id(), "Sender", vec!["engineering".into()], true)
        .unwrap();
    assert!(receiver.import(e, TIME).unwrap());
}
#[test]
fn strict_wire_bounds_and_json_escaping_preserve_exact_payload_bytes() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    trust(&mut receiver, &sender);
    let e = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    let json = serde_json::to_string(&e).unwrap();
    let unknown = json.replacen('{', "{\"extra\":1,", 1);
    assert!(serde_json::from_str::<Envelope>(&unknown).is_err());
    let duplicate = json.replacen('{', "{\"sequence\":1,", 1);
    assert!(serde_json::from_str::<Envelope>(&duplicate).is_err());
    assert!(serde_json::from_str::<Envelope>(&format!("{json} trailing")).is_err());
    assert!(serde_json::from_str::<Bundle>(r#"{"roots":["engineering"],"definitions":[],"documents":[{"id":"fact","title":"Fact","revision":1,"body":"text","execute":true}]}"#).is_err());
    let escaped = json.replace("engineering", "\\u0065ngineering");
    let decoded: Envelope = serde_json::from_str(&escaped).unwrap();
    assert_eq!(decoded.payload, e.payload);
    assert!(receiver.import(decoded, TIME).unwrap());
    let mut t = e.clone();
    t.sequence += 1;
    t.payload = " ".repeat(PAYLOAD + 1);
    assert!(receiver.import(sign(&sender, t), TIME).is_err());
    let mut t = e;
    t.sequence += 1;
    t.payload = "{\"roots\":[],\"definitions\":[],\"documents\":[],\"extra\":1}".into();
    assert!(receiver.import(sign(&sender, t), TIME).is_err());
}
#[test]
fn out_of_order_delivery_is_valid_and_quota_never_discards_receipts() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    trust(&mut receiver, &sender);
    let first = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    let second = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    assert!(receiver.import(second.clone(), TIME).unwrap());
    assert!(receiver.import(first.clone(), TIME).unwrap());
    receiver.ledger.imports.resize(256, second);
    let third = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    assert!(receiver.import(third, TIME).is_err());
    assert!(!receiver.import(first, TIME).unwrap());
    assert_eq!(receiver.ledger.imports.len(), 256);
    let mut huge = receiver.ledger.clone();
    huge.imports[0].payload = "x".repeat(STORE_BYTES);
    assert!(receiver.save(huge).is_err());
    assert_eq!(receiver.ledger.imports.len(), 256);
}
#[test]
fn state_lock_missing_identity_and_failed_commit_fail_closed() {
    let a = Temp::new();
    let mut store = Store::open(&a.0).unwrap();
    assert!(Store::open(&a.0).is_err());
    let destination = Temp::new();
    let receiver = Store::open(&destination.0).unwrap();
    // A preexisting temporary file cannot be followed/truncated. No sequence escapes.
    let temp = a.0.join(format!("ledger-{}-1.tmp", std::process::id()));
    create(&temp, b"planted").unwrap();
    assert!(store.export(bundle(), &receiver.id(), TIME).is_err());
    assert_eq!(store.ledger.sequence, 0);
    assert_eq!(read(&temp, 100, true).unwrap(), b"planted");
    fs::remove_file(temp).unwrap();
    store.export(bundle(), &receiver.id(), TIME).unwrap();
    drop(store);
    fs::remove_file(a.0.join("identity.pk8")).unwrap();
    assert!(Store::open(&a.0).is_err());
}
#[test]
fn failed_import_persistence_keeps_receipts_unchanged() {
    let a = Temp::new();
    let b = Temp::new();
    let mut sender = Store::open(&a.0).unwrap();
    let mut receiver = Store::open(&b.0).unwrap();
    trust(&mut receiver, &sender);
    let e = sender.export(bundle(), &receiver.id(), TIME).unwrap();
    let temp = b.0.join(format!("ledger-{}-0.tmp", std::process::id()));
    create(&temp, b"planted").unwrap();
    assert!(receiver.import(e.clone(), TIME).is_err());
    assert!(receiver.ledger.imports.is_empty());
    fs::remove_file(temp).unwrap();
    assert!(receiver.import(e, TIME).unwrap());
}
#[cfg(unix)]
#[test]
fn symlinks_fifos_oversized_files_and_public_keys_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = Temp::new();
    let store = Store::open(&temp.0).unwrap();
    let id = store.id();
    drop(store);
    symlink(temp.0.join("identity.pk8"), temp.0.join("link")).unwrap();
    assert!(read(&temp.0.join("link"), 1024, false).is_err());
    fs::set_permissions(
        temp.0.join("identity.pk8"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(Store::open(&temp.0).is_err());
    fs::set_permissions(
        temp.0.join("identity.pk8"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(Store::open(&temp.0).unwrap().id(), id);
    let path = temp.0.join("large");
    let file = options()
        .unwrap()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.set_len((CARRIER + 1) as u64).unwrap();
    assert!(read(&path, CARRIER, false).is_err());
    let fifo = temp.0.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert!(read(&fifo, CARRIER, false).is_err());
}
