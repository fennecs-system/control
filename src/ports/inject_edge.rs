//! The effectful edge of the key-injecting relay (docs/PORTS.md, "Key-injecting
//! relay"). It reads one request head from a byte stream, asks
//! [`inject::plan`] what may be sent, splices the account's key into the
//! planned head while writing it, forwards exactly the declared body, then
//! copies the response back as it arrives (server-sent events flow unbuffered)
//! until the provider closes. One request per connection; every request,
//! relayed or refused, leaves one metadata line in `secrets-audit.jsonl`.
//! No backend uses it yet.
use super::inject::{
    self, AttemptCap, Budget, Deny, HEAD_LIMIT, InjectionRule, Limits, Line, Millis, Origin, Outbound, ParsedHead,
    Refusal, Retention, Ticket,
};
use super::multiplex_transport::{Incoming, Transport};
use serde::{Deserialize, Serialize};
use std::{
    fmt, fs,
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Lends the edge an account's key for one call. The key is never kept:
/// implementations read it per call and clear it after `f` returns.
pub(crate) trait SecretSource<A> {
    fn with_secret<R>(&self, account: &A, f: impl FnOnce(&[u8]) -> R) -> io::Result<R>;
}

/// A byte stream whose reads can time out.
pub(crate) trait Conn: Read + Write {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
}
impl Conn for TcpStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
}
type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;
impl Conn for TlsStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(timeout)
    }
}

/// Opens a connection to a rule's fixed origin.
pub(crate) trait Upstream {
    type Conn: Conn;
    fn connect(&self, origin: &Origin) -> io::Result<Self::Conn>;
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// TLS from the host to the provider, with rustls (already linked through
/// reqwest) and the web PKI roots. The name resolves here, never in a container.
pub(crate) struct Tls(Arc<rustls::ClientConfig>);
impl Tls {
    pub(crate) fn new() -> io::Result<Self> {
        Self::trusting(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
    }
    fn trusting(roots: rustls::RootCertStore) -> io::Result<Self> {
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self(Arc::new(config)))
    }
}
impl Upstream for Tls {
    type Conn = TlsStream;
    fn connect(&self, origin: &Origin) -> io::Result<TlsStream> {
        let name = rustls::pki_types::ServerName::try_from(origin.host()).map_err(io::Error::other)?;
        let socket = (origin.host(), origin.port())
            .to_socket_addrs()?
            .find_map(|address| TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok())
            .ok_or_else(|| io::Error::other("Provider unreachable"))?;
        socket.set_nodelay(true)?;
        let connection = rustls::ClientConnection::new(self.0.clone(), name).map_err(io::Error::other)?;
        Ok(rustls::StreamOwned::new(connection, socket))
    }
}

/// How long a request may take.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// To receive the whole head.
    pub(crate) head: Duration,
    /// Between two reads of the body or the response.
    pub(crate) idle: Duration,
    /// For the whole exchange.
    pub(crate) total: Duration,
}
impl Timing {
    pub(crate) const DEFAULT: Self = Self {
        head: Duration::from_secs(10),
        idle: Duration::from_secs(300),
        total: Duration::from_secs(3600),
    };
}

/// Why the edge answered a request itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Denial {
    Refused(Refusal),
    Budget(Deny),
    HeadTimeout,
    BodyIncomplete,
    KeyUnavailable,
    Unreachable,
}
impl Denial {
    fn status(self) -> (u16, &'static str) {
        match self {
            Self::Refused(refusal) => (refusal.status().code(), refusal.status().reason()),
            Self::Budget(_) => (429, "Too Many Requests"),
            Self::HeadTimeout => (408, "Request Timeout"),
            Self::BodyIncomplete => (400, "Bad Request"),
            Self::KeyUnavailable => (503, "Service Unavailable"),
            Self::Unreachable => (502, "Bad Gateway"),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.name(),
            Self::Budget(deny) => deny.name(),
            Self::HeadTimeout => "head_timeout",
            Self::BodyIncomplete => "body_incomplete",
            Self::KeyUnavailable => "key_unavailable",
            Self::Unreachable => "upstream_unreachable",
        }
    }
}
/// Why a response stopped before the provider closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
    Idle,
    TimeLimit,
    Bytes,
    UpstreamError,
    ClientGone,
}
impl Cut {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle_timeout",
            Self::TimeLimit => "time_limit",
            Self::Bytes => Deny::Bytes.name(),
            Self::UpstreamError => "upstream_error",
            Self::ClientGone => "client_gone",
        }
    }
}

/// One audit line: metadata only. No header, body, query string,
/// placeholder or key ever goes in it.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    time: u64,
    attempt: String,
    account: String,
    method: Option<String>,
    host: String,
    route_class: Option<String>,
    status: Option<u16>,
    bytes_in: u64,
    bytes_out: u64,
    ms: u64,
    refusal: Option<String>,
}
/// The audit log, pruned by [`inject::prune`] whenever it is appended to.
pub(crate) struct AuditFile {
    path: PathBuf,
    retention: Retention,
    lock: Mutex<()>,
}
impl AuditFile {
    /// `<state>/secrets-audit.jsonl`.
    pub(crate) fn node() -> io::Result<Self> {
        let path = crate::config::state("secrets-audit.jsonl").map_err(io::Error::other)?;
        Ok(Self::at(path, Retention::DEFAULT))
    }
    fn at(path: PathBuf, retention: Retention) -> Self {
        Self {
            path,
            retention,
            lock: Mutex::new(()),
        }
    }
    fn append(&self, record: &Record) -> io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let _held = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("Audit log unavailable"))?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        let limit = self.retention.max_bytes as usize + HEAD_LIMIT;
        let existing = match crate::config::read_regular(&self.path, limit, true) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let lines: Vec<&[u8]> = existing.split_inclusive(|&b| b == b'\n').collect();
        let meta: Vec<Line> = lines
            .iter()
            .map(|text| Line {
                time: serde_json::from_slice::<Record>(text).ok().map(|r| r.time),
                bytes: text.len() as u64,
            })
            .collect();
        let dropped = inject::prune(&meta, record.time, line.len() as u64, self.retention);
        if dropped.is_empty() {
            return fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(&self.path)?
                .write_all(&line);
        }
        let kept: Vec<u8> = lines
            .iter()
            .enumerate()
            .filter(|(at, _)| !dropped.contains(at))
            .flat_map(|(_, text)| text.iter().copied())
            .chain(line)
            .collect();
        let directory = self.path.parent().ok_or_else(|| io::Error::other("Audit log path"))?;
        let name = self.path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        crate::config::write_in(directory, name, &kept)
    }
}

/// What one exchange did, for its audit line.
#[derive(Default)]
struct Trace {
    method: Option<&'static str>,
    route: Option<String>,
    status: Option<u16>,
    bytes_in: u64,
    bytes_out: u64,
}

/// One attempt's relay: its rule, capability, key source, upstream, budget
/// and audit log. Every stream of the attempt is served by [`Relay::serve`].
pub(crate) struct Relay<A, S, U> {
    rule: &'static InjectionRule,
    cap: AttemptCap<A>,
    secrets: S,
    upstream: U,
    audit: AuditFile,
    timing: Timing,
    budget: Mutex<Budget>,
    started: Instant,
}
impl<A: fmt::Display, S: SecretSource<A>, U: Upstream> Relay<A, S, U> {
    pub(crate) fn new(
        rule: &'static InjectionRule,
        cap: AttemptCap<A>,
        secrets: S,
        upstream: U,
        audit: AuditFile,
        limits: Limits,
        timing: Timing,
    ) -> Self {
        Self {
            rule,
            cap,
            secrets,
            upstream,
            audit,
            timing,
            budget: Mutex::new(Budget::new(limits)),
            started: Instant::now(),
        }
    }

    /// Serves the one request `client` sends, then closes it.
    pub(crate) fn serve(&self, mut client: impl Conn) {
        let started = Instant::now();
        let mut trace = Trace::default();
        let refusal = match self.exchange(&mut client, &mut trace, started) {
            Ok(cut) => cut.map(Cut::name),
            // Closed before sending anything: nothing was requested.
            Err(None) => return,
            Err(Some(denial)) => {
                let (code, reason) = denial.status();
                let body = format!("Control key relay: {}\n", denial.name());
                let _ = super::proxy::respond(&mut client, &format!("{code} {reason}"), &body);
                trace.status = Some(code);
                Some(denial.name())
            }
        };
        let record = Record {
            time: crate::config::unix_seconds(),
            attempt: self.cap.attempt().0.clone(),
            account: self.cap.account().to_string(),
            method: trace.method.map(str::to_owned),
            host: self.rule.origin.host().to_owned(),
            route_class: trace.route,
            status: trace.status,
            bytes_in: trace.bytes_in,
            bytes_out: trace.bytes_out,
            ms: started.elapsed().as_millis() as u64,
            refusal: refusal.map(str::to_owned),
        };
        // Losing an audit line must not hold up the attempt's request.
        let _ = self.audit.append(&record);
    }

    fn exchange<C: Conn>(
        &self,
        client: &mut C,
        trace: &mut Trace,
        started: Instant,
    ) -> Result<Option<Cut>, Option<Denial>> {
        let (head, early) = read_head(client, started + self.timing.head)?;
        trace.bytes_in = head.len() as u64;
        let parsed = ParsedHead::parse(&head).map_err(Denial::Refused)?;
        trace.method = Some(parsed.method.as_str());
        let outbound = inject::plan(&parsed, self.rule, &self.cap).map_err(Denial::Refused)?;
        trace.route = Some(outbound.route.to_string());
        let ticket = self.admit(trace.bytes_in + outbound.content_length)?;
        let result = self.forward(client, &outbound, &early, &ticket, trace, started + self.timing.total);
        self.release(ticket, trace.bytes_out);
        Ok(result?)
    }

    fn admit(&self, request: u64) -> Result<Ticket, Denial> {
        let now = Millis(self.started.elapsed().as_millis() as u64);
        let mut budget = self.budget.lock().map_err(|_| Denial::Budget(Deny::Streams))?;
        let (next, ticket) = budget.admit(now, request).map_err(Denial::Budget)?;
        *budget = next;
        Ok(ticket)
    }
    fn release(&self, ticket: Ticket, response: u64) {
        if let Ok(mut budget) = self.budget.lock() {
            *budget = budget.release(ticket, response);
        }
    }

    /// Sends the planned request with the key spliced in, then the body, and
    /// copies the response back.
    fn forward<C: Conn>(
        &self,
        client: &mut C,
        outbound: &Outbound,
        early: &[u8],
        ticket: &Ticket,
        trace: &mut Trace,
        deadline: Instant,
    ) -> Result<Option<Cut>, Denial> {
        let mut upstream = self
            .upstream
            .connect(&outbound.origin)
            .map_err(|_| Denial::Unreachable)?;
        // The key exists only inside this call: spliced into the writes of
        // the head and dropped (cleared) before the body is read.
        self.secrets
            .with_secret(self.cap.account(), |secret| {
                let parts = outbound.splice(secret).map_err(|_| Denial::KeyUnavailable)?;
                parts
                    .iter()
                    .try_for_each(|part| upstream.write_all(part))
                    .map_err(|_| Denial::Unreachable)
            })
            .map_err(|_| Denial::KeyUnavailable)??;
        let length = outbound.content_length;
        let first = &early[..early.len().min(length as usize)];
        upstream.write_all(first).map_err(|_| Denial::Unreachable)?;
        copy_body(
            client,
            &mut upstream,
            length - first.len() as u64,
            self.timing.idle,
            deadline,
        )?;
        upstream.flush().map_err(|_| Denial::Unreachable)?;
        trace.bytes_in += length;
        pump(
            &mut upstream,
            client,
            ticket.allowance(),
            self.timing.idle,
            deadline,
            trace,
        )
    }
}

/// Bytes read, or why reading stopped.
fn read_some(from: &mut impl Conn, into: &mut [u8], idle: Duration, deadline: Instant) -> Result<usize, Cut> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Cut::TimeLimit);
    }
    from.set_read_timeout(Some(idle.min(left)))
        .map_err(|_| Cut::UpstreamError)?;
    from.read(into).or_else(|error| match error.kind() {
        // rustls reports a close without close_notify as an early EOF.
        io::ErrorKind::UnexpectedEof => Ok(0),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut if idle < left => Err(Cut::Idle),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Err(Cut::TimeLimit),
        _ => Err(Cut::UpstreamError),
    })
}

/// The head up to and including its blank line, and any bytes read after it.
/// `None` when the client closed without sending anything.
fn read_head(client: &mut impl Conn, deadline: Instant) -> Result<(Vec<u8>, Vec<u8>), Option<Denial>> {
    let mut data = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let early = data.split_off(end + 4);
            return Ok((data, early));
        }
        if data.len() > HEAD_LIMIT {
            return Err(Some(Denial::Refused(Refusal::HeadTooLarge)));
        }
        match read_some(client, &mut chunk, Duration::MAX, deadline) {
            Ok(0) if data.is_empty() => return Err(None),
            Ok(0) => return Err(Some(Denial::Refused(Refusal::IncompleteHead))),
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(Cut::Idle | Cut::TimeLimit) => return Err(Some(Denial::HeadTimeout)),
            Err(Cut::Bytes | Cut::UpstreamError | Cut::ClientGone) => return Err(None),
        }
    }
}

/// Copies exactly `length` more body bytes from the client.
fn copy_body(
    client: &mut impl Conn,
    upstream: &mut impl Write,
    length: u64,
    idle: Duration,
    deadline: Instant,
) -> Result<(), Denial> {
    let mut chunk = [0; 16 * 1024];
    let mut left = length;
    while left > 0 {
        let want = chunk.len().min(left as usize);
        match read_some(client, &mut chunk[..want], idle, deadline) {
            Ok(0) | Err(_) => return Err(Denial::BodyIncomplete),
            Ok(n) => {
                upstream.write_all(&chunk[..n]).map_err(|_| Denial::Unreachable)?;
                left -= n as u64;
            }
        }
    }
    Ok(())
}

/// The status code of a response that starts with `prefix`.
fn status_of(prefix: &[u8]) -> Option<u16> {
    let code = prefix.strip_prefix(b"HTTP/1.1 ")?.get(..3)?;
    std::str::from_utf8(code).ok()?.parse().ok()
}

/// Copies the response to the client as each read returns, flushing every
/// chunk, until the provider closes. A provider that answers nothing is a 502.
fn pump(
    upstream: &mut impl Conn,
    client: &mut impl Conn,
    allowance: u64,
    idle: Duration,
    deadline: Instant,
    trace: &mut Trace,
) -> Result<Option<Cut>, Denial> {
    let mut chunk = [0; 16 * 1024];
    let mut prefix = Vec::new();
    loop {
        let n = match read_some(upstream, &mut chunk, idle, deadline) {
            Ok(0) => return Ok(None),
            Ok(n) => n,
            Err(_) if trace.bytes_out == 0 => return Err(Denial::Unreachable),
            Err(cut) => return Ok(Some(cut)),
        };
        if prefix.len() < 12 {
            prefix.extend_from_slice(&chunk[..n.min(12 - prefix.len())]);
            trace.status = status_of(&prefix);
        }
        // Up to the attempt's allowance, so a cut response spends it all.
        let fits = n.min(allowance.saturating_sub(trace.bytes_out) as usize);
        if client.write_all(&chunk[..fits]).and_then(|()| client.flush()).is_err() {
            return Ok(Some(Cut::ClientGone));
        }
        trace.bytes_out += fits as u64;
        if fits < n {
            return Ok(Some(Cut::Bytes));
        }
    }
}

/// Hands one stream from the multiplex relay to `relay`, through a private
/// loopback pair as the egress proxy does.
pub(super) fn session<A: fmt::Display, S: SecretSource<A>, U: Upstream>(
    relay: &Relay<A, S, U>,
    connection: Incoming,
    link: Arc<Transport>,
) {
    super::proxy::bridge(connection, link, |client| relay.serve(client));
}

#[cfg(test)]
mod tests;
