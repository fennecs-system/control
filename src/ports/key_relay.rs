//! The key-injecting relay for one attempt (docs/PORTS.md, "Key-injecting
//! relay"): which API-key turns go through it ([`decide`]), what such a turn
//! is granted instead of its key ([`grant`]), and serving the relay inside
//! the turn's container for exactly as long as the turn runs ([`during`]).
//! Stopping the relay is what revokes the attempt's placeholder.
use super::inject::{AttemptCap, AttemptId, InjectionRule, Limits, Token};
use super::inject_edge::{AuditFile, Relay, Timing, Tls, Upstream};
use super::inject_keys::{AccountKeys, KeyAccount};
use crate::accounts::{AccountKind, Store};
use crate::backend::Backend;
use control_agent::{Granted, KeyDelivery, Proxied};
use control_isolation::{ExecSpec, Input, Isolation, Stream, Streams, User};
use std::{
    fmt, io,
    num::NonZeroU16,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

/// How a turn on an API-key account receives its key.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Decision {
    /// Through the relay under this rule.
    Proxied(&'static InjectionRule),
    /// Mounted read-only, for this reason.
    Mounted(&'static str),
    /// Refused: the node requires proxied keys and this backend has none.
    Refused(&'static str),
}
/// The backend's declared delivery under the node's `require_proxied_keys`.
pub(crate) fn decide(delivery: KeyDelivery, require_proxied: bool) -> Decision {
    match (delivery, require_proxied) {
        (KeyDelivery::Proxied(rule), _) => Decision::Proxied(rule),
        (KeyDelivery::Mounted(reason), false) => Decision::Mounted(reason),
        (KeyDelivery::Mounted(reason), true) => Decision::Refused(reason),
    }
}
impl Decision {
    /// How `control accounts list`, `check`, doctor and the providers page
    /// show an API-key account's delivery.
    pub(crate) fn describe(self) -> String {
        match self {
            Self::Proxied(_) => "delivery: proxied".to_owned(),
            Self::Mounted(reason) => {
                format!("delivery: mounted ({reason}); warning: the key is readable in its container")
            }
            Self::Refused(reason) => {
                format!("delivery: refused (mounted keys are off by require_proxied_keys; {reason})")
            }
        }
    }
}
/// The decision for an API-key account of `backend` on this node.
pub(crate) fn decision(backend: Backend) -> Decision {
    decide(
        backend.agent().key_delivery(),
        control::permissions::read().require_proxied_keys,
    )
}

/// One proxied attempt's relay before it runs: the rule, the capability its
/// placeholder unlocks, the key source and the container user it runs as.
pub(crate) struct KeyRelay {
    rule: &'static InjectionRule,
    cap: AttemptCap<KeyAccount>,
    keys: AccountKeys,
    owner: User,
    port: NonZeroU16,
}
impl fmt::Debug for KeyRelay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyRelay({}, {})", self.cap.account(), self.rule.origin.host())
    }
}

/// The login a turn of `account` gets, with the relay a proxied key needs.
/// Any account but an API key, and an API key its backend mounts, is located
/// as before; a proxied key is located (its file must be private) but never
/// handed over: the turn gets a fresh placeholder for this attempt.
pub(crate) fn grant(
    account: &crate::logins::Account,
    store: &Store<Backend>,
    attempt: &str,
) -> io::Result<(Granted, Option<KeyRelay>)> {
    let located = crate::logins::locate(account)?;
    if account.kind != AccountKind::ApiKey {
        return Ok((located, None));
    }
    let rule = match decision(account.id.backend) {
        Decision::Proxied(rule) => rule,
        Decision::Mounted(_) => return Ok((located, None)),
        Decision::Refused(reason) => {
            return Err(io::Error::other(format!(
                "This node requires proxied API keys (require_proxied_keys), and {} mounts its key: {reason}.",
                account.id.backend.label()
            )));
        }
    };
    let mut bytes = [0; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| io::Error::other("Could not make the attempt's key placeholder"))?;
    let cap = AttemptCap::new(
        AttemptId(attempt.to_owned()),
        KeyAccount(account.id.clone()),
        Token::from_bytes(bytes),
    );
    let port = port()?;
    let placeholder = cap.placeholder(rule.placeholder).expose().to_owned();
    let granted = Granted::relayed(located.owner(), Proxied::new(placeholder, port));
    let relay = KeyRelay {
        rule,
        cap,
        keys: AccountKeys(store.clone()),
        owner: located.owner(),
        port,
    };
    Ok((granted, Some(relay)))
}
/// The port the relay listens on in the turn's container. Tests run the relay
/// on this machine, so each takes a free port of its own.
fn port() -> io::Result<NonZeroU16> {
    #[cfg(test)]
    {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        NonZeroU16::new(port).ok_or_else(|| io::Error::other("No free port"))
    }
    #[cfg(not(test))]
    Ok(control_agent::cli::RELAY_PORT)
}

/// Runs `turn` with `relay`, if any, served inside `container` until `turn`
/// returns; the relay and its placeholder end with it.
pub(crate) fn during<R>(
    relay: Option<KeyRelay>,
    isolation: &dyn Isolation,
    container: &str,
    turn: impl FnOnce() -> R,
) -> R {
    let Some(relay) = relay else {
        return turn();
    };
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            #[cfg(test)]
            if let Some(provider) = fakes::provider(&relay.cap.account().to_string()) {
                return serve(relay, provider, isolation, container, &stop);
            }
            // Without TLS every request would fail; the CLI reports its own error.
            if let Ok(tls) = Tls::new() {
                serve(relay, tls, isolation, container, &stop);
            }
        });
        let result = turn();
        stop.store(true, Ordering::Release);
        result
    })
}
/// Execs `control-tcp-relay listen-multi` into `container` as its user and
/// hands each stream to the edge, starting it again if it stops, until `stop`.
/// Before the container runs the exec fails at once, so the first rounds
/// only wait for it.
fn serve<U: Upstream + Send + Sync + 'static>(
    relay: KeyRelay,
    upstream: U,
    isolation: &dyn Isolation,
    container: &str,
    stop: &AtomicBool,
) {
    let KeyRelay {
        rule,
        cap,
        keys,
        owner,
        port,
    } = relay;
    let Ok(audit) = AuditFile::node() else {
        return;
    };
    let edge = Arc::new(Relay::new(
        rule,
        cap,
        keys,
        upstream,
        audit,
        Limits::DEFAULT,
        Timing::DEFAULT,
    ));
    let exec = ExecSpec {
        container: container.into(),
        user: Some(owner),
        input: Input::Attached,
        // The host's heartbeat keeps it alive; it stops within seconds of the host.
        argv: [
            "/bin/control-tcp-relay",
            "listen-multi",
            &port.to_string(),
            &u64::MAX.to_string(),
        ]
        .map(str::to_owned)
        .to_vec(),
        ..ExecSpec::default()
    };
    let running = || !stop.load(Ordering::Acquire);
    while running() {
        let edge = edge.clone();
        let _ = super::multiplex::serve(
            || isolation.exec(&exec, Streams::new(Stream::Piped, Stream::Piped, Stream::Null)),
            &running,
            &|| {},
            move |connection, link| super::inject_edge::session(&edge, connection, link),
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// Fake providers for tests that drive whole attempts: an account's relay
/// sends its requests to a plain local server instead of the provider.
#[cfg(test)]
pub(crate) mod fakes {
    use super::super::inject::Origin;
    use super::super::inject_edge::Upstream;
    use std::{
        io,
        net::{SocketAddr, TcpStream},
        sync::Mutex,
    };
    static PROVIDERS: Mutex<Vec<(String, SocketAddr)>> = Mutex::new(Vec::new());
    /// Sends the relay of every attempt on `account` (`backend+name`, unique
    /// to its test) to `address`.
    pub(crate) fn register(account: &str, address: SocketAddr) {
        PROVIDERS.lock().unwrap().push((account.to_owned(), address));
    }
    pub(super) fn provider(account: &str) -> Option<Plain> {
        PROVIDERS
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(name, _)| name == account)
            .map(|(_, address)| Plain(*address))
    }
    pub(crate) struct Plain(SocketAddr);
    impl Upstream for Plain {
        type Conn = TcpStream;
        fn connect(&self, _: &Origin) -> io::Result<TcpStream> {
            TcpStream::connect(self.0)
        }
    }
}

#[cfg(test)]
mod tests;
