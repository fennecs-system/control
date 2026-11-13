//! Offline, operator-mediated federation. Imported resources never enter active state.
use control_context::{Definition, DocumentInput, Library};
use ring::{
    digest,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const PROTOCOL: &str = "control-federation/0.1";
const CARRIER: usize = 2 * 1024 * 1024;
const PAYLOAD: usize = 512 * 1024;
const STORE_BYTES: usize = 64 * 1024 * 1024;
const WEEK: u64 = 7 * 86400;
type Result<T> = io::Result<T>;
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str, size: usize) -> Result<Vec<u8>> {
    if s.len() != size * 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(fail("Invalid lowercase hex length or encoding"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(io::Error::other))
        .collect()
}
fn public(id: &str) -> Result<Vec<u8>> {
    unhex(
        id.strip_prefix("ed25519:")
            .ok_or_else(|| fail("Expected ed25519 node ID"))?,
        32,
    )
}
fn hash(s: &str) -> String {
    format!(
        "sha256:{}",
        hex(digest::digest(&digest::SHA256, s.as_bytes()).as_ref())
    )
}
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs())
}
fn options() -> Result<OpenOptions> {
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(fail("Safe file IO unsupported on this platform"));
        o.mode(0o600);
    }
    #[cfg(not(unix))]
    return Err(fail("Safe file IO unsupported on this platform"));
    Ok(o)
}
fn regular(f: &File, private: bool) -> Result<()> {
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(fail("Expected a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if private && meta.permissions().mode() & 0o077 != 0 {
            return Err(fail("Federation state files must be private (0600)"));
        }
    }
    Ok(())
}
fn read(path: &Path, limit: usize, private: bool) -> Result<Vec<u8>> {
    crate::config::read_regular(path, limit, private)
}
fn create(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = options()?.write(true).create_new(true).open(path)?;
    let result = f.write_all(bytes).and_then(|()| f.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    roots: Vec<String>,
    definitions: Vec<Definition>,
    documents: Vec<DocumentInput>,
}
impl Bundle {
    fn validate(&self) -> Result<()> {
        if self.roots.is_empty()
            || self.roots.len() > 128
            || self.definitions.len() > 128
            || self.documents.len() > 128
            || self
                .definitions
                .iter()
                .map(|d| d.tools.len())
                .sum::<usize>()
                > 256
        {
            return Err(fail("Bundle cardinality limit"));
        }
        let mut ids = BTreeSet::new();
        for d in &self.definitions {
            if !ids.insert(&d.id) || d.tools.len() > 256 || d.tools.iter().any(|t| t.len() > 65536)
            {
                return Err(fail("Duplicate definition or excessive tools"));
            }
        }
        ids.clear();
        for d in &self.documents {
            if !control_context::valid_id(&d.id)
                || d.revision == 0
                || !ids.insert(&d.id)
                || d.title.len() > 65536
                || d.body.len() > 65536
            {
                return Err(fail("Invalid or duplicate document"));
            }
        }
        let roots: BTreeSet<_> = self.roots.iter().collect();
        if roots.len() != self.roots.len() {
            return Err(fail("Duplicate root"));
        }
        let library = Library {
            definitions: self.definitions.clone(),
        };
        library
            .validate(&self.documents)
            .map_err(io::Error::other)?;
        let resolved = library
            .resolve(&self.roots, &self.documents)
            .map_err(io::Error::other)?;
        if resolved.lenses.len() != self.definitions.len()
            || resolved.documents.len() != self.documents.len()
        {
            return Err(fail(
                "Bundle must contain exactly the reachable dependency closure",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol: String,
    sender: String,
    recipient: String,
    kind: String,
    sequence: u64,
    resource_id: String,
    issued_at: u64,
    expires_at: u64,
    payload: String,
    signature: String,
}
impl Envelope {
    fn preimage(&self) -> Vec<u8> {
        let mut out = PROTOCOL.as_bytes().to_vec();
        out.push(0);
        for s in [
            &self.sender,
            &self.recipient,
            &self.kind,
            &self.sequence.to_string(),
            &self.resource_id,
            &self.issued_at.to_string(),
            &self.expires_at.to_string(),
            &self.payload,
        ] {
            out.extend_from_slice(&(s.len() as u64).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    id: String,
    label: String,
    roots: Vec<String>,
    allow_documents: bool,
    revoked: bool,
}
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    sequence: u64,
    peers: Vec<Peer>,
    imports: Vec<Envelope>,
}
struct Store {
    dir: PathBuf,
    key: Ed25519KeyPair,
    ledger: Ledger,
    _lock: StoreLock,
}
// An unrelated fork may briefly inherit an open descriptor before exec closes
// it. Release ownership explicitly rather than waiting for the final fd close.
struct StoreLock(File);
impl Drop for StoreLock {
    fn drop(&mut self) { let _ = self.0.unlock(); }
}
impl Store {
    fn open(dir: &Path) -> Result<Self> {
        // State roots are operator-owned. Reject symlinked final directory components.
        fs::create_dir_all(dir)?;
        if fs::symlink_metadata(dir)?.file_type().is_symlink() {
            return Err(fail("State directory must not be a symlink"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let lock = options()?
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        regular(&lock, true)?;
        lock.try_lock()
            .map_err(|_| fail("Another federation command owns the state lock"))?;
        let keypath = dir.join("identity.pk8");
        let bytes = match read(&keypath, 1024, true) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Never regenerate a missing identity for an existing ledger.
                if fs::symlink_metadata(dir.join("ledger.json")).is_ok() {
                    return Err(fail("Identity missing from existing federation store"));
                }
                let pk = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .map_err(|_| fail("Key generation failed"))?;
                create(&keypath, pk.as_ref())?;
                pk.as_ref().to_vec()
            }
            Err(e) => return Err(e),
        };
        let key = Ed25519KeyPair::from_pkcs8(&bytes)
            .map_err(|_| fail("Invalid private identity file"))?;
        File::open(dir)?.sync_all()?;
        let ledger = match read(&dir.join("ledger.json"), STORE_BYTES, true) {
            Ok(b) => serde_json::from_slice(&b).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ledger::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            dir: dir.to_owned(),
            key,
            ledger,
            _lock: StoreLock(lock),
        })
    }
    fn id(&self) -> String {
        format!("ed25519:{}", hex(self.key.public_key().as_ref()))
    }
    fn save(&mut self, next: Ledger) -> Result<()> {
        let data = serde_json::to_vec(&next).map_err(io::Error::other)?;
        if data.len() > STORE_BYTES {
            return Err(fail("Federation store byte quota reached"));
        }
        // Unique create-new temporary names, never follow or truncate a planted path.
        let temp = self.dir.join(format!(
            "ledger-{}-{}.tmp",
            std::process::id(),
            next.sequence
        ));
        create(&temp, &data)?;
        if let Err(e) = fs::rename(&temp, self.dir.join("ledger.json")) {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }
        File::open(&self.dir)?.sync_all()?;
        self.ledger = next;
        Ok(())
    }
    fn trust(&mut self, id: &str, label: &str, roots: Vec<String>, docs: bool) -> Result<()> {
        public(id)?;
        if id == self.id()
            || label.is_empty()
            || label.len() > 128
            || label.chars().any(char::is_control)
            || roots.is_empty()
            || roots.len() > 128
            || roots.iter().any(|r| !control_context::valid_id(r))
        {
            return Err(fail("Invalid peer, label or explicit root scope"));
        }
        let mut next = self.ledger.clone();
        let peer = Peer {
            id: id.into(),
            label: label.into(),
            roots,
            allow_documents: docs,
            revoked: false,
        };
        if let Some(p) = next.peers.iter_mut().find(|p| p.id == id) {
            *p = peer;
        } else {
            if next.peers.len() >= 128 {
                return Err(fail("Peer limit reached"));
            }
            next.peers.push(peer);
        }
        self.save(next)
    }
    fn revoke(&mut self, id: &str) -> Result<()> {
        let mut next = self.ledger.clone();
        next.peers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| fail("Unknown peer"))?
            .revoked = true;
        self.save(next)
    }
    fn export(&mut self, bundle: Bundle, recipient: &str, timestamp: u64) -> Result<Envelope> {
        public(recipient)?;
        if recipient == self.id() {
            return Err(fail("Cannot export to self"));
        }
        bundle.validate()?;
        let payload = serde_json::to_string(&bundle).map_err(io::Error::other)?;
        if payload.len() > PAYLOAD {
            return Err(fail("Payload limit exceeded"));
        }
        let mut next = self.ledger.clone();
        next.sequence = next
            .sequence
            .checked_add(1)
            .ok_or_else(|| fail("Sequence exhausted"))?;
        let mut e = Envelope {
            protocol: PROTOCOL.into(),
            sender: self.id(),
            recipient: recipient.into(),
            kind: "lens_bundle".into(),
            sequence: next.sequence,
            resource_id: hash(&payload),
            issued_at: timestamp,
            expires_at: timestamp
                .checked_add(86400)
                .ok_or_else(|| fail("Clock overflow"))?,
            payload,
            signature: String::new(),
        };
        e.signature = hex(self.key.sign(&e.preimage()).as_ref());
        self.save(next)?; // reserve sequence durably before any exported bytes escape
        Ok(e)
    }
    fn import(&mut self, e: Envelope, timestamp: u64) -> Result<bool> {
        if e.protocol != PROTOCOL
            || e.kind != "lens_bundle"
            || e.sequence == 0
            || e.recipient != self.id()
            || e.sender == e.recipient
            || e.payload.len() > PAYLOAD
        {
            return Err(fail("Invalid envelope protocol, kind, recipient or size"));
        }
        let peer = self
            .ledger
            .peers
            .iter()
            .find(|p| p.id == e.sender && !p.revoked)
            .ok_or_else(|| fail("Sender not trusted or revoked"))?;
        if e.expires_at <= timestamp
            || e.issued_at > timestamp.saturating_add(300)
            || e.expires_at <= e.issued_at
            || e.expires_at - e.issued_at > WEEK
        {
            return Err(fail("Invalid or expired validity interval"));
        }
        if e.resource_id != hash(&e.payload) {
            return Err(fail("Payload digest mismatch"));
        }
        signature::UnparsedPublicKey::new(&signature::ED25519, public(&e.sender)?)
            .verify(&e.preimage(), &unhex(&e.signature, 64)?)
            .map_err(|_| fail("Invalid signature"))?;
        let bundle: Bundle = serde_json::from_str(&e.payload).map_err(io::Error::other)?;
        bundle.validate()?;
        if bundle.roots.iter().any(|r| !peer.roots.contains(r))
            || (!peer.allow_documents && !bundle.documents.is_empty())
        {
            return Err(fail("Bundle exceeds peer root/document scope"));
        }
        if let Some(previous) = self
            .ledger
            .imports
            .iter()
            .find(|p| p.sender == e.sender && p.sequence == e.sequence)
        {
            if previous.preimage() != e.preimage() || previous.signature != e.signature {
                return Err(fail("Sequence conflict"));
            }
            return Ok(false);
        }
        if self.ledger.imports.len() >= 256 {
            return Err(fail(
                "Import quota reached; replay records are never evicted",
            ));
        }
        let mut next = self.ledger.clone();
        next.imports.push(e);
        self.save(next)?;
        Ok(true)
    }
}

const HELP: &str = "control federation identity\ncontrol federation trust NODE --label NAME --root LENS [--root LENS] [--allow-documents]\ncontrol federation revoke NODE\ncontrol federation export LENS --recipient NODE --output FILE [--include-documents]\ncontrol federation import FILE\ncontrol federation inspect INDEX\ncontrol federation status\n\nState: .control/federation (or CONTROL_FEDERATION_DIR). Imports are quarantined; no network or remote execution.";
fn parse_options(
    args: &[String],
    flags: &[&str],
    switches: &[&str],
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if switches.contains(&args[i].as_str()) {
            if out.iter().any(|(k, _)| k == &args[i]) {
                return Err(fail("Repeated switch"));
            }
            out.push((args[i].clone(), String::new()));
            i += 1;
        } else if flags.contains(&args[i].as_str()) {
            let value = args
                .get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| fail("Missing option value"))?;
            if args[i] != "--root" && out.iter().any(|(k, _)| k == &args[i]) {
                return Err(fail("Repeated option"));
            }
            out.push((args[i].clone(), value.clone()));
            i += 2;
        } else {
            return Err(fail("Unknown argument; use control federation help"));
        }
    }
    Ok(out)
}
fn value<'a>(opts: &'a [(String, String)], name: &str) -> Result<&'a str> {
    opts.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| fail(&format!("Missing {name}")))
}
fn selected(root: &str, include: bool) -> Result<Bundle> {
    // Read a snapshot only; storage::load mutates run status on startup and must not be used here.
    let state: crate::State = match read(
        &crate::config::state("state.json").map_err(io::Error::other)?,
        STORE_BYTES,
        false,
    ) {
        Ok(b) => serde_json::from_slice(&b).map_err(io::Error::other)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => crate::State::initial(),
        Err(e) => return Err(e),
    };
    let docs: Vec<_> = state
        .documents
        .iter()
        .map(|d| DocumentInput {
            id: d.id.clone(),
            title: d.title.clone(),
            revision: d.revision,
            body: d.body.clone(),
        })
        .collect();
    let roots = vec![root.to_owned()];
    let resolved = state
        .lens_library
        .resolve(&roots, &docs)
        .map_err(io::Error::other)?;
    if !include && !resolved.documents.is_empty() {
        return Err(fail(
            "Lens contains documents; explicit --include-documents required",
        ));
    }
    let definitions = resolved
        .lenses
        .iter()
        .map(|v| state.lens_library.get(&v.id).unwrap().clone())
        .collect();
    Ok(Bundle {
        roots,
        definitions,
        documents: resolved.documents,
    })
}
fn display(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
pub fn run(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    if command == "help" || command == "--help" {
        println!("{HELP}");
        return Ok(());
    }
    let dir = std::env::var_os("CONTROL_FEDERATION_DIR")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| crate::config::state("federation"))
        .map_err(io::Error::other)?;
    // Parse all external inputs before creating state where practical.
    let mut store = Store::open(&dir)?;
    match command {
        "identity" if args.len() == 1 => println!("{}", store.id()),
        "trust" if args.len() >= 2 => {
            let opts = parse_options(&args[2..], &["--label", "--root"], &["--allow-documents"])?;
            store.trust(
                &args[1],
                value(&opts, "--label")?,
                opts.iter()
                    .filter(|(k, _)| k == "--root")
                    .map(|(_, v)| v.clone())
                    .collect(),
                opts.iter().any(|(k, _)| k == "--allow-documents"),
            )?;
            println!("Peer pinned; inbound lens scope saved.");
        }
        "revoke" if args.len() == 2 => {
            store.revoke(&args[1])?;
            println!("Peer revoked; replay history retained.");
        }
        "export" if args.len() >= 2 => {
            let opts = parse_options(
                &args[2..],
                &["--recipient", "--output"],
                &["--include-documents"],
            )?;
            let recipient = value(&opts, "--recipient")?;
            let output = Path::new(value(&opts, "--output")?);
            let bundle = selected(
                &args[1],
                opts.iter().any(|(k, _)| k == "--include-documents"),
            )?;
            let titles: Vec<_> = bundle.documents.iter().map(|d| display(&d.title)).collect();
            let e = store.export(bundle, recipient, now()?)?;
            let bytes = serde_json::to_vec_pretty(&e).map_err(io::Error::other)?;
            if bytes.len() > CARRIER {
                return Err(fail("Carrier limit exceeded"));
            }
            create(output, &bytes)?;
            println!(
                "Exported {} bytes, {}. Documents: {:?}",
                bytes.len(),
                e.resource_id,
                titles
            );
        }
        "import" if args.len() == 2 => {
            let bytes = read(Path::new(&args[1]), CARRIER, false)?;
            let e = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            let added = store.import(e, now()?)?;
            println!(
                "{}",
                if added {
                    "Accepted into quarantine; active context unchanged."
                } else {
                    "Duplicate; existing receipt retained."
                }
            );
        }
        "status" if args.len() == 1 => {
            println!(
                "Node: {}\nPeers: {}\nQuarantined deliveries: {}",
                store.id(),
                store.ledger.peers.len(),
                store.ledger.imports.len()
            );
            for p in &store.ledger.peers {
                println!(
                    "{} {} roots={:?} documents={} {}",
                    p.id,
                    display(&p.label),
                    p.roots,
                    p.allow_documents,
                    if p.revoked { "revoked" } else { "pinned" }
                );
            }
            for (i, e) in store.ledger.imports.iter().enumerate() {
                println!(
                    "[{i}] {} from {} sequence={}",
                    e.resource_id, e.sender, e.sequence
                );
            }
        }
        "inspect" if args.len() == 2 => {
            let index: usize = args[1].parse().map_err(io::Error::other)?;
            let e = store
                .ledger
                .imports
                .get(index)
                .ok_or_else(|| fail("Unknown quarantine index"))?;
            println!("Sender: {}\nResource: {}\nSequence: {}\nExpires: {}\nQuarantined; no local activation.\n{}",e.sender,e.resource_id,e.sequence,e.expires_at,display(&e.payload));
        }
        _ => return Err(fail(HELP)),
    }
    Ok(())
}

#[cfg(test)]
mod tests;
